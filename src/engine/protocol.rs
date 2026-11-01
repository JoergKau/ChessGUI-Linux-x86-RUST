//! Strongly-typed UCI protocol messages.
//!
//! Every command the UI thread sends to a UCI engine and every event the
//! engine emits back is captured by one of the data structures defined
//! here. The wire format itself is plain ASCII — single-token /
//! multi-token lines terminated by `\n` — but we never store the raw
//! strings downstream; once [`crate::engine::uci_parser`] has reduced a
//! line to a [`EngineEvent`], the rest of the crate only deals with these
//! typed values.
//!
//! ## Wire-format reference
//!
//! Inputs (UI → engine):
//!
//! ```text
//! uci
//! isready
//! ucinewgame
//! setoption name <key> value <val>
//! position [fen <fen> | startpos] moves <u1> <u2> ...
//! go [wtime N] [btime N] [winc N] [binc N] [movetime N] [depth N] [nodes N] [mate N] [infinite]
//! stop
//! quit
//! ```
//!
//! Outputs (engine → UI):
//!
//! ```text
//! id name <string>
//! id author <string>
//! option name <key> type <check|spin|combo|button|string> [default <v>] [min <n>] [max <n>] [var <v> ...]
//! uciok
//! readyok
//! info depth <n> [seldepth <n>] [time <ms>] [nodes <n>] [nps <n>]
//!      [hashfull <per-mille>] [tbhits <n>] [score cp <n> | score mate <n>]
//!      [currmove <uci>] [currmovenumber <n>] [multipv <n>] [pv <uci> <uci> ...]
//! bestmove <uci> [ponder <uci>]
//! ```
//!
//! All numeric values are non-negative integers; the lone signed integer is
//! the score (centipawns — positive favours the side to move).

use serde::{Deserialize, Serialize};

// ─────────────────────────────────────────────────────────────────────────────
// Commands (UI -> engine)
// ─────────────────────────────────────────────────────────────────────────────

/// One logical UCI command. Sent over a `tokio::sync::mpsc::Sender`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCommand {
    /// Handshake start. Engine replies with `id …` lines, then `option …`
    /// lines, then `uciok`.
    Initialize,
    /// Asks "are you ready to receive commands now?". Engine replies with
    /// `readyok`. Sent after `Initialize` whenever the UI wants a rendezvous.
    IsReady,
    /// `ucinewgame` — drops any cached state (hash tables, repetition
    /// counts) so the engine starts fresh.
    NewGame,
    /// `setoption name <n> value <v>` (or `value <v>` may be elided for
    /// `button` options).
    SetOption {
        name: String,
        value: String,
    },
    /// `position [startpos | fen <FEN>] moves <u1> <u2> …`.
    SetPosition {
        /// `None` ⇒ `startpos`. Some engines prefer the explicit
        /// `startpos` keyword for clarity; we always substitute into the
        /// FEN when the user starts from the standard position, so the
        /// engine gets identical state either way.
        fen: Option<String>,
        /// Move list in UCI square-name notation (`e2e4`, `e7e8q` for
        /// promotions). Empty if no moves are to be played from the start.
        moves: Vec<String>,
    },
    /// `go …` with the chosen sub-control set.
    Go(GoControl),
    /// `stop` — pause the current search; engine replies with a fresh
    /// `bestmove …` for the deepest finished line.
    Stop,
    /// `quit` — clean exit. The process exits on its own after this.
    Quit,
}

impl EngineCommand {
    /// Serialise back into the wire form. Used both for the production
    /// writer task and for snapshot test fixtures.
    pub fn to_wire_string(&self) -> String {
        match self {
            EngineCommand::Initialize => "uci".into(),
            EngineCommand::IsReady => "isready".into(),
            EngineCommand::NewGame => "ucinewgame".into(),
            EngineCommand::SetOption { name, value } => {
                // Per UCI spec, button options omit the value half.
                format!("setoption name {} value {}", name, value)
            }
            EngineCommand::SetPosition { fen, moves } => {
                let prefix = match fen {
                    Some(f) => format!("position fen {}", f),
                    None => "position startpos".to_string(),
                };
                if moves.is_empty() {
                    prefix
                } else {
                    format!("{} moves {}", prefix, moves.join(" "))
                }
            }
            EngineCommand::Go(ctrl) => ctrl.to_wire_string(),
            EngineCommand::Stop => "stop".into(),
            EngineCommand::Quit => "quit".into(),
        }
    }
}

/// Sub-control for the `go` command.
///
/// Mirrors the UCI spec for `go <X> <Y> …` — fields may be combined, with
/// `movetime` overriding any clock arguments if both are supplied. We
/// surface that choice to the caller via an enum so the UI code reflects
/// intent instead of wrangling a raw flag set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoControl {
    /// `go infinite` — search until told to stop.
    Infinite,
    /// `go depth N` — search to exactly `N` plies.
    Depth(u32),
    /// `go nodes N` — search up to `N` node visits.
    Nodes(u64),
    /// `go mate N` — search until a forced mate in `N`.
    Mate(u32),
    /// `go movetime N` — search for exactly `N` milliseconds.
    MoveTime(u64),
    /// `go wtime X btime Y winc i binc j [movetime T]`. Real-game clock
    /// mode — the engine decides how deep to go based on the remaining
    /// time and increment.
    Clock {
        wtime: u64,
        btime: u64,
        winc: u64,
        binc: u64,
        movetime: Option<u64>,
    },
}

impl Default for GoControl {
    /// Default: 5-minute Fischer with a 3-second increment (matches
    /// [`crate::chess::clock::TimeControl::default`]).
    fn default() -> Self {
        GoControl::Clock {
            wtime: 300_000,
            btime: 300_000,
            winc: 3_000,
            binc: 3_000,
            movetime: None,
        }
    }
}

impl GoControl {
    /// Render the sub-control as the `go …` suffix.
    pub fn to_wire_string(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        let arg = |val: &dyn std::fmt::Display, name: &str| -> String {
            format!("{} {}", name, val)
        };
        match self {
            GoControl::Infinite => parts.push("infinite".into()),
            GoControl::Depth(n) => parts.push(arg(&n, "depth")),
            GoControl::Nodes(n) => parts.push(arg(&n, "nodes")),
            GoControl::Mate(n) => parts.push(arg(&n, "mate")),
            GoControl::MoveTime(n) => parts.push(arg(&n, "movetime")),
            GoControl::Clock {
                wtime,
                btime,
                winc,
                binc,
                movetime,
            } => {
                parts.push(arg(wtime, "wtime"));
                parts.push(arg(btime, "btime"));
                parts.push(arg(winc, "winc"));
                parts.push(arg(binc, "binc"));
                if let Some(t) = movetime {
                    parts.push(arg(t, "movetime"));
                }
            }
        }
        format!("go {}", parts.join(" "))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Events (engine -> UI)
// ─────────────────────────────────────────────────────────────────────────────

/// One event delivered by an engine subprocess, reduced from a single
/// line of the engine's stdout.
///
/// The variant names mirror the engine's wire forms; we keep the names
/// lowercase to fit with the rest of the codebase (`UciOk` not
/// `UciOK`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineEvent {
    /// Engine's response to `uci` — engine name, author, and the
    /// exhaustive option list. The UI typically bothers to log only the
    /// engine name; the options are kept for the Phase 5 options dialog.
    UciOk {
        name: String,
        author: String,
        options: Vec<UciOption>,
    },
    /// Engine's response to `isready`.
    ReadyOk,
    /// `info …` line, fully reduced.
    Info(EngineStats),
    /// `bestmove <m> [ponder <m>]`.
    BestMove {
        best_move: String,
        ponder: Option<String>,
    },
    /// Engine process exited cleanly (or our supervisor killed it).
    Terminated,
}

// ─────────────────────────────────────────────────────────────────────────────
// EngineStats (parsed `info …` payload)
// ─────────────────────────────────────────────────────────────────────────────

/// Structured representation of one `info` line from the engine.
///
/// All numeric fields are `Option` because UCI `info` lines are allowed to
/// carry arbitrary subsets of the available keys — `info depth 24` is a
/// legal line on its own and we must not invent the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub depth: Option<u32>,
    pub seldepth: Option<u32>,
    pub time_ms: Option<u64>,
    pub nodes: Option<u64>,
    pub nps: Option<u64>,
    pub hashfull_per_mille: Option<u32>,
    pub tbhits: Option<u32>,
    pub multipv: Option<u32>,
    pub currmove: Option<String>,
    pub currmovenumber: Option<u32>,
    pub string: Option<String>,
    pub refutation: Option<Vec<String>>,
    pub currline: Option<Vec<u32>>,
    pub score: Option<Score>,
    pub pv: Vec<String>,
}

/// Centipawn- or mate-score reported by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Score {
    /// `score cp <n>` — centipawns from the side-to-move perspective.
    /// Positive ⇒ the side to move is better.
    Cp(i32),
    /// `score mate <n>` — forced mate in `n` plies, again relative to
    /// the side to move.
    Mate(i32),
}

impl Score {
    /// Choose the centipawn-equivalent for an evaluation bar (mate in N
    /// is treated as `±MATE_CP`).
    pub fn centipawn_equivalent(&self) -> i32 {
        const MATE_CP: i32 = 30_000;
        match self {
            Score::Cp(cp) => *cp,
            Score::Mate(n) => MATE_CP * n.signum(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// UCI options (parsed `option …` line)
// ─────────────────────────────────────────────────────────────────────────────

/// One declared UCI option.
///
/// We do not require engines to provide every field — for example
/// `option name Clear Hash type button` carries no default. So defaults
/// are stored as `Option<String>` on the parent and reflections of the
/// typed shape live in [`UciOptionKind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UciOption {
    pub name: String,
    pub kind: UciOptionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UciOptionKind {
    /// `type check` — boolean toggle.
    Check { default: bool },
    /// `type spin` — integer with bounds.
    Spin {
        default: i64,
        min: i64,
        max: i64,
    },
    /// `type combo` — one of a list of strings (and optionally a default).
    Combo {
        default: String,
        values: Vec<String>,
    },
    /// `type button` — press-like action; sending `setoption name <n>`
    /// without `value …` triggers it.
    Button,
    /// `type string` — arbitrary text.
    String { default: String },
}

impl UciOptionKind {
    /// Default value as a string, if applicable. Used by the Phase-5
    /// settings UI to render a "Reset to default" button without dragging
    /// the full [`UciOptionKind`] into a UI widget.
    pub fn default_as_string(&self) -> Option<String> {
        match self {
            UciOptionKind::Check { default } => Some(default.to_string()),
            UciOptionKind::Spin { default, .. } => Some(default.to_string()),
            UciOptionKind::Combo { default, .. } => Some(default.clone()),
            UciOptionKind::Button => None,
            UciOptionKind::String { default } => Some(default.clone()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Errors raised by the protocol layer (currently only the process
/// manager constructs these — the parser emits [`crate::engine::uci_parser::UciParseError`]).
#[derive(Debug)]
pub enum EngineError {
    /// The process failed to spawn (binary missing, permissions, etc.).
    SpawnFailed {
        binary: Option<std::path::PathBuf>,
        source: std::io::Error,
    },
    /// Tried to send a command after the writer task had exited.
    ChannelClosed,
    /// Watchdog aborted the search before the engine replied.
    WatchdogTimeout {
        waited_for: std::time::Duration,
    },
    /// Generic I/O error on the subprocess pipe.
    Io {
        source: std::io::Error,
    },
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::SpawnFailed { binary, source } => {
                write!(
                    f,
                    "could not spawn engine {}: {}",
                    binary.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "?".into()),
                    source
                )
            }
            EngineError::ChannelClosed => f.write_str("engine command channel closed"),
            EngineError::WatchdogTimeout { waited_for } => write!(
                f,
                "engine missed a watchdog deadline (waited {:?})",
                waited_for
            ),
            EngineError::Io { source } => write!(f, "engine I/O error: {}", source),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::SpawnFailed { source, .. } | EngineError::Io { source } => Some(source),
            EngineError::ChannelClosed | EngineError::WatchdogTimeout { .. } => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_command_wire_round_trip_examples() {
        assert_eq!(EngineCommand::Initialize.to_wire_string(), "uci");
        assert_eq!(EngineCommand::IsReady.to_wire_string(), "isready");
        assert_eq!(EngineCommand::NewGame.to_wire_string(), "ucinewgame");
        assert_eq!(
            EngineCommand::SetOption {
                name: "Hash".into(),
                value: "256".into(),
            }
            .to_wire_string(),
            "setoption name Hash value 256"
        );
        assert_eq!(
            EngineCommand::SetPosition {
                fen: None,
                moves: vec!["e2e4".into(), "e7e5".into()],
            }
            .to_wire_string(),
            "position startpos moves e2e4 e7e5"
        );
        assert_eq!(
            EngineCommand::SetPosition {
                fen: Some("rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1".into()),
                moves: vec!["g1f3".into()],
            }
            .to_wire_string(),
            "position fen rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq - 0 1 moves g1f3"
        );
        assert_eq!(EngineCommand::Stop.to_wire_string(), "stop");
        assert_eq!(EngineCommand::Quit.to_wire_string(), "quit");
    }

    #[test]
    fn go_control_wire_round_trip_examples() {
        assert_eq!(GoControl::Infinite.to_wire_string(), "go infinite");
        assert_eq!(GoControl::Depth(20).to_wire_string(), "go depth 20");
        assert_eq!(GoControl::Nodes(1_000_000).to_wire_string(), "go nodes 1000000");
        assert_eq!(GoControl::MoveTime(5_000).to_wire_string(), "go movetime 5000");
        assert_eq!(
            GoControl::Clock {
                wtime: 300_000,
                btime: 300_000,
                winc: 3_000,
                binc: 3_000,
                movetime: None,
            }
            .to_wire_string(),
            "go wtime 300000 btime 300000 winc 3000 binc 3000"
        );
        assert_eq!(
            GoControl::Clock {
                wtime: 60_000,
                btime: 60_000,
                winc: 0,
                binc: 0,
                movetime: Some(1_000),
            }
            .to_wire_string(),
            "go wtime 60000 btime 60000 winc 0 binc 0 movetime 1000"
        );
    }

    #[test]
    fn score_centipawn_equivalent() {
        assert_eq!(Score::Cp(45).centipawn_equivalent(), 45);
        assert_eq!(Score::Cp(-45).centipawn_equivalent(), -45);
        assert_eq!(Score::Mate(3).centipawn_equivalent(), 30_000);
        assert_eq!(Score::Mate(-3).centipawn_equivalent(), -30_000);
    }

    #[test]
    fn uci_option_default_as_string() {
        assert_eq!(
            UciOptionKind::Check { default: false }.default_as_string(),
            Some("false".into())
        );
        assert_eq!(
            UciOptionKind::Spin {
                default: 16,
                min: 1,
                max: 1024
            }
            .default_as_string(),
            Some("16".into())
        );
        assert_eq!(
            UciOptionKind::String {
                default: "nn-1a298aa575a0.nnue".into()
            }
            .default_as_string(),
            Some("nn-1a298aa575a0.nnue".into())
        );
        assert_eq!(UciOptionKind::Button.default_as_string(), None);
    }
}
