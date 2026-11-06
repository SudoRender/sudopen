//! Local socket between the `sudopen` command and the running app.
//!
//! Going through LaunchServices (`open -a`) costs tens of ms per call; a Unix
//! socket to the already-running app costs well under one. The app listens
//! on `~/Library/Application Support/sudopen/ipc.sock`.
//!
//! Wire format: a command line, argument lines, then an empty line.
//!   open\t<t0 ns>\t<wait 0|1>\n<path>[\t<line>]\n...\n
//!   activate\n\n
//!   snapshot\n<png path>\n\n
//!   quit\n\n
//! The app answers with zero or more lines and closes the connection.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

/// A file to open, optionally at a line (1-based).
pub struct Target {
    pub path: PathBuf,
    pub line: Option<u64>,
}

pub enum Request {
    Open { targets: Vec<Target>, t0: u64, wait: bool },
    Activate,
    Snapshot(PathBuf),
    Quit,
}

pub fn socket_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
    home.join("Library/Application Support/sudopen/ipc.sock")
}

pub fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

pub fn write_request(stream: &mut UnixStream, req: &Request) -> io::Result<()> {
    let mut msg = String::new();
    match req {
        Request::Open { targets, t0, wait } => {
            msg.push_str(&format!("open\t{t0}\t{}\n", *wait as u8));
            for t in targets {
                msg.push_str(&t.path.to_string_lossy());
                if let Some(line) = t.line {
                    msg.push_str(&format!("\t{line}"));
                }
                msg.push('\n');
            }
        }
        Request::Activate => msg.push_str("activate\n"),
        Request::Snapshot(p) => msg.push_str(&format!("snapshot\n{}\n", p.to_string_lossy())),
        Request::Quit => msg.push_str("quit\n"),
    }
    msg.push('\n');
    stream.write_all(msg.as_bytes())
}

fn read_request(reader: &mut impl BufRead) -> io::Result<Request> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches('\n');
        if line.is_empty() {
            break;
        }
        lines.push(line.to_string());
    }
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad request");
    let head = lines.first().ok_or_else(bad)?;
    let mut parts = head.split('\t');
    Ok(match parts.next() {
        Some("open") => {
            let t0 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let wait = parts.next() == Some("1");
            let targets = lines[1..]
                .iter()
                .map(|l| match l.rsplit_once('\t') {
                    Some((p, n)) if n.parse::<u64>().is_ok() => {
                        Target { path: p.into(), line: n.parse().ok() }
                    }
                    _ => Target { path: l.into(), line: None },
                })
                .collect();
            Request::Open { targets, t0, wait }
        }
        Some("activate") => Request::Activate,
        Some("snapshot") => Request::Snapshot(lines.get(1).map(PathBuf::from).ok_or_else(bad)?),
        Some("quit") => Request::Quit,
        _ => return Err(bad()),
    })
}

/// Starts listening. `handler` runs on a socket thread; whatever it sends
/// on the `Sender` (from any thread) is written back to the client, and the
/// connection closes once every clone of the `Sender` is dropped.
pub fn serve(handler: fn(Request, Sender<String>)) -> io::Result<()> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "another sudopen is running"));
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    std::thread::Builder::new().name("sudopen-ipc".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = handle(stream, handler);
            });
        }
    })?;
    Ok(())
}

fn handle(stream: UnixStream, handler: fn(Request, Sender<String>)) -> io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let req = read_request(&mut reader)?;
    let (tx, rx) = mpsc::channel();
    handler(req, tx);
    let mut stream = stream;
    for line in rx {
        writeln!(stream, "{line}")?;
    }
    Ok(())
}

/// Removes the socket file if it is ours.
pub fn cleanup() {
    let _ = std::fs::remove_file(socket_path());
}
