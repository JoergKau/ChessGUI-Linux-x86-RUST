//! Streaming PGN reader: one game per iteration, without loading the file.
//!
//! A downloaded collection is easily 500 MB–2.5 GB, so it must never be read
//! into one `String`. [`PgnStream`] reads line by line and hands the caller one
//! complete game — tag pairs plus movetext — at a time. The rule for "this line
//! starts a new game" lives in `chess::pgn::GameBoundary`, shared with the
//! in-memory splitter, so both readers agree on comments, header-only games and
//! wrapped annotation lines.
//!
//! The reader also counts the bytes it has consumed, which is what the import
//! dialog's progress bar follows: the total game count of a PGN is unknown
//! until the file has been read, but its size is known up front.

use std::io::{BufRead, Result};

use crate::chess::pgn::{decode_pgn_bytes, GameBoundary, PgnLineKind};

/// Yields one PGN game per iteration.
pub struct PgnStream<R: BufRead> {
    reader: R,
    /// The current line as raw bytes. Reading bytes (instead of a `String`)
    /// is what lets a file with a few legacy-encoded comments be repaired
    /// rather than rejected by `read_line`.
    line: Vec<u8>,
    bytes: u64,
    /// The first line of the next game, read while closing the current one.
    pending: Option<String>,
    finished: bool,
    /// Shared boundary rule, carried across lines (and across games) so brace
    /// comments survive a line split.
    boundary: GameBoundary,
}

impl<R: BufRead> PgnStream<R> {
    /// Wrap a buffered reader.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line: Vec::new(),
            bytes: 0,
            pending: None,
            finished: false,
            boundary: GameBoundary::new(),
        }
    }

    /// Bytes consumed from the source so far, including the game being read.
    pub fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// Read one line into `self.line`; `false` at end of file.
    fn next_line(&mut self) -> Result<bool> {
        self.line.clear();
        let read = self.reader.read_until(b'\n', &mut self.line)?;
        if read == 0 {
            return Ok(false);
        }
        self.bytes += read as u64;
        Ok(true)
    }
}

impl<R: BufRead> Iterator for PgnStream<R> {
    type Item = Result<String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut current = String::new();
        // A line read while closing the previous game already belongs to this
        // one, and the boundary machine has already classified it.
        if let Some(line) = self.pending.take() {
            current.push_str(&line);
        }
        loop {
            match self.next_line() {
                Ok(true) => {}
                Ok(false) => {
                    self.finished = true;
                    break;
                }
                Err(err) => {
                    self.finished = true;
                    return Some(Err(err));
                }
            }
            let text = decode_pgn_bytes(&self.line);
            if self.boundary.classify(&text) == PgnLineKind::NewGame {
                self.pending = Some(text.into_owned());
                break;
            }
            current.push_str(&text);
        }
        if current.trim().is_empty() {
            // Trailing blank lines at the end of the file.
            if self.finished {
                return None;
            }
            return self.next();
        }
        Some(Ok(current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn games(text: &str) -> Vec<String> {
        PgnStream::new(Cursor::new(text.as_bytes()))
            .map(|game| game.unwrap())
            .collect()
    }

    #[test]
    fn two_games_are_split_at_the_second_event_tag() {
        let text = "[Event \"One\"]\n\n1. e4 e5 1-0\n\n[Event \"Two\"]\n\n1. d4 d5 0-1\n";
        let games = games(text);
        assert_eq!(games.len(), 2);
        assert!(games[0].contains("1. e4 e5"));
        assert!(games[1].contains("1. d4 d5"));
        assert!(!games[0].contains("Game Two"));
    }

    #[test]
    fn a_header_only_game_stays_a_game_of_its_own() {
        let text = "[Event \"Empty\"]\n[White \"A\"]\n[Black \"B\"]\n\n\
                    [Event \"Real\"]\n[White \"C\"]\n[Black \"D\"]\n\n1. e4 e5 1-0\n";
        let games = games(text);
        assert_eq!(games.len(), 2);
        assert!(games[0].contains("[Event \"Empty\"]"));
        assert!(!games[0].contains("1. e4"));
        assert!(games[1].contains("[Event \"Real\"]"));
        assert!(games[1].contains("1. e4 e5"));
    }

    #[test]
    fn crlf_line_endings_are_fine() {
        let text =
            "[Event \"One\"]\r\n\r\n1. e4 e5 1-0\r\n\r\n[Event \"Two\"]\r\n\r\n1. d4 0-1\r\n";
        assert_eq!(games(text).len(), 2);
    }

    #[test]
    fn a_tag_less_game_is_still_one_game() {
        let games = games("1. e4 e5 2. Nf3 *\n");
        assert_eq!(games.len(), 1);
        assert!(games[0].contains("Nf3"));
    }

    #[test]
    fn an_eval_annotation_line_inside_a_comment_does_not_split_the_game() {
        // Regression for BUG-8-10: a wrapped comment can continue on a line
        // that begins with `[%eval …]`; that line is not a tag pair.
        let text = "[Event \"One\"]\n\n1. e4 {+0.2/30\n[%eval #5,37] 11s} e5 1-0\n\n\
                    [Event \"Two\"]\n\n1. d4 d5 0-1\n";
        let games = games(text);
        assert_eq!(games.len(), 2);
        assert!(games[0].contains("[%eval #5,37]"));
        assert!(games[0].contains("e5 1-0"));
        assert!(games[1].contains("1. d4 d5"));
    }

    #[test]
    fn a_bracketed_line_inside_a_comment_is_not_a_new_game() {
        let text = "[Event \"One\"]\n\n\
                    1. e4 {a note\n[Event \"not a game\"]\ncontinued} e5 1-0\n\n\
                    [Event \"Two\"]\n\n1. d4 0-1\n";
        let games = games(text);
        assert_eq!(games.len(), 2);
        assert!(games[0].contains("[Event \"not a game\"]"));
        assert!(games[0].contains("continued"));
        assert!(games[1].contains("1. d4"));
    }

    #[test]
    fn the_stream_and_the_in_memory_splitter_agree_on_a_tricky_file() {
        // The two readers share `GameBoundary`; this pins that they count the
        // same games even when comments wrap around annotation and tag lines.
        let text = "[Event \"One\"]\n\n\
                    1. e4 {+0.2/30\n[%eval #5,37] 11s} e5 {a note\n[Event \"not a game\"]\ncontinued} 2. Nf3 *\n\n\
                    [Event \"Two\"]\n\n1. d4 d5 0-1\n";
        let streamed = games(text);
        let collection = crate::chess::pgn::parse_pgn_collection(text).unwrap();
        assert_eq!(streamed.len(), 2);
        assert_eq!(streamed.len(), collection.games.len());
        assert_eq!(collection.games[1].event, "Two");
    }

    #[test]
    fn glued_move_numbers_are_kept_verbatim() {
        let games = games("[Event \"x\"]\n\n1.e4 e5 2.Nf3 Nc6 *\n");
        assert!(games[0].contains("1.e4 e5 2.Nf3"));
    }

    #[test]
    fn the_byte_counter_reaches_the_file_size() {
        let text = "[Event \"One\"]\n\n1. e4 1-0\n";
        let mut stream = PgnStream::new(Cursor::new(text.as_bytes()));
        while stream.next().is_some() {}
        assert_eq!(stream.bytes_read(), text.len() as u64);
    }

    #[test]
    fn a_legacy_encoded_comment_does_not_stop_the_stream() {
        // 0xE9 is "é" in Windows-1252 and invalid on its own in UTF-8.
        let bytes = b"[Event \"Caf\xe9\"]\n\n1. e4 e5 1-0\n\n[Event \"Two\"]\n\n1. d4 0-1\n";
        let games: Vec<String> = PgnStream::new(Cursor::new(bytes))
            .map(|game| game.unwrap())
            .collect();
        assert_eq!(games.len(), 2);
        assert!(games[0].contains("Café"));
        assert!(games[1].contains("1. d4"));
    }

    #[test]
    fn a_byte_order_mark_does_not_become_part_of_the_first_game() {
        let bytes = b"\xef\xbb\xbf[Event \"One\"]\n\n1. e4 1-0\n";
        let games = PgnStream::new(Cursor::new(bytes))
            .map(|game| game.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(games.len(), 1);
        assert!(games[0].starts_with("[Event"));
    }

    #[test]
    fn blank_lines_at_the_end_do_not_make_an_empty_game() {
        let games = games("[Event \"One\"]\n\n1. e4 1-0\n\n\n\n");
        assert_eq!(games.len(), 1);
    }
}
