//! NSApplication setup, the app delegate, and one window per file.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSBackingStoreType, NSBeep, NSBitmapImageFileType, NSMenuItem, NSModalResponseOK, NSOpenPanel,
    NSWindow, NSWindowDelegate, NSWindowStyleMask, NSWindowTabbingMode,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDictionary, NSNotification, NSObject, NSObjectProtocol, NSPoint,
    NSRect, NSSize, NSString, NSURL,
};
use sudopen_core::{Document, PreviewKind};

use crate::ipc::{self, Request};
use crate::preview::{self, Preview};
use crate::view::{FirstPaint, TextView};
use crate::{install, menu};

/// One open file.
struct Win {
    path: PathBuf,
    window: Retained<NSWindow>,
    text: Retained<TextView>,
    kind: Option<PreviewKind>,
    preview: Option<Preview>,
    showing_preview: bool,
}

thread_local! {
    static DELEGATE: OnceCell<Retained<AppDelegate>> = const { OnceCell::new() };
    static WINDOWS: RefCell<Vec<Win>> = const { RefCell::new(Vec::new()) };
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

        #[unsafe(method(togglePreview:))]
        fn toggle_preview(&self, _sender: Option<&AnyObject>) {
            let mtm = self.mtm();
            if let Some(w) = NSApplication::sharedApplication(mtm).keyWindow() {
                let on = !with_win(&w, |win| win.showing_preview).unwrap_or(true);
                set_preview(mtm, &w, on, None);
            }
        }

        /// Reached only when the first responder does not copy by itself:
        /// the CSV table.
        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            let Some(w) = NSApplication::sharedApplication(self.mtm()).keyWindow() else { return };
            let text = with_win(&w, |win| win.preview.as_ref().filter(|_| win.showing_preview).and_then(|p| p.selected_rows_tsv()));
            match text.flatten() {
                Some(t) => {
                    let pb = objc2_app_kit::NSPasteboard::generalPasteboard();
                    pb.clearContents();
                    pb.setString_forType(&NSString::from_str(&t), unsafe { objc2_app_kit::NSPasteboardTypeString });
                }
                None => NSBeep(),
            }
        }

        #[unsafe(method(installCommandLineTool:))]
        fn install_command_line_tool(&self, _sender: Option<&AnyObject>) {
            install::install_with_feedback(self.mtm());
        }

        #[unsafe(method(sudopenFirstLaunch:))]
        fn first_launch(&self, _sender: Option<&AnyObject>) {
            install::maybe_prompt(self.mtm());
        }

        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            if item.action() == Some(sel!(togglePreview:)) {
                validate_preview_item(self.mtm(), item)
            } else {
                true
            }
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
                open_path(mtm, &p, None, false, None);
            }
            activate(mtm);
            // Ask about the `sudopen` command once the first window is up.
            unsafe {
                let _: () = msg_send![self, performSelector: sel!(sudopenFirstLaunch:), withObject: None::<&AnyObject>, afterDelay: 0.8f64];
            }
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<NSURL>) {
            let mtm = self.mtm();
            for url in urls.iter() {
                if let Some(p) = url.path() {
                    open_path(mtm, Path::new(&p.to_string()), None, false, None);
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
                    ws.retain(|w| Retained::as_ptr(&w.window) as usize != ptr);
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

/// Enables "Show Preview" for previewable files and flips its title.
fn validate_preview_item(mtm: MainThreadMarker, item: &NSMenuItem) -> bool {
    let key = NSApplication::sharedApplication(mtm).keyWindow();
    let state = key.and_then(|w| with_win(&w, |win| (win.kind, win.showing_preview)));
    let (enabled, title) = match state {
        Some((Some(_), true)) => (true, "Show Source"),
        Some((Some(_), false)) => (true, "Show Preview"),
        _ => (false, "Show Preview"),
    };
    item.setTitle(&NSString::from_str(title));
    enabled
}

fn with_win<R>(window: &NSWindow, f: impl FnOnce(&Win) -> R) -> Option<R> {
    let ptr = window as *const NSWindow;
    WINDOWS.with_borrow(|ws| ws.iter().find(|w| Retained::as_ptr(&w.window) == ptr).map(f))
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
                    open_path(mtm, &t.path, t.line, t.preview, Some(fp));
                }
            }
            Request::Activate => {
                activate(mtm);
                if WINDOWS.with_borrow(Vec::is_empty) {
                    show_open_panel(mtm);
                }
            }
            Request::Snapshot(p) => snapshot(mtm, p, tx.clone()),
            Request::Quit => NSApplication::sharedApplication(mtm).terminate(None),
        }
        drop(tx);
    });
}

pub fn activate(mtm: MainThreadMarker) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

pub fn open_path(
    mtm: MainThreadMarker,
    path: &Path,
    line: Option<u64>,
    preview: bool,
    mut fp: Option<FirstPaint>,
) {
    let existing = WINDOWS.with_borrow(|ws| {
        ws.iter().find(|w| w.path == path).map(|w| (w.window.clone(), w.text.clone(), w.showing_preview))
    });
    if let Some((window, text, showing)) = existing {
        if let Some(line) = line {
            if showing {
                set_preview(mtm, &window, false, None);
            }
            text.goto_line(line);
        } else if preview && !showing {
            set_preview(mtm, &window, true, None);
        }
        window.makeKeyAndOrderFront(None);
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
                show_error(mtm, path, &e.to_string());
            }
            return;
        }
    };
    if let Some(fp) = fp.as_mut() {
        fp.mark("load");
    }
    let window = create_window(mtm, path, line, doc, fp);
    if preview && line.is_none() {
        let fp = with_win(&window, |w| w.text.take_first_paint()).flatten();
        set_preview(mtm, &window, true, fp);
    }
}

fn create_window(
    mtm: MainThreadMarker,
    path: &Path,
    line: Option<u64>,
    doc: Document,
    mut fp: Option<FirstPaint>,
) -> Retained<NSWindow> {
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

    let kind = doc.preview;
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
    WINDOWS.with_borrow_mut(|ws| {
        ws.push(Win {
            path: path.to_path_buf(),
            window: window.clone(),
            text: view,
            kind,
            preview: None,
            showing_preview: false,
        })
    });
    window
}

/// Shows the preview (`on`) or the text in `window`. The preview is built
/// on first use and kept.
fn set_preview(mtm: MainThreadMarker, window: &NSWindow, on: bool, fp: Option<FirstPaint>) {
    let Some((text, kind, built, showing)) =
        with_win(window, |w| (w.text.clone(), w.kind, w.preview.is_some(), w.showing_preview))
    else {
        return;
    };
    if on == showing {
        return;
    }
    if !on {
        window.setContentView(Some(&text));
        window.makeFirstResponder(Some(&text));
        WINDOWS.with_borrow_mut(|ws| set_showing(ws, window, false));
        return;
    }
    let Some(kind) = kind else {
        NSBeep();
        return;
    };
    if !built {
        let frame = window.contentView().map_or(text.frame(), |v| v.frame());
        // Build outside the WINDOWS borrow: an error alert runs a modal loop.
        match preview::build(mtm, kind, text.shared_source(), text.cancel_flag(), frame, fp) {
            Ok(p) => WINDOWS.with_borrow_mut(|ws| {
                if let Some(w) = ws.iter_mut().find(|w| std::ptr::eq(Retained::as_ptr(&w.window), window)) {
                    w.preview = Some(p);
                }
            }),
            Err(e) => {
                show_error(mtm, text.path().as_path(), &e);
                return;
            }
        }
    }
    let views = with_win(window, |w| w.preview.as_ref().map(|p| (p.root.clone(), p.focus.clone()))).flatten();
    if let Some((root, focus)) = views {
        window.setContentView(Some(&root));
        window.makeFirstResponder(Some(&focus));
        WINDOWS.with_borrow_mut(|ws| set_showing(ws, window, true));
    }
}

fn set_showing(ws: &mut [Win], window: &NSWindow, on: bool) {
    if let Some(w) = ws.iter_mut().find(|w| std::ptr::eq(Retained::as_ptr(&w.window), window)) {
        w.showing_preview = on;
    }
}

fn show_open_panel(mtm: MainThreadMarker) {
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setAllowsMultipleSelection(true);
    panel.setCanChooseDirectories(false);
    activate(mtm);
    if panel.runModal() == NSModalResponseOK {
        for url in panel.URLs().iter() {
            if let Some(p) = url.path() {
                open_path(mtm, Path::new(&p.to_string()), None, false, None);
            }
        }
    }
}

pub fn show_error(mtm: MainThreadMarker, path: &Path, msg: &str) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!(
        "Could not open “{}”",
        path.file_name().unwrap_or_default().to_string_lossy()
    )));
    alert.setInformativeText(&NSString::from_str(msg));
    activate(mtm);
    alert.runModal();
}

/// Debug helper behind `sudopen --snapshot`: PNG of the front window.
fn snapshot(mtm: MainThreadMarker, out: PathBuf, tx: Sender<String>) {
    let shown = out.clone();
    let reply = move |res: Result<(), String>| {
        let _ = tx.send(match res {
            Ok(()) => format!("wrote {}", shown.display()),
            Err(e) => format!("error\t{e}"),
        });
    };
    let Some(window) = NSApplication::sharedApplication(mtm)
        .keyWindow()
        .or_else(|| WINDOWS.with_borrow(|ws| ws.last().map(|w| w.window.clone())))
    else {
        return reply(Err("no window open".into()));
    };
    // Web previews render in another process and need WebKit's own snapshot.
    let out2 = out.clone();
    let tx2 = reply.clone();
    let web = WINDOWS.with_borrow(|ws| {
        ws.iter()
            .find(|w| Retained::as_ptr(&w.window) == Retained::as_ptr(&window))
            .filter(|w| w.showing_preview)
            .and_then(|w| w.preview.as_ref())
            .is_some_and(|p| preview::snapshot_web(p, out2.clone(), tx2.clone()))
    });
    if web {
        return;
    }
    let res = (|| {
        let view = window.contentView().ok_or("window has no view")?;
        let rect = view.bounds();
        let rep = view.bitmapImageRepForCachingDisplayInRect(rect).ok_or("could not create bitmap")?;
        view.cacheDisplayInRect_toBitmapImageRep(rect, &rep);
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
            .ok_or("could not encode PNG")?;
        if data.writeToFile_atomically(&NSString::from_str(&out.to_string_lossy()), true) {
            Ok(())
        } else {
            Err(format!("could not write {}", out.display()))
        }
    })();
    reply(res);
}
