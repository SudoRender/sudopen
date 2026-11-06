//! Registering the `sudopen` terminal command: offered once on first launch
//! from /Applications, and any time from the app menu.

use std::path::{Path, PathBuf};
use std::process::Command;

use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSAlertStyle};
use objc2_foundation::{MainThreadMarker, NSString, NSUserDefaults};

use crate::app::activate;

const PROMPTED_KEY: &str = "SudopenCLIPrompted";
const FINDER_EXT_KEY: &str = "SudopenFinderExtensionEnabled";
pub const FINDER_EXT_ID: &str = "com.sudorender.sudopen.finder";

/// Turns on the Finder extension ("Open in sudopen" in the right-click
/// menu) the first time the installed app runs. Users can switch it off in
/// System Settings; we never turn it back on after that.
pub fn enable_finder_extension_once() {
    let Some(exe) = app_binary() else { return };
    if !installed_app(&exe) {
        return;
    }
    if NSUserDefaults::standardUserDefaults().boolForKey(&NSString::from_str(FINDER_EXT_KEY)) {
        return;
    }
    // .../sudopen.app/Contents/MacOS/sudopen -> .../Contents/PlugIns/...
    let Some(appex) = exe.parent().and_then(Path::parent).map(|c| c.join("PlugIns/sudopen-finder.appex")) else {
        return;
    };
    std::thread::spawn(move || {
        let pluginkit = |args: &[&str]| Command::new("/usr/bin/pluginkit").args(args).output();
        // On a fresh install macOS may not have picked the extension up
        // yet: register it explicitly, then retry until it reports enabled.
        let _ = pluginkit(&["-a", &appex.to_string_lossy()]);
        for _ in 0..10 {
            let _ = pluginkit(&["-e", "use", "-i", FINDER_EXT_ID]);
            let enabled = pluginkit(&["-m", "-i", FINDER_EXT_ID])
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim_start().starts_with('+'));
            if enabled {
                // Only now: if it failed, the next launch tries again.
                NSUserDefaults::standardUserDefaults().setBool_forKey(true, &NSString::from_str(FINDER_EXT_KEY));
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

/// Fallback location; on the default PATH of every Mac.
const SYSTEM_BIN: &str = "/usr/local/bin";

/// Bin folders we can link into without a password, in order of preference.
/// /opt/homebrew/bin is only writable (and on PATH) when Homebrew is set up.
fn user_writable_bins() -> Vec<PathBuf> {
    ["/opt/homebrew/bin", SYSTEM_BIN].into_iter().map(PathBuf::from).filter(|d| writable_dir(d)).collect()
}

fn writable_dir(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else { return false };
    dir.is_dir() && unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0
}

fn app_binary() -> Option<PathBuf> {
    std::env::current_exe().ok()?.canonicalize().ok()
}

/// True when the app is installed, not run from a build folder or the DMG.
fn installed_app(exe: &Path) -> bool {
    let home_apps = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Applications"));
    exe.starts_with("/Applications") || home_apps.is_some_and(|h| exe.starts_with(h))
}

/// An existing `sudopen` link that already points at this app.
fn existing_link(exe: &Path) -> Option<PathBuf> {
    ["/opt/homebrew/bin", SYSTEM_BIN]
        .into_iter()
        .map(|d| Path::new(d).join("sudopen"))
        .find(|l| l.canonicalize().ok().as_deref() == Some(exe))
}

/// First launch: offer to install the command, once.
pub fn maybe_prompt(mtm: MainThreadMarker) {
    let Some(exe) = app_binary() else { return };
    if !installed_app(&exe) {
        return;
    }
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str(PROMPTED_KEY);
    if defaults.boolForKey(&key) {
        return;
    }
    defaults.setBool_forKey(true, &key);
    if existing_link(&exe).is_some() {
        return;
    }
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Install the sudopen command?"));
    alert.setInformativeText(&NSString::from_str(
        "Open any file from Terminal with\n\n    sudopen <file>\n\nYou can also do this later from the sudopen menu.",
    ));
    alert.addButtonWithTitle(&NSString::from_str("Install"));
    alert.addButtonWithTitle(&NSString::from_str("Not Now"));
    activate(mtm);
    if alert.runModal() == NSAlertFirstButtonReturn {
        install_with_feedback(mtm);
    }
}

/// Installs the command and tells the user how it went.
pub fn install_with_feedback(mtm: MainThreadMarker) {
    let alert = NSAlert::new(mtm);
    match install() {
        Ok(link) => {
            alert.setMessageText(&NSString::from_str("The sudopen command is installed"));
            alert.setInformativeText(&NSString::from_str(&format!(
                "{} opens files in sudopen. Open a new Terminal window and try:\n\n    sudopen README.md",
                link.display()
            )));
        }
        Err(e) => {
            alert.setAlertStyle(NSAlertStyle::Warning);
            alert.setMessageText(&NSString::from_str("Could not install the sudopen command"));
            alert.setInformativeText(&NSString::from_str(&e));
        }
    }
    activate(mtm);
    alert.runModal();
}

fn install() -> Result<PathBuf, String> {
    let exe = app_binary().ok_or("Could not find the sudopen binary.")?;
    if let Some(link) = existing_link(&exe) {
        return Ok(link);
    }
    if let Some(dir) = user_writable_bins().into_iter().next() {
        let link = dir.join("sudopen");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&exe, &link).map_err(|e| format!("{}: {e}", link.display()))?;
        return Ok(link);
    }
    // /usr/local/bin needs admin rights: macOS shows its password prompt.
    let link = Path::new(SYSTEM_BIN).join("sudopen");
    let shell = format!(
        "mkdir -p {} && ln -sf {} {}",
        sh_quote(SYSTEM_BIN),
        sh_quote(&exe.to_string_lossy()),
        sh_quote(&link.to_string_lossy())
    );
    let script = format!("do shell script \"{}\" with administrator privileges", as_escape(&shell));
    let out = Command::new("/usr/bin/osascript").arg("-e").arg(script).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(link)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(if err.contains("-128") { "Cancelled.".into() } else { err.trim().to_string() })
    }
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn as_escape(s: &str) -> String {
    s.replace('\\', r"\\").replace('"', "\\\"")
}
