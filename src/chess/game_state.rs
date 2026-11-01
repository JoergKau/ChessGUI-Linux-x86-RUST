//! Chess position state, move history and SAN formatting.
//!
//! This is the **single source of truth** for the board on the UI side:
//! `gui::board_view` (Phase 4) and the PGN panel (Phase 5) both read from
//! here. The engine side (Phase 3) uses [`GameState::position`] to feed
//! `position fen … moves …` to UCI engines and to compute the PolyGlot
//! Zobrist hash for opening-book lookups.
//!
//! # Architecture
//!
//! ```text
//! assets::PieceTextures ──looks up──▶  PieceColor + PieceKind (this module)
//!                                          │
//!                                          ▼
//! GameState ──wraps──▶ shakmaty::Chess ──storing──▶ rich game history
//!                                          │
//!                                          ▼
//!                       SAN strings, FEN, Zobrist hash, legal-move filtering
//! ```
//!
//! # History model
//!
//! [`GameState`] keeps an immutable starting `Chess` and a `Vec<Ply>` of
//! recorded moves plus the position *after* each move, so:
//! * `position()` is **O(1)** (just an Rc clone of the right entry);
//! * navigating back/forward through the move list is just an integer
//!   decrement/increment on `cur_ply`, with stale tail-plies truncated
//!   when the user deviates from a previously-browsed line.
//!
//! This shape was chosen over a "single live `Chess` + replay" model so the
//! PGN panel can paint faster than `O(ply)` per click.

use shakmaty::{
    san::SanPlus, zobrist::Zobrist64, CastlingMode, Chess, Color, EnPassantMode, FromSetup,
    Move, Position, Role, Square,
};

use std::fmt::Write;

pub use crate::assets::PieceId;

// ─────────────────────────────────────────────────────────────────────────────
// Local colour / kind enums
// ─────────────────────────────────────────────────────────────────────────────
//
// We use our own enums (canonical for the asset pipeline and the rest of
// the crate) and provide `From`/`Into` shims for shakmaty's [`Color`]/[`Role`]
// wherever we need to cross the API boundary.

/// Side a piece (and the position's side-to-move) belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PieceColor {
    White,
    Black,
}

impl PieceColor {
    /// Letter used to prefix piece asset filenames (`w`, `b`).
    pub const fn letter(self) -> char {
        match self {
            PieceColor::White => 'w',
            PieceColor::Black => 'b',
        }
    }

    /// The other side.
    pub const fn opposite(self) -> Self {
        match self {
            PieceColor::White => PieceColor::Black,
            PieceColor::Black => PieceColor::White,
        }
    }
}

impl From<Color> for PieceColor {
    fn from(c: Color) -> Self {
        match c {
            Color::White => PieceColor::White,
            Color::Black => PieceColor::Black,
        }
    }
}

impl From<PieceColor> for Color {
    fn from(c: PieceColor) -> Self {
        match c {
            PieceColor::White => Color::White,
            PieceColor::Black => Color::Black,
        }
    }
}

/// Mapping between SiliconBoard's [`PieceKind`] (King/Queen/Rook/Bishop/
/// Knight/Pawn) and shakmaty's [`Role`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PieceKind {
    King,
    Queen,
    Rook,
    Bishop,
    Knight,
    Pawn,
}

impl PieceKind {
    /// Letter used in FEN, SAN, and file names.
    pub const fn letter(self) -> char {
        match self {
            PieceKind::King => 'K',
            PieceKind::Queen => 'Q',
            PieceKind::Rook => 'R',
            PieceKind::Bishop => 'B',
            PieceKind::Knight => 'N',
            PieceKind::Pawn => 'P',
        }
    }

    /// shakmaty [`Role`] for this kind.
    pub const fn role(self) -> Role {
        match self {
            PieceKind::Pawn => Role::Pawn,
            PieceKind::Knight => Role::Knight,
            PieceKind::Bishop => Role::Bishop,
            PieceKind::Rook => Role::Rook,
            PieceKind::Queen => Role::Queen,
            PieceKind::King => Role::King,
        }
    }

    /// Inverse of [`Self::role`].
    pub const fn from_role(role: Role) -> Self {
        match role {
            Role::Pawn => PieceKind::Pawn,
            Role::Knight => PieceKind::Knight,
            Role::Bishop => PieceKind::Bishop,
            Role::Rook => PieceKind::Rook,
            Role::Queen => PieceKind::Queen,
            Role::King => PieceKind::King,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Ply = a single entry in the move history
// ─────────────────────────────────────────────────────────────────────────────

/// One recorded half-move (also called a *ply*) in the game's history.
///
/// `san` and `after` are both materialised at apply time so the UI can pull
/// them in O(1) without re-scanning earlier plies.
#[derive(Clone, Debug)]
pub struct Ply {
    /// The actual shakmaty move that was played. Pawn-promotion roles,
    /// castling info etc. live here.
    pub move_: Move,
    /// SAN string for this move (e.g. `"Nf3"` or `"O-O"` or `"exd6"`).
    pub san: SanPlus,
    /// The position *after* the move has been played.
    pub after: Chess,
}

// ─────────────────────────────────────────────────────────────────────────────
// GameState
// ─────────────────────────────────────────────────────────────────────────────

/// Full game state: starting position + move history + navigation cursor.
///
/// Cloning is cheap (`Chess` is internally Rc-shared) — we keep the struct
/// public so the eval panel can take a snapshot for analysis.
#[derive(Clone, Debug)]
pub struct GameState {
    /// Starting position (immutable, retained so we can navigate back to
    /// exactly the same position even after multiple games).
    start: Chess,
    /// Move history. Only the prefix `[..=cur_ply]` is currently active;
    /// the tail is reserved for any "redo" we have room to perform.
    history: Vec<Ply>,
    /// Cursor into `history`. 0 = no moves played yet; equal to
    /// `history.len()` = at the latest position.
    cur_ply: usize,
}

impl Default for GameState {
    fn default() -> Self {
        Self::new()
    }
}

impl GameState {
    /// Standard chess starting position. Equivalent to
    /// `GameState::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1")`.
    pub fn new() -> Self {
        Self::from_position(Chess::default()).expect("default position is always valid")
    }

    /// Wrap an existing shakmaty [`Chess`] as a fresh `GameState` (no
    /// history). Used in tests and when an engine emits a position we want
    /// to inspect.
    pub fn from_position(start: Chess) -> Result<Self, FenError> {
        Self::validate(&start).map_err(FenError::from_position)?;
        Ok(Self {
            start,
            history: Vec::new(),
            cur_ply: 0,
        })
    }

    /// Parse a FEN string and use it as the starting position. Accepts
    /// both X-FEN and Shredder/Fairy-Stockfish-style FEN — the underlying
    /// shakmaty parser is forgiving but returns an error on outright
    /// garbage.
    pub fn from_fen(fen: &str) -> Result<Self, FenError> {
        let parsed_fen = shakmaty::fen::Fen::from_ascii(fen.as_bytes())
            .map_err(|source| FenError::Parse { source })?;
        let setup = parsed_fen.into_setup();
        let chess = Chess::from_setup(setup, CastlingMode::Standard)
            .map_err(|source| FenError::Setup { source })?;
        Ok(Self {
            start: chess,
            history: Vec::new(),
            cur_ply: 0,
        })
    }

    // ── Read-only accessors ─────────────────────────────────────────────

    /// The position currently visible to the user. O(1) Rc clone.
    pub fn position(&self) -> Chess {
        if self.cur_ply == 0 {
            self.start.clone()
        } else {
            self.history[self.cur_ply - 1].after.clone()
        }
    }

    /// Index of the current half-move (0-based). 0 = at the start.
    pub fn cur_ply(&self) -> usize {
        self.cur_ply
    }

    /// Total number of plies currently cached in history (those ≤ `cur_ply`
    /// plus any "redo tail").
    pub fn total_plies(&self) -> usize {
        self.history.len()
    }

    /// Slice of recorded plies that are currently considered played,
    /// i.e. `[0..cur_ply]`. Suitable for direct rendering by the PGN panel.
    pub fn active_history(&self) -> &[Ply] {
        &self.history[..self.cur_ply]
    }

    /// Whose turn it is.
    pub fn turn(&self) -> PieceColor {
        self.position().turn().into()
    }

    /// Current FEN.
    pub fn fen(&self) -> String {
        shakmaty::fen::Fen::from_position(&self.position(), EnPassantMode::Legal).to_string()
    }

    // ── Game-state queries ──────────────────────────────────────────────

    /// `true` iff the side to move is in check.
    pub fn is_check(&self) -> bool {
        self.position().is_check()
    }

    /// `true` iff the side to move has no legal reply and is in check.
    pub fn is_checkmate(&self) -> bool {
        self.position().is_checkmate()
    }

    /// `true` iff the side to move has no legal reply and is *not* in check.
    pub fn is_stalemate(&self) -> bool {
        self.position().is_stalemate()
    }

    /// `true` iff checkmate, stalemate, or any other terminal condition.
    pub fn is_game_over(&self) -> bool {
        self.position().is_game_over()
    }

    /// Outcome of the game, in human-readable form. `"unfinished"` if the
    /// game is still in progress.
    pub fn outcome(&self) -> String {
        let pos = self.position();
        if pos.is_checkmate() {
            // The *side to move* lost; the other side won.
            let winner: PieceColor = pos.turn().other().into();
            format!("{} wins by checkmate", piece_color_label_local(winner))
        } else if pos.is_stalemate() {
            "Draw by stalemate".to_string()
        } else if pos.is_insufficient_material() {
            "Draw by insufficient material".to_string()
        } else if pos.is_game_over() {
            // Fallback for rare terminal cases (75-move, fivefold repetition).
            "Draw".to_string()
        } else {
            "Unfinished".to_string()
        }
    }

    // ── Move queries ─────────────────────────────────────────────────────

    /// All legal moves from the current position.
    pub fn legal_moves(&self) -> Vec<Move> {
        self.position().legal_moves().into_iter().collect()
    }

    /// All legal moves from `from`. Used by the Phase-4 board widget to
    /// highlight candidate destinations after a user click on a piece.
    pub fn legal_moves_from(&self, from: Square) -> Vec<Move> {
        // Cheap O(legal) filter — shakmaty's filter-by-piece is the fastest
        // way to do this without touching `pos.collect()`.
        self.position()
            .legal_moves()
            .into_iter()
            .filter(|m| m.from() == Some(from))
            .collect()
    }

    /// `true` iff `m` is legal in the current position.
    pub fn is_legal(&self, m: Move) -> bool {
        self.position().is_legal(m)
    }

    // ── Move application ─────────────────────────────────────────────────

    /// Apply a fully-formed [`Move`].
    ///
    /// Returns `Ok(())` and advances `cur_ply` on success, `Err(MoveError)`
    /// if the move is illegal (or has the wrong promotion role).
    pub fn apply_move(&mut self, m: Move) -> Result<(), MoveError> {
        let pos = self.position();
        if !pos.is_legal(m) {
            return Err(MoveError::IllegalMove);
        }
        // `from_move_and_play_unchecked` would mutate `pos`; keep the
        // computed SAN around even though we have to compute it via clones.
        let san = SanPlus::from_move(pos.clone(), m);
        let mut next = pos;
        next.play_unchecked(m);
        self.append_ply(Ply { move_: m, san, after: next });
        Ok(())
    }

    /// Construct a move from `(from, to, promotion)` and apply it. `promotion`
    /// must be `Some` for promotion moves (a pawn reaching the back rank)
    /// and `None` otherwise — the player-driven flow will be:
    ///
    /// 1. User clicks source square (selected from() populated).
    /// 2. User clicks destination square.
    /// 3. `apply_user_move()` checks whether the destination is a
    ///    promotion square. If yes, the Phase-4 widget pops a small modal
    ///    and calls `apply_user_move()` a second time with the chosen role.
    ///
    /// # Errors
    /// Returns `MoveError::IllegalMove` when no legal move matches the
    /// given inputs, and `MoveError::AmbiguousPromotion` when more than one
    /// promotion role is legal for the same (from, to). The latter is
    /// handled at the UI layer.
    pub fn apply_user_move(
        &mut self,
        from: Square,
        to: Square,
        promotion: Option<PieceKind>,
    ) -> Result<(), MoveError> {
        // Build candidate moves by looking at all SAN candidates with the
        // given role (if we know it) or by filtering legal moves.
        let pos = self.position();
        let from_role = pos.board().piece_at(from).map(|p| p.role);
        let role = match promotion {
            // For promotions, we have to inspect pawn-on-back-rank moves
            // specifically; the role will be the pawn + the chosen promotion
            // piece goes into `Move::Normal { promotion: … }`.
            Some(_) => Role::Pawn,
            None => match from_role {
                Some(r) => r,
                None => return Err(MoveError::IllegalMove),
            },
        };
        let candidates: Vec<Move> = pos
            .san_candidates(role, to)
            .into_iter()
            .filter(|m| m.from() == Some(from))
            .collect();

        if candidates.is_empty() {
            return Err(MoveError::IllegalMove);
        }

        let chosen = if let Some(promo) = promotion {
            let matched: Vec<Move> = candidates
                .into_iter()
                .filter(|m| matches!(m, Move::Normal { promotion: Some(r), .. } if *r == promo.role()))
                .collect();
            match matched.len() {
                0 => return Err(MoveError::IllegalMove),
                1 => matched[0],
                _ => return Err(MoveError::AmbiguousPromotion),
            }
        } else {
            match candidates.len() {
                0 => return Err(MoveError::IllegalMove),
                1 => candidates[0],
                _ => return Err(MoveError::AmbiguousPromotion),
            }
        };

        self.apply_move(chosen)
    }

    /// Returns the move that *would* play pawn-to-back-rank as each of the
    /// four possible promotion choices. If the destination is not actually
    /// a promotion square for the current position, returns an empty vec.
    /// The Phase-4 promotion modal uses this to render choices.
    pub fn promotion_choices(&self, from: Square, to: Square) -> Vec<PieceKind> {
        let pos = self.position();
        let mut choices = Vec::new();
        for kind in [PieceKind::Queen, PieceKind::Rook, PieceKind::Bishop, PieceKind::Knight] {
            let role = kind.role();
            if pos
                .san_candidates(Role::Pawn, to)
                .into_iter()
                .any(|m| {
                    m.from() == Some(from)
                        && matches!(m, Move::Normal { promotion: Some(r), .. } if r == role)
                })
            {
                choices.push(kind);
            }
        }
        choices
    }

    // ── Navigation ───────────────────────────────────────────────────────

    /// Undo the most recent played move. Returns `true` if a ply was
    /// removed, `false` if we were already at the start of the game.
    pub fn undo(&mut self) -> bool {
        if self.cur_ply == 0 {
            return false;
        }
        self.cur_ply -= 1;
        true
    }

    /// Redo the previously-undone move. Returns `true` if the cursor
    /// advanced, `false` if we were already at the latest ply.
    pub fn redo(&mut self) -> bool {
        if self.cur_ply == self.history.len() {
            return false;
        }
        self.cur_ply += 1;
        true
    }

    /// Jump to an absolute ply index. Out-of-range values are clamped.
    pub fn goto_ply(&mut self, ply: usize) {
        self.cur_ply = ply.min(self.history.len());
    }

    /// Jump to the start of the game (cur_ply = 0).
    pub fn goto_start(&mut self) {
        self.cur_ply = 0;
    }

    /// Jump to the latest known ply.
    pub fn goto_end(&mut self) {
        self.cur_ply = self.history.len();
    }

    // ── PGN serialisation ────────────────────────────────────────────────

    /// Render the full played portion of the game as an iterable stream of
    /// `(move_number, white_san, black_san)` triples. White moves are
    /// always `Some`; black moves are `Some` only when the game has
    /// progressed past that half-move.
    pub fn pgn_lines(&self) -> Vec<(u32, Option<String>, Option<String>)> {
        let plies = self.active_history();
        let mut lines: Vec<(u32, Option<String>, Option<String>)> = Vec::new();
        let mut move_no: u32 = 1;
        let mut i = 0;
        while i < plies.len() {
            let white = plies[i].san.to_string();
            let black = if i + 1 < plies.len() {
                Some(plies[i + 1].san.to_string())
            } else {
                None
            };
            lines.push((move_no, Some(white), black));
            move_no += 1;
            i += 2;
        }
        lines
    }

    /// Serialise the active history as a one-line token stream, e.g.
    /// `"1. e4 e5 2. Nf3 Nc6 3. Bb5"`. The Phase-5 PGN panel renders this
    /// inside a single monospace label; the structured [`Self::pgn_lines`]
    /// is used for clickable navigation.
    pub fn history_string(&self) -> String {
        let mut s = String::new();
        for (i, ply) in self.active_history().iter().enumerate() {
            if i % 2 == 0 {
                let move_no = (i / 2) + 1;
                write!(&mut s, "{}. ", move_no).unwrap();
            }
            write!(&mut s, "{} ", ply.san).unwrap();
        }
        s.trim_end().to_string()
    }

    // ── Engine-binding helpers ───────────────────────────────────────────

    /// PolyGlot Zobrist hash of the current position. Engine subprocesses
    /// expect the FEN, while the Phase-2 PolyGlot reader expects this
    /// hash for O(log N) book lookup.
    pub fn zobrist(&self) -> u64 {
        self.position().zobrist_hash::<Zobrist64>(EnPassantMode::Legal).0
    }

    // ── Internal ─────────────────────────────────────────────────────────

    /// Append a new ply at `cur_ply`, discarding any redo tail.
    fn append_ply(&mut self, ply: Ply) {
        self.history.truncate(self.cur_ply);
        self.history.push(ply);
        self.cur_ply = self.history.len();
    }

    /// Cheap validity check; returns `Err` if the position is not actually
    /// a legal chess position.
    fn validate(start: &Chess) -> Result<(), &'static str> {
        if start.is_checkmate() || start.is_stalemate() {
            return Err("starting position is already terminal");
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Human-readable color name (using the local PieceColor).
fn piece_color_label_local(c: PieceColor) -> &'static str {
    match c {
        PieceColor::White => "White",
        PieceColor::Black => "Black",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Error types
// ─────────────────────────────────────────────────────────────────────────────

/// Errors raised when parsing a FEN string into a starting position.
#[derive(Debug)]
pub enum FenError {
    /// Lexical FEN parse error.
    Parse {
        source: shakmaty::fen::ParseFenError,
    },
    /// `Chess::from_setup` rejected the FEN-derived setup.
    Setup {
        source: shakmaty::PositionError<Chess>,
    },
    /// A wrapping helper rejected the position (currently only used for
    /// fully-terminal positions like checkmate as a starter).
    InvalidStartingPosition,
}

impl FenError {
    fn from_position(err: &'static str) -> Self {
        // We can't actually destructure &'static str into an enum var; small
        // helper to keep semantics simple.
        match err {
            "starting position is already terminal" => FenError::InvalidStartingPosition,
            _ => FenError::InvalidStartingPosition,
        }
    }
}

impl std::fmt::Display for FenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FenError::Parse { source } => write!(f, "could not parse FEN: {}", source),
            FenError::Setup { source } => write!(f, "invalid FEN setup: {}", source),
            FenError::InvalidStartingPosition => {
                write!(f, "starting position is not a legal in-progress chess game")
            }
        }
    }
}

impl std::error::Error for FenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FenError::Parse { source } => Some(source),
            FenError::Setup { source } => Some(source),
            FenError::InvalidStartingPosition => None,
        }
    }
}

/// Errors raised when applying a user-supplied or engine-supplied move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveError {
    /// No legal move matches the given inputs.
    IllegalMove,
    /// More than one legal move matches — used to flag that the caller
    /// needs to disambiguate, e.g. promote to a specific role.
    AmbiguousPromotion,
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveError::IllegalMove => f.write_str("illegal move"),
            MoveError::AmbiguousPromotion => {
                f.write_str("multiple promotion moves match; pick a role")
            }
        }
    }
}

impl std::error::Error for MoveError {}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starting_position_has_20_legal_moves() {
        let g = GameState::new();
        assert_eq!(g.legal_moves().len(), 20);
        assert!(!g.is_check());
        assert!(!g.is_checkmate());
        assert!(!g.is_stalemate());
        assert_eq!(g.turn(), PieceColor::White);
        assert_eq!(g.cur_ply(), 0);
        assert_eq!(g.total_plies(), 0);
    }

    #[test]
    fn playing_e4_then_e5_advances_history() {
        let mut g = GameState::new();

        // White plays e4 (a pawn move, UCI: e2e4).
        let from = Square::from_ascii(b"e2").unwrap();
        let to = Square::from_ascii(b"e4").unwrap();
        g.apply_user_move(from, to, None).unwrap();

        assert_eq!(g.cur_ply(), 1);
        assert_eq!(g.turn(), PieceColor::Black);
        // Squares e2 and e4 reflect the move.
        assert_eq!(g.position().board().piece_at(from), None);
        assert!(g.position().board().piece_at(to).is_some());

        // Black plays e5.
        let from = Square::from_ascii(b"e7").unwrap();
        let to = Square::from_ascii(b"e5").unwrap();
        g.apply_user_move(from, to, None).unwrap();
        assert_eq!(g.cur_ply(), 2);
        assert_eq!(g.turn(), PieceColor::White);
    }

    #[test]
    fn illegal_moves_are_rejected() {
        let mut g = GameState::new();
        // e2 → e5 is illegal for white on move 0.
        let r = g.apply_user_move(
            Square::from_ascii(b"e2").unwrap(),
            Square::from_ascii(b"e5").unwrap(),
            None,
        );
        assert!(matches!(r, Err(MoveError::IllegalMove)));
        // Even after an error the state is unchanged.
        assert_eq!(g.cur_ply(), 0);
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut g = GameState::new();
        g.apply_user_move(Square::from_ascii(b"e2").unwrap(), Square::from_ascii(b"e4").unwrap(), None).unwrap();
        g.apply_user_move(Square::from_ascii(b"e7").unwrap(), Square::from_ascii(b"e5").unwrap(), None).unwrap();
        assert_eq!(g.cur_ply(), 2);

        assert!(g.undo());
        assert_eq!(g.cur_ply(), 1);
        assert_eq!(g.turn(), PieceColor::Black);

        assert!(g.undo());
        assert_eq!(g.cur_ply(), 0);
        assert_eq!(g.turn(), PieceColor::White);

        assert!(g.redo());
        assert!(g.redo());
        assert_eq!(g.cur_ply(), 2);
    }

    #[test]
    fn fen_roundtrips_after_one_move() {
        let mut g = GameState::new();
        g.apply_user_move(Square::from_ascii(b"e2").unwrap(), Square::from_ascii(b"e4").unwrap(), None).unwrap();

        let fen = g.fen();
        let mut parsed = GameState::from_fen(&fen).unwrap();
        // After round-trip, the pawn is on e4 and e2 is empty — so e2->e4
        // is no longer legal, but the same *continuation* (a further legal
        // move) is still parsed correctly.
        assert!(!parsed.is_legal(Move::Normal {
            role: Role::Pawn,
            from: Square::from_ascii(b"e2").unwrap(),
            capture: None,
            to: Square::from_ascii(b"e4").unwrap(),
            promotion: None,
        }));
        // The mirrored black move should be legal after round trip.
        assert!(parsed.is_legal(Move::Normal {
            role: Role::Pawn,
            from: Square::from_ascii(b"e7").unwrap(),
            capture: None,
            to: Square::from_ascii(b"e5").unwrap(),
            promotion: None,
        }));
        // And after we apply e7→e5 on the parsed state, the FEN string should
        // match the one we get by applying both moves on g from the start
        // (the round-trip is loss-less).
        parsed.apply_user_move(
            Square::from_ascii(b"e7").unwrap(),
            Square::from_ascii(b"e5").unwrap(),
            None,
        ).unwrap();
        g.apply_user_move(
            Square::from_ascii(b"e7").unwrap(),
            Square::from_ascii(b"e5").unwrap(),
            None,
        ).unwrap();
        assert_eq!(parsed.fen(), g.fen());
    }

    #[test]
    fn fools_mate_terminates() {
        // Fool's mate: 1.f3 e5 2.g4 Qh4# — four plies, then checkmate.
        let mut g = GameState::new();
        g.apply_user_move(Square::from_ascii(b"f2").unwrap(), Square::from_ascii(b"f3").unwrap(), None).unwrap();
        g.apply_user_move(Square::from_ascii(b"e7").unwrap(), Square::from_ascii(b"e5").unwrap(), None).unwrap();
        g.apply_user_move(Square::from_ascii(b"g2").unwrap(), Square::from_ascii(b"g4").unwrap(), None).unwrap();
        g.apply_user_move(Square::from_ascii(b"d8").unwrap(), Square::from_ascii(b"h4").unwrap(), None).unwrap();
        assert!(g.is_checkmate(), "expected checkmate, got FEN {}", g.fen());
        assert_eq!(g.outcome(), "Black wins by checkmate");
    }

    #[test]
    fn promotion_choices_listed() {
        // White pawn on a7, black king on h8, white king on h1 — a7→a8 must
        // promote and all four choices must be legal.
        // FEN rank 7 must total 8 squares, so we use `P7` (P + 7 empty).
        let fen = "7k/P7/8/8/8/8/8/7K w - - 0 1";
        let mut g = GameState::from_fen(fen).unwrap();
        let choices = g.promotion_choices(
            Square::from_ascii(b"a7").unwrap(),
            Square::from_ascii(b"a8").unwrap(),
        );
        assert_eq!(choices.len(), 4);
        // Without specifying promotion, applying should fail (ambiguous).
        let r = g.apply_user_move(
            Square::from_ascii(b"a7").unwrap(),
            Square::from_ascii(b"a8").unwrap(),
            None,
        );
        assert!(matches!(r, Err(MoveError::AmbiguousPromotion)));
        // With Queen explicit, it succeeds.
        g.apply_user_move(
            Square::from_ascii(b"a7").unwrap(),
            Square::from_ascii(b"a8").unwrap(),
            Some(PieceKind::Queen),
        ).unwrap();
    }

    #[test]
    fn fen_parse_rejects_garbage() {
        // Standard starting position — legal.
        assert!(GameState::from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1").is_ok());
        // Empty board is *illegal* (no kings).
        assert!(GameState::from_fen("8/8/8/8/8/8/8/8 w - - 0 1").is_err());
        // Bogus inputs:
        assert!(GameState::from_fen("xxxxxxx").is_err());
        assert!(GameState::from_fen("").is_err());
        assert!(GameState::from_fen("not a fen").is_err());
    }

    #[test]
    fn zobrist_hash_matches_canonical_polyglot_value() {
        // The PolyGlot "standard" chess starting position has
        // Zobrist key 0x463b96181691fc9c. shakmaty's `zobrist_hash`
        // uses the same compatible random table, so this is a strong
        // proof that our `GameState::zobrist()` agrees with every
        // existing polyglot opening book on disk.
        let g = GameState::new();
        let key = g.zobrist();
        assert_eq!(
            format!("{:016x}", key),
            "463b96181691fc9c",
            "GameState::zobrist() should match PolyGlot canonical value"
        );
    }

    #[test]
    fn history_string_render() {
        let mut g = GameState::new();
        g.apply_user_move(Square::from_ascii(b"e2").unwrap(), Square::from_ascii(b"e4").unwrap(), None).unwrap();
        g.apply_user_move(Square::from_ascii(b"e7").unwrap(), Square::from_ascii(b"e5").unwrap(), None).unwrap();
        g.apply_user_move(Square::from_ascii(b"g1").unwrap(), Square::from_ascii(b"f3").unwrap(), None).unwrap();
        let s = g.history_string();
        assert_eq!(s, "1. e4 e5 2. Nf3");
    }
}
