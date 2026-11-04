//! Engine watchdog: detects a search that has gone silent.
//!
//! The host only pings while the engine is searching. An idle loaded engine
//! does not need a heartbeat: a process that exits is caught by the reader,
//! and a hung engine is caught by the watchdog during its next search. That
//! keeps an idle session quiet — no pings, no repaint tick and no log noise
//! after a game or analysis has stopped.
//!
//! The UCI protocol requires `readyok` at any time, even during a search, so a
//! healthy engine answers every ping promptly. Some engines only read input
//! between search iterations, though, and can be slow to answer while they are
//! still streaming `info` lines. The watchdog therefore treats recent stdout
//! as proof of life as well: a ping is only overdue once the engine has both
//! ignored it *and* produced no output for the response timeout.
//!
//! [`Watchdog`] is pure timing: the host asks it what to do with the current
//! instant, sends a ping when told to, reports each `readyok`, and notes every
//! stdout line.

use std::time::{Duration, Instant};

/// How often a ping is sent while the engine is up.
pub const PING_INTERVAL: Duration = Duration::from_secs(5);

/// How long a ping may wait for its `readyok` before the engine is stalled.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// What the host should do after a check.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WatchdogAction {
    /// Nothing to do.
    Nothing,
    /// Send `isready` now.
    Ping,
    /// The last ping went unanswered past the timeout. Reported once per check
    /// after the timeout; the state stays stalled.
    Stalled,
}

/// Timing state for one engine's `isready` pings.
#[derive(Clone, Debug)]
pub struct Watchdog {
    /// Time between pings.
    ping_every: Duration,
    /// Time a ping may wait for its answer.
    response_within: Duration,
    /// When the last ping was sent, if any.
    last_ping: Option<Instant>,
    /// When the ping still waiting for `readyok` was sent, if any.
    awaiting: Option<Instant>,
    /// When the engine last produced output, if any.
    last_output: Option<Instant>,
    /// Sticky once a ping has gone unanswered past the timeout.
    stalled: bool,
}

impl Default for Watchdog {
    /// Pings every [`PING_INTERVAL`] and allows [`RESPONSE_TIMEOUT`] for an answer.
    fn default() -> Self {
        Self::new(PING_INTERVAL, RESPONSE_TIMEOUT)
    }
}

impl Watchdog {
    /// A watchdog with the given ping interval and response timeout.
    pub fn new(ping_every: Duration, response_within: Duration) -> Self {
        Self {
            ping_every,
            response_within,
            last_ping: None,
            awaiting: None,
            last_output: None,
            stalled: false,
        }
    }

    /// Decide what to do at `now`. Sends nothing by itself: a
    /// [`WatchdogAction::Ping`] means the caller must send `isready`.
    pub fn check(&mut self, now: Instant) -> WatchdogAction {
        if self.stalled {
            return WatchdogAction::Stalled;
        }
        if let Some(sent) = self.awaiting {
            if now.duration_since(sent) > self.response_within {
                // Output in the last window means the engine is alive, even
                // if it has not reached its input loop yet. Ping again and
                // extend the deadline instead of restarting a working search.
                let output_is_recent = self
                    .last_output
                    .is_some_and(|at| now.duration_since(at) <= self.response_within);
                if output_is_recent {
                    self.awaiting = Some(now);
                    self.last_ping = Some(now);
                    return WatchdogAction::Ping;
                }
                self.stalled = true;
                return WatchdogAction::Stalled;
            }
            return WatchdogAction::Nothing;
        }
        let due = match self.last_ping {
            None => true,
            Some(last) => now.duration_since(last) >= self.ping_every,
        };
        if due {
            self.awaiting = Some(now);
            self.last_ping = Some(now);
            WatchdogAction::Ping
        } else {
            WatchdogAction::Nothing
        }
    }

    /// Record a `readyok`. It answers the ping that is waiting, if any.
    pub fn answered(&mut self) {
        self.awaiting = None;
    }

    /// Record any line from the engine. Output during a search is proof that
    /// the engine is alive even when its `readyok` is late.
    pub fn note_output(&mut self, now: Instant) {
        self.last_output = Some(now);
    }

    /// Forget the timing of the previous search, so the next search starts a
    /// fresh ping window. Called while the engine is idle.
    pub fn idle(&mut self) {
        if !self.stalled {
            self.last_ping = None;
            self.awaiting = None;
            self.last_output = None;
        }
    }

    /// `true` once a ping has gone unanswered past the timeout.
    pub fn is_stalled(&self) -> bool {
        self.stalled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn first_check_pings_then_waits_for_the_interval() {
        let base = Instant::now();
        let mut w = Watchdog::new(Duration::from_millis(100), Duration::from_millis(50));
        assert_eq!(w.check(at(base, 0)), WatchdogAction::Ping);
        w.answered();
        assert_eq!(w.check(at(base, 50)), WatchdogAction::Nothing);
        assert_eq!(w.check(at(base, 100)), WatchdogAction::Ping);
    }

    #[test]
    fn no_second_ping_while_one_is_outstanding() {
        let base = Instant::now();
        let mut w = Watchdog::new(Duration::from_millis(100), Duration::from_millis(500));
        assert_eq!(w.check(at(base, 0)), WatchdogAction::Ping);
        assert_eq!(w.check(at(base, 200)), WatchdogAction::Nothing);
        assert!(!w.is_stalled(), "still inside the response timeout");
    }

    #[test]
    fn unanswered_ping_past_the_timeout_stalls_the_engine() {
        let base = Instant::now();
        let mut w = Watchdog::new(Duration::from_millis(100), Duration::from_millis(300));
        assert_eq!(w.check(at(base, 0)), WatchdogAction::Ping);
        assert_eq!(w.check(at(base, 300)), WatchdogAction::Nothing, "exactly at the timeout is not late");
        assert_eq!(w.check(at(base, 301)), WatchdogAction::Stalled);
        assert!(w.is_stalled());
        assert_eq!(w.check(at(base, 5_000)), WatchdogAction::Stalled, "stalled stays stalled");
    }

    #[test]
    fn recent_output_keeps_a_slow_engine_alive() {
        let base = Instant::now();
        let mut w = Watchdog::new(Duration::from_millis(100), Duration::from_millis(300));
        assert_eq!(w.check(at(base, 0)), WatchdogAction::Ping);
        // Search output arrives while the engine has not reached `isready`
        // yet. The ping is overdue, but the engine is clearly alive.
        w.note_output(at(base, 250));
        assert_eq!(
            w.check(at(base, 350)),
            WatchdogAction::Ping,
            "recent output must extend the deadline, not restart the search"
        );
        assert!(!w.is_stalled());
    }

    #[test]
    fn silence_after_the_last_output_still_stalls() {
        let base = Instant::now();
        let mut w = Watchdog::new(Duration::from_millis(100), Duration::from_millis(300));
        assert_eq!(w.check(at(base, 0)), WatchdogAction::Ping);
        w.note_output(at(base, 250));
        assert_eq!(w.check(at(base, 350)), WatchdogAction::Ping);
        // No output and no `readyok` since then: the engine is wedged.
        assert_eq!(w.check(at(base, 700)), WatchdogAction::Stalled);
        assert!(w.is_stalled());
    }

    #[test]
    fn an_answer_in_time_keeps_the_engine_healthy() {
        let base = Instant::now();
        let mut w = Watchdog::new(Duration::from_millis(100), Duration::from_millis(300));
        assert_eq!(w.check(at(base, 0)), WatchdogAction::Ping);
        w.answered();
        assert_eq!(w.check(at(base, 50)), WatchdogAction::Nothing);
        assert_eq!(w.check(at(base, 100)), WatchdogAction::Ping);
        w.answered();
        assert_eq!(w.check(at(base, 150)), WatchdogAction::Nothing);
        assert!(!w.is_stalled());
    }
}
