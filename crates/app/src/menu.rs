//! The menu bar. Actions go through the responder chain (target nil).

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};

fn item(mtm: MainThreadMarker, title: &str, action: Sel, key: &str) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(action),
            &NSString::from_str(key),
        )
    }
}

fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str, items: &[Retained<NSMenuItem>]) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for i in items {
        menu.addItem(i);
    }
    let holder = NSMenuItem::new(mtm);
    holder.setSubmenu(Some(&menu));
    bar.addItem(&holder);
    menu
}

pub fn install(app: &NSApplication, mtm: MainThreadMarker) {
    let bar = NSMenu::new(mtm);
    let sep = || NSMenuItem::separatorItem(mtm);

    let hide_others = item(mtm, "Hide Others", sel!(hideOtherApplications:), "h");
    hide_others.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Option);
    submenu(mtm, &bar, "sudopen", &[
        item(mtm, "About sudopen", sel!(orderFrontStandardAboutPanel:), ""),
        sep(),
        item(mtm, "Install Command Line Tool…", sel!(installCommandLineTool:), ""),
        sep(),
        item(mtm, "Hide sudopen", sel!(hide:), "h"),
        hide_others,
        item(mtm, "Show All", sel!(unhideAllApplications:), ""),
        sep(),
        item(mtm, "Quit sudopen", sel!(terminate:), "q"),
    ]);

    submenu(mtm, &bar, "File", &[
        item(mtm, "Open…", sel!(openDocument:), "o"),
        sep(),
        item(mtm, "Close", sel!(performClose:), "w"),
    ]);

    submenu(mtm, &bar, "Edit", &[
        item(mtm, "Copy", sel!(copy:), "c"),
        item(mtm, "Select All", sel!(selectAll:), "a"),
    ]);

    let full_screen = item(mtm, "Enter Full Screen", sel!(toggleFullScreen:), "f");
    full_screen.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Control);
    submenu(mtm, &bar, "View", &[
        item(mtm, "Show Preview", sel!(togglePreview:), "t"),
        sep(),
        full_screen,
    ]);

    let window_menu = submenu(mtm, &bar, "Window", &[
        item(mtm, "Minimize", sel!(performMiniaturize:), "m"),
        item(mtm, "Zoom", sel!(performZoom:), ""),
        sep(),
        item(mtm, "Bring All to Front", sel!(arrangeInFront:), ""),
    ]);

    app.setMainMenu(Some(&bar));
    app.setWindowsMenu(Some(&window_menu));
}
