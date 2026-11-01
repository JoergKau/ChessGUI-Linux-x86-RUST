//! UCI engine subsystem skeleton.
//!
//! Phase 1 only exists to confirm the module layout compiles. The async
//! protocol parser, command / event types, and process supervisor arrive
//! in Phase 3 of the implementation plan.
//!
//! Module hierarchy:
//!
//! ```text
//! src/engine/
//! ├── mod.rs        ← manager + public re-exports (Phase 3)
//! ├── protocol.rs   ← EngineCommand / EngineEvent data shapes
//! └── uci_parser.rs ← line-by-line UCI parser (Phase 3)
//! ```

pub mod protocol;
pub mod uci_parser;

pub use protocol::{EngineCommand, EngineEvent};
