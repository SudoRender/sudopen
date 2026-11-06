//! Finder Sync extension: adds "Open in sudopen" at the top level of
//! Finder's right-click menu for every file, whatever app normally opens it.
//!
//! The extension is sandboxed and cannot read the files, so it hands the
//! selected paths to the main app as `sudopen://open?path=...&path=...`.

use std::ffi::{CString, c_char, c_int};
use std::path::PathBuf;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::AnyObject;
use objc2::{ClassType, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSMenu, NSMenuItem, NSWorkspace};
use objc2_finder_sync::{FIFinderSync, FIFinderSyncController, FIMenuKind};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSBundle, NSSet, NSSize, NSString, NSURL, NSURLComponents,
    NSURLQueryItem,
};

define_class!(
    // SAFETY: FIFinderSync is designed to be subclassed; no Drop impl and no
    // ivars, so `init` from NSExtensionMain needs no Rust-side setup.
    #[unsafe(super(FIFinderSync))]
    #[name = "SudopenFinderSync"]
    struct FinderSync;

    impl FinderSync {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Option<Retained<Self>> {
            let this: Option<Retained<Self>> = unsafe { msg_send![super(this.set_ivars(())), init] };
            // Watch "/" only. Watching a folder that is in Finder's sidebar
            // (like a drive) makes Finder put our icon on it, the way Dropbox
            // badges its folder.
            let root = NSURL::fileURLWithPath(&NSString::from_str("/"));
            unsafe { FIFinderSyncController::defaultController().setDirectoryURLs(Some(&NSSet::from_slice(&[&*root]))) };
            log("init, watching /");
            this
        }

        #[unsafe(method_id(menuForMenuKind:))]
        fn menu_for_menu_kind(&self, kind: FIMenuKind) -> Option<Retained<NSMenu>> {
            let files = selected_files();
            let menu = build_menu(kind, &files);
            log(&format!(
                "menu kind={} files={} first={:?} -> {}",
                kind.0,
                files.len(),
                files.first(),
                menu.is_some()
            ));
            menu
        }

        #[unsafe(method(openInSudopen:))]
        fn open_in_sudopen(&self, _sender: Option<&AnyObject>) {
            let files = selected_files();
            log(&format!("open requested for {} file(s)", files.len()));
            if files.is_empty() {
                return;
            }
            let comps = NSURLComponents::new();
            comps.setScheme(Some(&NSString::from_str("sudopen")));
            comps.setHost(Some(&NSString::from_str("open")));
            let items: Vec<_> = files
                .iter()
                .map(|p| {
                    NSURLQueryItem::queryItemWithName_value(
                        &NSString::from_str("path"),
                        Some(&NSString::from_str(&p.to_string_lossy())),
                    )
                })
                .collect();
            comps.setQueryItems(Some(&NSArray::from_retained_slice(&items)));
            if let Some(url) = comps.URL() {
                let ok = NSWorkspace::sharedWorkspace().openURL(&url);
                log(&format!("open {} file(s) -> {ok}", files.len()));
            }
        }
    }
);

fn build_menu(kind: FIMenuKind, files: &[PathBuf]) -> Option<Retained<NSMenu>> {
    if kind != FIMenuKind::ContextualMenuForItems || files.is_empty() {
        return None;
    }
    // Finder calls menuForMenuKind: on a background thread, and expects the
    // menu built there (Apple's template does exactly this); the menu is only
    // serialized and sent back to Finder, never shown from this process.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Open in sudopen"),
            Some(sel!(openInSudopen:)),
            &NSString::from_str(""),
        )
    };
    if let Some(app) = containing_app() {
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&app.to_string_lossy()));
        icon.setSize(NSSize::new(16.0, 16.0));
        item.setImage(Some(&icon));
    }
    menu.addItem(&item);
    Some(menu)
}

/// Appends to Library/Logs/sudopen-finder.log inside the extension's
/// sandbox container (~/Library/Containers/com.sudorender.sudopen.finder/Data).
fn log(msg: &str) {
    use std::io::Write;
    let Some(home) = std::env::var_os("HOME") else { return };
    let path = PathBuf::from(home).join("Library/Logs/sudopen-finder.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

/// Selected files. Folders are skipped, judged from the URL (Finder gives
/// folder URLs a trailing slash) since the sandbox may not let us stat them.
fn selected_files() -> Vec<PathBuf> {
    let Some(urls) = (unsafe { FIFinderSyncController::defaultController().selectedItemURLs() }) else {
        return Vec::new();
    };
    urls.iter()
        .filter(|u| !u.hasDirectoryPath())
        .filter_map(|u| u.path())
        .map(|p| PathBuf::from(p.to_string()))
        .collect()
}

/// sudopen.app, from .../sudopen.app/Contents/PlugIns/sudopen-finder.appex.
fn containing_app() -> Option<PathBuf> {
    let me = PathBuf::from(NSBundle::mainBundle().bundleURL().path()?.to_string());
    let app = me.parent()?.parent()?.parent()?.to_path_buf();
    (app.extension()? == "app").then_some(app)
}

unsafe extern "C" {
    /// Foundation's entry point for app extensions.
    fn NSExtensionMain(argc: c_int, argv: *mut *mut c_char) -> c_int;
}

fn main() {
    // Register the principal class before NSExtensionMain looks it up by name.
    let _ = FinderSync::class();
    let args: Vec<CString> =
        std::env::args_os().filter_map(|a| CString::new(a.into_encoded_bytes()).ok()).collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let code = unsafe { NSExtensionMain(args.len() as c_int, argv.as_mut_ptr()) };
    std::process::exit(code);
}
