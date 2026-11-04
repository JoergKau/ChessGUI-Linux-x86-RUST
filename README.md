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

The **asset tree** is expected in a `public/` folder: board themes under
`public/images/board/`, piece sets (twelve SVGs per folder) under
`public/piece/`, the figurine font at `public/font/lichess-chess.ttf` and the
CC0 ECO openings table at `public/data/openings/openings.tsv`. SiliconBoard
finds it via `$SILICONBOARD_PUBLIC_ROOT`, by walking up from the executable,
or as `./public`. A PolyGlot book next to the working folder at
`books/gm2001.bin` is loaded at start-up; `$SILICONBOARD_BOOK` overrides the
path. The bundles in `assets/icon/` carry the application icon; the visible
piece artwork is not part of this repository.

## Development

```shell
cargo test --bin siliconboard     # unit + real-engine integration tests
cargo clippy --all-targets        # zero warnings
```

The real-engine tests need an available UCI engine fixture; the PolyGlot and
engine fixtures fail (rather than skip) when they are missing.

## Licence

MIT — see [LICENSE](LICENSE).

External assets follow their own licences (piece sets are typically
CC-BY-SA); the ECO openings table is CC0.
