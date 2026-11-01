//! Chess domain layer.
//!
//! Phase 2 filled out the previously-placeholder modules:
//!
//! ```text
//! src/chess/
//! ├── mod.rs            ← re-exports of the public surface
//! ├── game_state.rs     ← GameState wrapper around shakmaty::Chess
//! │                       (san, fen, history, navigation, promotion)
//! ├── opening_book.rs   ← PolyGlot .bin reader with binary search
//! └── clock.rs          ← live game clock with Fischer / sudden-death
//! ```
//!
//! Phase 4 (board_view) and Phase 5 (eval / PGN panel / menu bar) consume
//! these types. The engine side (Phase 3) reads `GameState::position()` to
//! feed `position fen … moves …` strings to UCI subprocesses.

pub mod clock;
pub mod game_state;
pub mod opening_book;

// Convenience re-exports so the rest of the crate can use
// `crate::chess::PieceColor` / `crate::chess::PieceKind` directly,
// matching the Phase-1-BIS asset pipeline contract.
pub use game_state::{GameState, PieceColor, PieceKind, Ply};
pub use opening_book::{BookEntry, BookError, PolyglotBook};
pub use clock::{Clock, TimeControl, TimeMode};
