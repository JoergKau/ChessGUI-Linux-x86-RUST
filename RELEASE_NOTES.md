# SiliconBoard — Release Notes

## v0.1.0 — unreleased (Phase 7 in progress)

A native macOS (Apple Silicon) UCI chess GUI in Rust: board on the left, live
engine analysis on the right, move list along the bottom.

### What works

- **Analysis:** two engine slots (A and B), Start / Abort, per-card play/pause,
  live depth, nodes, speed, evaluation and a two-line principal variation.
  Figurine piece letters in the PV are optional (off by default).
- **Play:** human vs engine (White or Black, against A or B) and engine A vs
  engine B. Human moves by click-click or drag-and-drop, with promotion and
  castling.
- **Clocks:** no clock, sudden death, Fischer increment and fixed time per move;
  player bars above and below the board; flags end the game.
- **Opening book:** PolyGlot (`.bin`), loaded at start-up from
  `books/gm2001.bin`; weighted-random book replies in games, switchable. The
  book box labels the current opening by name (ECO table) when it is known.
- **Games:** resign and draw by agreement, with the result written to the PGN.
- **Files:** open and save PGN (mainline), copy PGN, copy/save FEN.
- **Board themes:** View → Board theme picks any PNG/JPEG board from
  `public/images/board/`; the choice is remembered between launches.
- **Piece sets:** View → Piece set picks any complete cburnett-family set from
  `public/piece/` (41 here); the choice is remembered between launches.
- **Figurine notation:** View → Figurines in move list (on by default) and
  Figurines in engine evaluation (off by default). Saved and copied PGN files
  always stay standard SAN.
- **Engines:** UCI handshake and options, a watchdog that restarts a stalled
  engine, loaded engines remembered between launches.
- **Settings:** time control, book play, engine option values, window size and
  panel sizes survive a restart.
- **Performance:** engine output wakes the window directly, coalesced to about
  30 fps; an idle loaded engine causes no redraws. The board paint measures
  8–11 µs, and a live run with two Stockfish 19 instances (2.6–2.9 M NPS
  combined) measured a 0.27 ms average CPU cost per frame, 10.15 ms worst.

### Requirements

- macOS on Apple Silicon.
- A UCI engine binary (Stockfish 19 is used in the tests).
- The workspace `public/` asset tree, or `SILICONBOARD_PUBLIC_ROOT` pointing at
  one.

### Known limitations

Tracked in `features-list.md`:

- Linear history; browsing is read-only (variation trees: F-08).
- PGN import keeps the mainline only (comments and variations: F-09).
- One principal variation per engine (multi-PV: F-10).
- SVG board themes in `public/images/board/svg/` are not offered (F-11).
- Syzygy tablebases are not supported (F-13).
- No `.app` bundle yet; the asset resolver expects the workspace layout
  (F-15).
- The design's card extras (copy PV, pin, settings) are not built (F-16).

### Verification

```bash
cd siliconboard
cargo test --bin siliconboard
cargo clippy --all-targets
cargo build --release
cargo test --release --bin siliconboard board_paint -- --ignored --nocapture
```
