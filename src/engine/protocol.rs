//! Strongly-typed UCI commands and events.
//!
//! Shapes are documented here; full implementation lands in Phase 3 of the
//! `implementation-plan.md`.

/// All commands the UI thread can send toward an engine subprocess.
#[derive(Debug, Clone)]
pub enum EngineCommand {
    /// `uci`
    Initialize,
    /// `isready`
    IsReady,
    /// `ucinewgame`
    NewGame,
    /// `setoption name <n> value <v>`
    SetOption { name: String, value: String },
    /// `position fen <f> moves <m1> <m2> ...`
    SetPosition {
        fen: Option<String>,
        moves: Vec<String>,
    },
    /// `go ...`
    Go,
    /// `stop`
    Stop,
    /// `quit`
    Quit,
}

/// All messages the engine subprocess can send back to the UI thread.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    /// `id name <n>` / `id author <a>` / `uciok`
    UciOk {
        name: String,
        author: String,
    },
    /// `readyok`
    ReadyOk,
    /// Raw `info ...` line from the engine. Phase 3 turns this into a
    /// structured `EngineStats` payload; today we just store the source.
    Info(String),
    /// `bestmove <m> (ponder <p>)`
    BestMove(String),
    /// Engine process exited cleanly (or was killed by the supervisor).
    Terminated,
}
