# sudopen

Opens any file, fast. A native macOS viewer written in Rust.

- Opens text and code instantly, with syntax highlighting (200+ languages),
  following light/dark mode.
- Multi-GB files open as fast as tiny ones: the file is memory-mapped and only
  the lines on screen are read, laid out and highlighted.
- Markdown (GitHub style, highlighted code blocks), HTML (JavaScript off) and
  CSV/TSV (a native table that only parses the rows on screen) open rendered.
  **Cmd+T** switches to the source and back.
- Right-click any file in Finder → sudopen this, whatever app normally
  opens it (a Finder extension, switched on at first launch; System Settings
  → General → Login Items & Extensions → Finder to turn it off).
- Stays running in the background, so opening the next file takes a few ms.
- `sudopen` command: offered on first launch, or sudopen menu → Install
  Command Line Tool.

```sh
sudopen file.rs              # open
sudopen app.log:1200         # open at a line
sudopen -s README.md         # open as text instead of rendered
sudopen --timing big.log     # print time to first frame, by phase
```

Planned: Edit mode, Quick Look for every other file type, Windows.

## Install

Download `sudopen-<version>.dmg` from
[Releases](https://github.com/SudoRender/sudopen/releases), open it and drag
sudopen into Applications. On first launch it offers to install the
`sudopen` command. macOS 13 or later, Apple Silicon or Intel.

## Build

```sh
scripts/bundle.sh          # target/release/sudopen.app
scripts/install.sh         # copy to /Applications and link the `sudopen` command
scripts/dmg.sh             # signed (and notarized, if set up) target/dist/sudopen-<version>.dmg
scripts/bench.sh           # open-to-first-frame timings
scripts/make-icon.sh       # regenerate macos/AppIcon.icns
scripts/notices.sh         # regenerate THIRD_PARTY_NOTICES.md after changing dependencies
cargo test --workspace
```

Releases are built by CI: bump `version` in `Cargo.toml`, commit, and push a
`vX.Y.Z` tag. `.github/workflows/release.yml` builds a universal app, signs
it with the SudoRender Developer ID, notarizes the DMG and attaches it to the
GitHub release.

## Speed (M-series Mac, `scripts/bench.sh`)

| | time to first frame |
|---|---|
| open a file, app already running | ~20-35 ms |
| open a file, cold start | ~150-200 ms |
| 1.3 GB log | ~25-50 ms |
| 100 MB single-line JSON | ~15-50 ms |
| jump to a line in an already open file | ~1-5 ms |
| CSV preview (small or 70 MB) | ~10-20 ms |
| HTML preview, page rendered | ~120 ms (WebKit starts its process) |

Measured from the moment the `sudopen` command starts to the end of the
first `drawRect`. The frame reaches the screen on the next display refresh.
About 45 ms of a cold start is AppKit connecting to the window server.

## License

MIT, see [LICENSE](LICENSE). Bundled third-party software and its licenses
are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
