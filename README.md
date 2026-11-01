# SiliconBoard

A modern, high-performance UCI chess GUI for **macOS Apple Silicon**, written in Rust.

> **Phase status:** 2 — Chess domain (`GameState`, `PolyglotBook`, `Clock`)
> is fully wired up. The Phase-1-BIS asset pipeline (cburnett SVG pieces
> rasterised at startup + `blue.png` board) is still in place. Full board,
> engine integration and analysis dashboard arrive in later phases
> (see `../implementation-plan.md`).

## Highlights (target architecture)

- **wgpu / Metal** rendering via `egui` + `eframe`.
- **Vector-piece asset pipeline**: cburnett SVG → `usvg` parser → `resvg`
  → `tiny-skia` → RGBA8 → wgpu. Crisp at any board scale on Apple Retina.
- **High-resolution board texture** decoded once at startup from
  `public/images/board/blue.png` (Phase 5 will add a board-picker).
- **`shakmaty`** for legal-move generation, SAN/FEN, PGN.
- **PolyGlot binary opening books** (`.bin`) with binary search over the
  standard Zobrist key — Phase 2 ships the reader; Phase 6 hooks it
  into the game loop.
- **Live game clock** supporting Sudden Death, Fischer (base + increment)
  and Fixed-Per-Move modes — drives the Phase-5 / Phase-6 arbiter UI.
- **Tokio-powered UCI subprocess supervisor** with dedicated read/write
  tasks (Phase 3, lands next).
- **Resizable dark UI** in the spirit of Nibbler — dual-engine cards,
  eval bar, PGN history.

## Layout

```text
Chess-GUI/                                  # workspace root
├── public/                                 # <- canonical asset tree
│   ├── images/board/blue.png               # board, 2048x2048 (v0.1 hardcoded)
│   └── piece/cburnett/{wK,wQ,wR,wB,wN,wP,bK,bQ,bR,bB,bN,bP}.svg
└── siliconboard/
    ├── Cargo.toml                          # eframe 0.37, shakmaty 0.30, byteorder 1.5,
    │                                       # resvg 0.49, usvg 0.49, tiny-skia 0.12, …
    ├── README.md
    ├── src/
    │   ├── main.rs                         # eframe startup, Phase-1-BIS readiness panel
    │   ├── assets.rs                       # Assets (board+pieces), SVG rasterizer, resolver
    │   ├── chess/
    │   │   ├── mod.rs                      # re-exports
    │   │   ├── game_state.rs               # GameState (history / FEN / SAN / promotion)
    │   │   ├── opening_book.rs             # PolyglotBook (BigEndian .bin + binary search)
    │   │   └── clock.rs                    # live game clock (Fischer / sudden-death)
    │   ├── engine/                         # UCI supervisor (Phase 3 placeholder)
    │   └── gui/                            # Board / eval / PGN / menu (Phase 4-5)
    └── tests/                              # reserved for the eventual PolyGlot + UCI tests
```

## Running locally

```bash
cd siliconboard
cargo run --release                       # quiet boot
RUST_LOG=siliconboard=debug cargo run     # see texture uploads + Zobrist hashes
cargo test --bin siliconboard             # 26 unit tests
```

### Asset directory resolution

`assets::resolve_asset_root` searches, in order:

1. The `SILICONBOARD_PUBLIC_ROOT` environment variable (must point at a
   directory that contains `images/board/` and `piece/cburnett/`).
2. Walk up from `<exe>/..` to find the closest ancestor with a `public/`
   directory that contains both `images/board/` and `piece/cburnett/`.
   Tolerance: 8 parent levels, which easily reaches the workspace root
   from `target/{release,debug}/siliconboard`.
3. Fallback to `./public` (current working directory, useful for tests).

### Chess domain API quick tour

```rust
use siliconboard::chess::{GameState, PieceKind, PolyglotBook};

// Standard starting position.
let mut game = GameState::new();

// Apply a half-move via the user-friendly (from, to, promotion) tuple.
game.apply_user_move(
    shakmaty::Square::from_ascii(b"e2").unwrap(),
    shakmaty::Square::from_ascii(b"e4").unwrap(),
    None,
).unwrap();

// Read SAN/precomputed move history and emit the (move_no, white, black)
// triples expected by the PGN panel.
let lines = game.pgn_lines();
assert_eq!(lines[0].0, 1);
assert_eq!(lines[0].1.as_deref(), Some("e4"));

// Compute the PolyGlot Zobrist key (canonical value for the starting
// position is 0x463b96181691fc9c).
assert_eq!(format!("{:016x}", game.zobrist()), "463b96181691fc9c");

// Load a PolyGlot opening book and ask for moves at the current key.
let book = PolyglotBook::open(std::path::Path::new("public/book/torino.bin"))?;
let candidates = book.find(game.zobrist());
```

## Roadmap

See `../implementation-plan.md` for the full breakdown. Phases 3–7 will
fill in `src/engine/`, `src/gui/` and the game-loop arbiter.

## License

MIT.
