# sudopen

Opens any file, fast. A native macOS viewer written in Rust.

- Opens text and code instantly, with syntax highlighting (200+ languages).
- Multi-GB files open as fast as tiny ones: the file is memory-mapped and only
  the lines on screen are read, laid out and highlighted.
- Stays running in the background, so opening the next file takes a few ms.
- `sudopen` command: `sudopen file.rs`, `sudopen app.log:1200` (open at a line).

Planned: preview toggle (Cmd+T) for Markdown, HTML and CSV, an Edit mode, Quick
Look for everything else, Windows.

## Build and install

```sh
scripts/bundle.sh          # builds target/release/sudopen.app
scripts/install.sh         # copies it to /Applications and links the `sudopen` command
scripts/bench.sh           # open-to-first-frame timings
cargo test --workspace
```

## Speed (M-series Mac, `scripts/bench.sh`)

| | time to first frame |
|---|---|
| open a file, app already running | ~15-35 ms |
| open a file, cold start | ~120-180 ms |
| 1.3 GB log | ~30-50 ms |
| 100 MB single-line JSON | ~50 ms |
| jump to a line in an already open file | ~2-5 ms |

Measured from the moment the `sudopen` command starts to the end of the
first `drawRect`. The frame reaches the screen on the next display refresh.
About 45 ms of a cold start is AppKit connecting to the window server.
