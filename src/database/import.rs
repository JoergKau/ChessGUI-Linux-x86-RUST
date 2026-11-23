//! PGN import: source file → `.sbd` rows.
//!
//! Published collections are stored **verbatim**: the importer splits the file
//! into games with [`PgnStream`], reads the headers, counts the mainline moves
//! and writes the original PGN text plus a dedupe fingerprint. The one change
//! to the text is encoding repair: bytes that are not valid UTF-8 (legacy
//! Windows-1252 annotations such as `½-½`) are transcoded, so the stored game
//! opens in a strict UTF-8 reader instead of being rejected. Moves are not
//! validated by default — a game collection that is already in circulation has
//! been through a real engine, and re-validating every move is 20–50× slower
//! (the same conclusion the reference implementations reached). A game that is
//! broken is therefore stored too and reported when the user opens it; the
//! *Validate moves* option turns the opposite behaviour on for hand-kept files.
//!
//! Everything here runs inside a blocking task, one connection per job, and
//! posts progress through the closure the caller passes in. Cancelling is
//! cooperative: the open batch is committed and the games up to that point are
//! kept.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use rusqlite::Connection;

use crate::chess::game_state::GameState;
use crate::chess::pgn::{self, MovetextScan, PgnResult};

use super::dedupe::{fingerprint, FingerprintFields};
use super::model::{ImportOptions, ImportSummary, JobProgress};
use super::pgn_stream::PgnStream;
use super::store::{self, DbError};

/// Games between progress callbacks (and cancel checks).
const PROGRESS_EVERY: u64 = 100;
/// Games per transaction. A crash or a cancel loses at most one batch.
const COMMIT_EVERY: u64 = 1_000;
/// How many skip reasons the summary keeps for the dialog.
const MAX_REASONS: usize = 5;
/// Position rows per multi-row INSERT. 300 rows = 900 bound values, under the
/// oldest SQLite variable limit (999), and still one statement for a normal
/// game.
const ROWS_PER_INSERT: usize = 300;

/// Import a PGN file into an existing database.
///
/// The database must already exist ([`store::create`]); this writes game rows
/// into it, so the same function serves "new database from PGN", "add games"
/// and "re-import from source".
pub fn import_pgn(
    db_path: &Path,
    source: &Path,
    options: &ImportOptions,
    progress: &mut dyn FnMut(JobProgress),
    cancel: &AtomicBool,
) -> Result<ImportSummary, DbError> {
    let total_bytes = std::fs::metadata(source)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let file = File::open(source).map_err(|source_err| DbError::Io {
        path: source.to_path_buf(),
        source: source_err,
    })?;
    let reader = BufReader::with_capacity(1 << 20, file);
    let mut stream = PgnStream::new(reader);

    let mut conn = store::connection(db_path)?;
    // A bulk import is one long write: fsync per commit only costs time and
    // the source file can always be imported again. WAL stays on, so a crash
    // leaves the database readable up to the last commit.
    conn.execute_batch("PRAGMA synchronous = OFF;")?;
    // Duplicates *inside* the source file are kept; only games that were
    // already in the database before this import are skipped. The housekeeping
    // action can then show and remove the source's own duplicates.
    allow_duplicate_fingerprints(&conn)?;
    let dedupe_before = dedupe_baseline(&conn)?;
    let mut cache = NameCache::default();
    let started = Instant::now();
    let mut summary = ImportSummary::default();
    let mut batch: u64 = 0;
    let mut tx = conn.transaction()?;

    while let Some(game) = stream.next() {
        let text = game.map_err(|source_err| DbError::Io {
            path: source.to_path_buf(),
            source: source_err,
        })?;
        match insert_game(&tx, &mut cache, &text, options, dedupe_before)? {
            Outcome::Imported => summary.imported += 1,
            Outcome::Duplicate => summary.duplicates += 1,
            Outcome::Skipped(reason) => {
                summary.skipped += 1;
                if summary.reasons.len() < MAX_REASONS {
                    summary.reasons.push(reason);
                }
            }
        }
        batch += 1;
        if batch.is_multiple_of(PROGRESS_EVERY) {
            progress(JobProgress {
                games: summary.imported + summary.duplicates + summary.skipped,
                total_games: None,
                bytes: stream.bytes_read(),
                total_bytes,
            });
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
        }
        if batch.is_multiple_of(COMMIT_EVERY) {
            tx.commit()?;
            tx = conn.transaction()?;
        }
    }
    tx.commit()?;

    summary.elapsed = started.elapsed();
    finish(&conn, summary)
}

/// Scid `.si4`/`.si5` import, behind the `scid` Cargo feature.
#[cfg(feature = "scid")]
pub fn import_scid(
    db_path: &Path,
    source: &Path,
    options: &ImportOptions,
    progress: &mut dyn FnMut(JobProgress),
    cancel: &AtomicBool,
) -> Result<ImportSummary, DbError> {
    use super::scid;

    let format = scid::detect(source).ok_or_else(|| DbError::NotADatabase {
        path: source.to_path_buf(),
    })?;
    let total_bytes = std::fs::metadata(source)
        .map(|meta| meta.len())
        .unwrap_or(0);
    let total_games = scid::game_count(source, format)
        .map(|count| count as u64)
        .ok();

    let conn = store::connection(db_path)?;
    conn.execute_batch("PRAGMA synchronous = OFF;")?;
    allow_duplicate_fingerprints(&conn)?;
    let dedupe_before = dedupe_baseline(&conn)?;
    let mut cache = NameCache::default();
    let started = Instant::now();
    let mut summary = ImportSummary::default();
    let mut batch: u64 = 0;

    // A manual BEGIN/COMMIT (rather than `Connection::transaction`) lets the
    // per-game callback commit its own batch: the closure is called many times
    // while it runs, so it cannot hold a `Transaction` borrow across calls.
    conn.execute_batch("BEGIN")?;
    let outcome = scid::for_each_game(source, format, cancel, |_index, game| {
        match insert_game(&conn, &mut cache, game, options, dedupe_before)? {
            Outcome::Imported => summary.imported += 1,
            Outcome::Duplicate => summary.duplicates += 1,
            Outcome::Skipped(reason) => {
                summary.skipped += 1;
                if summary.reasons.len() < MAX_REASONS {
                    summary.reasons.push(reason);
                }
            }
        }
        batch += 1;
        if batch.is_multiple_of(PROGRESS_EVERY) {
            progress(JobProgress {
                games: summary.imported + summary.duplicates + summary.skipped,
                total_games,
                bytes: 0,
                total_bytes,
            });
        }
        if batch.is_multiple_of(COMMIT_EVERY) {
            conn.execute_batch("COMMIT; BEGIN")?;
        }
        Ok(())
    });
    conn.execute_batch("COMMIT")?;

    match outcome {
        Ok(cancelled) => summary.cancelled = cancelled,
        Err(err) => {
            summary.reasons.push(err.to_string());
        }
    }
    summary.elapsed = started.elapsed();
    finish(&conn, summary)
}

/// Write the stored game count, tidy the file and hand back the summary.
fn finish(conn: &Connection, summary: ImportSummary) -> Result<ImportSummary, DbError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))?;
    store::set_info(conn, "game_count", &total.to_string())?;
    store::set_info(conn, "last_import_at", &store::unix_now().to_string())?;
    conn.execute_batch("PRAGMA optimize;")?;
    log::info!(
        "database import: {} imported, {} duplicates, {} skipped in {:.1}s",
        summary.imported,
        summary.duplicates,
        summary.skipped,
        summary.elapsed.as_secs_f64()
    );
    Ok(summary)
}

/// What one game did.
enum Outcome {
    /// A new row.
    Imported,
    /// The fingerprint was already present.
    Duplicate,
    /// The game could not be stored; the string is the reason.
    Skipped(String),
}

/// Interned names, so a big import does not resolve the same player or event
/// through SQL again and again.
#[derive(Default)]
struct NameCache {
    players: HashMap<String, i64>,
    events: HashMap<String, i64>,
    sites: HashMap<String, i64>,
}

/// Highest game id before an import starts.
///
/// The dedupe check only looks at rows at or below this id. A source file's own
/// duplicates are therefore stored (and can be inspected or removed with the
/// housekeeping action), while importing the same file again still adds
/// nothing: every game of the second run matches a row that existed before it.
fn dedupe_baseline(conn: &Connection) -> Result<i64, DbError> {
    Ok(
        conn.query_row("SELECT COALESCE(MAX(id), 0) FROM games", [], |row| {
            row.get(0)
        })?,
    )
}

/// Older databases declared the dedupe index `UNIQUE`, which made the second
/// copy of a game inside one source file unimportable. Replace it with a plain
/// index the first time such a file is imported; the baseline check above still
/// keeps a repeated import a no-op. A new database gets the plain index from
/// the schema directly.
fn allow_duplicate_fingerprints(conn: &Connection) -> Result<(), DbError> {
    let unique: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_index_list('games')
         WHERE name = 'games_dedupe' AND \"unique\" = 1",
        [],
        |row| row.get(0),
    )?;
    if unique > 0 {
        conn.execute_batch(
            "DROP INDEX games_dedupe;
             CREATE INDEX games_dedupe ON games(dedupe);",
        )?;
        log::info!("database import: dedupe index is no longer unique");
    }
    Ok(())
}

/// Store one game.
fn insert_game(
    conn: &Connection,
    cache: &mut NameCache,
    text: &str,
    options: &ImportOptions,
    dedupe_before: i64,
) -> Result<Outcome, DbError> {
    let (tags, movetext) = pgn::scan_game_text(text);
    let tag = |name: &str| {
        tags.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty() && *value != "?")
    };

    let scan = pgn::scan_movetext(&movetext);
    if scan.plies == 0 && tags.is_empty() {
        // A blank block between games is not a game. A header-only game *is*
        // stored with zero plies, so the overview's Remove Empty Games can
        // find and delete it (the importer used to drop it silently).
        return Ok(Outcome::Skipped("empty".to_string()));
    }
    let mut parsed: Option<pgn::PgnImport> = None;
    if options.validate_moves || options.index_positions {
        // Both need a parsed game; a bad one is reported instead of stored
        // when the caller asked for validation, and skipped either way when
        // the position index was requested. The parsed game is kept so the
        // index does not have to parse the same text a second time.
        match pgn::parse_pgn_with_report(text) {
            Ok(import) => parsed = Some(import),
            Err(err) => {
                if options.validate_moves {
                    return Ok(Outcome::Skipped(err.to_string()));
                }
            }
        }
    }

    let white_name = tag("White").unwrap_or("?");
    let black_name = tag("Black").unwrap_or("?");
    let white_norm = normalize_player(white_name);
    let black_norm = normalize_player(black_name);
    let date_key = tag("Date").map_or(0, parse_date_key);
    let result = tag("Result")
        .and_then(PgnResult::parse)
        .or_else(|| movetext.split_whitespace().rev().find_map(PgnResult::parse))
        .map_or(3, result_code);
    let fields = FingerprintFields {
        white_norm: white_norm.clone(),
        black_norm: black_norm.clone(),
        date_key,
        result,
        ply_count: scan.plies as i64,
        first_moves: scan.first_moves.clone(),
        last_moves: scan.last_moves.clone(),
    };
    let fingerprint = fingerprint(&fields);

    // Dedupe before doing the rest of the work: a large re-import is mostly
    // hits, and each hit should cost one indexed lookup. Only rows that were
    // there before this import count, so a game repeated inside the source is
    // stored both times.
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM games WHERE dedupe = ?1 AND id <= ?2)",
        rusqlite::params![&fingerprint[..], dedupe_before],
        |row| row.get(0),
    )?;
    if exists {
        return Ok(Outcome::Duplicate);
    }

    let white_id = intern(conn, cache, NameKind::Player, white_name, &white_norm)?;
    let black_id = intern(conn, cache, NameKind::Player, black_name, &black_norm)?;
    let event_id = tag("Event")
        .map(|name| intern(conn, cache, NameKind::Event, name, name))
        .transpose()?;
    let site_id = tag("Site")
        .map(|name| intern(conn, cache, NameKind::Site, name, name))
        .transpose()?;

    let flags = flags(&scan, tag("FEN").is_some());
    let mut insert = conn.prepare_cached(
        "INSERT INTO games(
            white_id, black_id, event_id, site_id, round, date_key, result,
            white_elo, black_elo, white_title, black_title, eco, ply_count,
            start_fen, termination, time_control, flags, pgn, dedupe, added_at
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                  ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
    )?;
    insert.execute(rusqlite::params![
        white_id,
        black_id,
        event_id,
        site_id,
        tag("Round"),
        date_key,
        result,
        tag("WhiteElo").and_then(parse_elo),
        tag("BlackElo").and_then(parse_elo),
        tag("WhiteTitle"),
        tag("BlackTitle"),
        tag("ECO").filter(|eco| eco.len() == 3),
        scan.plies as i64,
        tag("FEN"),
        tag("Termination"),
        tag("TimeControl"),
        flags,
        text.as_bytes(),
        &fingerprint[..],
        store::unix_now(),
    ])?;
    let id = conn.last_insert_rowid();

    if options.index_positions {
        if let Some(import) = parsed.as_mut() {
            index_positions(conn, id, &mut import.game)?;
        }
    }
    Ok(Outcome::Imported)
}

/// Which name table [`intern`] works on.
enum NameKind {
    Player,
    Event,
    Site,
}

/// Resolve a name to its row id, inserting it when new.
fn intern(
    conn: &Connection,
    cache: &mut NameCache,
    kind: NameKind,
    name: &str,
    norm: &str,
) -> Result<i64, DbError> {
    let key = match kind {
        NameKind::Player => norm,
        NameKind::Event | NameKind::Site => name,
    };
    let table = match kind {
        NameKind::Player => &mut cache.players,
        NameKind::Event => &mut cache.events,
        NameKind::Site => &mut cache.sites,
    };
    if let Some(id) = table.get(key) {
        return Ok(*id);
    }
    let id = match kind {
        NameKind::Player => {
            conn.execute(
                "INSERT INTO players(name, norm) VALUES(?1, ?2)
                 ON CONFLICT(norm) DO NOTHING",
                rusqlite::params![name, norm],
            )?;
            conn.query_row("SELECT id FROM players WHERE norm = ?1", [norm], |row| {
                row.get(0)
            })?
        }
        NameKind::Event => {
            conn.execute(
                "INSERT INTO events(name) VALUES(?1) ON CONFLICT(name) DO NOTHING",
                [name],
            )?;
            conn.query_row("SELECT id FROM events WHERE name = ?1", [name], |row| {
                row.get(0)
            })?
        }
        NameKind::Site => {
            conn.execute(
                "INSERT INTO sites(name) VALUES(?1) ON CONFLICT(name) DO NOTHING",
                [name],
            )?;
            conn.query_row("SELECT id FROM sites WHERE name = ?1", [name], |row| {
                row.get(0)
            })?
        }
    };
    table.insert(key.to_string(), id);
    Ok(id)
}

/// Write the Polyglot key of every position of one game.
///
/// Indexing the whole game (not just the opening) is what lets the Position
/// filter find a FEN taken from any point of a game, including the final
/// position. The cost is one row per half-move; a long game has ~100 of them.
/// The rows of one game are written in a single multi-row statement, because
/// a per-row round trip through SQLite was the dominant cost of an import.
fn index_positions(conn: &Connection, id: i64, game: &mut GameState) -> Result<(), DbError> {
    let plies = game.total_plies();
    let mut values: Vec<i64> = Vec::with_capacity((plies + 1) * 3);
    for ply in 0..=plies {
        game.goto_ply(ply);
        values.push(game.zobrist() as i64);
        values.push(id);
        values.push(ply as i64);
    }
    for chunk in values.chunks(ROWS_PER_INSERT * 3) {
        let rows = chunk.len() / 3;
        let mut sql = String::with_capacity(48 + rows * 18);
        sql.push_str("INSERT OR IGNORE INTO positions(hash, game_id, ply) VALUES");
        for row in 0..rows {
            if row > 0 {
                sql.push(',');
            }
            let base = row * 3 + 1;
            let _ = std::fmt::Write::write_fmt(
                &mut sql,
                format_args!("(?{}, ?{}, ?{})", base, base + 1, base + 2),
            );
        }
        let mut statement = conn.prepare_cached(&sql)?;
        statement.execute(rusqlite::params_from_iter(chunk.iter()))?;
    }
    Ok(())
}

/// Rebuild the position index of every game in an existing database.
///
/// Databases imported before the index covered whole games keep the old,
/// opening-only entries; this job replaces them. Returns the number of games
/// indexed. Cancelling keeps whatever was written so far.
pub fn rebuild_position_index(
    db_path: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<u64, DbError> {
    let conn = store::connection(db_path)?;
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM games", [], |row| row.get(0))?;
    let total = total.max(0) as u64;
    let mut done = 0u64;
    let mut statement = conn.prepare("SELECT id, pgn FROM games ORDER BY id")?;
    let mut rows = statement.query([])?;
    conn.execute_batch("BEGIN")?;
    while let Some(row) = rows.next()? {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let id: i64 = row.get(0)?;
        let pgn: Vec<u8> = row.get(1)?;
        if let Ok(text) = String::from_utf8(pgn) {
            if let Ok(import) = pgn::parse_pgn_with_report(&text) {
                let mut game = import.game;
                conn.execute("DELETE FROM positions WHERE game_id = ?1", [id])?;
                index_positions(&conn, id, &mut game)?;
            }
        }
        done += 1;
        if done.is_multiple_of(PROGRESS_EVERY) {
            progress(done, total);
        }
        if done.is_multiple_of(COMMIT_EVERY) {
            conn.execute_batch("COMMIT; BEGIN")?;
        }
    }
    conn.execute_batch("COMMIT")?;
    conn.execute_batch("PRAGMA optimize;")?;
    progress(done, total);
    Ok(done)
}

/// Bit set stored in `games.flags`.
fn flags(scan: &MovetextScan, from_fen: bool) -> i64 {
    let mut flags = 0;
    if scan.variations > 0 {
        flags |= 2;
    }
    if scan.comments {
        flags |= 4;
    }
    if scan.nags {
        flags |= 8;
    }
    if from_fen {
        flags |= 16;
    }
    flags
}

/// `games.result` code from a PGN result.
fn result_code(result: PgnResult) -> i64 {
    match result {
        PgnResult::WhiteWin => 0,
        PgnResult::BlackWin => 1,
        PgnResult::Draw => 2,
    }
}

/// `YYYY.MM.DD` → `yyyymmdd`, unknown month/day as `01`, unknown year as `0`.
///
/// Filling the unknown parts with `01` keeps a year search inclusive: a game
/// dated `2024.??.??` matches `2024` instead of falling outside the range.
pub fn parse_date_key(text: &str) -> i64 {
    let mut parts = text.trim().split('.');
    let year = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .filter(|year| (1000..=9999).contains(year));
    let Some(year) = year else {
        return 0;
    };
    let month = parts
        .next()
        .and_then(|part| part.parse::<i32>().ok())
        .filter(|month| (1..=12).contains(month))
        .unwrap_or(1);
    let day = parts
        .next()
        .and_then(|part| part.parse::<i32>().ok())
        .filter(|day| (1..=31).contains(day))
        .unwrap_or(1);
    year * 10_000 + i64::from(month) * 100 + i64::from(day)
}

/// Parse a PGN rating; `?`, `-` and anything non-numeric is "unrated".
pub fn parse_elo(text: &str) -> Option<i64> {
    let text = text.trim();
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    digits.parse::<i64>().ok().filter(|elo| *elo > 0)
}

/// Fold a player name into the searchable form stored in `players.norm`.
///
/// Lowercase, common diacritics folded to ASCII, everything that is not a
/// letter or digit turned into a space, and the remaining tokens sorted — so
/// `Carlsen, Magnus`, `Magnus Carlsen` and `CARLSEN M.` all contain
/// `carlsen` and `magnus`.
pub fn normalize_player(name: &str) -> String {
    let mut folded = String::with_capacity(name.len());
    for ch in name.chars() {
        let mapped = match ch.to_ascii_lowercase() {
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            'š' => 's',
            'ž' => 'z',
            'ý' => 'y',
            'ł' => 'l',
            'đ' => 'd',
            'ß' => 's',
            other => other,
        };
        if mapped.is_ascii_alphanumeric() {
            folded.push(mapped);
        } else {
            folded.push(' ');
        }
    }
    let mut tokens: Vec<&str> = folded.split_whitespace().collect();
    tokens.sort_unstable();
    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::model::ImportOptions;

    #[test]
    fn dates_fill_unknown_parts_so_a_year_search_matches() {
        assert_eq!(parse_date_key("2024.05.12"), 20_240_512);
        assert_eq!(parse_date_key("2024.??.??"), 20_240_101);
        assert_eq!(parse_date_key("????.??.??"), 0);
        assert_eq!(parse_date_key(""), 0);
    }

    #[test]
    fn ratings_are_optional_and_ignore_junk() {
        assert_eq!(parse_elo("2750"), Some(2750));
        assert_eq!(parse_elo("?"), None);
        assert_eq!(parse_elo(""), None);
        assert_eq!(parse_elo("-"), None);
        assert_eq!(parse_elo("123456"), None);
    }

    #[test]
    fn player_names_fold_and_sort() {
        assert_eq!(normalize_player("Carlsen, Magnus"), "carlsen magnus");
        assert_eq!(normalize_player("Magnus CARLSEN"), "carlsen magnus");
        assert_eq!(normalize_player("Polgár, Judit"), "judit polgar");
        assert_eq!(
            normalize_player("Nepomniachtchi, Ian"),
            "ian nepomniachtchi"
        );
        assert_eq!(normalize_player(""), "");
    }

    #[test]
    fn a_game_is_stored_with_its_metadata_and_a_repeat_is_a_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.sbd");
        super::super::store::create(&db, "T", "").unwrap();
        let mut conn = super::super::store::connection(&db).unwrap();
        let mut cache = NameCache::default();
        let text = "[Event \"World Championship\"]\n[Site \"Reykjavik\"]\n[Date \"1972.07.11\"]\n[White \"Spassky, Boris V\"]\n[Black \"Fischer, Robert James\"]\n[Result \"0-1\"]\n[WhiteElo \"2660\"]\n[BlackElo \"2785\"]\n[ECO \"D59\"]\n\n1. d4 Nf6 2. c4 e6 0-1\n";
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            insert_game(&tx, &mut cache, text, &ImportOptions::default(), 0).unwrap(),
            Outcome::Imported
        ));
        tx.commit().unwrap();
        // A later import compares against the rows that were already there, so
        // the same game is a duplicate even though it was inserted by hand.
        let tx = conn.transaction().unwrap();
        assert!(matches!(
            insert_game(&tx, &mut cache, text, &ImportOptions::default(), 1).unwrap(),
            Outcome::Duplicate
        ));
        tx.commit().unwrap();

        let (white, black, date, result, eco): (String, String, i64, i64, String) = conn
            .query_row(
                "SELECT w.name, b.name, g.date_key, g.result, g.eco
                 FROM games g JOIN players w ON w.id = g.white_id
                 JOIN players b ON b.id = g.black_id",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(white, "Spassky, Boris V");
        assert_eq!(black, "Fischer, Robert James");
        assert_eq!(date, 19_720_711);
        assert_eq!(result, 1);
        assert_eq!(eco, "D59");
    }

    #[test]
    fn a_header_only_game_is_stored_so_it_can_be_removed_later() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.sbd");
        let pgn = dir.path().join("games.pgn");
        std::fs::write(
            &pgn,
            "[Event \"Empty\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"*\"]\n\n\
             [Event \"Real\"]\n[White \"C\"]\n[Black \"D\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n",
        )
        .unwrap();
        super::super::store::create(&db, "T", "").unwrap();
        let cancel = AtomicBool::new(false);
        let summary =
            import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(summary.imported, 2, "the header-only game is stored too");
        let conn = super::super::store::connection(&db).unwrap();
        assert_eq!(
            super::super::query::remove_empty_games(&conn, &mut |_, _| {}, &AtomicBool::new(false))
                .unwrap()
                .0,
            1
        );
        assert_eq!(super::super::stats(&db).unwrap().games, 1);
    }

    #[test]
    fn wrapped_comment_annotations_do_not_affect_empty_game_cleanup() {
        // Regression for BUG-8-10 through the database path: the comment
        // continues on a line starting with `[%eval …]`. It must stay one
        // game, keep its move count, and leave only the header-only game for
        // Remove Empty Games to delete.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.sbd");
        let pgn = dir.path().join("games.pgn");
        std::fs::write(
            &pgn,
            "[Event \"One\"]\n[White \"A\"]\n[Black \"B\"]\n[Result \"1-0\"]\n\n\
             1. e4 {+0.20/30\n[%eval #5,37] 11s} e5 2. Nf3 Nc6 1-0\n\n\
             [Event \"Empty\"]\n[White \"C\"]\n[Black \"D\"]\n[Result \"*\"]\n\n",
        )
        .unwrap();
        super::super::store::create(&db, "T", "").unwrap();
        let cancel = AtomicBool::new(false);
        let summary =
            import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(
            summary.imported, 2,
            "one real game and one header-only game"
        );
        assert_eq!(
            summary.skipped, 0,
            "the comment continuation must not become a phantom game"
        );
        let conn = super::super::store::connection(&db).unwrap();
        let (count, longest): (i64, i64) = conn
            .query_row("SELECT COUNT(*), MAX(ply_count) FROM games", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(
            longest, 4,
            "the wrapped annotation must not change the move count"
        );
        assert_eq!(
            super::super::query::remove_empty_games(&conn, &mut |_, _| {}, &AtomicBool::new(false))
                .unwrap()
                .0,
            1
        );
        assert_eq!(super::super::stats(&db).unwrap().games, 1);
    }

    #[test]
    fn a_repeated_game_with_wrapped_annotations_is_removed_once_and_saved() {
        // Remove Duplicates must see the wrapped comment as part of the game,
        // not as an extra game or as a reason to split the move count.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.sbd");
        let pgn = dir.path().join("games.pgn");
        let game = "[Event \"A\"]\n[White \"One\"]\n[Black \"Two\"]\n[Result \"1-0\"]\n\n\
                    1. e4 {+0.2/30\n[%eval #5,37] 11s} e5 1-0\n\n";
        std::fs::write(&pgn, format!("{game}{game}")).unwrap();
        super::super::store::create(&db, "T", "").unwrap();
        let cancel = AtomicBool::new(false);
        let summary =
            import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(summary.imported, 2, "both copies are kept for cleanup");
        assert_eq!(summary.skipped, 0);

        let removal =
            super::super::remove_duplicates(&db, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(removal.removed, 1);
        let saved = std::fs::read_to_string(removal.saved.expect("removed copy is saved")).unwrap();
        assert!(saved.contains("[%eval #5,37]"), "saved PGN: {saved}");
        assert!(saved.contains("1. e4"), "saved PGN: {saved}");
        assert_eq!(super::super::stats(&db).unwrap().games, 1);
    }

    #[test]
    fn a_game_repeated_inside_the_source_is_stored_both_times() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.sbd");
        let pgn = dir.path().join("games.pgn");
        // The same game twice, exactly as a user doubling a file produces.
        let game =
            "[Event \"A\"]\n[White \"One\"]\n[Black \"Two\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n\n";
        std::fs::write(&pgn, format!("{game}{game}")).unwrap();
        super::super::store::create(&db, "T", "").unwrap();
        let cancel = AtomicBool::new(false);
        let first = import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(first.imported, 2, "both copies are kept for cleanup");
        assert_eq!(first.duplicates, 0);
        let second =
            import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(second.imported, 0, "a repeat import still adds nothing");
        assert_eq!(second.duplicates, 2);
        assert_eq!(super::super::stats(&db).unwrap().games, 2);

        // And the housekeeping action finds the second copy and saves it.
        let removal =
            super::super::remove_duplicates(&db, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(removal.removed, 1);
        let saved = removal.saved.expect("the removed game is saved");
        assert!(std::fs::read_to_string(saved).unwrap().contains("1. e4 e5"));
        assert_eq!(super::super::stats(&db).unwrap().games, 1);
    }

    #[test]
    fn importing_a_two_game_file_counts_both_and_a_second_run_adds_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("test.sbd");
        let pgn = dir.path().join("games.pgn");
        std::fs::write(
            &pgn,
            "[Event \"A\"]\n[White \"One\"]\n[Black \"Two\"]\n[Result \"1-0\"]\n\n1. e4 e5 1-0\n\n[Event \"B\"]\n[White \"Three\"]\n[Black \"Four\"]\n[Result \"0-1\"]\n\n1. d4 d5 0-1\n",
        )
        .unwrap();
        super::super::store::create(&db, "T", "").unwrap();
        let cancel = AtomicBool::new(false);
        let first = import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(first.imported, 2);
        let second =
            import_pgn(&db, &pgn, &ImportOptions::default(), &mut |_| {}, &cancel).unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.duplicates, 2);
    }
}
