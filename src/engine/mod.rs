//! UCI engine subsystem.
//!
//! Implements an async, tokio-based UCI engine supervisor whose architecture
//! mirrors the proven `Rust-Chess-GUI-0.2.1` reference: **three dedicated
//! tokio tasks** per engine subprocess —
//!
//! 1. **Writer task** — drains `cmd_rx` and writes each line to the
//!    child's stdin via a buffered writer.
//! 2. **Reader task** — pulls lines from the child's stdout via
//!    `read_line` and publishes each line to a `tokio::sync::broadcast`
//!    channel so multiple consumers can subscribe.
//! 3. **Supervisor task** — waits on either `kill_rx` or
//!    `child.wait()`; whichever fires first determines cleanup.
//!
//! Splitting writer/reader into separate tasks is essential because UCI
//! engines can produce dozens of `info …` lines per second — without the
//! split, a slow downstream consumer could deadlock the writer.
//
//! ## Why no `tokio::select!` in the inner loop?
//!
//! An earlier draft multiplexed read + write + terminate inside a single
//! `tokio::select!`, but the compile-time handshake between variadic
//! branches and `?Sized` future outputs was brittle — and `Rust-Chess-GUI`
//! demonstrates that the three-task layout is just as performant and far
//! easier to reason about. We follow that pattern here.

pub mod protocol;
pub mod uci_parser;

pub use protocol::{
    EngineCommand, EngineError, EngineEvent, EngineStats, GoControl, Score, UciOption,
    UciOptionKind,
};
pub use uci_parser::{parse_line, HandshakeAccumulator, UciParseError};

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc, Mutex};

// ─────────────────────────────────────────────────────────────────────────────
// EngineProcess
// ─────────────────────────────────────────────────────────────────────────────

/// Capacity for the broadcast channel of stdout lines.
/// Sized for high-volume engines (Stockfish can emit thousands of `info`
/// lines at depth 30+, and a missed bestmove would be catastrophic).
const BROADCAST_BUFFER: usize = 10_000;

/// Capacity for the command mpsc. Four slots is enough to absorb UI bursts
/// without back-pressuring the user.
const COMMAND_BUFFER: usize = 4;

/// Handle for sending commands to a running engine.
///
/// Cloning is cheap (`mpsc::Sender` is `Clone`), so multiple UI components
/// can hold their own copy (a future "stop search" button next to the main
/// board widget, for example).
#[derive(Clone)]
pub struct EngineProcess {
    /// Channel for sending wire-encoded UCI commands to the writer task.
    cmd_tx: mpsc::Sender<EngineCommand>,
    /// Channel for signalling termination.
    kill_tx: mpsc::Sender<()>,
    /// Broadcast channel for engine stdout lines (one line per message).
    pub events: broadcast::Sender<ParsedUciLine>,
    /// `true` while the supervisor task is still alive. UI can poll this
    /// without blocking.
    pub is_alive: Arc<Mutex<bool>>,
    /// Path of the spawned binary, kept for logging / settings.
    pub binary: PathBuf,
}

/// One event from the engine, already trimmed and reduced.
#[derive(Debug, Clone)]
pub enum ParsedUciLine {
    /// Line was successfully reduced to an event. Use this in tests and
    /// downstream subscribers that want the typed events; the broadcast
    /// layer ensures every subscriber sees every line exactly once.
    Event(EngineEvent),
    /// Line was unreadable / unparseable. We carry the raw line so the UI
    /// can render it in a debug log if needed.
    ParseError {
        line: String,
        source: UciParseError,
    },
    /// Unknown / engine-specific line (treated as info, not an error).
    Unknown(String),
}

impl ParsedUciLine {
    /// Returns the raw engine-side line, regardless of category.
    pub fn raw_line(&self) -> &str {
        match self {
            ParsedUciLine::Event(_) => "",
            ParsedUciLine::ParseError { line, .. } => line,
            ParsedUciLine::Unknown(line) => line,
        }
    }
}

impl EngineProcess {
    /// Spawn the engine binary as a tokio subprocess and start the three
    /// supervisor tasks. The returned [`EngineProcess`] is the public
    /// handle — the spawned tasks automatically close when stdin/stdout
    /// close (engine exit) or when [`Self::terminate`] is called.
    pub async fn spawn(binary: &Path) -> Result<Self, EngineError> {
        let mut cmd = Command::new(binary);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|source| EngineError::SpawnFailed {
            binary: Some(binary.to_path_buf()),
            source,
        })?;

        let stdin = child.stdin.take().ok_or(EngineError::Io {
            source: std::io::Error::new(std::io::ErrorKind::BrokenPipe, "missing stdin pipe"),
        })?;
        let stdout = child.stdout.take().ok_or(EngineError::Io {
            source: std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "missing stdout pipe",
            ),
        })?;

        let (cmd_tx, stdin_rx) = mpsc::channel::<EngineCommand>(COMMAND_BUFFER);
        let (kill_tx, mut kill_rx) = mpsc::channel::<()>(1);
        let (events, _) = broadcast::channel::<ParsedUciLine>(BROADCAST_BUFFER);

        let is_alive = Arc::new(Mutex::new(true));

        // ── Writer task ──
        let writer_is_alive = Arc::clone(&is_alive);
        tokio::spawn(async move {
            run_writer(stdin, stdin_rx, writer_is_alive).await;
        });

        // ── Reader task ──
        let events_for_reader = events.clone();
        let reader_is_alive = Arc::clone(&is_alive);
        tokio::spawn(async move {
            run_reader(stdout, events_for_reader, reader_is_alive).await;
        });

        // ── Supervisor task ──
        let sup_is_alive = Arc::clone(&is_alive);
        tokio::spawn(async move {
            run_supervisor(child, &mut kill_rx, sup_is_alive).await;
        });

        Ok(Self {
            cmd_tx,
            kill_tx,
            events,
            is_alive,
            binary: binary.to_path_buf(),
        })
    }

    /// Send one command. Awaits a slot in the bounded channel. Returns
    /// `EngineError::ChannelClosed` if the writer task has shut down.
    pub async fn send(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        self.cmd_tx
            .send(cmd)
            .await
            .map_err(|_| EngineError::ChannelClosed)
    }

    /// Subscribe a consumer to the broadcast event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<ParsedUciLine> {
        self.events.subscribe()
    }

    /// Graceful termination: send `quit` over the command channel, then
    /// after a small grace window escalate to a hard `kill` if the engine
    /// is still alive.
    pub async fn terminate(&self) -> Result<(), EngineError> {
        // Best-effort polite quit — if the engine is dead, ignore.
        let _ = self.cmd_tx.send(EngineCommand::Quit).await;
        // Schedule an escalation to hard kill (500 ms timeout matches what
        // the Rust-Chess-GUI reference uses for the same transition).
        let kill_tx = self.kill_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let _ = kill_tx.send(()).await;
        });
        Ok(())
    }

    /// Immediate hard kill — no polite quit. Used by the Phase-5 game-loop
    /// arbiter when the engine becomes unresponsive.
    pub fn kill(&self) -> Result<(), EngineError> {
        self.kill_tx
            .try_send(())
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(())
    }

    /// Poll-driven non-blocking view of `is_alive`. Cheap because we use
    /// `try_lock` on a `Mutex<bool>` — the writer/reader/supervisor tasks
    /// set it once on exit and never touch it again.
    pub fn is_alive(&self) -> bool {
        self.is_alive
            .try_lock()
            .map(|guard| *guard)
            .unwrap_or(true)
    }

    /// Path of the spawned binary, kept for logging / settings dialog.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Task bodies
// ─────────────────────────────────────────────────────────────────────────────

/// Writer task: pulls commands off `cmd_rx` and writes each one as a
/// single line ("{cmd}\n") into stdin via a `BufWriter`.
async fn run_writer(
    stdin: tokio::process::ChildStdin,
    mut cmd_rx: mpsc::Receiver<EngineCommand>,
    is_alive: Arc<Mutex<bool>>,
) {
    let mut writer = BufWriter::new(stdin);
    while let Some(cmd) = cmd_rx.recv().await {
        let line = cmd.to_wire_string();
        if writer.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if !line.ends_with('\n') {
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
        if writer.flush().await.is_err() {
            break;
        }
    }
    // Shutdown: flush any buffered data and let the pipe close.
    let _ = writer.shutdown().await;
    // Mark dead so the UI sees the engine as offline.
    *is_alive.lock().await = false;
}

/// Reader task: pulls lines from stdout via `BufReader::read_line` and
/// publishes each parsed event to the broadcast channel.
///
/// While the engine is mid-handshake (sending `id …`, `option …`,
/// `uciok` lines), we accumulate locally and only emit **one**
/// consolidated [`EngineEvent::UciOk`] at `uciok`. Other event types
/// (`info`, `bestmove`, etc.) are forwarded immediately.
///
/// Lags on the broadcast are silent — the engine anyway emits its
/// bestmove on the same channel so subscribers can detect a missed
/// line and react.
async fn run_reader(
    stdout: tokio::process::ChildStdout,
    events: broadcast::Sender<ParsedUciLine>,
    is_alive: Arc<Mutex<bool>>,
) {
    let mut reader = BufReader::new(stdout);
    let mut line_buf = String::new();
    let mut acc = HandshakeAccumulator::new();
    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf).await {
            Ok(0) => break, // EOF — engine closed stdout.
            Ok(_) => {
                let trimmed = line_buf.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    continue;
                }
                log::trace!("engine → {}", trimmed);
                let event = parse_line(trimmed);
                let message = match event {
                    Ok(ev) => acc
                        .fold(ev)
                        .map(ParsedUciLine::Event)
                        .unwrap_or_else(|| {
                            // Mid-handshake line — emit silently at trace
                            // level only; the consolidated UciOk will
                            // arrive shortly.
                            log::trace!("engine handshake: {}", trimmed);
                            ParsedUciLine::Unknown(trimmed.to_string())
                        }),
                    Err(UciParseError::UnknownLine { line }) => {
                        log::debug!("engine noise: {}", line);
                        ParsedUciLine::Unknown(line)
                    }
                    Err(source) => {
                        log::warn!("engine parse error `{}`: {}", trimmed, source);
                        ParsedUciLine::ParseError {
                            line: trimmed.to_string(),
                            source,
                        }
                    }
                };
                // Ignore the SendError when no listeners exist (tests
                // that don't subscribe).
                let _ = events.send(message);
            }
            Err(source) => {
                log::warn!("engine stdout read failed: {}", source);
                break;
            }
        }
    }
    *is_alive.lock().await = false;
}

/// Supervisor task: waits on either `kill_rx` (UI-driven termination) or
/// `child.wait()` (engine exited on its own). The first one to fire wins;
/// the supervisor then ensures the child is dead before returning.
async fn run_supervisor(
    mut child: tokio::process::Child,
    kill_rx: &mut mpsc::Receiver<()>,
    is_alive: Arc<Mutex<bool>>,
) {
    tokio::select! {
        _ = kill_rx.recv() => {
            let _ = child.kill().await;
        }
        _ = child.wait() => {}
    }
    *is_alive.lock().await = false;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — real-world Stockfish
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Returns Some(path) if a test engine binary exists on disk.
    /// `SILICONBOARD_TEST_ENGINE=/path/to/stockfish` overrides the default.
    fn test_engine_path() -> Option<PathBuf> {
        if let Ok(custom) = std::env::var("SILICONBOARD_TEST_ENGINE") {
            let p = PathBuf::from(custom);
            if p.exists() {
                return Some(p);
            }
        }
        let default = PathBuf::from("/opt/homebrew/bin/stockfish");
        if default.exists() {
            Some(default)
        } else {
            None
        }
    }

    /// Wait up to `timeout` for the predicate to become true, draining
    /// broadcasts meanwhile so the broadcast buffer does not fill up.
    async fn wait_until<F>(mut rx: broadcast::Receiver<ParsedUciLine>, timeout: Duration, mut pred: F) -> Vec<ParsedUciLine>
    where
        F: FnMut(&ParsedUciLine) -> bool,
    {
        let start = Instant::now();
        let mut drained = Vec::new();
        while start.elapsed() < timeout {
            match tokio::time::timeout(Duration::from_millis(250), rx.recv()).await {
                Ok(Ok(line)) => {
                    let matched = pred(&line);
                    drained.push(line);
                    if matched {
                        return drained;
                    }
                }
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => break,
                Err(_) => continue, // tick
            }
        }
        drained
    }

    #[tokio::test]
    async fn stockfish_handshake_emits_uciok_with_options() {
        let Some(binary) = test_engine_path() else {
            eprintln!("skipping handshake test — no engine binary");
            return;
        };

        let engine = EngineProcess::spawn(&binary).await.unwrap();
        let mut rx = engine.subscribe();

        engine.send(EngineCommand::Initialize).await.unwrap();

        let drained = wait_until(rx, Duration::from_secs(5), |line| {
            matches!(
                line,
                ParsedUciLine::Event(EngineEvent::UciOk { name, options, .. })
                    if !name.is_empty() && !options.is_empty()
            )
        })
        .await;

        let found = drained.iter().find_map(|line| match line {
            ParsedUciLine::Event(EngineEvent::UciOk {
                name,
                options,
                author,
            }) if !name.is_empty() => Some((name.clone(), options.clone(), author.clone())),
            _ => None,
        });
        let (name, options, _author) =
            found.expect("did not see a populated UciOk within 5s");
        // Stockfish 19's `id name …` line is "Stockfish 19"; older
        // releases are "Stockfish N.NN" or just "Stockfish". We assert
        // the prefix so the test survives version skews.
        assert!(
            name.starts_with("Stockfish"),
            "expected Stockfish family name, got `{}`",
            name
        );
        assert!(!options.is_empty(), "UciOk must aggregate options");

        let names: Vec<&str> = options.iter().map(|o| o.name.as_str()).collect();
        assert!(
            names.contains(&"Threads"),
            "missing `Threads` option in {:?}",
            names
        );
        assert!(
            names.contains(&"Hash"),
            "missing `Hash` option in {:?}",
            names
        );
        assert!(
            names.contains(&"Clear Hash"),
            "missing `Clear Hash` option in {:?}",
            names
        );

        // Spawn pattern of "Clear Hash" option must be Button.
        let clear_hash = options
            .iter()
            .find(|o| o.name == "Clear Hash")
            .expect("Clear Hash");
        assert_eq!(clear_hash.kind, UciOptionKind::Button);

        engine.terminate().await.unwrap();
        // Give the supervisor a beat to mark us dead.
        tokio::time::sleep(Duration::from_millis(600)).await;
    }

    #[tokio::test]
    async fn stockfish_depth_4_search_returns_bestmove() {
        let Some(binary) = test_engine_path() else {
            eprintln!("skipping go/bestmove test — no engine binary");
            return;
        };

        let engine = EngineProcess::spawn(&binary).await.unwrap();
        let mut rx = engine.subscribe();

        engine.send(EngineCommand::Initialize).await.unwrap();
        engine.send(EngineCommand::IsReady).await.unwrap();

        // Wait for ReadyOk so we know the handshake is fully past.
        let _ = wait_until(rx.resubscribe(), Duration::from_secs(5), |line| {
            matches!(line, ParsedUciLine::Event(EngineEvent::ReadyOk))
        })
        .await;

        engine.send(EngineCommand::NewGame).await.unwrap();
        engine
            .send(EngineCommand::SetPosition {
                fen: None,
                moves: Vec::new(),
            })
            .await
            .unwrap();
        engine
            .send(EngineCommand::Go(GoControl::Depth(4)))
            .await
            .unwrap();

        // Search for bestmove + at least one info line.
        let start = Instant::now();
        let timeout = Duration::from_secs(30);
        let mut saw_info = false;
        let mut bestmove: Option<String> = None;

        while start.elapsed() < timeout {
            match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Ok(line)) => match line {
                    ParsedUciLine::Event(EngineEvent::Info(_)) => saw_info = true,
                    ParsedUciLine::Event(EngineEvent::BestMove { best_move, .. }) => {
                        bestmove = Some(best_move);
                        break;
                    }
                    _ => {}
                },
                Ok(Err(_)) => break,
                Err(_) => continue,
            }
        }

        let bm = bestmove.expect("Stockfish did not emit bestmove within 30 s");
        assert!(saw_info, "Stockfish depth 4 emitted no info lines");
        assert!(
            ["e2e4", "d2d4", "c2c4", "g1f3", "b1c3"].contains(&bm.as_str()),
            "unexpected first-move reply: {}",
            bm
        );

        engine.terminate().await.unwrap();
    }
}
