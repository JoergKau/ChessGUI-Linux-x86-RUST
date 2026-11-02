//! Analysis state machine: keeps one engine searching the displayed position.
//!
//! [`AnalysisSession`] is pure: it takes engine events and returns the UCI
//! commands to send, with no I/O. [`crate::engine::EngineHost`] feeds it the
//! real event stream and forwards its commands to the process.
//!
//! Retargeting rule: when the position changes while the engine is searching,
//! send `stop` and wait for its `bestmove` before issuing `position` and
//! `go infinite`. Otherwise `info` lines from the old search would be shown
//! against the new position.

use crate::engine::protocol::{EngineCommand, EngineEvent, EngineStats, GoControl};

/// What the evaluation panel shows for an engine.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AnalysisStatus {
    /// Process started, UCI handshake not finished yet.
    Starting,
    /// Handshake done; analysis disabled or idle.
    Ready,
    /// Searching (or stopping a search before restarting).
    Thinking,
    /// Process exited or its output stream closed.
    Offline,
}

/// Internal search phase.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// No search running.
    Idle,
    /// `go infinite` has been sent and its `info` lines are accepted.
    Searching,
    /// `stop` has been sent; waiting for `bestmove` before restarting.
    AwaitingStop,
}

/// Analysis state for one engine. See the module docs for the protocol rules.
#[derive(Debug)]
pub struct AnalysisSession {
    /// Engine name from `id name …`, set when the handshake completes.
    name: Option<String>,
    /// `true` once the engine has answered `readyok`.
    ready: bool,
    /// `true` once the engine's stream ended.
    offline: bool,
    /// Whether analysis is switched on by the user.
    enabled: bool,
    /// FEN of the position the user is looking at.
    target: Option<String>,
    /// Current search phase.
    phase: Phase,
    /// Statistics of the current search.
    stats: EngineStats,
    /// `ucinewgame` was requested while a search was running; it is sent
    /// after the `bestmove` of that search.
    new_game_pending: bool,
}

impl Default for AnalysisSession {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisSession {
    /// A session with no engine handshake yet and analysis switched off.
    pub fn new() -> Self {
        Self {
            name: None,
            ready: false,
            offline: false,
            enabled: false,
            target: None,
            phase: Phase::Idle,
            stats: EngineStats::default(),
            new_game_pending: false,
        }
    }

    /// Commands that start the UCI handshake. Sent once when the process attaches.
    pub fn handshake(&mut self) -> Vec<EngineCommand> {
        vec![EngineCommand::Initialize]
    }

    /// A new game starts (`ucinewgame`). While a search runs, it is stopped
    /// first and `ucinewgame` goes out after its `bestmove`, because the UCI
    /// protocol does not allow `ucinewgame` during a search.
    pub fn new_game(&mut self) -> Vec<EngineCommand> {
        if self.offline {
            return Vec::new();
        }
        if self.phase == Phase::Idle {
            return vec![EngineCommand::NewGame];
        }
        self.new_game_pending = true;
        if self.phase == Phase::Searching {
            self.phase = Phase::AwaitingStop;
            return vec![EngineCommand::Stop];
        }
        Vec::new()
    }

    /// Switch analysis on or off. Turning it off stops a running search.
    pub fn set_enabled(&mut self, enabled: bool) -> Vec<EngineCommand> {
        if self.enabled == enabled {
            return Vec::new();
        }
        self.enabled = enabled;
        if enabled {
            return self.restart_if_possible();
        }
        match self.phase {
            Phase::Searching => {
                self.phase = Phase::AwaitingStop;
                vec![EngineCommand::Stop]
            }
            Phase::Idle | Phase::AwaitingStop => Vec::new(),
        }
    }

    /// Point the analysis at a new position (FEN). A repeated FEN is a no-op.
    pub fn set_target(&mut self, fen: &str) -> Vec<EngineCommand> {
        if self.target.as_deref() == Some(fen) {
            return Vec::new();
        }
        self.target = Some(fen.to_string());
        self.restart_if_possible()
    }

    /// Feed one engine event; returns the commands it triggers.
    pub fn on_event(&mut self, event: &EngineEvent) -> Vec<EngineCommand> {
        match event {
            EngineEvent::UciOk { name, .. } => {
                if !name.is_empty() {
                    self.name = Some(name.clone());
                }
                vec![EngineCommand::IsReady]
            }
            EngineEvent::ReadyOk => {
                if self.ready {
                    return Vec::new();
                }
                self.ready = true;
                self.restart_if_possible()
            }
            EngineEvent::Info(update) => {
                // Only the principal line (MultiPV 1) is shown.
                if self.phase == Phase::Searching && update.multipv.unwrap_or(1) == 1 {
                    self.stats.merge(update);
                }
                Vec::new()
            }
            EngineEvent::BestMove { .. } => match self.phase {
                Phase::AwaitingStop => {
                    self.phase = Phase::Idle;
                    let mut commands = self.take_pending_new_game();
                    commands.extend(self.restart_if_possible());
                    commands
                }
                Phase::Searching | Phase::Idle => {
                    self.phase = Phase::Idle;
                    self.take_pending_new_game()
                }
            },
            EngineEvent::Terminated => {
                self.mark_offline();
                Vec::new()
            }
        }
    }

    /// Record that the engine can no longer be reached.
    pub fn mark_offline(&mut self) {
        self.offline = true;
        self.phase = Phase::Idle;
    }

    /// Current status for the UI badge.
    pub fn status(&self) -> AnalysisStatus {
        if self.offline {
            AnalysisStatus::Offline
        } else if !self.ready {
            AnalysisStatus::Starting
        } else if self.phase != Phase::Idle {
            AnalysisStatus::Thinking
        } else {
            AnalysisStatus::Ready
        }
    }

    /// Statistics of the current search (empty until the first `info` line).
    pub fn stats(&self) -> &EngineStats {
        &self.stats
    }

    /// Engine name from the handshake, when known.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// `ucinewgame` if one was requested during a search, else nothing.
    fn take_pending_new_game(&mut self) -> Vec<EngineCommand> {
        if std::mem::take(&mut self.new_game_pending) {
            vec![EngineCommand::NewGame]
        } else {
            Vec::new()
        }
    }

    /// Start a search when everything needed is present, or ask the running
    /// search to stop so the next one can start.
    fn restart_if_possible(&mut self) -> Vec<EngineCommand> {
        if !self.enabled || !self.ready || self.offline || self.target.is_none() {
            return Vec::new();
        }
        match self.phase {
            Phase::Idle => self.start_search(),
            Phase::Searching => {
                self.phase = Phase::AwaitingStop;
                vec![EngineCommand::Stop]
            }
            // The pending `bestmove` will restart with the latest target.
            Phase::AwaitingStop => Vec::new(),
        }
    }

    /// Begin an infinite search on the current target, with cleared statistics.
    fn start_search(&mut self) -> Vec<EngineCommand> {
        let Some(fen) = self.target.clone() else {
            return Vec::new();
        };
        self.stats = EngineStats::default();
        self.phase = Phase::Searching;
        vec![
            EngineCommand::SetPosition {
                fen: Some(fen),
                moves: Vec::new(),
            },
            EngineCommand::Go(GoControl::Infinite),
        ]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::protocol::Score;

    const START: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";
    const AFTER_E4: &str = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1";

    /// A session that has completed the handshake.
    fn ready_session() -> AnalysisSession {
        let mut s = AnalysisSession::new();
        s.handshake();
        s.on_event(&EngineEvent::UciOk {
            name: "Fake 1".into(),
            author: String::new(),
            options: Vec::new(),
        });
        s.on_event(&EngineEvent::ReadyOk);
        s
    }

    fn depth_info(depth: u32, cp: i32) -> EngineEvent {
        EngineEvent::Info(EngineStats {
            depth: Some(depth),
            score: Some(Score::Cp(cp)),
            pv: vec!["e2e4".into()],
            ..EngineStats::default()
        })
    }

    #[test]
    fn handshake_requests_init_then_isready_on_uciok() {
        let mut s = AnalysisSession::new();
        assert_eq!(s.handshake(), vec![EngineCommand::Initialize]);
        assert_eq!(s.status(), AnalysisStatus::Starting);
        let cmds = s.on_event(&EngineEvent::UciOk {
            name: "Fake 1".into(),
            author: String::new(),
            options: Vec::new(),
        });
        assert_eq!(cmds, vec![EngineCommand::IsReady]);
        assert_eq!(s.name(), Some("Fake 1"));
    }

    #[test]
    fn enabling_analysis_after_readyok_starts_an_infinite_search() {
        let mut s = ready_session();
        assert_eq!(s.status(), AnalysisStatus::Ready);
        assert!(s.set_target(START).is_empty(), "analysis is still off");
        let cmds = s.set_enabled(true);
        assert_eq!(
            cmds,
            vec![
                EngineCommand::SetPosition {
                    fen: Some(START.into()),
                    moves: Vec::new(),
                },
                EngineCommand::Go(GoControl::Infinite),
            ]
        );
        assert_eq!(s.status(), AnalysisStatus::Thinking);
    }

    #[test]
    fn target_set_before_readyok_starts_once_the_engine_is_ready() {
        let mut s = AnalysisSession::new();
        s.handshake();
        s.set_enabled(true);
        assert!(s.set_target(START).is_empty());
        let cmds = s.on_event(&EngineEvent::ReadyOk);
        assert_eq!(cmds.len(), 2);
        assert_eq!(s.status(), AnalysisStatus::Thinking);
    }

    #[test]
    fn retargeting_while_searching_stops_first_and_restarts_on_bestmove() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        s.on_event(&depth_info(5, 20));

        // The position changes: only `stop` goes out for now.
        assert_eq!(s.set_target(AFTER_E4), vec![EngineCommand::Stop]);
        assert_eq!(s.status(), AnalysisStatus::Thinking);

        // Late info from the old search is ignored.
        assert!(s.on_event(&depth_info(9, -999)).is_empty());
        assert_eq!(s.stats().depth, Some(5));

        // bestmove arrives: the new position is searched.
        let restart = s.on_event(&EngineEvent::BestMove {
            best_move: Some("e7e5".into()),
            ponder: None,
        });
        assert_eq!(
            restart,
            vec![
                EngineCommand::SetPosition {
                    fen: Some(AFTER_E4.into()),
                    moves: Vec::new(),
                },
                EngineCommand::Go(GoControl::Infinite),
            ]
        );
        assert_eq!(s.stats(), &EngineStats::default(), "stats reset for the new position");
    }

    #[test]
    fn repeated_target_sends_nothing() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        assert!(s.set_target(START).is_empty());
    }

    #[test]
    fn info_lines_merge_into_the_running_statistics() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        s.on_event(&depth_info(4, 30));
        s.on_event(&EngineEvent::Info(EngineStats {
            depth: Some(6),
            nps: Some(1_200_000),
            ..EngineStats::default()
        }));
        assert_eq!(s.stats().depth, Some(6));
        assert_eq!(s.stats().nps, Some(1_200_000));
        assert_eq!(s.stats().score, Some(Score::Cp(30)), "score kept from earlier line");
    }

    #[test]
    fn secondary_multipv_lines_are_ignored() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        s.on_event(&EngineEvent::Info(EngineStats {
            depth: Some(8),
            multipv: Some(2),
            score: Some(Score::Cp(-50)),
            ..EngineStats::default()
        }));
        assert_eq!(s.stats().depth, None);
    }

    #[test]
    fn disabling_analysis_stops_and_does_not_restart() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        assert_eq!(s.set_enabled(false), vec![EngineCommand::Stop]);
        let after = s.on_event(&EngineEvent::BestMove {
            best_move: Some("e2e4".into()),
            ponder: None,
        });
        assert!(after.is_empty());
        assert_eq!(s.status(), AnalysisStatus::Ready);
    }

    #[test]
    fn terminated_engine_reports_offline() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        s.on_event(&EngineEvent::Terminated);
        assert_eq!(s.status(), AnalysisStatus::Offline);
        assert!(s.set_target(AFTER_E4).is_empty(), "offline engines get no commands");
    }

    #[test]
    fn new_game_during_a_search_waits_for_bestmove() {
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        assert_eq!(s.new_game(), vec![EngineCommand::Stop]);
        // The new position arrives while the stop is pending: nothing yet.
        assert!(s.set_target(AFTER_E4).is_empty());
        let after_stop = s.on_event(&EngineEvent::BestMove {
            best_move: Some("e2e4".into()),
            ponder: None,
        });
        assert_eq!(
            after_stop,
            vec![
                EngineCommand::NewGame,
                EngineCommand::SetPosition {
                    fen: Some(AFTER_E4.into()),
                    moves: Vec::new(),
                },
                EngineCommand::Go(GoControl::Infinite),
            ]
        );
    }

    #[test]
    fn new_game_is_not_sent_to_an_offline_engine() {
        let mut s = ready_session();
        s.mark_offline();
        assert!(s.new_game().is_empty());
    }

    #[test]
    fn new_game_while_idle_is_sent_immediately() {
        let mut s = ready_session();
        assert_eq!(s.new_game(), vec![EngineCommand::NewGame]);
    }

    #[test]
    fn bestmove_none_does_not_break_the_cycle() {
        // Mate/stalemate positions answer `bestmove (none)`.
        let mut s = ready_session();
        s.set_enabled(true);
        s.set_target(START);
        let cmds = s.on_event(&EngineEvent::BestMove {
            best_move: None,
            ponder: None,
        });
        assert!(cmds.is_empty());
        assert_eq!(s.status(), AnalysisStatus::Ready);
    }
}
