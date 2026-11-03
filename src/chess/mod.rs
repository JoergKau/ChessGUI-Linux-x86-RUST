//! Chess domain layer.
//!
//! ```text
//! src/chess/
//! ├── mod.rs            ← re-exports of the public surface
//! ├── game_state.rs     ← GameState wrapper around shakmaty::Chess
//! │                       (SAN, FEN, history, navigation, promotion, UCI→SAN)
//! ├── opening_book.rs   ← PolyGlot .bin reader with binary search
//! ├── openings.rs      ← ECO opening names keyed by position
//! ├── pgn.rs            ← PGN writer and mainline reader
//! ├── players.rs        ← game modes and the seat (human or engine) of each colour
//! └── clock.rs          ← live game clock (Phase 6 arbiter)
//! ```
//!
//! The board, eval and PGN panels read [`GameState`]; the engine layer uses
//! `GameState::fen()` and `GameState::uci_moves_to_san()`.

pub mod game_state;
pub mod opening_book;
pub mod openings;
pub mod pgn;
pub mod players;

/// Time-control clock, driven by the game loop in `app.rs` (Phase 6).
pub mod clock;

pub use game_state::{GameState, PieceColor, PieceKind};
pub use opening_book::{BookError, PolyglotBook};
