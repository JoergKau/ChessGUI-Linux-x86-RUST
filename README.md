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
- At least one **UCI engine** of your choice — SiliconBoard ships none; see
  the next section to install one

## Loading a UCI engine (example: Stockfish)

No engine is distributed with SiliconBoard: _Engine → Load engine A…_ and
_Load engine B…_ always open the native file picker, and whichever engine
the user points them at fills the slot (loading into an occupied slot
replaces it). The chosen file is checked before it starts — a launcher
script must name an existing interpreter, and a non-program or a non-UCI
binary is refused with the reason. Each engine keeps its own option values,
which are sent again after a restart and restored at the next launch.

Installing the free Stockfish:

1. Download from <https://stockfishchess.org/download/> (the macOS build).
2. Unpack the file; the UCI binary inside is
   `stockfish-macos-universal`.
3. In the GUI, _Engine → Load engine A…_ and point it at that binary.

The process is the same for any other UCI engine: unpack it and point the
menu at its executable.

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

SiliconBoard does **not** ship an opening book and loads none by default:
the app starts bookless. Load one via _Engine → Load PolyGlot book…_ —
the choice is kept in the settings and restored at the next start-up.
`$SILICONBOARD_BOOK` names an explicit start-up book instead. For a book
to download, we recommend the
[PolyGlot book collection](https://github.com/ChrisWhittington/polyglot-books).

## Release as a macOS `.app` bundle

```shell
./package-macos.sh
```

The script (F-15) runs `cargo build --release`, then assembles
`SiliconBoard.app` in the repository root: the binary as
`Contents/MacOS/SiliconBoard`, the icon and the asset tree as
`Contents/Resources/public` — the layout the asset resolver checks first in
a bundle (unit-tested in `src/assets.rs`) — and writes
`Contents/Info.plist` with the version taken from `Cargo.toml`, the bundle
identifier `org.chess-gui.SiliconBoard` and the About-panel copyright
(`macos_about.rs` reads both from the bundle). `plutil -lint` guards the
plist, and the bundle is signed sign ad-hoc:

```shell
codesign --force --deep --sign - SiliconBoard.app   # the script does this
```

The bundle is a build artefact and stays out of git.

**Distribution:** the script also packs the signed app into
`SiliconBoard-<version>-macos-arm64.zip` — one folder holding the app plus
`LICENSE` and `README.md` — and prints the zip's SHA-256 for the release
listing. The app is signed _before_ it is zipped (modifying a signed app
breaks the seal), and the zip is verified by unzipping it and re-checking
the signature.

**Install locally:** `./package-macos.sh --install` (or `-i`) also copies
the freshly signed bundle into `/Applications`, replacing an existing
copy there; without the flag nothing is installed, and an unknown
argument is rejected with the usage line. If `/Applications` cannot be
written, the script says so and the `.app` can be dragged by hand.

**What to do with it:** double-click to launch, or drag it to
`/Applications`. A copy that came over the internet is quarantined by
macOS: right-click → **Open** the first time.

Nothing else is bundled, so on first launch:

2. Load an engine via _Engine → Load engine A…_ (and B as needed) — no UCI
   engine is distributed with SiliconBoard; the engines keep running from
   wherever their binaries live (e.g. `~/engines/`).
3. For opening-book play in games, load your book once via _Engine → Load
   PolyGlot book…_ — the choice is remembered across starts.

## Development

```shell
cargo test --bin siliconboard     # unit + real-engine integration tests
cargo clippy --all-targets        # zero warnings
```

The real-engine tests need an available UCI engine fixture; the PolyGlot and
engine fixtures fail (rather than skip) when they are missing.

## Licence

MIT — see [LICENSE](LICENSE).

**Credits:** the bundled board themes, piece sets and the figurine font are
**lichess artwork** (`lichess.org`, asset repository
[`lichess-org/lifat`](https://github.com/lichess-org/lifat)), by the
respective CC-BY-SA-type upstream authors (e.g. the default `cburnett`
pieces by Colin M. L. Burnett). The ECO openings table is CC0. The summary
with sources and terms per asset family is in
[`public/LICENSING.md`](public/LICENSING.md).
