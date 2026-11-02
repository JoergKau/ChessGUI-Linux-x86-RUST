//! PGN reading and writing for single games (mainline only).
//!
//! * [`to_pgn`] renders a [`GameState`] as a PGN game: tag pairs, a `FEN`
//!   header when the game does not start from the standard position, and
//!   wrapped movetext.
//! * [`parse_pgn`] reads the first game of a PGN text. Comments, NAGs,
//!   variations and move numbers are skipped; each SAN move is validated
//!   against the position it is played in.
//!
//! Variations are dropped on purpose: the game model is linear (see the
//! project plan, §13), so a PGN that is only a line of play round-trips
//! exactly.

use std::fmt::Write;

use shakmaty::{fen::Fen, san::San, Color, EnPassantMode, Position};

use crate::chess::game_state::{FenError, GameState};

/// Movetext is wrapped before this many columns.
const LINE_WIDTH: usize = 80;

/// FEN of the standard starting position.
const STANDARD_START_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

/// Result token for the game's current state: `1-0`, `0-1`, `1/2-1/2`, or `*`
/// while the game is still running.
pub fn result_token(game: &GameState) -> &'static str {
    let pos = game.position();
    if pos.is_checkmate() {
        // The side to move is mated.
        match pos.turn() {
            Color::White => "0-1",
            Color::Black => "1-0",
        }
    } else if pos.is_game_over() {
        "1/2-1/2"
    } else {
        "*"
    }
}

/// Render the played part of `game` as a PGN game.
pub fn to_pgn(game: &GameState) -> String {
    let start = game.start_position();
    let mut out = String::new();
    push_tag(&mut out, "Event", "SiliconBoard game");
    push_tag(&mut out, "Site", "?");
    push_tag(&mut out, "Date", "????.??.??");
    push_tag(&mut out, "Round", "?");
    push_tag(&mut out, "White", "?");
    push_tag(&mut out, "Black", "?");
    push_tag(&mut out, "Result", result_token(game));
    let start_fen = Fen::from_position(&start, EnPassantMode::Legal).to_string();
    if start_fen != STANDARD_START_FEN {
        push_tag(&mut out, "SetUp", "1");
        push_tag(&mut out, "FEN", &start_fen);
    }
    out.push('\n');

    let mut tokens: Vec<String> = Vec::new();
    let first_turn = start.turn();
    let first_number = start.fullmoves().get();
    for (i, ply) in game.active_history().iter().enumerate() {
        let side = if i % 2 == 0 {
            first_turn
        } else {
            first_turn.other()
        };
        let number = first_number + ((i as u32) + u32::from(first_turn == Color::Black)) / 2;
        match side {
            Color::White => tokens.push(format!("{}.", number)),
            Color::Black if i == 0 => tokens.push(format!("{}...", number)),
            Color::Black => {}
        }
        tokens.push(ply.san.to_string());
    }
    tokens.push(result_token(game).to_string());

    let mut line = String::new();
    for token in tokens {
        if !line.is_empty() && line.len() + 1 + token.len() > LINE_WIDTH {
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&token);
    }
    out.push_str(&line);
    out.push('\n');
    out
}

/// Append one `[Name "value"]` tag pair. Quotes and backslashes in the value
/// are escaped as the PGN standard requires.
fn push_tag(out: &mut String, name: &str, value: &str) {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    let _ = writeln!(out, "[{} \"{}\"]", name, escaped);
}

/// Parse the first game of a PGN text into a [`GameState`] at its final
/// position. Test convenience; the app uses [`parse_pgn_with_report`].
///
/// # Errors
/// [`PgnError::Empty`] if the text holds no movetext or tags,
/// [`PgnError::Fen`] for an unusable `FEN` tag,
/// [`PgnError::IllegalMove`] when a move does not fit the position it is in.
#[cfg(test)]
pub fn parse_pgn(text: &str) -> Result<GameState, PgnError> {
    parse_pgn_with_report(text).map(|import| import.game)
}

/// Like [`parse_pgn`], but also reports what was left out: variations,
/// comments and further games in the file. The app shows this so nothing is
/// dropped silently.
pub fn parse_pgn_with_report(text: &str) -> Result<PgnImport, PgnError> {
    let (tags, movetext, extra_games) = split_tags_and_movetext(text);
    if tags.is_empty() && movetext.trim().is_empty() {
        return Err(PgnError::Empty);
    }
    let mut game = match tags.iter().find(|(name, _)| name == "FEN") {
        Some((_, fen)) => GameState::from_fen(fen).map_err(PgnError::Fen)?,
        None => GameState::new(),
    };
    let (tokens, variations, comments) = movetext_tokens(&movetext);
    for (index, token) in tokens.into_iter().enumerate() {
        let position = game.position();
        let san = San::from_ascii(normalise_castling(&token).as_bytes()).map_err(|_| {
            PgnError::IllegalMove {
                ply: index + 1,
                san: token.clone(),
            }
        })?;
        let m = san.to_move(&position).map_err(|_| PgnError::IllegalMove {
            ply: index + 1,
            san: token.clone(),
        })?;
        game.apply_move(m).map_err(|_| PgnError::IllegalMove {
            ply: index + 1,
            san: token.clone(),
        })?;
    }
    Ok(PgnImport {
        game,
        ignored: IgnoredContent {
            variations,
            comments,
            games: extra_games,
        },
    })
}

/// A parsed PGN game together with the content that was skipped.
#[derive(Clone, Debug)]
pub struct PgnImport {
    /// The mainline game, at its final position.
    pub game: GameState,
    /// What was skipped while reading it.
    pub ignored: IgnoredContent,
}

/// Counts of PGN content the importer does not keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IgnoredContent {
    /// Variations (`( … )`) at any depth.
    pub variations: usize,
    /// Comments (`{ … }` and `; …`).
    pub comments: usize,
    /// Further games after the first one in the file.
    pub games: usize,
}

impl IgnoredContent {
    /// Human-readable list such as `"1 variation, 2 comments"`, or `None`
    /// when nothing was skipped.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.variations > 0 {
            parts.push(format!("{} variation(s)", self.variations));
        }
        if self.comments > 0 {
            parts.push(format!("{} comment(s)", self.comments));
        }
        if self.games > 0 {
            parts.push(format!("{} further game(s)", self.games));
        }
        if parts.is_empty() {
            None
        } else {
            Some(format!("ignored {}", parts.join(", ")))
        }
    }
}

/// Split the first game's tag pairs from its movetext. Also returns how many
/// further games follow it in the text; their content is not collected.
fn split_tags_and_movetext(text: &str) -> (Vec<(String, String)>, String, usize) {
    let mut tags = Vec::new();
    let mut movetext = String::new();
    let mut in_movetext = false;
    let mut previous_was_tag = false;
    let mut collecting = true;
    let mut extra_games = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        let is_tag = trimmed.starts_with('[');
        if is_tag && !in_movetext {
            if let Some(tag) = parse_tag(trimmed) {
                tags.push(tag);
            }
        } else if is_tag {
            // A tag line after movetext starts another game.
            if !previous_was_tag {
                extra_games += 1;
            }
            collecting = false;
        } else if collecting && (!trimmed.is_empty() || in_movetext) {
            in_movetext = true;
            movetext.push_str(line);
            movetext.push('\n');
        }
        previous_was_tag = is_tag;
    }
    (tags, movetext, extra_games)
}

/// Parse a `[Name "value"]` line. Returns `None` for malformed tags, which
/// are ignored rather than rejected.
fn parse_tag(line: &str) -> Option<(String, String)> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (name, rest) = inner.split_once(' ')?;
    let value = rest.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some((name.to_string(), value.replace("\\\"", "\"").replace("\\\\", "\\")))
}

/// Movetext → SAN move tokens, plus the number of variations and comments
/// that were skipped. Skips comments (`{…}` and `;…`), variations (`(…)`,
/// nested), NAGs (`$n`), move numbers (`12.` / `12...`), annotation glyphs
/// (`!?`) and the game result.
fn movetext_tokens(movetext: &str) -> (Vec<String>, usize, usize) {
    let mut tokens = Vec::new();
    let mut chars = movetext.chars().peekable();
    let mut variation_depth = 0usize;
    let mut variations = 0usize;
    let mut comments = 0usize;
    let mut current = String::new();

    let flush = |current: &mut String, tokens: &mut Vec<String>, depth: usize| {
        if current.is_empty() {
            return;
        }
        if depth == 0 && !is_move_number(current) && !is_result(current) && !current.starts_with('$') {
            let cleaned = current.trim_end_matches(['!', '?']).to_string();
            if !cleaned.is_empty() {
                tokens.push(cleaned);
            }
        }
        current.clear();
    };

    while let Some(c) = chars.next() {
        match c {
            '{' => {
                flush(&mut current, &mut tokens, variation_depth);
                comments += 1;
                for inner in chars.by_ref() {
                    if inner == '}' {
                        break;
                    }
                }
            }
            ';' => {
                flush(&mut current, &mut tokens, variation_depth);
                comments += 1;
                for inner in chars.by_ref() {
                    if inner == '\n' {
                        break;
                    }
                }
            }
            '(' => {
                flush(&mut current, &mut tokens, variation_depth);
                if variation_depth == 0 {
                    variations += 1;
                }
                variation_depth += 1;
            }
            ')' => {
                flush(&mut current, &mut tokens, variation_depth);
                variation_depth = variation_depth.saturating_sub(1);
            }
            c if c.is_whitespace() => flush(&mut current, &mut tokens, variation_depth),
            c => current.push(c),
        }
    }
    flush(&mut current, &mut tokens, variation_depth);
    (tokens, variations, comments)
}

/// `12.`, `12...` and similar move-number tokens.
fn is_move_number(token: &str) -> bool {
    let digits = token.trim_end_matches('.');
    !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && token.len() - digits.len() <= 3
}

/// The four PGN result tokens.
fn is_result(token: &str) -> bool {
    matches!(token, "1-0" | "0-1" | "1/2-1/2" | "*")
}

/// Accept `0-0` / `0-0-0` (zero castling, common in exported games) as
/// `O-O` / `O-O-O`.
fn normalise_castling(token: &str) -> String {
    if token.starts_with("0-0") {
        token.replace('0', "O")
    } else {
        token.to_string()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Errors raised while reading a PGN game.
#[derive(Debug)]
pub enum PgnError {
    /// The text holds neither tags nor movetext.
    Empty,
    /// The `FEN` tag does not describe a legal position.
    Fen(FenError),
    /// A move token does not match a legal move in its position.
    IllegalMove {
        /// 1-based half-move number within the game.
        ply: usize,
        /// The SAN token as written in the file.
        san: String,
    },
}

impl std::fmt::Display for PgnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgnError::Empty => f.write_str("the PGN file contains no game"),
            PgnError::Fen(source) => write!(f, "invalid FEN tag: {}", source),
            PgnError::IllegalMove { ply, san } => {
                write!(f, "illegal move `{}` at half-move {}", san, ply)
            }
        }
    }
}

impl std::error::Error for PgnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PgnError::Fen(source) => Some(source),
            PgnError::Empty | PgnError::IllegalMove { .. } => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shakmaty::Square;

    fn sq(name: &str) -> Square {
        Square::from_ascii(name.as_bytes()).unwrap()
    }

    fn play(game: &mut GameState, from: &str, to: &str) {
        game.play_squares(sq(from), sq(to), None).unwrap();
    }

    fn sans(game: &GameState) -> Vec<String> {
        game.active_history().iter().map(|p| p.san.to_string()).collect()
    }

    #[test]
    fn written_game_parses_back_to_the_same_moves() {
        let mut game = GameState::new();
        play(&mut game, "e2", "e4");
        play(&mut game, "e7", "e5");
        play(&mut game, "g1", "f3");
        play(&mut game, "b8", "c6");
        let text = to_pgn(&game);
        assert!(text.contains("1. e4 e5 2. Nf3 Nc6 *"), "movetext: {}", text);
        let parsed = parse_pgn(&text).unwrap();
        assert_eq!(sans(&parsed), vec!["e4", "e5", "Nf3", "Nc6"]);
        assert_eq!(parsed.fen(), game.fen());
    }

    #[test]
    fn result_tag_matches_checkmate() {
        // Fool's mate: Black wins.
        let mut game = GameState::new();
        play(&mut game, "f2", "f3");
        play(&mut game, "e7", "e5");
        play(&mut game, "g2", "g4");
        play(&mut game, "d8", "h4");
        assert_eq!(result_token(&game), "0-1");
        assert!(to_pgn(&game).contains("[Result \"0-1\"]"));
        assert!(to_pgn(&game).trim_end().ends_with("0-1"));
    }

    #[test]
    fn game_from_fen_writes_fen_tags_and_move_numbers_with_dots() {
        // Black to move at move 12: moves are written as `12...`.
        let fen = "rnbqkbnr/pppppppp/8/8/4P3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 12";
        let mut game = GameState::from_fen(fen).unwrap();
        play(&mut game, "c7", "c5");
        let text = to_pgn(&game);
        assert!(text.contains("[SetUp \"1\"]"));
        assert!(text.contains("[FEN \""));
        assert!(text.contains("12... c5"), "movetext: {}", text);
        let parsed = parse_pgn(&text).unwrap();
        assert_eq!(parsed.fen(), game.fen());
    }

    #[test]
    fn comments_nags_variations_and_numbers_are_skipped() {
        let text = r#"
[Event "Test"]
[Result "1-0"]

1. e4 {the king's pawn} e5 $1 2. Nf3 (2. Bc4 Bc5 3. d3) 2... Nc6 ; line ends
3. Bb5 a6 1-0
"#;
        let game = parse_pgn(text).unwrap();
        assert_eq!(sans(&game), vec!["e4", "e5", "Nf3", "Nc6", "Bb5", "a6"]);
    }

    #[test]
    fn zero_castling_is_accepted() {
        let text = "[Event \"Castle\"]\n\n1. e4 e5 2. Nf3 Nc6 3. Bc4 Bc5 4. 0-0 *\n";
        let game = parse_pgn(text).unwrap();
        assert_eq!(sans(&game).last().map(String::as_str), Some("O-O"));
    }

    #[test]
    fn illegal_move_reports_its_ply() {
        let text = "[Event \"Bad\"]\n\n1. e4 e6 2. e5 e5 *\n";
        match parse_pgn(text) {
            Err(PgnError::IllegalMove { ply, san }) => {
                assert_eq!(ply, 4);
                assert_eq!(san, "e5");
            }
            other => panic!("expected IllegalMove, got {:?}", other),
        }
    }

    #[test]
    fn empty_text_is_an_error_and_movetext_only_is_a_game() {
        assert!(matches!(parse_pgn("   \n"), Err(PgnError::Empty)));
        let game = parse_pgn("1. d4 d5 *").unwrap();
        assert_eq!(sans(&game), vec!["d4", "d5"]);
    }

    #[test]
    fn report_counts_variations_comments_and_further_games() {
        let text = "[Event \"One\"]\n\n1. e4 {a} e5 (1... c5) 2. Nf3 ; note\n*\n\n[Event \"Two\"]\n\n1. d4 *\n";
        let import = parse_pgn_with_report(text).unwrap();
        assert_eq!(sans(&import.game), vec!["e4", "e5", "Nf3"]);
        assert_eq!(
            import.ignored,
            IgnoredContent { variations: 1, comments: 2, games: 1 }
        );
        assert_eq!(
            import.ignored.summary().as_deref(),
            Some("ignored 1 variation(s), 2 comment(s), 1 further game(s)")
        );
    }

    #[test]
    fn clean_game_reports_nothing_ignored() {
        let import = parse_pgn_with_report("1. e4 e5 *").unwrap();
        assert_eq!(import.ignored.summary(), None);
    }

    #[test]
    fn only_the_first_game_of_a_file_is_read() {
        let text = "[Event \"One\"]\n\n1. e4 *\n\n[Event \"Two\"]\n\n1. d4 *\n";
        let game = parse_pgn(text).unwrap();
        assert_eq!(sans(&game), vec!["e4"]);
    }

    #[test]
    fn quoted_tag_values_survive_escaping() {
        let (tags, _, _) = split_tags_and_movetext("[Event \"A \\\"quoted\\\" name\"]\n\n*\n");
        assert_eq!(tags, vec![("Event".to_string(), "A \"quoted\" name".to_string())]);
    }
}
