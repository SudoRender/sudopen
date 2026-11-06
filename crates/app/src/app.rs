//! NSApplication setup, the app delegate, and one window per file.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSBackingStoreType, NSBitmapImageFileType, NSModalResponseOK, NSOpenPanel, NSWindow,
    NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSURL,
};
use sudopen_core::Document;

use crate::ipc::{self, Request};
use crate::menu;
use crate::view::{FirstPaint, TextView};

thread_local! {
    static DELEGATE: OnceCell<Retained<AppDelegate>> = const { OnceCell::new() };
    static WINDOWS: RefCell<Vec<(PathBuf, Retained<NSWindow>)>> = const { RefCell::new(Vec::new()) };
    static CASCADE: Cell<Option<NSPoint>> = const { Cell::new(None) };
}

/// Process start (ns). Startup phases are kept so `--timing` can show them
/// on a cold start.
pub static MAIN_NS: AtomicU64 = AtomicU64::new(0);
static STARTUP: std::sync::Mutex<Vec<(&'static str, u64)>> = std::sync::Mutex::new(Vec::new());

fn startup_mark(phase: &'static str) {
    STARTUP.lock().unwrap().push((phase, ipc::now_ns()));
}

pub struct DelegateIvars {
    pending: RefCell<Vec<PathBuf>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and AppDelegate does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SudopenAppDelegate"]
    #[ivars = DelegateIvars]
    struct AppDelegate;

    impl AppDelegate {
        #[unsafe(method(openDocument:))]
        fn open_document(&self, _sender: Option<&AnyObject>) {
            show_open_panel(self.mtm());
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            startup_mark("launched");
            let mtm = self.mtm();
            let pending = std::mem::take(&mut *self.ivars().pending.borrow_mut());
            for p in pending {
                open_path(mtm, &p, None, None);
            }
            activate(mtm);
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            let mtm = self.mtm();
            for url in urls.iter() {
                if let Some(p) = url.path() {
                    open_path(mtm, Path::new(&p.to_string()), None, None);
                }
            }
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            // Stay running: the next file then opens in a few ms.
            false
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, has_visible: bool) -> bool {
            if !has_visible {
                show_open_panel(self.mtm());
            }
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            ipc::cleanup();
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, n: &NSNotification) {
            let Some(obj) = n.object() else { return };
            let ptr = Retained::as_ptr(&obj) as usize;
            // Release on the next turn of the run loop, not in the middle of
            // AppKit's close.
            DispatchQueue::main().exec_async(move || {
                WINDOWS.with_borrow_mut(|ws| {
                    ws.retain(|(_, w)| Retained::as_ptr(w) as usize != ptr);
                    if ws.is_empty() {
                        CASCADE.set(None);
                    }
                });
            });
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, pending: Vec<PathBuf>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars { pending: RefCell::new(pending) });
        unsafe { msg_send![super(this), init] }
    }
}

pub fn run(initial: Vec<PathBuf>) -> ! {
    startup_mark("exec");
    sudopen_core::highlight::warm_up();
    if let Err(e) = ipc::serve(handle_request) {
        eprintln!("sudopen: command-line socket unavailable: {e}");
    }
    startup_mark("socket");

    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    startup_mark("nsapp");
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    startup_mark("policy");
    let delegate = AppDelegate::new(mtm, initial);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    DELEGATE.with(|d| d.set(delegate).ok());
    menu::install(&app, mtm);
    startup_mark("menu");
    app.run();
    ipc::cleanup();
    std::process::exit(0);
}

/// Runs on a socket thread; hops to the main thread for anything UI.
fn handle_request(req: Request, tx: Sender<String>) {
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().expect("main queue runs on the main thread");
        match req {
            Request::Open { targets, t0, wait } => {
                for t in targets {
                    let path = t.path.to_string_lossy().into_owned();
                    let mut fp = FirstPaint::new(t0, wait.then(|| tx.clone()), path);
                    // Cold start: the app was launched by this request.
                    if MAIN_NS.load(Ordering::Relaxed) > t0 {
                        fp.marks.extend(STARTUP.lock().unwrap().iter().copied());
                    }
                    fp.mark("to-main");
                    open_path(mtm, &t.path, t.line, Some(fp));
                }
            }
            Request::Activate => {
                activate(mtm);
                if WINDOWS.with_borrow(|ws| ws.is_empty()) {
                    show_open_panel(mtm);
                }
            }
            Request::Snapshot(p) => {
                let reply = match snapshot(mtm, &p) {
                    Ok(()) => format!("wrote {}", p.display()),
                    Err(e) => format!("error\t{e}"),
                };
                let _ = tx.send(reply);
            }
            Request::Quit => NSApplication::sharedApplication(mtm).terminate(None),
        }
        drop(tx);
    });
}

pub fn activate(mtm: MainThreadMarker) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

pub fn open_path(mtm: MainThreadMarker, path: &Path, line: Option<u64>, mut fp: Option<FirstPaint>) {
    let existing = WINDOWS.with_borrow(|ws| ws.iter().find(|(p, _)| p == path).map(|(_, w)| w.clone()));
    if let Some(w) = existing {
        if let (Some(line), Some(view)) = (line, w.contentView())
            && let Ok(view) = view.downcast::<TextView>() {
                view.goto_line(line);
            }
        w.makeKeyAndOrderFront(None);
        activate(mtm);
        if let Some(FirstPaint { t0, tx: Some(tx), path, .. }) = fp {
            let ms = ipc::now_ns().saturating_sub(t0) as f64 / 1e6;
            let _ = tx.send(format!("painted\t{ms:.1}\t{path} (already open)"));
        }
        return;
    }
    let doc = match Document::open(path) {
        Ok(d) => d,
        Err(e) => {
            if let Some(FirstPaint { tx: Some(tx), .. }) = &fp {
                let _ = tx.send(format!("error\t{}: {e}", path.display()));
            } else {
                show_error(mtm, path, &e);
            }
            return;
        }
    };
    if let Some(fp) = fp.as_mut() {
        fp.mark("load");
    }
    create_window(mtm, path, line, doc, fp);
}

fn create_window(
    mtm: MainThreadMarker,
    path: &Path,
    line: Option<u64>,
    doc: Document,
    mut fp: Option<FirstPaint>,
) {
    let size = NSSize::new(980.0, 760.0);
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: we keep our own reference in WINDOWS.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    let name = path.file_name().map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy());
    window.setTitle(&NSString::from_str(&name));
    window.setRepresentedURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))));
    window.setContentMinSize(NSSize::new(320.0, 200.0));
    if let Some(fp) = fp.as_mut() {
        fp.mark("window");
    }

    let view = TextView::new(mtm, doc, rect, fp);
    window.setSubtitle(&NSString::from_str(&view.subtitle()));
    window.setBackgroundColor(Some(&view.background()));
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    if let Some(line) = line {
        view.goto_line(line);
    }
    view.mark("view");

    match CASCADE.get() {
        None => {
            window.center();
            CASCADE.set(Some(window.cascadeTopLeftFromPoint(NSPoint::new(0.0, 0.0))));
        }
        Some(p) => CASCADE.set(Some(window.cascadeTopLeftFromPoint(p))),
    }
    DELEGATE.with(|d| {
        if let Some(d) = d.get() {
            window.setDelegate(Some(ProtocolObject::from_ref(&**d)));
        }
    });
    window.makeKeyAndOrderFront(None);
    view.mark("shown");
    activate(mtm);
    view.mark("activated");
    WINDOWS.with_borrow_mut(|ws| ws.push((path.to_path_buf(), window)));
}

fn show_open_panel(mtm: MainThreadMarker) {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setAllowsMultipleSelection(true);
    panel.setCanChooseDirectories(false);
    activate(mtm);
    if panel.runModal() == NSModalResponseOK {
        for url in panel.URLs().iter() {
            if let Some(p) = url.path() {
                open_path(mtm, Path::new(&p.to_string()), None, None);
            }
        }
    }
}

fn show_error(mtm: MainThreadMarker, path: &Path, e: &std::io::Error) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!(
        "Could not open “{}”",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
    alert.setInformativeText(&NSString::from_str(&e.to_string()));
    activate(mtm);
    alert.runModal();
}

/// Debug helper behind `sudopen --snapshot`: PNG of the front window.
fn snapshot(mtm: MainThreadMarker, path: &Path) -> Result<(), String> {
    let window = NSApplication::sharedApplication(mtm)
        .keyWindow()
        .or_else(|| WINDOWS.with_borrow(|ws| ws.last().map(|(_, w)| w.clone())))
        .ok_or("no window open")?;
    let view = window.contentView().ok_or("window has no view")?;
    let rect = view.bounds();
    let rep = view.bitmapImageRepForCachingDisplayInRect(rect).ok_or("could not create bitmap")?;
    view.cacheDisplayInRect_toBitmapImageRep(rect, &rep);
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
        .ok_or("could not encode PNG")?;
    if data.writeToFile_atomically(&NSString::from_str(&path.to_string_lossy()), true) {
        Ok(())
    } else {
        Err(format!("could not write {}", path.display()))
    }
}
