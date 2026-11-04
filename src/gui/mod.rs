//! Presentation layer. Every widget draws what it is given and reports user
//! intent; [`crate::app`] owns the state and performs the actions.
//!
//! ```text
//! src/gui/
//! ├── mod.rs        ← module index
//! ├── theme.rs      ← dark palette and egui visuals
//! ├── board_view.rs ← chess board: paint, click / drag, promotion modal
//! ├── eval_panel.rs ← right panel: engine cards, eval bar, opening book
//! ├── figurines.rs ← SAN piece letters as figurines (PV and move list)
//! ├── pgn_panel.rs  ← bottom panel: move list and navigation
//! ├── menu_bar.rs   ← top menu bar and FEN bar
//! ├── options_dialog.rs ← engine options window (UCI setoption)
//! ├── tournament_dialog.rs ← engine-match config and tournament windows
//! └── time_control.rs ← clock labels above the board and the time-control dialog
//! ```

pub mod about;
pub mod board_view;
pub mod eval_panel;
pub mod figurines;
pub mod menu_bar;
pub mod options_dialog;
pub mod pgn_panel;
pub mod theme;
pub mod time_control;
pub mod tournament_dialog;
