//! One binary, two roles:
//! - launched by Finder/LaunchServices (or with `--app`): the GUI app;
//! - run from a terminal: the `sudopen` command, which forwards files to the
//!   running app over a local socket.

mod app;
mod cli;
mod ipc;
mod menu;
mod view;

use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;

fn main() {
    let t0 = ipc::now_ns();
    app::MAIN_NS.store(t0, std::sync::atomic::Ordering::Relaxed);
    let mut args: Vec<OsString> = std::env::args_os()
        .skip(1)
        // Older macOS passes a process serial number when launching from Finder.
        .filter(|a| !a.to_string_lossy().starts_with("-psn_"))
        .collect();

    if args.first().is_some_and(|a| a == "--app") {
        let files = args.drain(1..).map(PathBuf::from).collect();
        app::run(files);
    }
    // LaunchServices starts apps with no arguments and stdin on /dev/null.
    if args.is_empty() && !std::io::stdin().is_terminal() {
        app::run(Vec::new());
    }
    std::process::exit(cli::run(args, t0));
}
