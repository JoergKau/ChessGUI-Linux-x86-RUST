//! One running engine as seen by the UI: process handle, event subscription
//! and analysis state, bundled so the UI only has to call [`EngineHost::pump`]
//! once per frame and read [`EngineHost::view`].
//!
//! Everything here is non-blocking: commands go out with
//! [`EngineProcess::try_send`], events come in with `try_recv`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::engine::analysis::{AnalysisSession, AnalysisStatus, SearchGoal};
use crate::engine::protocol::{EngineCommand, EngineEvent, EngineStats, Score, UciOption};
use crate::engine::watchdog::{Watchdog, WatchdogAction};
use crate::engine::{EngineProcess, ParsedUciLine};

/// Read-only snapshot of an engine for drawing its card.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineView {
    /// Engine name from the handshake, when known.
    pub name: Option<String>,
    /// File name of the binary, for display before the handshake finishes.
    pub binary_name: String,
    /// Badge state.
    pub status: AnalysisStatus,
    /// Statistics of the current search.
    pub stats: EngineStats,
}

/// A running engine plus its analysis state.
pub struct EngineHost {
    /// Process handle used to send commands and to shut the engine down.
    process: EngineProcess,
    /// This host's own lossless event stream.
    events: mpsc::UnboundedReceiver<ParsedUciLine>,
    /// Analysis state machine that decides which commands to send.
    analysis: AnalysisSession,
    /// Pings the engine with `isready` and notices when it stops answering.
    watchdog: Watchdog,
}

impl EngineHost {
    /// Take ownership of a freshly spawned engine and start its UCI handshake.
    ///
    /// Subscribes before sending anything, so no output is missed.
    pub fn attach_with_settings(process: EngineProcess, settings: BTreeMap<String, String>) -> Self {
        Self::attach_with(process, Watchdog::default(), settings)
    }

    /// [`Self::attach_with_settings`] with the watchdog timing given, so tests
    /// need not wait for the default intervals.
    pub fn attach_with(process: EngineProcess, watchdog: Watchdog, settings: BTreeMap<String, String>) -> Self {
        let events = process
            .take_event_receiver()
            .expect("an engine event receiver is handed out once");
        let mut host = Self {
            process,
            events,
            analysis: AnalysisSession::with_settings(settings),
            watchdog,
        };
        let handshake = host.analysis.handshake();
        host.dispatch(handshake);
        host
    }

    /// Turn analysis on or off and point it at `fen` with `goal`. The position
    /// is set first, so switching on starts a search on the new position.
    pub fn set_analysis(&mut self, enabled: bool, fen: &str, goal: SearchGoal) {
        self.set_position(fen, goal);
        let commands = self.analysis.set_enabled(enabled);
        self.dispatch(commands);
    }

    /// Point the engine at `fen` with `goal`. Repeating both is free.
    pub fn set_position(&mut self, fen: &str, goal: SearchGoal) {
        let commands = self.analysis.set_target(fen, goal);
        self.dispatch(commands);
    }

    /// Set a UCI option to `value`. It is applied now, or when the engine is
    /// idle again, and it is sent again after the engine is restarted.
    pub fn set_option(&mut self, name: &str, value: &str) {
        let commands = self.analysis.set_option(name, value);
        self.dispatch(commands);
    }

    /// Press a button option, such as `Clear Hash`.
    pub fn press_button(&mut self, name: &str) {
        let commands = self.analysis.press_button(name);
        self.dispatch(commands);
    }

    /// The options the engine declared at its handshake.
    pub fn options(&self) -> &[UciOption] {
        self.analysis.options()
    }

    /// The move the engine chose for its requested position, once known.
    pub fn take_engine_move(&mut self) -> Option<String> {
        self.analysis.take_engine_move()
    }

    /// The score of a finished evaluation, once (see `AnalysisSession::take_evaluation`).
    pub fn take_evaluation(&mut self) -> Option<Option<Score>> {
        self.analysis.take_evaluation()
    }

    /// A new game starts in this engine (`ucinewgame`, after any running search).
    pub fn new_game(&mut self) {
        let commands = self.analysis.new_game();
        self.dispatch(commands);
    }

    /// Drain every pending engine event and react to it. Call once per frame.
    /// Returns `true` when the engine's state may have changed, so the UI
    /// knows to repaint.
    pub fn pump(&mut self) -> bool {
        // The sticky flag catches an engine that exited before this host
        // subscribed, whose `Terminated` event was therefore never delivered.
        let before = self.analysis.status();
        if self.process.has_exited() {
            self.analysis.mark_offline();
        }
        let mut changed = before != self.analysis.status();
        loop {
            match self.events.try_recv() {
                Ok(ParsedUciLine::Event(event)) => {
                    changed = true;
                    if event == EngineEvent::ReadyOk {
                        self.watchdog.answered();
                    }
                    let commands = self.analysis.on_event(&event);
                    self.dispatch(commands);
                }
                Ok(ParsedUciLine::Unknown(line)) => {
                    log::trace!("{}: unhandled line `{}`", self.process.binary().display(), line);
                }
                Ok(ParsedUciLine::ParseError { line, source }) => {
                    log::debug!(
                        "{}: could not parse `{}`: {}",
                        self.process.binary().display(),
                        line,
                        source
                    );
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.analysis.mark_offline();
                    changed = true;
                    break;
                }
            }
        }
        changed |= self.check_watchdog();
        changed
    }

    /// Ping the engine when the watchdog says so. Returns `true` when the
    /// engine has just been found stalled. Nothing is pinged before the
    /// handshake is done, or after the engine is offline.
    fn check_watchdog(&mut self) -> bool {
        if self.watchdog.is_stalled()
            || matches!(self.analysis.status(), AnalysisStatus::Starting | AnalysisStatus::Offline)
        {
            return false;
        }
        match self.watchdog.check(Instant::now()) {
            WatchdogAction::Nothing => false,
            WatchdogAction::Ping => {
                self.dispatch(vec![EngineCommand::IsReady]);
                false
            }
            WatchdogAction::Stalled => {
                log::warn!("{} did not answer isready in time", self.process.binary().display());
                true
            }
        }
    }

    /// `true` once the engine has stopped answering `isready`. The app then
    /// shuts the host down and starts the engine again.
    pub fn is_stalled(&self) -> bool {
        self.watchdog.is_stalled()
    }

    /// The file the engine was started from, so it can be started again.
    pub fn binary_path(&self) -> PathBuf {
        self.process.binary().to_path_buf()
    }

    /// `true` once the engine's output has ended, whether or not its
    /// `Terminated` event has been seen yet.
    pub fn has_exited(&self) -> bool {
        self.process.has_exited()
    }

    /// Mark the engine offline and return its process so the caller can
    /// quit it. The host stays in its slot, so the card shows `Offline`.
    pub fn shutdown(&mut self) -> EngineProcess {
        self.analysis.mark_offline();
        self.process.clone()
    }

    /// Snapshot for the evaluation panel.
    pub fn view(&self) -> EngineView {
        EngineView {
            name: self.analysis.name().map(str::to_string),
            binary_name: self
                .process
                .binary()
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            status: self.analysis.status(),
            stats: self.analysis.stats().clone(),
        }
    }

    /// Forward commands to the writer task without waiting. A full or closed
    /// queue is logged; the analysis state recovers at the next position change.
    fn dispatch(&self, commands: Vec<EngineCommand>) {
        for command in commands {
            log::debug!("{} <- {}", self.process.binary().display(), command.to_wire_string());
            if let Err(error) = self.process.try_send(command.clone()) {
                log::warn!(
                    "could not send `{}` to {}: {}",
                    command.to_wire_string(),
                    self.process.binary().display(),
                    error
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — real Stockfish
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::GameState;
    use crate::engine::protocol::{GoControl, Score};
    use crate::engine::watchdog::Watchdog;
    use std::time::{Duration, Instant};

    /// Same fixture order as the engine tests: env override, Homebrew Stockfish, Triumviratus.
    /// Pump the host until `done` holds or the timeout passes.
    fn pump_until(host: &mut EngineHost, timeout: Duration, mut done: impl FnMut(&EngineView) -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            host.pump();
            if done(&host.view()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_analysis_reports_score_and_legal_pv() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        let game = GameState::new();
        let fen = game.fen();
        host.set_analysis(true, &fen, SearchGoal::Analyse);

        let searched = pump_until(&mut host, Duration::from_secs(20), |v| {
            v.stats.depth.is_some_and(|d| d >= 4) && !v.stats.pv.is_empty()
        });
        assert!(searched, "engine should reach depth 4 with a principal variation");

        let view = host.view();
        assert!(view.name.as_deref().is_some_and(|n| !n.is_empty()), "handshake name");
        assert!(view.stats.score.is_some(), "score present on search lines");
        let san = game.uci_moves_to_san(&view.stats.pv);
        assert_eq!(
            san.len(),
            view.stats.pv.len(),
            "every PV move must be legal from the analysed position"
        );

        host.set_analysis(false, &fen, SearchGoal::Analyse);
        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abort_stops_the_search_but_the_engine_stays_loaded_and_can_restart() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        let fen = GameState::new().fen();
        host.set_analysis(true, &fen, SearchGoal::Analyse);
        assert!(pump_until(&mut host, Duration::from_secs(20), |v| v.stats.depth.is_some()));

        // Abort: stop thinking, keep the engine.
        host.set_analysis(false, &fen, SearchGoal::Analyse);
        assert!(
            pump_until(&mut host, Duration::from_secs(20), |v| v.status == AnalysisStatus::Ready),
            "after Abort the engine must be Ready, not Thinking"
        );
        assert_ne!(host.view().status, AnalysisStatus::Offline, "Abort must not unload the engine");

        // Start again: a fresh search runs.
        host.set_analysis(true, &fen, SearchGoal::Analyse);
        assert!(pump_until(&mut host, Duration::from_secs(20), |v| v.status == AnalysisStatus::Thinking));
        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shut_down_engine_reads_as_offline_and_quits() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        host.set_analysis(true, &GameState::new().fen(), SearchGoal::Analyse);
        assert!(pump_until(&mut host, Duration::from_secs(20), |v| v.stats.depth.is_some()));

        let process = host.shutdown();
        assert_eq!(host.view().status, AnalysisStatus::Offline, "card must read Offline at once");
        process.terminate().await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process.has_exited() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(process.has_exited(), "process must actually exit after quit");
        host.pump();
        assert_eq!(host.view().status, AnalysisStatus::Offline, "host stays Offline");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn engine_that_exited_before_attach_shows_offline() {
        use std::os::unix::fs::PermissionsExt;

        // The process ends before the host subscribes, so the `Terminated`
        // event has already been sent. The sticky exit flag must still show it.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("exits-at-once.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let process = EngineProcess::spawn(&script).await.unwrap();
        while !process.has_exited() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        host.set_analysis(true, &GameState::new().fen(), SearchGoal::Analyse);
        host.pump();
        assert_eq!(host.view().status, AnalysisStatus::Offline);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_declares_its_options_and_takes_a_saved_value() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut settings = BTreeMap::new();
        settings.insert("Hash".to_string(), "32".to_string());
        let mut host = EngineHost::attach_with_settings(process, settings);
        assert!(
            pump_until(&mut host, Duration::from_secs(20), |v| v.status == AnalysisStatus::Ready),
            "handshake"
        );
        assert!(
            host.options().iter().any(|option| option.name == "Hash"),
            "the engine must declare the Hash option"
        );
        host.set_option("Hash", "64");
        host.pump();
        assert_eq!(host.view().status, AnalysisStatus::Ready, "a setoption must not disturb the engine");

        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn healthy_engine_answers_pings_while_it_searches() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let watchdog = Watchdog::new(Duration::from_millis(50), Duration::from_secs(3));
        let mut host = EngineHost::attach_with(process, watchdog, BTreeMap::new());
        host.set_analysis(true, &GameState::new().fen(), SearchGoal::Analyse);

        let until = Instant::now() + Duration::from_millis(1_500);
        while Instant::now() < until {
            host.pump();
            assert!(!host.is_stalled(), "an engine that searches must still answer isready");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(host.view().status, AnalysisStatus::Thinking);
        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn engine_that_stops_answering_is_reported_as_stalled() {
        use std::os::unix::fs::PermissionsExt;

        // Answers the handshake's `isready`, then goes silent.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("answers-once.sh");
        let body = "#!/bin/sh\nanswered=0\nwhile IFS= read -r line; do\n  case \"$line\" in\n    uci) echo 'id name Mute'; echo uciok;;\n    isready) if [ \"$answered\" = 0 ]; then echo readyok; answered=1; fi;;\n  esac\ndone\n";
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let process = EngineProcess::spawn(&script).await.unwrap();
        let watchdog = Watchdog::new(Duration::from_millis(50), Duration::from_millis(300));
        let mut host = EngineHost::attach_with(process, watchdog, BTreeMap::new());
        assert!(
            pump_until(&mut host, Duration::from_secs(10), |v| v.status == AnalysisStatus::Ready),
            "the mute engine must complete the handshake"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        while !host.is_stalled() && Instant::now() < deadline {
            host.pump();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(host.is_stalled(), "an engine that ignores isready must be reported as stalled");
        assert_eq!(host.binary_path(), script);

        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_retargets_to_the_new_position() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        let mut game = GameState::new();
        host.set_analysis(true, &game.fen(), SearchGoal::Analyse);
        assert!(pump_until(&mut host, Duration::from_secs(20), |v| v.stats.depth.is_some()));

        // Black to move after 1.e4: the engine must report the new side's score.
        game.play_squares(
            shakmaty::Square::from_ascii(b"e2").unwrap(),
            shakmaty::Square::from_ascii(b"e4").unwrap(),
            None,
        )
        .unwrap();
        host.set_position(&game.fen(), SearchGoal::Analyse);
        assert_eq!(host.view().status, AnalysisStatus::Thinking);
        let retargeted = pump_until(&mut host, Duration::from_secs(20), |v| {
            v.stats.pv.first().is_some_and(|m| m.len() == 4 && m != "e2e4")
        });
        assert!(retargeted, "PV must restart for the new position");
        let view = host.view();
        assert!(matches!(view.stats.score, Some(Score::Cp(_)) | Some(Score::Mate(_))));

        host.shutdown().terminate().await.unwrap();
    }

    /// Pump the host until it hands over a move, or give up after 20 s.
    fn await_engine_move(host: &mut EngineHost) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            host.pump();
            if let Some(uci) = host.take_engine_move() {
                return uci;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("engine must answer the move request with a bestmove");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_plays_a_legal_move_for_the_requested_position() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        let mut game = GameState::new();
        let fen = game.fen();
        let goal = SearchGoal::Move(GoControl::MoveTime(300));
        host.set_analysis(true, &fen, goal);

        let uci = await_engine_move(&mut host);
        assert!(game.apply_uci(&uci).is_ok(), "`{}` must be legal in the start position", uci);
        assert_eq!(host.view().status, AnalysisStatus::Ready, "the move search is over");

        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_plays_a_move_within_the_clock_it_is_given() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach_with_settings(process, BTreeMap::new());
        let game = GameState::new();
        let fen = game.fen();
        // The wire form `go wtime … btime … winc … binc …` must be accepted.
        let goal = SearchGoal::Move(GoControl::Clock {
            wtime: 3_000,
            btime: 3_000,
            winc: 0,
            binc: 0,
            movetime: None,
        });
        host.set_analysis(true, &fen, goal);

        let started = Instant::now();
        let uci = await_engine_move(&mut host);
        let mut replay = game.clone();
        assert!(replay.apply_uci(&uci).is_ok(), "`{}` must be legal in the start position", uci);
        assert!(started.elapsed() < Duration::from_secs(10), "3 s on the clock must not take 10 s");

        host.shutdown().terminate().await.unwrap();
    }
}
