//! One running engine as seen by the UI: process handle, event subscription
//! and analysis state, bundled so the UI only has to call [`EngineHost::pump`]
//! once per frame and read [`EngineHost::view`].
//!
//! Everything here is non-blocking: commands go out with
//! [`EngineProcess::try_send`], events come in with `try_recv`.

use tokio::sync::broadcast::{self, error::TryRecvError};

use crate::engine::analysis::{AnalysisSession, AnalysisStatus};
use crate::engine::protocol::{EngineCommand, EngineStats};
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
    /// This host's own subscription to the engine's output.
    events: broadcast::Receiver<ParsedUciLine>,
    /// Analysis state machine that decides which commands to send.
    analysis: AnalysisSession,
}

impl EngineHost {
    /// Take ownership of a freshly spawned engine and start its UCI handshake.
    ///
    /// Subscribes before sending anything, so no output is missed.
    pub fn attach(process: EngineProcess) -> Self {
        let events = process.subscribe();
        let mut host = Self {
            process,
            events,
            analysis: AnalysisSession::new(),
        };
        let handshake = host.analysis.handshake();
        host.dispatch(handshake);
        host
    }

    /// Turn analysis on or off and point it at `fen`.
    pub fn set_analysis(&mut self, enabled: bool, fen: &str) {
        let commands = self.analysis.set_enabled(enabled);
        self.dispatch(commands);
        self.set_position(fen);
    }

    /// Point the analysis at `fen`. Repeating the same FEN is free.
    pub fn set_position(&mut self, fen: &str) {
        let commands = self.analysis.set_target(fen);
        self.dispatch(commands);
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
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Lagged(skipped)) => {
                    log::warn!(
                        "{}: UI fell behind, {} engine lines dropped",
                        self.process.binary().display(),
                        skipped
                    );
                }
                Err(TryRecvError::Closed) => {
                    self.analysis.mark_offline();
                    changed = true;
                    break;
                }
            }
        }
        changed
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
    use crate::engine::protocol::Score;
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
        let mut host = EngineHost::attach(process);
        let game = GameState::new();
        let fen = game.fen();
        host.set_analysis(true, &fen);

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

        host.set_analysis(false, &fen);
        host.shutdown().terminate().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shut_down_engine_reads_as_offline_and_quits() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach(process);
        host.set_analysis(true, &GameState::new().fen());
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
        let mut host = EngineHost::attach(process);
        host.set_analysis(true, &GameState::new().fen());
        host.pump();
        assert_eq!(host.view().status, AnalysisStatus::Offline);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn real_engine_retargets_to_the_new_position() {
        let binary = crate::engine::test_engine_path();
        let process = EngineProcess::spawn(&binary).await.unwrap();
        let mut host = EngineHost::attach(process);
        let mut game = GameState::new();
        host.set_analysis(true, &game.fen());
        assert!(pump_until(&mut host, Duration::from_secs(20), |v| v.stats.depth.is_some()));

        // Black to move after 1.e4: the engine must report the new side's score.
        game.play_squares(
            shakmaty::Square::from_ascii(b"e2").unwrap(),
            shakmaty::Square::from_ascii(b"e4").unwrap(),
            None,
        )
        .unwrap();
        host.set_position(&game.fen());
        assert_eq!(host.view().status, AnalysisStatus::Thinking);
        let retargeted = pump_until(&mut host, Duration::from_secs(20), |v| {
            v.stats.pv.first().is_some_and(|m| m.len() == 4 && m != "e2e4")
        });
        assert!(retargeted, "PV must restart for the new position");
        let view = host.view();
        assert!(matches!(view.stats.score, Some(Score::Cp(_)) | Some(Score::Mate(_))));

        host.shutdown().terminate().await.unwrap();
    }
}
