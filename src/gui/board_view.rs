//! Chess board rendering and interaction.
//!
//! Implementation lands in **Phase 4** of `implementation-plan.md`. Until
//! then this file only declares the eventual public surface so the
//! surrounding modules can refer to it.

/// Default board orientation flag used by the upcoming Phase-4 widget.
///
/// Returning `false` means "white pieces at the bottom of the screen" — a
/// safe default until the user toggles the orientation via the menu bar.
pub fn flip_board_default() -> bool {
    false
}
