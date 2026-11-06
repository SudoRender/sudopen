//! Syntax highlighting with syntect, done per block of lines.
//!
//! Only the blocks on screen are highlighted. Highlighting a block needs the
//! parser state at its first line, which depends on everything above it (are
//! we inside a block comment?). For files up to `EXACT_STATE_LIMIT`, a
//! background thread walks the file once and records that state at every
//! block boundary. Until it reaches a block, the block is highlighted from a
//! fresh state (usually right for code) and flagged as inexact so the view
//! can redo it later.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use syntect::highlighting::{Color, HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

use crate::lines::line_bounds;

pub const BLOCK: u64 = 128;
/// Lines longer than this are shown unhighlighted.
pub const MAX_HL_LINE: usize = 8 * 1024;
/// Files up to this size get exact parser states computed in the background.
pub const EXACT_STATE_LIMIT: u64 = 32 << 20;

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEMES: OnceLock<Themes> = OnceLock::new();

pub struct Themes {
    pub light: Theme,
    pub dark: Theme,
}

pub fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

pub fn themes() -> &'static Themes {
    THEMES.get_or_init(|| {
        use two_face::theme::EmbeddedThemeName as T;
        let set = two_face::theme::extra();
        Themes { light: set.get(T::Github).clone(), dark: set.get(T::OneHalfDark).clone() }
    })
}

/// Loads syntaxes and themes on a background thread so the first window does
/// not wait for them.
pub fn warm_up() {
    std::thread::Builder::new()
        .name("sudopen-warmup".into())
        .spawn(|| {
            syntaxes();
            themes();
        })
        .ok();
}

/// Picks a syntax from the file name, extension, then first line (shebang,
/// `<?xml`, ...). Plain text returns `None`.
pub fn detect(path: &Path, first_line: &[u8]) -> Option<&'static SyntaxReference> {
    let ss = syntaxes();
    let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy()).unwrap_or_default();
    let found = (!name.is_empty())
        .then(|| ss.find_syntax_by_extension(&name))
        .flatten()
        .or_else(|| (!ext.is_empty()).then(|| ss.find_syntax_by_extension(&ext)).flatten())
        .or_else(|| name.strip_prefix('.').and_then(|n| ss.find_syntax_by_extension(n)))
        .or_else(|| {
            let line = std::str::from_utf8(first_line).ok()?;
            ss.find_syntax_by_first_line(line)
        });
    found.filter(|s| s.name != "Plain Text")
}

/// Packs a color as 0xRRGGBBAA.
#[inline]
pub fn pack(c: Color) -> u32 {
    u32::from_be_bytes([c.r, c.g, c.b, c.a])
}

/// A colored byte range within one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub color: u32,
}

pub struct HlBlock {
    pub lines: Vec<Vec<Span>>,
    pub exact: bool,
}

type State = (ParseState, ScopeStack);

/// Parser state that can sit in a `Mutex` shared across threads.
///
/// With the oniguruma backend, `ParseState` holds a cached `onig::Region`,
/// which is a plain heap allocation behind raw pointers. It has no thread
/// affinity, so moving it between threads is fine; the `Mutex` guarantees it
/// is never touched from two threads at once.
struct SendState(State);
// SAFETY: see above.
unsafe impl Send for SendState {}

pub struct DocHighlighter {
    pub syntax: &'static SyntaxReference,
    /// `states[k]` = parser state at the first line of block `k`.
    states: Mutex<Vec<SendState>>,
    builder_done: AtomicBool,
}

impl DocHighlighter {
    pub fn new(syntax: &'static SyntaxReference) -> DocHighlighter {
        DocHighlighter {
            syntax,
            states: Mutex::new(vec![SendState((ParseState::new(syntax), ScopeStack::new()))]),
            builder_done: AtomicBool::new(false),
        }
    }

    /// Number of blocks with an exact starting state.
    pub fn exact_blocks(&self) -> u64 {
        self.states.lock().unwrap().len() as u64
    }

    pub fn builder_done(&self) -> bool {
        self.builder_done.load(Ordering::Acquire)
    }

    pub fn mark_builder_done(&self) {
        self.builder_done.store(true, Ordering::Release);
    }

    /// Walks the whole file recording the state at each block boundary.
    pub fn build_states(&self, bytes: &[u8], cancel: &AtomicBool) {
        let ss = syntaxes();
        let (mut ps, mut stack) = self.states.lock().unwrap()[0].0.clone();
        let mut text = String::new();
        let mut off = 0usize;
        let mut line = 0u64;
        while off < bytes.len() {
            if line > 0 && line.is_multiple_of(BLOCK) {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                self.states.lock().unwrap().push(SendState((ps.clone(), stack.clone())));
            }
            let (end, next) = line_bounds(bytes, off as u64);
            if let Some(ops) = parse(&mut ps, &bytes[off..end as usize], ss, &mut text) {
                for (_, op) in &ops {
                    let _ = stack.apply(op);
                }
            }
            off = next as usize;
            line += 1;
        }
        self.mark_builder_done();
    }

    /// Highlights up to `BLOCK` lines of block `block`, whose first line
    /// starts at byte `start`.
    pub fn highlight_block(&self, theme: &Theme, bytes: &[u8], block: u64, start: u64) -> HlBlock {
        let (exact, (mut ps, stack)) = match self.states.lock().unwrap().get(block as usize) {
            Some(s) => (true, s.0.clone()),
            None => (false, (ParseState::new(self.syntax), ScopeStack::new())),
        };
        let ss = syntaxes();
        let highlighter = Highlighter::new(theme);
        let mut hs = HighlightState::new(&highlighter, stack);
        let mut text = String::new();
        let mut lines = Vec::with_capacity(BLOCK as usize);
        let mut off = start;
        for _ in 0..BLOCK {
            if off > bytes.len() as u64 {
                break;
            }
            let (end, next) = line_bounds(bytes, off);
            let line = &bytes[off as usize..end as usize];
            let mut spans: Vec<Span> = Vec::new();
            if let Some(ops) = parse(&mut ps, line, ss, &mut text) {
                let mut pos = 0usize;
                for (style, piece) in HighlightIterator::new(&mut hs, &ops, &text, &highlighter) {
                    let s = pos;
                    pos += piece.len();
                    let e = pos.min(line.len());
                    if s >= e {
                        continue;
                    }
                    let color = pack(style.foreground);
                    match spans.last_mut() {
                        Some(last) if last.color == color && last.end as usize == s => {
                            last.end = e as u32
                        }
                        _ => spans.push(Span { start: s as u32, end: e as u32, color }),
                    }
                }
            }
            lines.push(spans);
            off = next;
        }
        HlBlock { lines, exact }
    }
}

/// Parses one line. Returns `None` (and leaves the state alone) for lines
/// that are too long or not valid UTF-8.
fn parse(
    ps: &mut ParseState,
    line: &[u8],
    ss: &SyntaxSet,
    text: &mut String,
) -> Option<Vec<(usize, ScopeStackOp)>> {
    if line.len() > MAX_HL_LINE {
        return None;
    }
    let s = std::str::from_utf8(line).ok()?;
    text.clear();
    text.push_str(s);
    text.push('\n');
    ps.parse_line(text, ss).ok()
}

/// Highlighted blocks kept for redraws. FIFO eviction.
pub struct BlockCache {
    map: HashMap<u64, Arc<HlBlock>>,
    order: VecDeque<u64>,
    cap: usize,
}

impl BlockCache {
    pub fn new(cap: usize) -> BlockCache {
        BlockCache { map: HashMap::new(), order: VecDeque::new(), cap }
    }

    pub fn get_or_insert(&mut self, block: u64, f: impl FnOnce() -> HlBlock) -> Arc<HlBlock> {
        if let Some(b) = self.map.get(&block) {
            return b.clone();
        }
        let b = Arc::new(f());
        self.map.insert(block, b.clone());
        self.order.push_back(block);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        b
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    /// Drops inexact blocks that now have an exact state. Returns true if
    /// any were dropped.
    pub fn drop_stale(&mut self, exact_blocks: u64) -> bool {
        let before = self.map.len();
        self.map.retain(|k, b| b.exact || *k >= exact_blocks);
        self.order.retain(|k| self.map.contains_key(k));
        self.map.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_files() {
        let d = |p: &str, first: &str| detect(Path::new(p), first.as_bytes()).map(|s| s.name.as_str());
        assert_eq!(d("main.rs", ""), Some("Rust"));
        assert_eq!(d("a.ts", ""), Some("TypeScript"));
        assert_eq!(d("Cargo.toml", ""), Some("TOML"));
        assert_eq!(d("Makefile", ""), Some("Makefile"));
        assert_eq!(d("script", "#!/bin/bash"), Some("Bourne Again Shell (bash)"));
        assert_eq!(d("notes.txt", ""), None);
    }

    #[test]
    fn block_comment_needs_exact_state() {
        // Line 128 (first line of block 1) is inside a block comment opened on line 0.
        let mut src = String::from("/*\n");
        for _ in 0..200 {
            src.push_str("let x = 1;\n");
        }
        src.push_str("*/\n");
        let syntax = detect(Path::new("a.rs"), b"").unwrap();
        let theme = &themes().light;
        let hl = DocHighlighter::new(syntax);
        let bytes = src.as_bytes();
        let start = src.match_indices('\n').nth(127).unwrap().0 as u64 + 1;

        let approx = hl.highlight_block(theme, bytes, 1, start);
        assert!(!approx.exact);
        hl.build_states(bytes, &AtomicBool::new(false));
        let exact = hl.highlight_block(theme, bytes, 1, start);
        assert!(exact.exact);
        // Inside the comment the whole line is one color; outside it is not.
        assert_eq!(exact.lines[0].len(), 1);
        assert!(approx.lines[0].len() > 1);
    }
}
