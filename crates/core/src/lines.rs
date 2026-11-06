//! Line index: where each line starts.
//!
//! The index is sparse (one offset every `STRIDE` lines) to keep memory low on
//! huge files; the exact start of a line is found by skipping at most
//! `STRIDE - 1` newlines with memchr. Big files are indexed on a background
//! thread while the window is already showing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use memchr::{memchr, memchr_iter, memrchr};

pub const STRIDE: u64 = 32;

/// Index this much per lock; keeps progress visible on huge files.
const CHUNK: usize = 8 << 20;

#[derive(Default)]
struct State {
    /// `starts[k]` = byte offset of line `k * STRIDE`.
    starts: Vec<u64>,
    newlines: u64,
    scanned: u64,
    max_line_len: u64,
    done: bool,
    total_lines: u64,
}

pub struct LineIndex {
    state: Mutex<State>,
    len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Exact line count when `done`, otherwise an estimate.
    pub lines: u64,
    /// Lines whose start is known exactly.
    pub indexed_lines: u64,
    pub max_line_len: u64,
    pub done: bool,
}

impl LineIndex {
    pub fn new(len: u64) -> LineIndex {
        LineIndex {
            state: Mutex::new(State { starts: vec![0], ..Default::default() }),
            len,
        }
    }

    /// Scans `bytes` and fills the index. Call once, from any thread.
    pub fn build(&self, bytes: &[u8], cancel: &AtomicBool) {
        let mut newlines = 0u64;
        let mut line_start = 0u64;
        let mut max_len = 0u64;
        let mut pending = Vec::new();
        let mut pos = 0usize;
        while pos < bytes.len() {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let end = (pos + CHUNK).min(bytes.len());
            for i in memchr_iter(b'\n', &bytes[pos..end]) {
                let nl = (pos + i) as u64;
                max_len = max_len.max(nl - line_start);
                line_start = nl + 1;
                newlines += 1;
                if newlines.is_multiple_of(STRIDE) {
                    pending.push(line_start);
                }
            }
            pos = end;
            let mut st = self.state.lock().unwrap();
            st.starts.append(&mut pending);
            st.newlines = newlines;
            st.scanned = pos as u64;
            st.max_line_len = max_len.max(pos as u64 - line_start);
        }
        let mut st = self.state.lock().unwrap();
        st.max_line_len = max_len.max(bytes.len() as u64 - line_start);
        st.total_lines = match bytes.last() {
            None => 1,
            Some(b'\n') => newlines,
            Some(_) => newlines + 1,
        };
        st.done = true;
    }

    pub fn progress(&self) -> Progress {
        let st = self.state.lock().unwrap();
        if st.done {
            return Progress {
                lines: st.total_lines,
                indexed_lines: st.total_lines,
                max_line_len: st.max_line_len,
                done: true,
            };
        }
        let estimate = if st.scanned == 0 {
            1
        } else {
            (st.newlines as f64 * self.len as f64 / st.scanned as f64) as u64
        };
        Progress {
            lines: estimate.max(st.newlines + 1),
            indexed_lines: st.newlines,
            max_line_len: st.max_line_len,
            done: false,
        }
    }

    /// Exact start of line `n`, if that part of the file is indexed.
    pub fn line_start(&self, bytes: &[u8], n: u64) -> Option<u64> {
        let (mut off, skip) = {
            let st = self.state.lock().unwrap();
            if !st.done && n > st.newlines {
                return None;
            }
            let block = (n / STRIDE) as usize;
            (*st.starts.get(block)?, n % STRIDE)
        };
        for _ in 0..skip {
            off += memchr(b'\n', &bytes[off as usize..])? as u64 + 1;
        }
        Some(off)
    }

    /// Best guess for the start of line `n` of `total`, used before indexing
    /// reaches it. Always lands on a line boundary.
    pub fn approx_line_start(&self, bytes: &[u8], n: u64, total: u64) -> u64 {
        if n == 0 || bytes.is_empty() {
            return 0;
        }
        let frac = (n as f64 / total.max(1) as f64).min(1.0);
        let guess = ((bytes.len() as f64 * frac) as usize).min(bytes.len());
        // Back up to the start of the line containing `guess`. If `guess` is
        // past the final newline, this lands on the last line.
        let search_end = if guess == bytes.len() && bytes.last() == Some(&b'\n') {
            guess - 1
        } else {
            guess
        };
        match memrchr(b'\n', &bytes[..search_end]) {
            Some(i) => i as u64 + 1,
            None => 0,
        }
    }
}

/// Bounds of the line starting at `start`: (content end, next line start).
/// Content excludes the trailing `\n` and a `\r` before it.
#[inline]
pub fn line_bounds(bytes: &[u8], start: u64) -> (u64, u64) {
    let s = start as usize;
    match memchr(b'\n', &bytes[s..]) {
        Some(i) => {
            let nl = s + i;
            let end = if nl > s && bytes[nl - 1] == b'\r' { nl - 1 } else { nl };
            (end as u64, nl as u64 + 1)
        }
        None => (bytes.len() as u64, bytes.len() as u64 + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(text: &[u8]) -> LineIndex {
        let idx = LineIndex::new(text.len() as u64);
        idx.build(text, &AtomicBool::new(false));
        idx
    }

    #[test]
    fn counts_lines() {
        assert_eq!(index(b"").progress().lines, 1);
        assert_eq!(index(b"a").progress().lines, 1);
        assert_eq!(index(b"a\n").progress().lines, 1);
        assert_eq!(index(b"a\nb").progress().lines, 2);
        assert_eq!(index(b"\n\n").progress().lines, 2);
    }

    #[test]
    fn finds_line_starts_across_strides() {
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let idx = index(text.as_bytes());
        let b = text.as_bytes();
        for n in [0u64, 1, 31, 32, 33, 500, 999] {
            let start = idx.line_start(b, n).unwrap() as usize;
            let (end, _) = line_bounds(b, start as u64);
            assert_eq!(&text[start..end as usize], format!("line {n}"));
        }
        assert_eq!(idx.progress().max_line_len, "line 999".len() as u64);
    }

    #[test]
    fn strips_crlf() {
        let b = b"ab\r\ncd";
        assert_eq!(line_bounds(b, 0), (2, 4));
        assert_eq!(line_bounds(b, 4), (6, 7));
    }

    #[test]
    fn approx_start_is_a_line_boundary() {
        let text = b"aaaa\nbbbb\ncccc\n";
        let idx = LineIndex::new(text.len() as u64);
        assert_eq!(idx.approx_line_start(text, 0, 3), 0);
        assert_eq!(idx.approx_line_start(text, 1, 3), 5);
        assert_eq!(idx.approx_line_start(text, 3, 3), 10);
    }
}
