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

pub mod analysis;
pub mod host;
pub mod protocol;
pub mod uci_parser;
pub mod watchdog;

pub use analysis::{AnalysisStatus, SearchGoal};
pub use host::{EngineHost, EngineView};
pub use protocol::{EngineCommand, EngineError, EngineEvent, Score};

use uci_parser::{parse_line, HandshakeAccumulator, UciParseError};

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};

// ─────────────────────────────────────────────────────────────────────────────
// EngineProcess
// ─────────────────────────────────────────────────────────────────────────────

/// Capacity for the broadcast channel of stdout lines.
/// Sized for high-volume engines (Stockfish can emit thousands of `info`
/// lines at depth 30+, and a missed bestmove would be catastrophic).
const BROADCAST_BUFFER: usize = 10_000;

/// Capacity for the command mpsc. Sized for a UI frame's worth of commands
/// (stop + position + go, for several engines) without dropping any.
const COMMAND_BUFFER: usize = 64;

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
    /// Broadcast channel for engine stdout lines (one line per message). The UI
    /// host uses the lossless `event_rx`; this remains for tests and observers.
    #[allow(dead_code, reason = "kept for tests and external observers; the UI host uses event_rx")]
    pub events: broadcast::Sender<ParsedUciLine>,
    /// Lossless stream for the UI host. The broadcast channel is kept for
    /// tests and other observers; the host drains this unbounded channel so a
    /// busy search can never lag `bestmove`/`readyok` away.
    event_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<ParsedUciLine>>>>,
    /// Path of the spawned binary, kept for logging and display.
    binary: PathBuf,
    /// Sticky flag set once the engine's output ends. Unlike a broadcast
    /// event it is still visible to a subscriber that attaches late.
    exited: Arc<AtomicBool>,
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

/// Called by the engine reader when it publishes a line, so an event-driven UI
/// can run a frame at once instead of polling a timer. The flag is `true` for
/// lines that must be seen immediately (handshake answer, `bestmove`, exit)
/// and `false` for `info` lines, which may be coalesced. Runs on the reader
/// task and must be cheap and non-blocking.
pub type EngineWake = Arc<dyn Fn(bool) + Send + Sync>;

impl EngineProcess {
    /// Spawn without a wake callback, for tests and for callers that drain
    /// [`Self::subscribe`] themselves. The app uses [`Self::spawn_waking`].
    #[allow(dead_code, reason = "the app always passes a wake callback; tests spawn without one")]
    pub async fn spawn(binary: &Path) -> Result<Self, EngineError> {
        Self::spawn_inner(binary, None).await
    }

    /// Spawn the engine and call `wake` whenever output arrives, so the window
    /// is redrawn as soon as there is something new to show.
    pub async fn spawn_waking(binary: &Path, wake: EngineWake) -> Result<Self, EngineError> {
        Self::spawn_inner(binary, Some(wake)).await
    }

    /// Shared body of [`Self::spawn`] and [`Self::spawn_waking`]. Starts the
    /// three supervisor tasks; they close when stdin/stdout close (engine
    /// exit) or when [`Self::terminate`] is called.
    async fn spawn_inner(binary: &Path, wake: Option<EngineWake>) -> Result<Self, EngineError> {
        check_executable(binary)?;
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
        let (line_tx, line_rx) = mpsc::unbounded_channel::<ParsedUciLine>();
        let exited = Arc::new(AtomicBool::new(false));

        // ── Writer task ──
        tokio::spawn(async move {
            run_writer(stdin, stdin_rx).await;
        });

        // ── Reader task ──
        let events_for_reader = events.clone();
        let reader_exited = Arc::clone(&exited);
        tokio::spawn(async move {
            run_reader(stdout, events_for_reader, line_tx, reader_exited, wake).await;
        });

        // ── Supervisor task ──
        let supervisor_exited = Arc::clone(&exited);
        tokio::spawn(async move {
            run_supervisor(child, &mut kill_rx, supervisor_exited).await;
        });

        Ok(Self {
            cmd_tx,
            kill_tx,
            events,
            event_rx: Arc::new(Mutex::new(Some(line_rx))),
            binary: binary.to_path_buf(),
            exited,
        })
    }

    /// Take the lossless event stream once, when the UI host attaches.
    pub fn take_event_receiver(&self) -> Option<mpsc::UnboundedReceiver<ParsedUciLine>> {
        self.event_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Non-blocking send for the UI thread. Fails immediately when the
    /// command queue is full or the engine has exited, so the caller never
    /// waits on an engine.
    pub fn try_send(&self, cmd: EngineCommand) -> Result<(), EngineError> {
        self.cmd_tx
            .try_send(cmd)
            .map_err(|_| EngineError::ChannelClosed)
    }

    /// Subscribe a consumer to the broadcast event stream.
    #[allow(dead_code, reason = "used by tests and external observers; the UI host uses the lossless channel")]
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

    /// `true` once the engine's output has ended (the process exited or was
    /// killed). Safe to poll at any time, including after the `Terminated`
    /// event was already sent.
    pub fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    /// Path of the spawned binary, kept for logging and display.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

/// Reject anything that is not a regular file with an execute bit set, with a
/// message that says why. Scripts and wrapper programs pass this check;
/// whether the file really speaks UCI is decided by the handshake.
fn check_executable(binary: &Path) -> Result<(), EngineError> {
    use std::os::unix::fs::PermissionsExt;

    let fail = |reason: &str| EngineError::SpawnFailed {
        binary: Some(binary.to_path_buf()),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, reason.to_string()),
    };
    let metadata = std::fs::metadata(binary).map_err(|source| EngineError::SpawnFailed {
        binary: Some(binary.to_path_buf()),
        source,
    })?;
    if !metadata.is_file() {
        return Err(fail("not a regular file"));
    }
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(fail("file is not marked executable"));
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Task bodies
// ─────────────────────────────────────────────────────────────────────────────

/// Writer task: pulls commands off `cmd_rx` and writes each one as a
/// single line ("{cmd}\n") into stdin via a `BufWriter`.
async fn run_writer(stdin: tokio::process::ChildStdin, mut cmd_rx: mpsc::Receiver<EngineCommand>) {
    let mut writer = BufWriter::new(stdin);
    while let Some(cmd) = cmd_rx.recv().await {
        let line = cmd.to_wire_string();
        if writer.write_all(line.as_bytes()).await.is_err() {
            break;
        }
        if !line.ends_with('\n') && writer.write_all(b"\n").await.is_err() {
            break;
        }
        if writer.flush().await.is_err() {
            break;
        }
    }
    // Shutdown: flush any buffered data and let the pipe close.
    let _ = writer.shutdown().await;
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
    lines: mpsc::UnboundedSender<ParsedUciLine>,
    exited: Arc<AtomicBool>,
    wake: Option<EngineWake>,
) {
    let mut reader = BufReader::new(stdout);
    let mut raw: Vec<u8> = Vec::new();
    let mut acc = HandshakeAccumulator::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw).await {
            Ok(0) => break, // EOF — engine closed stdout.
            Ok(_) => {
                // Engines occasionally print non-UTF-8 bytes; keep reading
                // rather than treating the whole engine as dead.
                let text = String::from_utf8_lossy(&raw);
                let trimmed = text.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    continue;
                }
                log::trace!("engine → {}", trimmed);
                let message = match parse_line(trimmed) {
                    Ok(ev) => acc.fold(ev).map(ParsedUciLine::Event).unwrap_or_else(|| {
                        // Mid-handshake line — the consolidated UciOk
                        // arrives at `uciok`.
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
                let important = !matches!(
                    &message,
                    ParsedUciLine::Event(EngineEvent::Info(_))
                        | ParsedUciLine::Unknown(_)
                        | ParsedUciLine::ParseError { .. }
                );
                let _ = lines.send(message.clone());
                let _ = events.send(message);
                if let Some(wake) = &wake {
                    wake(important);
                }
            }
            Err(source) => {
                log::warn!("engine stdout read failed: {}", source);
                break;
            }
        }
    }
    // Tell subscribers the engine is gone so the UI can show it as offline.
    exited.store(true, Ordering::Release);
    let terminated = ParsedUciLine::Event(EngineEvent::Terminated);
    let _ = lines.send(terminated.clone());
    let _ = events.send(terminated);
    if let Some(wake) = &wake {
        wake(true);
    }
}

/// Supervisor task: waits on either `kill_rx` (UI-driven termination) or
/// `child.wait()` (engine exited on its own). The first one to fire wins;
/// the supervisor then ensures the child is dead before returning.
async fn run_supervisor(
    mut child: tokio::process::Child,
    kill_rx: &mut mpsc::Receiver<()>,
    exited: Arc<AtomicBool>,
) {
    tokio::select! {
        _ = kill_rx.recv() => {
            let _ = child.kill().await;
        }
        _ = child.wait() => {}
    }
    exited.store(true, Ordering::Release);
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — real-world Stockfish
// ─────────────────────────────────────────────────────────────────────────────

/// The UCI engine used by the real-engine tests. Search order:
/// `SILICONBOARD_TEST_ENGINE`, Homebrew Stockfish, then the workspace
/// Triumviratus build. Panics when none exists: a silently skipped test
/// would pass without testing anything.
#[cfg(test)]
pub(crate) fn test_engine_path() -> PathBuf {
    let mut tried = Vec::new();
    if let Ok(custom) = std::env::var("SILICONBOARD_TEST_ENGINE") {
        let p = PathBuf::from(custom);
        if p.exists() {
            return p;
        }
        tried.push(p);
    }
    let stockfish = PathBuf::from("/opt/homebrew/bin/stockfish");
    if stockfish.exists() {
        return stockfish;
    }
    tried.push(stockfish);
    let triumviratus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("Triumviratus")
        .join("Triumviratus_8.0_macos_arm");
    if triumviratus.exists() {
        return triumviratus;
    }
    tried.push(triumviratus);
    panic!(
        "no test engine found; install Stockfish or set SILICONBOARD_TEST_ENGINE (tried {:?})",
        tried
    );
}

#[cfg(test)]
mod tests {
    use super::protocol::{GoControl, UciOptionKind};
    use super::*;
    use std::time::{Duration, Instant};

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

    #[test]
    fn executable_check_names_the_problem() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();

        let plain = dir.path().join("notes.md");
        std::fs::write(&plain, "not a program").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = check_executable(&plain).unwrap_err();
        assert!(err.to_string().contains("not marked executable"), "{}", err);

        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        let err = check_executable(&folder).unwrap_err();
        assert!(err.to_string().contains("not a regular file"), "{}", err);

        // A script with the execute bit passes; only the handshake can tell
        // whether it is a UCI engine.
        let script = dir.path().join("wrapper.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_executable(&script).is_ok());
    }

    #[tokio::test]
    async fn engine_exit_publishes_terminated_event() {
        use std::os::unix::fs::PermissionsExt;

        // A stand-in "engine" that stays alive briefly and then exits on its own.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("short-lived-engine.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 0.3\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let engine = EngineProcess::spawn(&script).await.unwrap();
        let rx = engine.subscribe();
        let drained = wait_until(rx, Duration::from_secs(5), |line| {
            matches!(line, ParsedUciLine::Event(EngineEvent::Terminated))
        })
        .await;
        assert!(
            drained
                .iter()
                .any(|line| matches!(line, ParsedUciLine::Event(EngineEvent::Terminated))),
            "engine exit must be announced to subscribers"
        );
        assert!(engine.has_exited(), "the sticky exit flag must be set too");
    }

    #[tokio::test]
    async fn engine_output_calls_the_wake_callback() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::AtomicUsize;

        // A stand-in engine that prints one info line and then stays alive.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("chatty-engine.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 0.1\necho 'info depth 1 score cp 0'\nsleep 1\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let engine = EngineProcess::spawn_waking(
            &script,
            Arc::new(move |_important: bool| {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .await
        .unwrap();
        let rx = engine.subscribe();

        let drained = wait_until(rx, Duration::from_secs(5), |line| {
            matches!(line, ParsedUciLine::Event(EngineEvent::Info(_)))
        })
        .await;
        assert!(
            drained.iter().any(|line| matches!(line, ParsedUciLine::Event(EngineEvent::Info(_)))),
            "the script engine's info line must arrive"
        );
        assert!(
            wakes.load(Ordering::Relaxed) > 0,
            "the reader must wake the UI when engine output arrives"
        );
    }

    #[tokio::test]
    async fn real_engine_handshake_emits_uciok_with_options() {
        let binary = test_engine_path();

        let engine = EngineProcess::spawn(&binary).await.unwrap();
        let rx = engine.subscribe();

        engine.try_send(EngineCommand::Initialize).unwrap();

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
        // The test engine may be Stockfish or any other UCI engine, so only the
        // shape of the handshake is asserted, not a family name.
        assert!(!name.is_empty(), "the handshake must name the engine");
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
        let binary = test_engine_path();

        let engine = EngineProcess::spawn(&binary).await.unwrap();
        let mut rx = engine.subscribe();

        engine.try_send(EngineCommand::Initialize).unwrap();
        engine.try_send(EngineCommand::IsReady).unwrap();

        // Wait for ReadyOk so we know the handshake is fully past.
        let _ = wait_until(rx.resubscribe(), Duration::from_secs(5), |line| {
            matches!(line, ParsedUciLine::Event(EngineEvent::ReadyOk))
        })
        .await;

        engine.try_send(EngineCommand::NewGame).unwrap();
        engine
            .try_send(EngineCommand::SetPosition {
                fen: None,
                moves: Vec::new(),
            })
            .unwrap();
        engine
            .try_send(EngineCommand::Go(GoControl::Depth(4)))
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
                        bestmove = best_move;
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
