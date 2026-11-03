//! Engine watchdog: detects an engine that stops answering `isready`.
//!
//! The UCI protocol requires `readyok` at any time, even during a search. So
//! a healthy engine answers every ping promptly. [`Watchdog`] is pure timing:
//! the host asks it what to do with the current instant, sends a ping when told
//! to, and reports each `readyok`. Once a ping goes unanswered for longer than
//! the response timeout, the engine counts as stalled.

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
