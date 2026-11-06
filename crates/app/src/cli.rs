//! `sudopen` used from a terminal: hands files to the running app, starting
//! it first if needed.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::ipc::{self, Request, Target};

const HELP: &str = "\
sudopen: open any file, fast.

Usage: sudopen [options] <file>[:line]...

Options:
  -p, --preview    open Markdown, HTML and CSV files rendered (Cmd+T toggles)
  --timing         wait until each window has drawn and print how long it took
  --quit           quit the running app
  -V, --version    print version
  -h, --help       print this help
";

pub fn run(args: Vec<OsString>, t0: u64) -> i32 {
    let mut files = Vec::new();
    let mut timing = false;
    let mut preview = false;
    let mut quit = false;
    let mut snapshot = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                print!("{HELP}");
                return 0;
            }
            Some("-V" | "--version") => {
                println!("sudopen {}", env!("CARGO_PKG_VERSION"));
                return 0;
            }
            Some("--timing") => timing = true,
            Some("-p" | "--preview") => preview = true,
            Some("--quit") => quit = true,
            // Debug: write a PNG of the frontmost window.
            Some("--snapshot") => snapshot = it.next().map(PathBuf::from),
            Some("--") => files.extend(it.by_ref().map(PathBuf::from)),
            Some(s) if s.starts_with('-') && s.len() > 1 => {
                eprintln!("sudopen: unknown option {s}\n\n{HELP}");
                return 2;
            }
            _ => files.push(PathBuf::from(arg)),
        }
    }

    let mut status = 0;
    let mut targets = Vec::new();
    for f in &files {
        match resolve(f) {
            Ok(t) if t.path.is_dir() => {
                eprintln!("sudopen: {}: is a directory", f.display());
                status = 1;
            }
            Ok(t) => targets.push(t),
            Err(e) => {
                eprintln!("sudopen: {}: {e}", f.display());
                status = 1;
            }
        }
    }
    if !files.is_empty() && targets.is_empty() {
        return status;
    }

    let req = if quit {
        Request::Quit
    } else if let Some(p) = snapshot {
        Request::Snapshot(std::path::absolute(p).unwrap_or_default())
    } else if targets.is_empty() {
        Request::Activate
    } else {
        for t in &mut targets {
            t.preview = preview;
        }
        Request::Open { targets, t0, wait: timing }
    };
    let wants_reply = timing || matches!(req, Request::Snapshot(_));

    let mut stream = match UnixStream::connect(ipc::socket_path()) {
        Ok(s) => s,
        Err(_) if quit => return 0,
        Err(_) => match launch_and_connect() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("sudopen: could not start the app: {e}");
                return 1;
            }
        },
    };
    if let Err(e) = ipc::write_request(&mut stream, &req) {
        eprintln!("sudopen: {e}");
        return 1;
    }
    if wants_reply {
        // Never hang if the app stops answering.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let mut parts = line.splitn(4, '\t');
            match (parts.next(), parts.next(), parts.next(), parts.next()) {
                (Some("painted"), Some(ms), Some(path), detail) => {
                    println!("{ms:>7} ms  {path}");
                    if let Some(d) = detail {
                        println!("            {d}");
                    }
                }
                (Some("error"), Some(msg), _, _) => {
                    eprintln!("sudopen: {msg}");
                    status = 1;
                }
                _ => println!("{line}"),
            }
        }
    }
    status
}

/// Starts the app and waits for its socket.
fn launch_and_connect() -> std::io::Result<UnixStream> {
    let exe = std::env::current_exe()?.canonicalize()?;
    match enclosing_bundle(&exe) {
        // A real .app goes through LaunchServices. Spawning the binary
        // ourselves is 40-90 ms faster, but the app then belongs to the
        // terminal session and dies when it closes, and macOS treats the
        // terminal as responsible for its file-access permissions.
        Some(bundle) => {
            let ok = Command::new("/usr/bin/open").arg("-a").arg(bundle).stdin(Stdio::null()).status()?;
            if !ok.success() {
                return Err(std::io::Error::other("`open -a` failed"));
            }
        }
        // A bare dev binary: run it in app mode in its own session.
        None => {
            let mut cmd = Command::new(&exe);
            cmd.arg("--app").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            // SAFETY: setsid is async-signal-safe.
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
            cmd.spawn()?;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect(ipc::socket_path()) {
            Ok(s) => return Ok(s),
            Err(e) if Instant::now() > deadline => return Err(e),
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

/// `/x/sudopen.app` for `/x/sudopen.app/Contents/MacOS/sudopen`.
fn enclosing_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let ok = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && bundle.extension()? == "app";
    ok.then(|| bundle.to_path_buf())
}

/// `file` or `file:line` (only when `file:line` itself does not exist).
fn resolve(arg: &std::path::Path) -> std::io::Result<Target> {
    match std::fs::canonicalize(arg) {
        Ok(path) => Ok(Target { path, line: None, preview: false }),
        Err(e) => {
            let s = arg.to_string_lossy();
            let parsed = s.rsplit_once(':').and_then(|(p, n)| Some((p, n.parse::<u64>().ok()?)));
            match parsed {
                Some((p, line)) => {
                    Ok(Target { path: std::fs::canonicalize(p)?, line: Some(line.max(1)), preview: false })
                }
                None => Err(e),
            }
        }
    }
}
