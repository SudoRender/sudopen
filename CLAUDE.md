# sudopen

Native macOS file viewer in Rust. Goal number one: opens ultra fast. Every
change that touches the open path must keep `scripts/bench.sh` numbers where
they are (README has the current ones).

## Product decisions (Nov 2026)

- Viewer first. An Edit button that switches to an editor comes later; a
  short delay when switching is acceptable, opening is not.
- Cmd+T toggles text/rendered preview for HTML, Markdown and CSV (next
  milestone). HTML preview is static: JavaScript off.
- One window per file, no tabs (so Cmd+T is free).
- App stays running in the background so opens are instant.
- Distribution: direct download, Developer ID + notarization. No App Store
  (the sandbox would break the CLI and opening arbitrary paths).
- Mac first. Windows later as a separate front end on `sudopen-core`.
- Personal tool first, maybe sold later.

## Layout

- `crates/core` (`sudopen-core`): no UI. File loading (`source.rs`, mmap at
  256 KB+), sparse line index (`lines.rs`, built in the background above 4 MB),
  syntax detection and block highlighting with syntect + two-face
  (`highlight.rs`), raw-byte to grid layout (`layout.rs`). A future Windows
  front end reuses all of it.
- `crates/app` (`sudopen` binary): AppKit through objc2.
  - `main.rs`: same binary is the GUI app (launched by LaunchServices with no
    args and stdin not a tty, or with `--app`) or the CLI.
  - `cli.rs` / `ipc.rs`: CLI sends files over `~/Library/Application
    Support/sudopen/ipc.sock`; if nothing listens it spawns its own binary
    with `--app` (faster than `open -a`) and waits for the socket.
  - `app.rs`: delegate, one window per file, app stays running after the last
    window closes.
  - `view.rs`: custom read-only NSView. Own scrolling and overlay scroll bars
    (NSScrollView breaks on documents hundreds of millions of px tall). Draws
    only visible lines and columns.
  - `menu.rs`: menu bar.
- `macos/Info.plist`: bundle id `com.sudorender.sudopen`, registered as an
  Alternate viewer for text and for `public.data`.

## Commands

- `scripts/bundle.sh [release|debug]`: build `target/<profile>/sudopen.app`
  (ad-hoc signed).
- `scripts/install.sh`: install to /Applications and link `sudopen` on PATH.
- `scripts/bench.sh`: timings. Restarts the app.
- `sudopen --timing <file>`: total time to first frame plus a per-phase
  breakdown (cold starts include process startup phases).
- `sudopen --snapshot out.png`: PNG of the front window. Use it to check
  rendering without screen-recording permission.
- `sudopen --quit`: quit the running app. Do this after rebuilding, otherwise
  the CLI talks to the old process.

## Conventions

- Commit as the global git identity (gabgren <github@gabgren.com>).
- Keep `core` free of platform code.
- No new dependencies on the open path without measuring.

## Known gaps

- Preview toggle (Cmd+T: Markdown, HTML, CSV) not built yet.
- If another process truncates a mapped file while it is open, reading the
  missing pages crashes the app (SIGBUS). Fix: chunked reads for files that
  change, or catch the signal.
- Wide characters (CJK, emoji) are counted as 2 columns but drawn at the
  font's real width, so selection can drift on those lines.
- No find, no word wrap, no reload when the file changes on disk.
- Text is drawn with NSAttributedString per line; switch to Core Text if
  drawing ever shows up in `--timing`.
