//! Chess domain layer.
//!
//! ```text
//! src/chess/
//! ├── mod.rs            ← re-exports of the public surface
//! ├── game_state.rs     ← GameState wrapper around shakmaty::Chess
//! │                       (SAN, FEN, history, navigation, promotion, UCI→SAN)
//! ├── opening_book.rs   ← PolyGlot .bin reader with binary search
//! ├── pgn.rs            ← PGN writer and mainline reader
//! └── clock.rs          ← live game clock (Phase 6 arbiter)
//! ```
//!
//! The board, eval and PGN panels read [`GameState`]; the engine layer uses
//! `GameState::fen()` and `GameState::uci_moves_to_san()`.

pub mod game_state;
pub mod opening_book;
pub mod pgn;

/// Time-control clock. The Phase-6 game-loop arbiter is its first consumer;
/// until that lands its API has no callers, so dead-code lints are allowed
/// for this module only.
#[allow(dead_code)]
pub mod clock;

pub use game_state::{GameState, PieceColor, PieceKind};
pub use opening_book::{BookError, PolyglotBook};
