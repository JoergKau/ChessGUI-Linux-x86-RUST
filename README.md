# SiliconBoard

A modern, high-performance UCI chess GUI for **macOS Apple Silicon**, written in Rust.

> **Phase status (2026-11-02):** Phase 4 shipped — board rendering with click-click
> and drag-and-drop interaction driving a typed `BoardAction` flow into the caller.
> The stack now spans:
> * **Phase 1 / 1 BIS** — Apple Silicon toolchain + `public/` asset tree
>   (cburnett SVG pieces rasterised at startup via `usvg`/`resvg`/`tiny-skia`
>   into 512×512 RGBA8; board texture from `public/images/board/blue.png`).
> * **Phase 2** — `GameState` shakmaty wrapper, `PolyglotBook` `.bin` reader
>   (real-world fixture `../books/gm2001.bin`, **30 416 records** verified),
>   and a live `Clock` (Sudden Death / Fischer / Fixed-Per-Move).
> * **Phase 3** — UCI protocol, line parser, and a three-task async engine
>   supervisor (writer / reader / supervisor) modelled on the proven
>   `Rust-Chess-GUI-0.2.1/src-tauri/src/uci.rs`. Real-world-tested against
>   Stockfish 19 (`/opt/homebrew/bin/stockfish`) and the secondary engine
>   `Triumviratus/Triumviratus_8.0_macos_arm`.
> * **Phase 4** — board view widget with the blue-board texture, cburnett
>   piece rendering, click / drag state machine, and typed `BoardAction`s.
>
> **50 tests pass**, all on Apple Silicon. Pending within Phase 4: promotion
> mini-modal, drag-drop back on own-origin-reselect, and drop target for a
> drag released mid-board. See `../implementation-plan.md` § 6.1.

## Highlights (target architecture)

- **wgpu / Metal** rendering via `egui` 0.37 + `eframe` 0.37.
- **Vector-piece asset pipeline**: cburnett SVG → `usvg` parser → `resvg` →
  `tiny-skia` → RGBA8 → wgpu. Crisp at any board scale on Apple Retina.
- **High-resolution board texture** decoded once at startup from
  `public/images/board/blue.png` (2410 bytes, 2048 × 2048 PNG).
- **`shakmaty` 0.30** for legal-move generation, SAN/FEN, PGN.
- **PolyGlot binary opening books** (`.bin`) with lower-bound binary search
  over the canonical Zobrist key. Real-world fixture `../books/gm2001.bin`
  is verified by unit test.
- **Live game clock** supporting Sudden Death, Fischer (base + increment)
  and Fixed-Per-Move modes — drives the Phase-5 arbiter UI.
- **Tokio-powered UCI subprocess supervisor** with three dedicated tasks
  (writer / reader / supervisor) over `tokio::sync::mpsc` +
  `tokio::sync::broadcast` — real-world-tested against Stockfish 19 and
  Triumviratus.
- **Board view widget** that turns each frame's click / drag interactions
  into a typed `BoardAction`, plus a click-drag-promote cycle.
- **Resizable dark UI** in the spirit of Nibbler — the Phase-5 work will
  fill in the eval panel / PGN panel / menu bar.

## Layout

```text
Chess-GUI/                                  # workspace root (own git repo)
├── Project-goal.md                         # master blueprint + Engineering Rules
├── implementation-plan.md                  # full design + §6.1 phase log + §7 verification
├── new-bugs.md                             # cross-project bug backlog
├── books/gm2001.bin                        # PolyGlot opening book (30 416 records)
├── public/                                 # canonical asset tree
│   ├── images/board/blue.png               # board, 2048×2048 (v0.1 hardcoded)
│   └── piece/cburnett/*.svg                # 12 cburnett pieces (wK…bP.svg)
├── Triumviratus/Triumviratus_8.0_macos_arm # secondary UCI engine fixture
└── siliconboard/                           # the app — own git repo
    ├── Cargo.toml                          # eframe 0.37, shakmaty 0.30, tokio 1.40,
    │                                       # resvg 0.49, usvg 0.49, tiny-skia 0.12,
    │                                       # image 0.25, byteorder 1.5
    ├── README.md
    ├── src/
    │   ├── main.rs                         # eframe startup + board layout
    │   ├── assets.rs                       # Assets bundle + workspace-root resolver
    │   ├── chess/
    │   │   ├── mod.rs                      # re-exports
    │   │   ├── game_state.rs               # GameState (history / FEN / SAN / promotion)
    │   │   ├── opening_book.rs             # PolyglotBook (BE .bin + binary search)
    │   │   └── clock.rs                    # live game clock (Fischer / sudden-death)
    │   ├── engine/
    │   │   ├── mod.rs                      # 3-task async supervisor
    │   │   ├── protocol.rs                 # typed EngineCommand / EngineEvent
    │   │   └── uci_parser.rs               # line parser + handshake accumulator
    │   └── gui/
    │       ├── mod.rs                      # module shell
    │       ├── board_view.rs               # Phase-4 board widget
    │       ├── eval_panel.rs               # (Phase 5) eval dashboard placeholder
    │       ├── pgn_panel.rs                # (Phase 5) PGN panel placeholder
    │       └── menu_bar.rs                 # (Phase 5) menu bar placeholder
    └── tests/                              # reserved for Phase-3 / Phase-4 tests
                                            # (unit tests currently live in-module)
```

## Running locally

```bash
cd siliconboard
cargo run --release                       # quiet boot (blue board + pieces)
RUST_LOG=siliconboard=debug cargo run --release      # texture uploads + engine logs
cargo test --bin siliconboard             # 50 unit tests, real-world included
```

The window shows a Nibbler-flavoured layout: a top menu placeholder, a
right-hand sidebar (status / FEN / move history / Copy FEN), and a central
board panel. Click a piece to see legal destinations (dots for quiet, rings
for captures); click or drag a destination to play. Copy FEN writes to the
system clipboard via `egui::Context::copy_text()`.

### Asset directory resolution

`assets::resolve_asset_root` searches, in order:

1. `SILICONBOARD_PUBLIC_ROOT` environment variable (must point at a
   directory with both `images/board/` and `piece/cburnett/`).
2. Walk up from `<exe>/..` to find the closest ancestor with a `public/`
   directory that contains both. Tolerance: 8 parent levels, which
   reaches the workspace root from `siliconboard/target/{release,debug}`.
3. Fallback to `./public` (current working directory, useful for tests).

### Chess domain API quick tour

```rust
use siliconboard::chess::{GameState, PieceKind, PolyglotBook};
use siliconboard::assets::Assets;
use shakmaty::Square;

// Standard starting position.
let mut game = GameState::new();

// Apply a half-move via the user-friendly (from, to, promotion) triplet.
game.apply_user_move(
    Square::from_ascii(b"e2").unwrap(),
    Square::from_ascii(b"e4").unwrap(),
    None,
).unwrap();

// Read the SAN/precomputed move history and emit the (move_no, white, black)
// triples the PGN panel wants.
let lines = game.pgn_lines();
assert_eq!(lines[0].0, 1);
assert_eq!(lines[0].1.as_deref(), Some("e4"));

// Compute the PolyGlot Zobrist key (canonical value for the starting
// position is 0x463b96181691fc9c).
assert_eq!(format!("{:016x}", game.zobrist()), "463b96181691fc9c");

// Load a PolyGlot opening book and ask for moves at the current key.
let book = PolyglotBook::open(std::path::Path::new("../books/gm2001.bin"))
    .expect("real-world book");
let candidates = book.find(game.zobrist());
assert!(!candidates.is_empty());

// Tell the engine to make a move (Phase 6 wires this into the game loop).
use siliconboard::engine::{EngineCommand, EngineProcess, GoControl};
let (engine, mut events) = EngineProcess::spawn("/opt/homebrew/bin/stockfish".as_ref()).await?;
engine.send(EngineCommand::Initialize).await?;
engine.send(EngineCommand::Go(GoControl::Depth(4))).await?;
// Drain events, wait for EngineEvent::BestMove or EngineEvent::Info.
```

### Engine test fixtures

The engine-level integration tests pick the first available binary from
this priority list (see `engine::tests::test_engine_path`):

1. `SILICONBOARD_TEST_ENGINE=/path/to/binary` environment override.
2. `/opt/homebrew/bin/stockfish` — Stockfish 19, homebrew on arm64.
3. `Triumviratus/Triumviratus_8.0_macos_arm` — the workspace `Triumviratus/`
   folder (Stockfish-derived NNUE engine, `id name Triumviratus - 8.0`).

If none of these exist locally the tests print a warning and skip rather
than fail, so CI can run the rest of the project without an engine.

## Roadmap

See `../implementation-plan.md` for the full breakdown. Phases 5 and 6 will
fill in `src/gui/eval_panel.rs`, `src/gui/pgn_panel.rs`, `src/gui/menu_bar.rs`,
and the game-loop arbiter.

## License

MIT.
