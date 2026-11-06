//! Rendered previews behind Cmd+T. Markdown and HTML go in a WKWebView with
//! JavaScript off; CSV/TSV go in a native NSTableView that only parses the
//! rows on screen. Nothing here is created until the first toggle, so it
//! costs nothing on the open path.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use block2::{DynBlock, RcBlock};
use dispatch2::DispatchQueue;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBitmapImageFileType, NSBitmapImageRep, NSFont,
    NSFontWeightRegular, NSImage, NSLineBreakMode, NSScrollView, NSTableColumn, NSTableView,
    NSTableViewColumnAutoresizingStyle, NSTableViewDataSource,
    NSTableViewGridLineStyle, NSTableViewStyle, NSTextAlignment, NSView, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSError, NSInteger, NSNumber, NSObject, NSObjectProtocol,
    NSRect, NSString, NSURL,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationType, WKWebView, WKWebViewConfiguration,
};
use sudopen_core::PreviewKind;
use sudopen_core::markdown;
use sudopen_core::source::Source;
use sudopen_core::table::{CHUNK, Table};

use crate::view::FirstPaint;

pub struct Preview {
    /// View to put in the window.
    pub root: Retained<NSView>,
    /// View that should get keyboard focus.
    pub focus: Retained<NSView>,
    web: Option<Retained<WKWebView>>,
    table: Option<Retained<NSTableView>>,
    // Delegates and data sources are weak references in AppKit/WebKit.
    _nav: Option<Retained<NavDelegate>>,
    _table: Option<Retained<TableSource>>,
}

pub fn build(
    mtm: MainThreadMarker,
    kind: PreviewKind,
    source: Arc<Source>,
    cancel: Arc<AtomicBool>,
    frame: NSRect,
    fp: Option<FirstPaint>,
) -> Result<Preview, String> {
    match kind {
        PreviewKind::Markdown | PreviewKind::Html => web(mtm, kind, &source, frame, fp),
        PreviewKind::Csv | PreviewKind::Tsv => {
            let p = table(mtm, kind, source, cancel, frame);
            if let Some(mut fp) = fp {
                fp.mark("table");
                fp.report();
            }
            Ok(p)
        }
    }
}

fn sizable() -> NSAutoresizingMaskOptions {
    NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable
}

fn ns_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

// ---------------------------------------------------------------- web view

fn web(
    mtm: MainThreadMarker,
    kind: PreviewKind,
    source: &Source,
    frame: NSRect,
    fp: Option<FirstPaint>,
) -> Result<Preview, String> {
    let path = source.path();
    let page = match kind {
        PreviewKind::Html => path.to_path_buf(),
        _ => render_markdown(source)?,
    };

    let config = unsafe { WKWebViewConfiguration::new(mtm) };
    unsafe { config.defaultWebpagePreferences().setAllowsContentJavaScript(false) };
    let web = unsafe { WKWebView::initWithFrame_configuration(WKWebView::alloc(mtm), frame, &config) };
    web.setAutoresizingMask(sizable());
    // No white flash in dark mode while the page loads: the window
    // background shows through until the page paints.
    unsafe {
        let _: () = msg_send![&*web, setValue: &*NSNumber::new_bool(false), forKey: &*NSString::from_str("drawsBackground")];
    }
    let nav = NavDelegate::new(mtm, fp);
    unsafe {
        web.setNavigationDelegate(Some(ProtocolObject::from_ref(&*nav)));
        web.loadFileURL_allowingReadAccessToURL(&ns_url(&page), &ns_url(Path::new("/")));
    }
    let view: Retained<NSView> = Retained::into_super(web.clone());
    Ok(Preview { root: view.clone(), focus: view, web: Some(web), table: None, _nav: Some(nav), _table: None })
}

/// Renders the Markdown to an HTML file in the cache folder and returns it.
fn render_markdown(source: &Source) -> Result<PathBuf, String> {
    let path = source.path();
    let text = String::from_utf8_lossy(source.bytes());
    let dir = path.parent().unwrap_or(Path::new("/")).to_string_lossy().into_owned();
    let base = format!("{}/", markdown::path_to_file_url(dir.trim_end_matches('/')));
    let title = path.file_name().unwrap_or_default().to_string_lossy();
    let html = markdown::to_html(&text, &title, &base);

    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    let out_dir = home.join("Library/Caches/sudopen/preview");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let out = out_dir.join(format!("{:016x}.html", h.finish()));
    std::fs::write(&out, html).map_err(|e| format!("could not write preview: {e}"))?;
    Ok(out)
}

pub struct NavIvars {
    first_paint: RefCell<Option<FirstPaint>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SudopenNavDelegate"]
    #[ivars = NavIvars]
    pub struct NavDelegate;

    unsafe impl NSObjectProtocol for NavDelegate {}

    unsafe impl WKNavigationDelegate for NavDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            web: &WKWebView,
            action: &WKNavigationAction,
            handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            handler.call((policy_for(web, action),));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, _web: &WKWebView, _nav: Option<&WKNavigation>) {
            if let Some(mut fp) = self.ivars().first_paint.take() {
                fp.mark("page-loaded");
                fp.report();
            }
        }
    }
);

impl NavDelegate {
    fn new(mtm: MainThreadMarker, fp: Option<FirstPaint>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(NavIvars { first_paint: RefCell::new(fp) });
        unsafe { msg_send![super(this), init] }
    }
}

/// Links: same-page anchors scroll, other local files open in sudopen,
/// everything else opens in the default app (browser, mail).
fn policy_for(web: &WKWebView, action: &WKNavigationAction) -> WKNavigationActionPolicy {
    if unsafe { action.navigationType() } != WKNavigationType::LinkActivated {
        return WKNavigationActionPolicy::Allow;
    }
    let Some(url) = unsafe { action.request() }.URL() else {
        return WKNavigationActionPolicy::Allow;
    };
    let no_fragment = |u: &NSURL| {
        u.absoluteString().map(|s| s.to_string().split('#').next().unwrap_or("").to_string())
    };
    let current = unsafe { web.URL() };
    if current.is_some_and(|c| no_fragment(&c) == no_fragment(&url)) {
        return WKNavigationActionPolicy::Allow;
    }
    if url.isFileURL() {
        if let Some(p) = url.path() {
            let p = PathBuf::from(p.to_string());
            DispatchQueue::main().exec_async(move || {
                let mtm = MainThreadMarker::new().expect("main queue");
                crate::app::open_path(mtm, &p, None, false, None);
            });
        }
    } else {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
    WKNavigationActionPolicy::Cancel
}

impl Preview {
    /// Selected table rows as tab-separated text, or `None` if this is not
    /// a table or nothing is selected.
    pub fn selected_rows_tsv(&self) -> Option<String> {
        let (tv, src) = (self.table.as_ref()?, self._table.as_ref()?);
        let rows = tv.selectedRowIndexes();
        if rows.count() == 0 {
            return None;
        }
        let ncols = tv.numberOfColumns() as usize - 1; // minus the row-number column
        let mut out = String::new();
        let mut i = rows.firstIndex();
        while i != objc2_foundation::NSNotFound as usize {
            let record = i as u64 + 1;
            let chunk = src.chunk(record / CHUNK);
            let fields = chunk.as_ref().and_then(|c| c.get((record % CHUNK) as usize));
            let line: Vec<&str> = (0..ncols)
                .map(|c| fields.and_then(|f| f.get(c)).map_or("", String::as_str))
                .collect();
            out.push_str(&line.join("\t"));
            out.push('\n');
            i = rows.indexGreaterThanIndex(i);
        }
        Some(out)
    }
}

/// PNG of a web preview (WKWebView content lives in another process, so
/// `cacheDisplayInRect` cannot capture it). Calls `done` with the result.
pub fn snapshot_web(p: &Preview, out: PathBuf, done: impl Fn(Result<(), String>) + 'static) -> bool {
    let Some(web) = &p.web else { return false };
    let block = RcBlock::new(move |img: *mut NSImage, _err: *mut NSError| {
        let res = unsafe { img.as_ref() }
            .and_then(|img| img.TIFFRepresentation())
            .and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff))
            .and_then(|rep| unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) })
            .filter(|png| png.writeToFile_atomically(&NSString::from_str(&out.to_string_lossy()), true))
            .map(|_| ())
            .ok_or_else(|| "could not snapshot the web view".to_string());
        done(res);
    });
    unsafe { web.takeSnapshotWithConfiguration_completionHandler(None, &block) };
    true
}

// ---------------------------------------------------------------- table

pub struct TableIvars {
    table: Arc<Table>,
    chunks: RefCell<HashMap<u64, Rc<Vec<Vec<String>>>>>,
    view: RefCell<Weak<NSTableView>>,
    rows_seen: Cell<u64>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop impl.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SudopenTableSource"]
    #[ivars = TableIvars]
    pub struct TableSource;

    impl TableSource {
        #[unsafe(method(sudopenTick:))]
        fn tick(&self, _arg: Option<&AnyObject>) {
            let Some(view) = self.ivars().view.borrow().load() else { return };
            let p = self.ivars().table.progress();
            let rows = p.records.saturating_sub(1);
            if rows != self.ivars().rows_seen.get() {
                self.ivars().rows_seen.set(rows);
                view.noteNumberOfRowsChanged();
            }
            if !p.done {
                self.schedule_tick();
            }
        }
    }

    unsafe impl NSObjectProtocol for TableSource {}

    unsafe impl NSTableViewDataSource for TableSource {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            let rows = self.ivars().table.progress().records.saturating_sub(1);
            self.ivars().rows_seen.set(rows);
            rows as NSInteger
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn value(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<AnyObject>> {
            self.cell_text(column, row).map(|text| {
                let s: Retained<NSObject> = Retained::into_super(NSString::from_str(&text));
                Retained::into_super(s)
            })
        }
    }

);

impl TableSource {
    fn new(mtm: MainThreadMarker, table: Arc<Table>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TableIvars {
            table,
            chunks: RefCell::new(HashMap::new()),
            view: RefCell::new(Weak::default()),
            rows_seen: Cell::new(0),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn cell_text(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<String> {
        let id = column?.identifier().to_string();
        if id == "#" {
            return Some((row + 1).to_string());
        }
        let col: usize = id.parse().ok()?;
        // Row 0 of the table is record 1: record 0 is the header.
        let record = row as u64 + 1;
        let chunk = self.chunk(record / CHUNK)?;
        chunk.get((record % CHUNK) as usize)?.get(col).cloned()
    }

    fn chunk(&self, k: u64) -> Option<Rc<Vec<Vec<String>>>> {
        if let Some(c) = self.ivars().chunks.borrow().get(&k) {
            return Some(c.clone());
        }
        let c = Rc::new(self.ivars().table.chunk(k)?);
        let mut chunks = self.ivars().chunks.borrow_mut();
        if chunks.len() > 256 {
            chunks.clear();
        }
        chunks.insert(k, c.clone());
        Some(c)
    }

    fn schedule_tick(&self) {
        unsafe {
            let _: () = msg_send![self, performSelector: sel!(sudopenTick:), withObject: None::<&AnyObject>, afterDelay: 0.25f64];
        }
    }
}

fn table(
    mtm: MainThreadMarker,
    kind: PreviewKind,
    source: Arc<Source>,
    cancel: Arc<AtomicBool>,
    frame: NSRect,
) -> Preview {
    let data = Table::new(source, kind == PreviewKind::Tsv, cancel);
    let ctrl = TableSource::new(mtm, data.clone());
    let sample = ctrl.chunk(0).unwrap_or_default();
    let header = sample.first().cloned().unwrap_or_default();
    let ncols = sample.iter().map(Vec::len).max().unwrap_or(0).max(data.progress().max_fields);

    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setAutoresizingMask(sizable());

    let tv = NSTableView::initWithFrame(NSTableView::alloc(mtm), frame);
    tv.setStyle(NSTableViewStyle::Plain);
    tv.setUsesAlternatingRowBackgroundColors(true);
    tv.setGridStyleMask(NSTableViewGridLineStyle::SolidVerticalGridLineMask);
    tv.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::NoColumnAutoresizing);
    tv.setAllowsMultipleSelection(true);
    tv.setRowHeight(20.0);

    let font = NSFont::monospacedDigitSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular });
    let char_w = 7.2;
    let add_column = |id: &str, title: &str, width: f64, right: bool| {
        let col = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str(id));
        col.setTitle(&NSString::from_str(title));
        col.setWidth(width);
        col.setMinWidth(36.0);
        let cell = col.dataCell();
        unsafe {
            let _: () = msg_send![&*cell, setFont: &*font];
            let _: () = msg_send![&*cell, setLineBreakMode: NSLineBreakMode::ByTruncatingTail];
            if right {
                let _: () = msg_send![&*cell, setAlignment: NSTextAlignment::Right];
            }
        }
        tv.addTableColumn(&col);
    };

    let rows_hint = (data.progress().records.max(sample.len() as u64)).max(1000);
    add_column("#", "", (rows_hint.ilog10() as f64 + 2.0) * char_w + 8.0, true);
    for i in 0..ncols {
        let title = header.get(i).map(String::as_str).unwrap_or("");
        let widest = sample
            .iter()
            .filter_map(|r| r.get(i))
            .map(|f| f.chars().count())
            .max()
            .unwrap_or(4)
            .clamp(4, 56);
        let numeric = sample.iter().skip(1).filter_map(|r| r.get(i)).all(|f| f.trim().parse::<f64>().is_ok());
        add_column(&i.to_string(), title, widest as f64 * char_w + 18.0, numeric && sample.len() > 1);
    }

    unsafe {
        tv.setDataSource(Some(ProtocolObject::from_ref(&*ctrl)));
    }
    scroll.setDocumentView(Some(&tv));
    *ctrl.ivars().view.borrow_mut() = Weak::from_retained(&tv);
    if !data.progress().done {
        ctrl.schedule_tick();
    }

    Preview {
        root: Retained::into_super(scroll),
        focus: Retained::into_super(Retained::into_super(tv.clone())),
        web: None,
        table: Some(tv),
        _nav: None,
        _table: Some(ctrl),
    }
}
