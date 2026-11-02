//! Time-control arbiter.
//!
//! Tracks the remaining time on both sides and applies the appropriate
//! increment / fixed-time rules on each move. Although full integration
//! lands in Phase 5, the data structure and helper methods are already
//! usable so the Phase 4 menu builder can wire up a "Time Control" dialog
//! against a real `Clock` instance.
//!
//! # Properties
//! * Microsoft-style monophonic design — there is only ever one active
//!   player at a time, tracked by [`Clock::side`].
//! * Time accounting uses `std::time::Instant` for monotonic wall-clock
//!   reading; we never compare against `SystemTime` (so DST or NTP
//!   leaps have no effect).
//! * The clock is **greedy** for the active side — every read of
//!   [`Clock::remaining_ms`] reflects the live wall-clock delta since
//!   the last `tap()`. The Phase-5 game loop is expected to call
//!   `tap()` immediately after each engine / user move.

use std::time::Instant;

use crate::chess::PieceColor;

// ─────────────────────────────────────────────────────────────────────────────
// Time-control configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Time-control mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeMode {
    /// Single countdown per side. Reaching 0 forfeits the game.
    SuddenDeath,
    /// Fischer style: `base_ms + increment_ms` per move.
    FischerIncrement,
    /// Each player gets exactly `base_ms` for the whole game, with no
    /// per-move increment. Useful for blitz.
    FixedPerMove,
}

/// Configuration parameters describing a time control. Reads as a recipe;
/// turned into a live [`Clock`] via [`TimeControl::into_clock`].
#[derive(Copy, Clone, Debug)]
pub struct TimeControl {
    pub mode: TimeMode,
    pub base_ms: u64,
    pub increment_ms: u64,
}

impl Default for TimeControl {
    /// 5-minute Fischer with a 3-second increment.
    fn default() -> Self {
        Self {
            mode: TimeMode::FischerIncrement,
            base_ms: 5 * 60 * 1000,
            increment_ms: 3 * 1000,
        }
    }
}

impl TimeControl {
    /// Construct a new time control with the given parameters.
    pub const fn new(mode: TimeMode, base_ms: u64, increment_ms: u64) -> Self {
        Self {
            mode,
            base_ms,
            increment_ms,
        }
    }

    /// Sudden death (e.g. 60 minutes, no increment).
    pub const fn sudden_death(base_ms: u64) -> Self {
        Self::new(TimeMode::SuddenDeath, base_ms, 0)
    }

    /// Fischer increment (e.g. 5+3).
    pub const fn fischer(base_ms: u64, increment_ms: u64) -> Self {
        Self::new(TimeMode::FischerIncrement, base_ms, increment_ms)
    }

    /// Fixed time per move (e.g. 10 s per move): every turn starts with the
    /// full `base_ms` allowance and unused time is not carried over.
    pub const fn fixed_per_move(base_ms: u64) -> Self {
        Self::new(TimeMode::FixedPerMove, base_ms, 0)
    }

    /// Convert into a live [`Clock`] starting from the beginning of the
    /// game (white to move).
    pub fn into_clock(self) -> Clock {
        Clock::new(self, PieceColor::White)
    }

    /// Human-readable label for the time control — used in menus and in
    /// the Phase-5 eval panel's "current game" widget.
    pub fn label(&self) -> String {
        let base_min = self.base_ms / 60_000;
        let base_sec = (self.base_ms % 60_000) / 1000;
        match self.mode {
            TimeMode::SuddenDeath => {
                if base_min >= 1 {
                    format!("Sudden Death {}m", base_min)
                } else {
                    format!("Sudden Death {}s", base_sec)
                }
            }
            TimeMode::FischerIncrement => {
                let inc_sec = self.increment_ms / 1000;
                if base_min >= 1 {
                    format!("Fischer {}+{}", base_min, inc_sec)
                } else {
                    format!("Fischer {}s +{}s", base_sec, inc_sec)
                }
            }
            TimeMode::FixedPerMove => format!("Fixed {}s per move", self.base_ms / 1000),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Live clock
// ─────────────────────────────────────────────────────────────────────────────

/// Live game clock. Constructed via [`TimeControl::into_clock`].
///
/// Cloning is intentionally fine — once a clock exists, multiple widgets
/// (board label, eval dashboard, system tray) can hold owned copies; all
/// edits go through `&mut self`.
#[derive(Clone, Debug)]
pub struct Clock {
    control: TimeControl,
    /// Remaining time per side, in milliseconds.
    remaining: [u64; 2],
    /// Whose turn it currently is.
    pub side: PieceColor,
    /// Wall-clock instant the current side's turn began. None iff the
    /// clock is paused (game not yet started / explicitly paused).
    turn_started: Option<Instant>,
    /// `true` iff a side has run out of time. Frozen — further taps are
    /// no-ops until [`Clock::reset`] is called.
    expired_for: Option<PieceColor>,
}

impl Clock {
    /// Build a fresh clock from a [`TimeControl`], starting with the given
    /// side to move. By default `turn_started = None`; call
    /// [`Clock::start`] to begin countdown.
    pub fn new(control: TimeControl, starting_side: PieceColor) -> Self {
        let base_ms = control.base_ms;
        Self {
            control,
            remaining: [base_ms, base_ms],
            side: starting_side,
            turn_started: None,
            expired_for: None,
        }
    }

    /// Begin counting down. Idempotent.
    pub fn start(&mut self) {
        if self.turn_started.is_none() && self.expired_for.is_none() {
            self.turn_started = Some(Instant::now());
        }
    }

    /// Pause without switching sides; resume later with another `start`.
    pub fn pause(&mut self) {
        self.commit_active();
        self.turn_started = None;
    }

    /// Reset to the initial state of the configured time control. Used
    /// when starting a new game.
    pub fn reset(&mut self, starting_side: PieceColor) {
        let base = self.control.base_ms;
        self.remaining = [base, base];
        self.side = starting_side;
        self.expired_for = None;
        self.turn_started = None;
    }

    /// Switch the active side — apply the configured time-control rules
    /// to the side that just finished a move and start the new side's
    /// timer.
    ///
    /// # Returns
    /// `Some(side)` of the player who has run out of time, otherwise
    /// `None`.
    pub fn tap(&mut self) -> Option<PieceColor> {
        if let Some(flagged) = self.expired_for {
            return Some(flagged);
        }
        self.commit_active();

        // The side that just moved ran out of time while thinking: it loses
        // immediately and gets no increment.
        if let Some(flagged) = self.expired_for {
            self.turn_started = None;
            return Some(flagged);
        }

        // Apply increment (only Fischer-style gets it).
        if matches!(self.control.mode, TimeMode::FischerIncrement) {
            let just_played_idx = self.side.index();
            self.remaining[just_played_idx] =
                self.remaining[just_played_idx].saturating_add(self.control.increment_ms);
        }

        // Switch sides.
        self.side = self.side.opposite();
        self.turn_started = Some(Instant::now());

        // Fixed-per-move: every turn gets the full allowance, nothing carries over.
        if matches!(self.control.mode, TimeMode::FixedPerMove) {
            self.remaining[self.side.index()] = self.control.base_ms;
        }

        // Did we just expire the new active side?
        if self.remaining[self.side.index()] == 0 {
            self.expired_for = Some(self.side);
            self.turn_started = None;
            return Some(self.side);
        }
        None
    }

    /// Live remaining time for a given side, *including* the live
    /// countdown of the active side.
    pub fn remaining_ms(&self, color: PieceColor) -> u64 {
        let idx = color.index();
        let stored = self.remaining[idx];
        if color == self.side {
            if let Some(started) = self.turn_started {
                let elapsed = started.elapsed().as_millis() as u64;
                stored.saturating_sub(elapsed)
            } else {
                stored
            }
        } else {
            stored
        }
    }

    /// `true` iff the active side has run out of time (and the clock has
    /// not been reset since).
    pub fn is_expired(&self) -> bool {
        self.expired_for.is_some()
    }

    /// `Some(side)` of the player whose clock has expired, `None` if the
    /// game is still live. Equivalent to [`Self::is_expired`] but more
    /// ergonomically named for callers building UI labels.
    pub fn flagged(&self) -> Option<PieceColor> {
        self.expired_for
    }

    /// The configured time control.
    pub fn control(&self) -> TimeControl {
        self.control
    }

    /// Format the remaining time of `color` as e.g. `"00:04:32"`.
    /// Convenience for the eval panel's "clock" label.
    pub fn remaining_label(&self, color: PieceColor) -> String {
        let ms = self.remaining_ms(color);
        let secs = ms / 1000;
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }

    // ── Internal ─────────────────────────────────────────────────────────

    /// Snapshot the live countdown back into `remaining[active]`.
    fn commit_active(&mut self) {
        if let Some(started) = self.turn_started.take() {
            let elapsed = started.elapsed().as_millis() as u64;
            let idx = self.side.index();
            self.remaining[idx] = self.remaining[idx].saturating_sub(elapsed);
            // Detect "just expired" so we can flag without further taps.
            if self.remaining[idx] == 0 {
                self.expired_for = Some(self.side);
            }
        }
    }
}

// Tiny extension trait on `Color` so the clock can `index()` into a `[u64; 2]`.
trait ColorIndexExt {
    fn index(self) -> usize;
}

impl ColorIndexExt for PieceColor {
    #[inline]
    fn index(self) -> usize {
        match self {
            PieceColor::White => 0,
            PieceColor::Black => 1,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fischer_increment_adds_after_each_move() {
        let mut clock = TimeControl::fischer(60_000, 5_000).into_clock();
        clock.start();
        // Simulate white playing for 2 seconds.
        std::thread::sleep(Duration::from_millis(20));
        // Tap while sleep is tiny so timing won't flake — we just want to
        // know that *some* time was subtracted before increment.
        clock.tap();
        // After white's first move, white hasn't run out, and increment
        // has been applied (≥ 5_000ms added). We can't reliably test
        // millisecond-precise subtraction in a test, but we can assert
        // the lower bound.
        assert!(clock.remaining_ms(PieceColor::White) >= 60_000);

        // Simulate black playing and swapping back.
        std::thread::sleep(Duration::from_millis(10));
        clock.tap();
        assert!(clock.remaining_ms(PieceColor::Black) >= 60_000);
    }

    #[test]
    fn reset_restores_initial_time() {
        let mut clock = TimeControl::sudden_death(120_000).into_clock();
        clock.start();
        clock.remaining[0] = 5_000;
        clock.reset(PieceColor::White);
        assert_eq!(clock.remaining_ms(PieceColor::White), 120_000);
        assert!(!clock.is_expired());
    }

    #[test]
    fn label_formatting_examples() {
        let one_min_sudden = TimeControl::sudden_death(60_000);
        assert_eq!(one_min_sudden.label(), "Sudden Death 1m");
        let five_three = TimeControl::fischer(300_000, 3_000);
        assert_eq!(five_three.label(), "Fischer 5+3");
        let ten = TimeControl::sudden_death(10_000);
        assert_eq!(ten.label(), "Sudden Death 10s");
    }

    #[test]
    fn tap_reports_side_that_flagged() {
        // 10 ms of base time, 5 s increment: White overruns on the first tap.
        let mut clock = TimeControl::fischer(10, 5_000).into_clock();
        clock.start();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(clock.tap(), Some(PieceColor::White));
        assert_eq!(clock.flagged(), Some(PieceColor::White));
        assert!(clock.is_expired());
        // The increment must not revive a side that already flagged.
        assert_eq!(clock.remaining_ms(PieceColor::White), 0);
    }

    #[test]
    fn fixed_per_move_resets_clock_each_turn() {
        let mut clock = TimeControl::fixed_per_move(1_000).into_clock();
        clock.start();
        std::thread::sleep(Duration::from_millis(30));
        clock.tap();
        // Black's clock starts a fresh 1 s turn.
        assert!(clock.remaining_ms(PieceColor::Black) >= 990);
        std::thread::sleep(Duration::from_millis(30));
        clock.tap();
        // White's clock is reset to the full per-move allowance again.
        assert!(clock.remaining_ms(PieceColor::White) >= 990);
    }

    #[test]
    fn pause_then_start_does_not_double_count() {
        let mut clock = TimeControl::fischer(60_000, 0).into_clock();
        clock.start();
        std::thread::sleep(Duration::from_millis(20));
        clock.pause();
        let t_paused = clock.remaining_ms(PieceColor::White);

        // Sleep through a chunk that should NOT be deducted.
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(clock.remaining_ms(PieceColor::White), t_paused);

        clock.start();
        std::thread::sleep(Duration::from_millis(20));
        let t_after_resume = clock.remaining_ms(PieceColor::White);
        assert!(t_after_resume < t_paused, "expected timer to resume");
    }
}
