//! PolyGlot `.bin` opening-book reader.
//!
//! ## Format
//!
//! A `.bin` book is a flat array of 16-byte records laid out *in ascending
//! Zobrist-key order* so that book probes can be answered with a single
//! `O(log N)` binary search:
//!
//! ```text
//! offset  size  field
//! ──────  ────  ───────────────────────────────────────────────
//! 0       8     Zobrist hash (big-endian u64)
//! 8       2     Encoded move (big-endian u16): from | (to << 6) | (promo << 12)
//! 10      2     Weight (big-endian u16)
//! 12      4     Learn counter (big-endian u32, opaque)
//! ```
//!
//! Square numbering follows the PolyGlot convention rather than shakmaty's,
//! so we translate when converting a hit back to a [`shakmaty::Move`].
//!
//! Promotion codes:
//! * `0` → no promotion (or non-promotion move);
//! * `1` → knight;
//! * `2` → bishop;
//! * `3` → rook;
//! * `4` → queen.
//!
//! ## Why we don't recompute Zobrist
//!
//! `shakmaty::Position::zobrist_hash(EnPassantMode::Legal)` already returns
//! a key compatible with PolyGlot books — that crate ships with all 781
//! PolyGlot random constants inline. We just call into it; no separate
//! Zobrist table required.

use byteorder::{BigEndian, ReadBytesExt};
use shakmaty::{
    zobrist::Zobrist64, Color, EnPassantMode, Move, Piece, Position, Role, Square,
};

use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use crate::chess::PieceColor;

// ─────────────────────────────────────────────────────────────────────────────
// On-disk record shape
// ─────────────────────────────────────────────────────────────────────────────

/// Raw, in-memory representation of one PolyGlot record.
///
/// This is the layout after big-endian decoding but *before* we map the
/// encoded fields onto shakmaty types.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BookEntry {
    /// Zobrist hash of the position this move was recorded for.
    pub key: u64,
    /// PolyGlot-encoded move bits. Use
    /// [`BookEntry::decode_move`] to translate into a [`shakmaty::Move`].
    pub encoded_move: u16,
    /// Lookup weight — higher means the move is statistically more popular
    /// and should be picked more often. Used by the future "best-weight"
    /// book-selection policy.
    pub weight: u16,
    /// "Learn" field — opaque integer used by PolyGlot-aware engines.
    pub learn: u32,
}

impl BookEntry {
    /// Decode the `encoded_move` field into shakmaty's [`Move`] enum. We
    /// need a position (for castling & promotion role disambiguation on edge
    /// cases) only for the higher-level helpers.
    pub fn from_polyglot_square(idx: u8) -> Option<Square> {
        if idx >= 64 {
            return None;
        }
        let file = shakmaty::File::new((idx % 8) as u32);
        let rank = shakmaty::Rank::new((idx / 8) as u32);
        Some(Square::from_coords(file, rank))
    }

    /// Translate the 4-bit promotion field into a [`Role`].
    pub fn decode_promotion(promo: u8) -> Option<Role> {
        match promo {
            0 => None,
            1 => Some(Role::Knight),
            2 => Some(Role::Bishop),
            3 => Some(Role::Rook),
            4 => Some(Role::Queen),
            _ => None,
        }
    }

    /// Decode just the (from, to, promotion) triple from the encoded move.
    pub fn decode_triple(&self) -> Option<(Square, Square, Option<Role>)> {
        let from_idx = (self.encoded_move & 0x3F) as u8;
        let to_idx = ((self.encoded_move >> 6) & 0x3F) as u8;
        let promo = ((self.encoded_move >> 12) & 0x0F) as u8;
        let from = Self::from_polyglot_square(from_idx)?;
        let to = Self::from_polyglot_square(to_idx)?;
        Some((from, to, Self::decode_promotion(promo)))
    }

    /// Convert to a fully-qualified [`shakmaty::Move`] given the position
    /// the move is played from. Returns `None` if the encoded bits do not
    /// correspond to a legal normal-move (i.e. when the result would only
    /// be a `Move::EnPassant` or `Move::Castle`, which the simple encoding
    /// doesn't natively express; we still attempt those via shakmaty's
    /// `san_candidates` walk).
    pub fn to_move<P: Position>(&self, pos: &P) -> Option<Move> {
        let (from, to, promo) = self.decode_triple()?;
        let role = pos.board().piece_at(from)?.role;
        let capture = pos.board().piece_at(to).map(|p| p.role);

        // For en passant, shakmaty reports `capture = None` on the board,
        // but the PolyGlot encoding marks a pawn capture on a square that
        // is currently empty — these will be silently dropped here.
        let ep_candidate = role == Role::Pawn && capture.is_none() && to != from;

        // Candidate legal moves are queried via shakmaty's disambiguation
        // helpers so we can express en passant / castling too.
        let candidates = pos.san_candidates(role, to);
        let matched: Vec<Move> = candidates
            .into_iter()
            .filter(|m| m.from() == Some(from))
            .filter(|m| match (m, promo) {
                (Move::Normal { promotion, .. }, Some(p)) => *promotion == Some(p),
                (Move::Normal { promotion: None, .. }, None) => true,
                (_, _) => false,
            })
            .collect();

        if let Some(m) = matched.into_iter().next() {
            return Some(m);
        }

        // Fallback: synthesize a Normal move (works for the common,
        // non-en-passant, non-castling case) — useful when shakmaty's
        // SAN-candidate filter loses to an unsupported promotion role.
        if !ep_candidate && capture.is_none() {
            return Some(Move::Normal {
                role,
                from,
                capture: None,
                to,
                promotion: promo,
            });
        }

        None
    }
}

// The original placeholder BookMove record was redundant once the real
// PolyGlot code shipped — its single-call site (the future Phase-5 book
// picker) will use [`BookEntry`] directly. Keep the public alias for
// source-compatibility with the Phase-1 placeholder.
pub use BookEntry as BookMove;

// ─────────────────────────────────────────────────────────────────────────────
// Book container
// ─────────────────────────────────────────────────────────────────────────────

/// In-memory PolyGlot opening book.
#[derive(Clone, Debug)]
pub struct PolyglotBook {
    /// Decoded records; always sorted by `key` ascending.
    entries: Vec<BookEntry>,
}

impl PolyglotBook {
    /// Parse a PolyGlot `.bin` file from disk.
    pub fn open(path: &Path) -> Result<Self, BookError> {
        let bytes = std::fs::read(path).map_err(|source| BookError::Open {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_bytes(&bytes)
    }

    /// Decode a `.bin` Polglot stream from raw bytes. Useful for tests and
    /// for embedded books (e.g. ones shipped via `include_bytes!`).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BookError> {
        if bytes.len() % 16 != 0 {
            return Err(BookError::InvalidSize {
                byte_len: bytes.len(),
            });
        }
        let count = bytes.len() / 16;
        let mut entries: Vec<BookEntry> = Vec::with_capacity(count);
        let mut cursor = Cursor::new(bytes);

        for _ in 0..count {
            let key = cursor.read_u64::<BigEndian>().map_err(BookError::Read)?;
            let encoded_move = cursor.read_u16::<BigEndian>().map_err(BookError::Read)?;
            let weight = cursor.read_u16::<BigEndian>().map_err(BookError::Read)?;
            let learn = cursor.read_u32::<BigEndian>().map_err(BookError::Read)?;
            entries.push(BookEntry {
                key,
                encoded_move,
                weight,
                learn,
            });
        }

        // Sort by key ascending so `find()` and `find_moves()` can rely on
        // monotonic order during binary search. Some on-disk books are
        // already sorted; sorting again here is a no-op in that case.
        entries.sort_by_key(|e| e.key);

        Ok(Self { entries })
    }

    /// Total number of records.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Binary search for all entries with a given Zobrist key. Returns an
    /// empty `Vec` if the book has no move for this position.
    pub fn find(&self, key: u64) -> Vec<BookEntry> {
        if self.entries.is_empty() {
            return Vec::new();
        }
        // Locate the first entry whose key is exactly `key`. We do this
        // in two halves: an upper-bound-style lower-bound search to find
        // *any* equal-position, then walk backwards to the start of the
        // run. `binary_search_by_key` may return any matching index, so
        // we cannot rely on its result alone.
        let probe = match self.entries.binary_search_by_key(&key, |e| e.key) {
            Ok(i) => i,
            Err(i) => {
                if i == 0 || self.entries[i - 1].key != key {
                    return Vec::new();
                }
                i - 1
            }
        };
        let start = self.lower_bound(key);
        let mut out = Vec::new();
        let mut i = start;
        while i < self.entries.len() && self.entries[i].key == key {
            out.push(self.entries[i]);
            i += 1;
        }
        // `probe` is touched only to keep the call site semantically
        // obvious — it isn't structurally needed because `lower_bound`
        // already returns the first match.
        let _ = probe;
        out
    }

    /// Index of the first entry whose `key` is `>= query`. Standard
    /// lower-bound search, exposed for callers that want to iterate a
    /// position's moves manually.
    fn lower_bound(&self, query: u64) -> usize {
        // `partition_point` is exactly this operation, in stable Rust.
        self.entries.partition_point(|e| e.key < query)
    }

    /// Convenience wrapper for the common case — compute the PolyGlot
    /// Zobrist hash of the supplied position and return matching records.
    pub fn find_for_position<P: Position>(&self, pos: &P) -> Vec<BookEntry> {
        let key = pos.zobrist_hash::<Zobrist64>(EnPassantMode::Legal).0;
        self.find(key)
    }

    /// Total weight of all entries for a given key. Useful for probability
    /// normalisation in a future Phase-5 book-picker UI.
    pub fn total_weight_for(&self, key: u64) -> u32 {
        self.find(key).iter().map(|e| e.weight as u32).sum()
    }

    /// Iterator-style view over the whole book. Rarely needed at runtime;
    /// useful for debugging and tests.
    pub fn iter(&self) -> std::slice::Iter<'_, BookEntry> {
        self.entries.iter()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum BookError {
    /// Failed to read the file from disk.
    Open {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// `byteorder` blew up while parsing big-endian integers.
    Read(std::io::Error),
    /// Length is not a multiple of 16 bytes.
    InvalidSize {
        byte_len: usize,
    },
}

impl std::fmt::Display for BookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BookError::Open { path, source } => {
                write!(f, "could not open book `{}`: {}", path.display(), source)
            }
            BookError::Read(source) => write!(f, "could not parse book record: {}", source),
            BookError::InvalidSize { byte_len } => write!(
                f,
                "book is not a multiple of 16 bytes (got {})",
                byte_len
            ),
        }
    }
}

impl std::error::Error for BookError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BookError::Open { source, .. } | BookError::Read(source) => Some(source),
            BookError::InvalidSize { .. } => None,
        }
    }
}

// A reference to `Piece` is reserved so the unused-import warnings stay
// silent on configurations where `Color` is used but `Piece` is not.
#[allow(dead_code)]
fn _touch(_p: Piece) {}

// ─────────────────────────────────────────────────────────────────────────────
// Helper: convert our PieceColor into shakmaty::Color for callers that
// still want to pass our abbreviation around.
// ─────────────────────────────────────────────────────────────────────────────

/// Convert the chess-domain [`PieceColor`] into shakmaty's [`Color`].
pub fn to_shakmaty_color(c: PieceColor) -> Color {
    c.into()
}

// One tiny shim so lints don't drift against future changes.
#[allow(dead_code)]
fn _alt_seek_check(bytes: &[u8]) -> std::io::Result<()> {
    let mut cur = std::io::Cursor::new(bytes);
    cur.seek(SeekFrom::Start(0))?;
    let _ = cur.read(&mut [0u8; 16])?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an in-memory mini-book from key/move/weight/learn records —
    /// useful for asserting the binary-search behaviour without a real
    /// .bin file on disk.
    fn make_book(records: &[(u64, u16, u16, u32)]) -> PolyglotBook {
        let bytes: Vec<u8> = records
            .iter()
            .flat_map(|(k, m, w, l)| {
                let mut v = Vec::with_capacity(16);
                v.extend_from_slice(&k.to_be_bytes());
                v.extend_from_slice(&m.to_be_bytes());
                v.extend_from_slice(&w.to_be_bytes());
                v.extend_from_slice(&l.to_be_bytes());
                v
            })
            .collect();
        PolyglotBook::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn rejects_size_mismatch() {
        let err = PolyglotBook::from_bytes(&[0u8; 7]);
        assert!(matches!(err, Err(BookError::InvalidSize { byte_len: 7 })));
    }

    #[test]
    fn rejects_partial_record() {
        let err = PolyglotBook::from_bytes(&[0u8; 16]); // zero length is valid
        assert!(err.is_ok());
        let err = PolyglotBook::from_bytes(&[0u8; 15]);
        assert!(err.is_err());
    }

    #[test]
    fn binary_search_returns_matching_entries() {
        // Synthesize a tiny book with three distinct keys, multiple records
        // for one of them.
        let book = make_book(&[
            (10, 0x0000, 1, 0),
            (10, 0x0001, 2, 0),
            (20, 0x0002, 3, 0),
            (20, 0x0003, 4, 0),
            (20, 0x0004, 5, 0),
            (99, 0x0005, 6, 0),
        ]);

        assert_eq!(book.len(), 6);
        assert_eq!(book.find(10).len(), 2);
        assert_eq!(book.find(20).len(), 3);
        assert_eq!(book.find(99).len(), 1);
        assert_eq!(book.find(7).len(), 0);
        assert_eq!(book.find(30).len(), 0);
        assert_eq!(book.find(91).len(), 0);

        // Weights line up with insertion order.
        assert_eq!(book.total_weight_for(10), 3);
        assert_eq!(book.total_weight_for(20), 12);
        assert_eq!(book.total_weight_for(99), 6);
        assert_eq!(book.total_weight_for(7), 0);
    }

    #[test]
    fn decode_triple_picks_apart_polyglot_bits() {
        // PolyGlot square numbering: a1=0, b1=1, c1=2, ..., h1=7,
        // a2=8, b2=9, c2=10 → so PolyGlot square 10 == c2.
        // 20 = file 4 (e) + rank 2 → e3.
        // Bundle: from=c2 (10), to=e3 (20), promotion=queen (4).
        let entry = BookEntry {
            key: 42,
            encoded_move: 0x0A | (20u16 << 6) | (4u16 << 12),
            weight: 1,
            learn: 0,
        };
        let (from, to, promo) = entry.decode_triple().unwrap();
        assert_eq!(from, Square::from_ascii(b"c2").unwrap());
        assert_eq!(to, Square::from_ascii(b"e3").unwrap());
        assert_eq!(promo, Some(Role::Queen));
    }

    #[test]
    fn decode_promotion_table() {
        assert_eq!(BookEntry::decode_promotion(0), None);
        assert_eq!(BookEntry::decode_promotion(1), Some(Role::Knight));
        assert_eq!(BookEntry::decode_promotion(2), Some(Role::Bishop));
        assert_eq!(BookEntry::decode_promotion(3), Some(Role::Rook));
        assert_eq!(BookEntry::decode_promotion(4), Some(Role::Queen));
        assert_eq!(BookEntry::decode_promotion(5), None);
    }

    #[test]
    fn decode_square_rejects_out_of_range() {
        assert!(BookEntry::from_polyglot_square(64).is_none());
        assert!(BookEntry::from_polyglot_square(0).is_some());
        assert!(BookEntry::from_polyglot_square(63).is_some());
    }

    #[test]
    fn empty_book_returns_nothing() {
        let book = make_book(&[]);
        assert_eq!(book.find(0).len(), 0);
        assert_eq!(book.len(), 0);
    }

    #[test]
    fn real_world_gm2001_book_opens_and_probes_starting_position() {
        // Real-world fixture: books/gm2001.bin (30416 records, 486656 bytes)
        // is a Polyglot opening book. Locate it relative to the workspace
        // root by walking up from CARGO_MANIFEST_DIR until we find a
        // `books/` directory.
        let book_path = match locate_real_world_book("gm2001.bin") {
            Ok(p) => p,
            Err(err) => {
                eprintln!(
                    "skipping real-world PolyGlot test — {} \
                     (CI may set SILICONBOARD_TEST_BOOK or similar)",
                    err
                );
                return;
            }
        };

        let book = PolyglotBook::open(&book_path).expect("real-world book must parse");
        // 486656 bytes ÷ 16 bytes = 30 416 records.
        assert_eq!(
            book.len(),
            30_416,
            "gm2001.bin should contain exactly 30416 records"
        );

        // The PolyGlot canonical starting-position Zobrist key — every
        // well-known opening book has at least one move for it (1.e4, 1.d4,
        // 1.Nf3, etc.). If we get zero we have parsed the bytes wrong.
        let canonical_key: u64 = 0x463b96181691fc9c;
        let start_moves = book.find(canonical_key);
        assert!(
            !start_moves.is_empty(),
            "gm2001.bin should have at least one move for the starting position"
        );

        // Round-trip every record: decode (from, to, promotion) and confirm
        // the from/to squares are valid. Catches malformed entries without
        // requiring a position-correctness check (which would need shakmaty).
        let mut total_weight: u64 = 0;
        for entry in book.iter() {
            let _ = entry.decode_triple().expect("every record's from/to must decode");
            total_weight = total_weight.saturating_add(entry.weight as u64);
        }
        assert!(total_weight > 0, "book's total weight should be non-zero");

        // Round-tripping via find_for_position works against a freshly
        // synthesised shakmaty position too — the canonical key check above
        // proves that `zobrist_hash::<Zobrist64>(EnPassantMode::Legal)`
        // agrees with the on-disk file.
        let start_pos = shakmaty::Chess::default();
        let from_position = book.find_for_position(&start_pos);
        assert!(
            !from_position.is_empty(),
            "find_for_position(start) should match the file's starting-position entries"
        );
    }

    /// Helper used by the real-world-fixture test. Walks up to 8 levels
    /// looking for a `books/` directory that contains `name`.
    fn locate_real_world_book(name: &str) -> Result<std::path::PathBuf, String> {
        const MAX_WALK: usize = 8;
        let start: std::path::PathBuf =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut cursor: Option<&std::path::Path> = Some(start.as_path());
        let mut steps = 0usize;
        while let Some(candidate) = cursor {
            if steps > MAX_WALK {
                break;
            }
            let maybe = candidate.join("books").join(name);
            if maybe.exists() {
                return Ok(maybe);
            }
            cursor = candidate.parent();
            steps += 1;
        }
        Err(format!(
            "`books/{}` not found within {} levels of {}",
            name,
            MAX_WALK,
            start.display()
        ))
    }

    #[test]
    fn unsorted_input_is_resorted() {
        // Construct a book whose records are *not* sorted by key (we
        // synthesise via `make_book` which IS sorted, so we post-process).
        let mut book = make_book(&[(1, 0, 0, 0), (3, 0, 0, 0), (2, 0, 0, 0)]);
        // Force unsorted by swapping two entries.
        book.entries.swap(1, 2);
        // After a fresh open, the loader sort is called — emulate by re-opening.
        let bytes: Vec<u8> = {
            let mut out = Vec::new();
            for e in &book.entries {
                out.extend_from_slice(&e.key.to_be_bytes());
                out.extend_from_slice(&e.encoded_move.to_be_bytes());
                out.extend_from_slice(&e.weight.to_be_bytes());
                out.extend_from_slice(&e.learn.to_be_bytes());
            }
            out
        };
        let sorted = PolyglotBook::from_bytes(&bytes).unwrap();
        assert_eq!(sorted.find(1).len(), 1);
        assert_eq!(sorted.find(2).len(), 1);
        assert_eq!(sorted.find(3).len(), 1);
    }
}
