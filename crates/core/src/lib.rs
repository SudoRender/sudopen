//! Platform-independent part of sudopen: loading files, indexing lines,
//! detecting the language, highlighting and laying out visible text.
//! No UI code lives here so a Windows front end can reuse all of it.

pub mod highlight;
pub mod layout;
pub mod lines;
pub mod markdown;
pub mod source;
pub mod table;

pub use syntect;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use highlight::{BlockCache, DocHighlighter, EXACT_STATE_LIMIT, HlBlock};
use lines::{LineIndex, Progress};
use source::Source;
use syntect::highlighting::Theme;

/// Files up to this size are indexed before the window shows (a few ms).
const SYNC_INDEX_LIMIT: u64 = 4 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Markdown,
    Html,
    Csv,
    Tsv,
}

pub fn preview_kind(path: &Path) -> Option<PreviewKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "md" | "markdown" | "mdown" | "mkd" | "mdx" => PreviewKind::Markdown,
        "html" | "htm" | "xhtml" => PreviewKind::Html,
        "csv" => PreviewKind::Csv,
        "tsv" | "tab" => PreviewKind::Tsv,
        _ => return None,
    })
}

pub struct Document {
    source: Arc<Source>,
    index: Arc<LineIndex>,
    hl: Option<Arc<DocHighlighter>>,
    cancel: Arc<AtomicBool>,
    pub binary: bool,
    pub preview: Option<PreviewKind>,
}

impl Document {
    pub fn open(path: &Path) -> io::Result<Document> {
        Ok(Self::from_source(Source::open(path)?))
    }

    pub fn from_source(source: Source) -> Document {
        let source = Arc::new(source);
        let cancel = Arc::new(AtomicBool::new(false));
        let index = Arc::new(LineIndex::new(source.len()));
        let bytes = source.bytes();

        if source.len() <= SYNC_INDEX_LIMIT {
            index.build(bytes, &cancel);
        } else {
            let (s, i, c) = (source.clone(), index.clone(), cancel.clone());
            spawn("sudopen-index", move || i.build(s.bytes(), &c));
        }

        let head = &bytes[..bytes.len().min(8192)];
        let binary = memchr::memchr(0, head).is_some();
        let first_nl = memchr::memchr(b'\n', head);
        // Minified one-line files: lines that long are never highlighted, and
        // walking them for parser state would cost a full scan.
        let minified = first_nl.is_none() && bytes.len() > head.len();
        let first_line = &head[..first_nl.unwrap_or(head.len()).min(256)];
        let syntax = if binary || minified { None } else { highlight::detect(source.path(), first_line) };
        let hl = syntax.map(|s| Arc::new(DocHighlighter::new(s)));
        if let Some(h) = &hl {
            if source.len() <= EXACT_STATE_LIMIT {
                let (s, h, c) = (source.clone(), h.clone(), cancel.clone());
                spawn("sudopen-highlight", move || h.build_states(s.bytes(), &c));
            } else {
                h.mark_builder_done();
            }
        }

        let preview = preview_kind(source.path());
        Document { source, index, hl, cancel, binary, preview }
    }

    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.source.bytes()
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// Shared handle on the bytes, for previews built from the same file.
    pub fn shared_source(&self) -> Arc<Source> {
        self.source.clone()
    }

    /// Cancelled when the document is dropped; hand it to background work
    /// tied to this document.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn path(&self) -> &Path {
        self.source.path()
    }

    pub fn progress(&self) -> Progress {
        self.index.progress()
    }

    /// Start of line `n`: exact if indexed, otherwise a guess on a line
    /// boundary. The bool says whether it is exact.
    pub fn line_start(&self, n: u64, total: u64) -> (u64, bool) {
        match self.index.line_start(self.bytes(), n) {
            Some(off) => (off, true),
            None => (self.index.approx_line_start(self.bytes(), n, total), false),
        }
    }

    pub fn syntax_name(&self) -> Option<&'static str> {
        self.hl.as_ref().map(|h| h.syntax.name.as_str())
    }

    pub fn is_highlighted(&self) -> bool {
        self.hl.is_some()
    }

    /// True while background work that changes what is drawn is running.
    pub fn is_busy(&self) -> bool {
        !self.index.progress().done || self.hl.as_ref().is_some_and(|h| !h.builder_done())
    }

    pub fn exact_blocks(&self) -> u64 {
        self.hl.as_ref().map_or(0, |h| h.exact_blocks())
    }

    /// Highlighted block `block` (lines `block * BLOCK ..`), from the cache
    /// when possible. `None` for plain text or if the block is not indexed.
    pub fn highlighted_block(
        &self,
        block: u64,
        theme: &Theme,
        cache: &mut BlockCache,
    ) -> Option<Arc<HlBlock>> {
        let hl = self.hl.as_ref()?;
        let start = self.index.line_start(self.bytes(), block * highlight::BLOCK)?;
        Some(cache.get_or_insert(block, || hl.highlight_block(theme, self.bytes(), block, start)))
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().name(name.into()).spawn(f).ok();
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else if v < 10.0 { format!("{v:.1} {}", UNITS[u]) } else { format!("{v:.0} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(2_300_000_000), "2.3 GB");
    }
}
