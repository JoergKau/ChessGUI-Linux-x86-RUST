# SiliconBoard

A native **macOS (Apple Silicon) UCI chess GUI** written in Rust. The layout
takes its cues from the Nibbler analysis GUI: board on the left, live engine
analysis on the right, move list along the bottom, and a flat
MD-Viewer-style toolbar across the top.

- **Analysis:** two engine slots with live depth, nodes, speed, evaluation
  and principal variation; Start / Abort; per-card play/pause.
- **Play:** human vs engine (either colour), engine vs engine, clocks with
  flag handling, resign, draw offers, threefold-repetition claims.
- **Engine care:** a watchdog restarts or stops an unresponsive engine,
  launcher scripts and broken files are checked before start, failed starts
  report a reason.
- **Tournaments:** a two-engine match dialog plays a configurable engine
  match on the main board (time control, books, adjudication, PGN output).
- **Everything else:** PolyGlot opening books, PGN open/save including a
  multi-game browser, FEN import/export, board themes and piece sets,
  figurine piece letters, persistent settings, a session log.

The full list of features, planned items and backlog lives in
[features-list.md](features-list.md); what ships in v1.0 is in
[RELEASE_NOTES.md](RELEASE_NOTES.md).

## Requirements

- macOS on Apple Silicon (other platforms are out of scope for now)
- Rust 2021 edition (`cargo`)
- At least one **UCI engine** (e.g. Stockfish) — choose it via
  *Engine → Load engine A/B…*; no engine is bundled

## Building and running

```shell
cargo run --release
```

The **asset tree** is bundled in this repository under `public/`: board
themes under `public/images/board/`, piece sets (twelve SVGs per folder)
under `public/piece/`, the figurine font at `public/font/lichess-chess.ttf`
and the CC0 ECO openings table at `public/data/openings/openings.tsv` — a
fresh clone runs fully dressed. The per-asset licences (lichess and its
piece authors, typically CC-BY-SA) are documented in
[`public/LICENSING.md`](public/LICENSING.md). SiliconBoard finds the tree
via `$SILICONBOARD_PUBLIC_ROOT`, by walking up from the executable, or as
`./public` (the bundled copy is normally found by the walk-up);
`$SILICONBOARD_PUBLIC_ROOT` points a development setup at a different tree.
The workspace copy is refreshed from the canonical shared tree with
`./sync-public.sh`. The window icon is compiled
in from `assets/icon/` (`siliconboard-512.png`).

SiliconBoard does **not** ship an opening book. If a PolyGlot book named
`books/gm2001.bin` (exactly that file name) exists in a folder above the
executable, it is loaded at start-up; `$SILICONBOARD_BOOK` overrides the
path. *Engine → Load PolyGlot book…* replaces the current book at run time
(a loaded book is not kept in the settings). For a book to download, we
recommend the [PolyGlot book collection](https://github.com/ChrisWhittington/polyglot-books).

## Release as a macOS `.app` bundle

The packaging script is still on the backlog (F-15), so these are the manual
steps. The asset resolver already understands the bundle layout: a bundled
app reads its assets from `Contents/Resources/public` — also
`Contents/Resources` itself counts — as unit-tested in `src/assets.rs`.

Assemble the bundle:

```shell
cargo build --release

mkdir -p SiliconBoard.app/Contents/MacOS SiliconBoard.app/Contents/Resources
cp target/release/siliconboard   SiliconBoard.app/Contents/MacOS/SiliconBoard
cp assets/icon/SiliconBoard.icns SiliconBoard.app/Contents/Resources/
cp -R public                     SiliconBoard.app/Contents/Resources/public
```

Note the `cp` renames the binary to `SiliconBoard`, which the plist then
names as the executable. Create `SiliconBoard.app/Contents/Info.plist`
(the About panel reads the version and the copyright from this file when
the app runs inside a bundle):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>               <string>SiliconBoard</string>
    <key>CFBundleDisplayName</key>        <string>SiliconBoard</string>
    <key>CFBundleExecutable</key>         <string>SiliconBoard</string>
    <key>CFBundleIdentifier</key>         <string>org.chess-gui.SiliconBoard</string>
    <key>CFBundlePackageType</key>        <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>1.0.0</string>
    <key>CFBundleVersion</key>            <string>1</string>
    <key>CFBundleIconFile</key>           <string>SiliconBoard.icns</string>
    <key>LSMinimumSystemVersion</key>     <string>11.0</string>
    <key>NSHighResolutionCapable</key>    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright © 2026 Joerg Kauschke. MIT License.</string>
</dict>
</plist>
```

An ad-hoc signature is enough for local use:

```shell
codesign --force --deep --sign - SiliconBoard.app
```

**What to do with it:** double-click to launch, or drag it to
`/Applications`. Nothing else is bundled, so on first launch:

1. Load an engine via *Engine → Load engine A…* (and B as needed) — no UCI
   engine is distributed with SiliconBoard; the engines keep running from
   wherever their binaries live (e.g. `~/engines/`).
2. For opening-book play in games, copy your book into the bundle as
   `SiliconBoard.app/books/gm2001.bin` (the start-up search walks the
   folders above the executable), set `$SILICONBOARD_BOOK` before launch,
   or load it each session via *Engine → Load PolyGlot book…* — like the
   engines, books are not distributed and need to be downloaded separately,
   for example from the
   [PolyGlot book collection](https://github.com/ChrisWhittington/polyglot-books).

## Development

```shell
cargo test --bin siliconboard     # unit + real-engine integration tests
cargo clippy --all-targets        # zero warnings
```

The real-engine tests need an available UCI engine fixture; the PolyGlot and
engine fixtures fail (rather than skip) when they are missing.

## Licence

MIT — see [LICENSE](LICENSE).

External assets follow their own licences — the bundled `public/` tree
carries its source (lichess `lifat`) and per-asset terms in
[`public/LICENSING.md`](public/LICENSING.md); the ECO openings table is CC0.
