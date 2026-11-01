//! Token-by-token parser for UCI protocol lines.
//!
//! Implementation lands in **Phase 3** of `implementation-plan.md`. The
//! placeholder [`parse_line`] function returns `None` for every input so
//! the rest of the codebase can be wired without committing to a parser
//! shape yet.

/// Stub parser — full implementation arrives in Phase 3.
pub fn parse_line(_line: &str) -> Option<crate::engine::protocol::EngineEvent> {
    None
}
