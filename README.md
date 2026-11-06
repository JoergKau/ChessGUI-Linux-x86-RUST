# SiliconBoard

A native **Linux UCI chess GUI** written in Rust. The layout takes its cues
from the Nibbler analysis GUI: board on the left, live engine analysis on the
right, move list along the bottom, and a flat toolbar across the top.

- **Analysis:** two engine slots with live depth, nodes, speed, evaluation
  and principal variation; Start / Abort; per-card play/pause. Each card's
  engine name is a pulldown of the known engines (loaded A/B and the engines
  used before, remembered across launches, the same list as the tournament
  window) with Browse….
- **Play:** human vs engine (either colour), engine vs engine, clocks with
  flag handling, resign, draw offers, threefold-repetition claims.
- **Play from a position:** paste a FEN into the top bar and choose
  _Game → Start from here…_ — two engines play on from that position with the
  current time control, and the saved PGN starts there (`SetUp`/`FEN`).
- **Engine care:** a watchdog restarts or stops an unresponsive engine,
  launcher scripts and broken files are checked before start, failed starts
  report a reason.
- **Tournaments:** a two-engine match dialog plays a configurable engine
  match on the main board (time control, books, adjudication, PGN output).
- **Everything else:** PolyGlot opening books, PGN open/save including a
  multi-game browser, FEN import/export, board themes and piece sets,
  figurine piece letters, persistent settings, a session log, an ⓘ About
  panel in the toolbar.

A short summary of what SiliconBoard does lives in
[features-list.md](features-list.md); what ships in v1.0 is in
[RELEASE_NOTES.md](RELEASE_NOTES.md).

## The ready-to-run release

Each release carries a built, packed binary:

```
SiliconBoard-1.0.0-linux-x86_64.tar.gz
```

(one folder — `SiliconBoard-1.0.0-linux-x86_64/` — holding the binary in
`bin/`, the asset tree in `public/`, the start-up PGN in `PGN/`, `LICENSE`
and this README). The SHA-256 is printed by `./package-linux.sh` and listed
with each release; check a download with:

```shell
sha256sum SiliconBoard-1.0.0-linux-x86_64.tar.gz
```

To use it:

1. Unpack the tarball: `tar -xzf SiliconBoard-1.0.0-linux-x86_64.tar.gz`
2. Run straight from the folder:
   `SiliconBoard-1.0.0-linux-x86_64/bin/siliconboard`
3. For desktop integration — a menu entry, and the dock/taskbar icon (also
   for minimised windows) — run `./package-linux.sh` once from the unpacked
   folder. It installs into `~/.local` (see *Packaging* below) and you can
   then start SiliconBoard from the desktop menu or with `siliconboard`.
4. The app ships no engine. Load yours via _Engine → Load engine A…_ (and B
   if you want two engines); see the next section.

## Requirements

- Linux on x86-64 or aarch64 (X11 or Wayland; a Vulkan-capable GPU driver,
  or llvmpipe as a software fallback)
- At least one **UCI engine** of your choice — SiliconBoard ships none; see
  the next section to install one

For **building from source**, the usual GUI development packages are needed
(rfd's file dialogs use GTK 3, wgpu uses Vulkan):

```shell
sudo apt install build-essential pkg-config libgtk-3-dev \
    libvulkan-dev vulkan-tools
```

At runtime the desktop session usually provides everything; on a minimal
setup install `libgtk-3-0` and a Vulkan loader such as `libvulkan1` (or
Mesa's).

## Where the files live

- **Settings:** `~/.config/siliconboard/settings.json` (XDG)
- **Session logs:** `~/.local/state/siliconboard/logs/`
- **Default match PGN:** `~/Documents/SiliconBoard/engine-match.pgn`
- The `SILICONBOARD_SETTINGS` and `SILICONBOARD_LOG_FILE` environment
  variables override the first two.

## Loading a UCI engine (example: Stockfish)

No engine is distributed with SiliconBoard: _Engine → Load engine A…_ and
_Load engine B…_ always open the file picker, and whichever engine the user
points them at fills the slot (loading into an occupied slot replaces it).
The chosen file is checked before it starts — a launcher script must name an
existing interpreter, a non-ELF file or a non-UCI binary is refused with the
reason. Each engine keeps its own option values, which are sent again after
a restart and restored at the next launch.

Installing the free Stockfish:

1. `sudo apt install stockfish` (the binary lands in `/usr/games/stockfish`)
2. In the GUI, _Engine → Load engine A…_ and point it at that binary.

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
`./sync-public.sh`. The window icon is compiled in from `assets/icon/`
(`siliconboard-512.png`).

SiliconBoard does **not** ship an opening book and loads none by default:
the app starts bookless. Load one via _Engine → Load PolyGlot book…_ —
the choice is kept in the settings and restored at the next start-up.
`$SILICONBOARD_BOOK` names an explicit start-up book instead. For a book
to download, we recommend the
[PolyGlot book collection](https://github.com/ChrisWhittington/polyglot-books).

## Syzygy tablebases

Engines can use Syzygy tablebase files (`*.rtbw` / `*.rtbz`) for endgame
knowledge — perfect play once few pieces are on the board. Point the GUI at
your installation once via _Engine → Load tablebases…_: enter the tablebase
folder(s) in the dialog (or pick them with **Browse…**, which accepts
several folders) and press Apply. The path is kept in the settings and sent
to every loaded engine — and to every engine loaded later — as the
`SyzygyPath` option.

- Several folders are joined into **one string**, separated by `:`. This
  matters when the 5-men and 6-men files live in different folders, as in
  `/home/you/EGTB/Syzygy-3-4-5:/home/you/EGTB/Syzygy-6`.
- Applying an empty line clears the tablebases again: the loaded engines get
  an empty path, and engines started afterwards get none.
- The GUI forwards the string untouched — which folder holds which files,
  and whether an engine supports tablebases at all, is the engine's
  business; a user choosing paths knows what they are doing. Look in the
  session log for the engine's own report, e.g.
  `Found 510 WDL and 510 DTZ tablebase files (up to 6-man)`. The files and
  what to download are described at
  [syzygy-tables.info](https://syzygy-tables.info/).

## Packaging

```shell
./package-linux.sh
```

The script runs `cargo build --release`, then assembles
`SiliconBoard-<version>-linux-<arch>/` in the repository root: the binary in
`bin/`, the asset tree in `public/` and the start-up PGN in `PGN/` — the
layout the resolver finds by walking up from the executable (unit-tested in
`src/assets.rs`) — plus a `.desktop` launcher, the menu icon, `LICENSE` and
`README.md`. It packs the folder into
`SiliconBoard-<version>-linux-<arch>.tar.gz`, verifies the tarball by
unpacking it again, and prints the SHA-256 for the release listing. That
tarball is the **published release artifact**.

The script **installs by default**, replacing the installed copy, so the
daily-use app is always the one just built: the binary, assets and PGN go
under `~/.local/share/siliconboard/`, a wrapper goes on `$PATH` at
`~/.local/bin/siliconboard`, and the launcher and icons (16–512 px) go into
`~/.local/share/` — after that SiliconBoard starts from the desktop menu.
The installed launcher is also where the **taskbar/dock icon comes from on
Wayland**: the desktop environment matches the window's `app_id`
(`siliconboard`, set in `main.rs`) against the `siliconboard.desktop` entry
and shows its icon, also for minimised windows. Without an installed
launcher — e.g. a plain `cargo run` — Wayland shows a generic icon there.
`--no-install` (or `-n`) skips the install step and leaves the assembled
folder in the repository; the old `--install` (or `-i`) flags still work
and mean the same; an unknown argument is rejected.

## Development

```shell
cargo test --bin siliconboard     # unit + real-engine integration tests
cargo clippy --all-targets        # zero warnings
```

The real-engine tests look for Stockfish in the usual system locations
(`/usr/games/stockfish`, `/usr/local/bin`, `/usr/bin`) or take
`SILICONBOARD_TEST_ENGINE`; the test fails (rather than skips) when no
engine is found. The opening-book tests use small built-in books, so no
external `.bin` file is needed.

## AI Assisted by DeepSeek V.4.1 Flash

## Licence

MIT — see [LICENSE](LICENSE).

**Credits:** the bundled board themes, piece sets and the figurine font are
**lichess artwork** (`lichess.org`, asset repository
[`lichess-org/lifat`](https://github.com/lichess-org/lifat)), by the
respective CC-BY-SA-type upstream authors (e.g. the default `cburnett`
pieces by Colin M. L. Burnett). The ECO openings table is CC0. The summary
with sources and terms per asset family is in
[`public/LICENSING.md`](public/LICENSING.md).
