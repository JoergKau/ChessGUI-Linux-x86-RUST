//! Presentation layer. Every widget draws what it is given and reports user
//! intent; [`crate::app`] owns the state and performs the actions.
//!
//! ```text
//! src/gui/
//! ├── mod.rs        ← module index
//! ├── theme.rs      ← dark palette and egui visuals
//! ├── board_view.rs ← chess board: paint, click / drag, promotion modal
//! ├── eval_panel.rs ← right panel: engine cards, eval bar, opening book
//! ├── pgn_panel.rs  ← bottom panel: move list and navigation
//! └── menu_bar.rs   ← top menu bar and FEN bar
//! ```

pub mod board_view;
pub mod eval_panel;
pub mod menu_bar;
pub mod pgn_panel;
pub mod theme;
