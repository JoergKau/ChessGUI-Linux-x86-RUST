//! GUI module skeleton.
//!
//! Phase 1 only needs the module structure to exist so `main.rs` can
//! reference the layout that arrives in Phase 4 / Phase 5:
//!
//! ```text
//! src/gui/
//! ├── mod.rs        ← top-level UI container, theme setup
//! ├── board_view.rs ← (Phase 4) chess board & drag-and-drop
//! ├── eval_panel.rs ← (Phase 5) dual engine eval dashboard
//! ├── pgn_panel.rs  ← (Phase 5) move history viewer
//! └── menu_bar.rs   ← (Phase 5) top menu bar
//! ```

pub mod board_view;
pub mod eval_panel;
pub mod menu_bar;
pub mod pgn_panel;
