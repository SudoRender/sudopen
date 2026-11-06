# sudopen

Native macOS file viewer in Rust. Goal number one: opens ultra fast. Every
change that touches the open path must keep `scripts/bench.sh` numbers where
they are (README has the current ones).

## Product decisions (Nov 2026)

- Viewer first. An Edit button that switches to an editor comes later; a
  short delay when switching is acceptable, opening is not.
- HTML, Markdown and CSV/TSV open in preview first (decided Nov 6); Cmd+T
  shows the source. `file:line` and `-s/--source` open the text. HTML
  preview is static: JavaScript off.
- First launch from /Applications offers to install the `sudopen` command.
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
  (`highlight.rs`), raw-byte to grid layout (`layout.rs`), Markdown to HTML
  with highlighted code blocks (`markdown.rs`), CSV record index parsed a
  chunk at a time (`table.rs`). A future Windows front end reuses all of it.
- `crates/app` (`sudopen` binary): AppKit through objc2.
  - `main.rs`: same binary is the GUI app (launched by LaunchServices with no
    args and stdin not a tty, or with `--app`) or the CLI.
  - `cli.rs` / `ipc.rs`: CLI sends files over `~/Library/Application
    Support/sudopen/ipc.sock`; if nothing listens it starts the app with
    `open -a` and waits for the socket. Do not spawn the bundled binary
    directly: it then belongs to the terminal session and dies with it (a
    bare dev binary is spawned with `--app` in its own session instead).
  - `app.rs`: delegate, one window per file, app stays running after the last
    window closes.
  - `view.rs`: custom read-only NSView. Own scrolling and overlay scroll bars
    (NSScrollView breaks on documents hundreds of millions of px tall). Draws
    only visible lines and columns.
  - `preview.rs`: Cmd+T views, built on first toggle. WKWebView for Markdown
    (rendered to `~/Library/Caches/sudopen/preview/`) and HTML, JS off, links
    open in the browser or in sudopen. NSTableView (cell-based, data source
    only) for CSV/TSV.
  - `install.rs`: `sudopen` command install (first-launch prompt, menu item);
    links into /opt/homebrew/bin if writable, else /usr/local/bin via an
    admin prompt.
  - `menu.rs`: menu bar.
- `macos/Info.plist`: bundle id `com.sudorender.sudopen`, registered as an
  Alternate viewer for text and for `public.data`, and the `sudopen://`
  URL scheme (`sudopen://open?path=...`, one `path` per file).
- `crates/finder` + `macos/finder/`: Finder Sync extension (sandboxed
  .appex in `Contents/PlugIns`) that puts "Open in sudopen" at the top level
  of Finder's right-click menu for every file. It cannot read files, so it
  opens a `sudopen://` link. The app enables it once on first launch
  (`pluginkit -e use -i com.sudorender.sudopen.finder`); `pluginkit -m -v -i
  com.sudorender.sudopen.finder` shows its state. Sign it before the app
  (inside out), with `macos/finder/entitlements.plist`.
- `macos/icon/icon.swift`: draws the icon (`scripts/make-icon.sh` builds
  `macos/AppIcon.icns`). `macos/dmg/`: DMG background and dmgbuild layout.

## Commands

- `scripts/bundle.sh [release|debug]`: build `target/<profile>/sudopen.app`
  (ad-hoc signed).
- `scripts/install.sh`: install to /Applications and link `sudopen` on PATH.
- `scripts/dmg.sh`: Developer ID signed app and DMG in `target/dist/`,
  notarized and stapled when the notarytool profile `gabgren` (or
  `NOTARY_PROFILE`) exists in the keychain; otherwise it says so and skips.
- `scripts/bench.sh`: timings. Restarts the app.
- `sudopen --timing <file>`: total time to first frame plus a per-phase
  breakdown (cold starts include process startup phases).
- `sudopen --snapshot out.png`: PNG of the front window, web previews
  included. Use it to check rendering without screen-recording permission.
- `sudopen -s <file>`: open a previewable file as text.
- `sudopen --quit`: quit the running app. Do this after rebuilding, otherwise
  the CLI talks to the old process.

## Conventions

- Commit as the global git identity (gabgren <github@gabgren.com>).
- Keep `core` free of platform code.
- No new dependencies on the open path without measuring.

## Known gaps

- Notarization has not run yet: the `gabgren` notarytool profile was not
  found from the agent's shell. Unnotarized DMGs are rejected by Gatekeeper
  on other Macs.
- One SIGABRT on quit was seen once (Nov 5, stripped binary, not
  reproduced). Panics now go to `~/Library/Logs/sudopen.log`.
- Previews are built once per window and not refreshed if the file changes.
- CSV preview always treats the first record as the header.
- If another process truncates a mapped file while it is open, reading the
  missing pages crashes the app (SIGBUS). Fix: chunked reads for files that
  change, or catch the signal.
- Wide characters (CJK, emoji) are counted as 2 columns but drawn at the
  font's real width, so selection can drift on those lines.
- No find, no word wrap, no reload when the file changes on disk.
- Text is drawn with NSAttributedString per line; switch to Core Text if
  drawing ever shows up in `--timing`.
