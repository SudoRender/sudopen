//! Turning raw line bytes into what is drawn on a monospace grid.
//!
//! Everything here works on raw bytes so that offsets match the file exactly:
//! invalid UTF-8 bytes each count as one U+FFFD column, tabs expand to the
//! next tab stop, control characters show as a middle dot. Only the columns
//! in view are produced.

use unicode_width::UnicodeWidthChar;

use crate::highlight::Span;

pub const TAB: u32 = 4;
/// Lines longer than this are laid out approximately (one byte, one column),
/// so a 200 MB single-line JSON stays instant.
pub const LONG_LINE: usize = 64 * 1024;
/// Run color meaning "use the default foreground".
pub const DEFAULT_COLOR: u32 = 0;

/// Calls `f(byte_offset, char, byte_len)` for each char until it returns false.
pub fn for_each_char(bytes: &[u8], mut f: impl FnMut(usize, char, usize) -> bool) {
    let mut base = 0usize;
    let mut rest = bytes;
    while !rest.is_empty() {
        let (valid, bad) = match std::str::from_utf8(rest) {
            Ok(s) => (s, 0),
            Err(e) => {
                let v = unsafe { std::str::from_utf8_unchecked(&rest[..e.valid_up_to()]) };
                (v, e.error_len().unwrap_or(rest.len() - e.valid_up_to()))
            }
        };
        for (i, c) in valid.char_indices() {
            if !f(base + i, c, c.len_utf8()) {
                return;
            }
        }
        let mut pos = valid.len();
        for _ in 0..bad {
            if !f(base + pos, char::REPLACEMENT_CHARACTER, 1) {
                return;
            }
            pos += 1;
        }
        base += pos;
        rest = &rest[pos..];
    }
}

/// Columns taken by `c` when it starts at column `col`.
#[inline]
pub fn char_cols(c: char, col: u32) -> u32 {
    match c {
        '\t' => TAB - col % TAB,
        c if (c as u32) < 0x20 || c == '\u{7f}' => 1,
        c => c.width().unwrap_or(1) as u32,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    /// UTF-16 offset into `Visible::text`.
    pub start: u32,
    pub len: u32,
    pub color: u32,
}

#[derive(Debug, Default)]
pub struct Visible {
    pub text: String,
    /// Column where `text` starts.
    pub first_col: u32,
    pub runs: Vec<Run>,
}

/// Lays out the part of `line` between columns `col0` and `col1`.
pub fn layout_visible(line: &[u8], spans: &[Span], col0: u32, col1: u32) -> Visible {
    let mut out = Visible::default();
    if line.len() > LONG_LINE {
        let mut s = (col0 as usize).min(line.len());
        while s < line.len() && (line[s] & 0xC0) == 0x80 {
            s += 1;
        }
        let e = (col1 as usize + 4).min(line.len());
        out.first_col = s as u32;
        for_each_char(&line[s..e.max(s)], |_, c, _| {
            out.text.push(display_char(c));
            true
        });
        return out;
    }

    let mut col = 0u32;
    let mut started = false;
    let mut si = 0usize;
    let mut u16_pos = 0u32;
    for_each_char(line, |off, c, _| {
        let w = char_cols(c, col);
        if col < col0 && col + w <= col0 {
            col += w;
            return true;
        }
        if col >= col1 {
            return false;
        }
        if !started {
            started = true;
            out.first_col = col;
        }
        let off = off as u32;
        while si < spans.len() && spans[si].end <= off {
            si += 1;
        }
        let color = match spans.get(si) {
            Some(s) if s.start <= off => s.color,
            _ => DEFAULT_COLOR,
        };
        let before = u16_pos;
        if c == '\t' {
            for _ in 0..w {
                out.text.push(' ');
            }
            u16_pos += w;
        } else {
            let d = display_char(c);
            out.text.push(d);
            u16_pos += d.len_utf16() as u32;
        }
        let len = u16_pos - before;
        match out.runs.last_mut() {
            Some(r) if r.color == color => r.len += len,
            _ => out.runs.push(Run { start: before, len, color }),
        }
        col += w;
        true
    });
    out
}

#[inline]
fn display_char(c: char) -> char {
    if (c as u32) < 0x20 || c == '\u{7f}' { '\u{b7}' } else { c }
}

/// Byte offset in `line` nearest to column position `x` (in columns, may be
/// fractional). Clicking past the middle of a char lands after it.
pub fn byte_at_col(line: &[u8], x: f64) -> usize {
    if x <= 0.0 {
        return 0;
    }
    if line.len() > LONG_LINE {
        let mut b = (x.round() as usize).min(line.len());
        while b < line.len() && (line[b] & 0xC0) == 0x80 {
            b += 1;
        }
        return b;
    }
    let mut col = 0u32;
    let mut result = line.len();
    for_each_char(line, |off, c, _| {
        let w = char_cols(c, col);
        if x < col as f64 + w as f64 / 2.0 {
            result = off;
            return false;
        }
        col += w;
        true
    });
    result
}

/// Column at which byte `byte` of `line` starts.
pub fn col_at_byte(line: &[u8], byte: usize) -> u32 {
    if line.len() > LONG_LINE {
        return byte.min(line.len()) as u32;
    }
    let mut col = 0u32;
    for_each_char(line, |off, c, _| {
        if off >= byte {
            return false;
        }
        col += char_cols(c, col);
        true
    });
    col
}

/// Byte range of the word (or single other char) at `byte`.
pub fn word_at(line: &[u8], byte: usize) -> (usize, usize) {
    if line.len() > LONG_LINE || line.is_empty() {
        return (byte.min(line.len()), byte.min(line.len()));
    }
    let mut chars: Vec<(usize, char, usize)> = Vec::new();
    for_each_char(line, |o, c, l| {
        chars.push((o, c, l));
        true
    });
    let Some(i) = chars.iter().position(|&(o, _, l)| byte < o + l).or(chars.len().checked_sub(1))
    else {
        return (0, 0);
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    if !is_word(chars[i].1) {
        return (chars[i].0, chars[i].0 + chars[i].2);
    }
    let mut a = i;
    while a > 0 && is_word(chars[a - 1].1) {
        a -= 1;
    }
    let mut b = i;
    while b + 1 < chars.len() && is_word(chars[b + 1].1) {
        b += 1;
    }
    (chars[a].0, chars[b].0 + chars[b].2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_bytes_are_one_column_each() {
        let mut seen = Vec::new();
        for_each_char(b"a\xFF\xFEb", |o, c, l| {
            seen.push((o, c, l));
            true
        });
        assert_eq!(
            seen,
            vec![(0, 'a', 1), (1, '\u{FFFD}', 1), (2, '\u{FFFD}', 1), (3, 'b', 1)]
        );
    }

    #[test]
    fn expands_tabs_and_clips_columns() {
        let v = layout_visible(b"\tab", &[], 0, 100);
        assert_eq!(v.text, "    ab");
        let v = layout_visible(b"abcdef", &[], 2, 4);
        assert_eq!((v.text.as_str(), v.first_col), ("cd", 2));
    }

    #[test]
    fn maps_spans_to_runs() {
        let spans = [Span { start: 0, end: 2, color: 7 }, Span { start: 3, end: 4, color: 9 }];
        let v = layout_visible(b"fn x", &spans, 0, 100);
        assert_eq!(
            v.runs,
            vec![
                Run { start: 0, len: 2, color: 7 },
                Run { start: 2, len: 1, color: DEFAULT_COLOR },
                Run { start: 3, len: 1, color: 9 },
            ]
        );
    }

    #[test]
    fn hit_testing_round_trips() {
        let line = "a\tb€c".as_bytes();
        assert_eq!(col_at_byte(line, 2), 4); // 'b' after the tab
        assert_eq!(byte_at_col(line, 4.0), 2);
        assert_eq!(byte_at_col(line, 5.2), 3); // '€' starts at byte 3
        assert_eq!(byte_at_col(line, 99.0), line.len());
    }

    #[test]
    fn selects_words() {
        let line = b"let foo_bar = 1;";
        assert_eq!(word_at(line, 6), (4, 11));
        assert_eq!(word_at(line, 12), (12, 13));
    }
}
