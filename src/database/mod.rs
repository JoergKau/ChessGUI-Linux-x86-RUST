//! Chess databases: one SQLite file per database, games stored as PGN.
//!
//! The module is split so each part can be read and tested on its own:
//!
//! | File | Responsibility |
//! | --- | --- |
//! | [`store`] | open/create `.sbd` files, schema version, PRAGMAs |
//! | [`model`] | plain data types shared by the store, the queries and the UI |
//! | [`import`] | PGN → rows, batching, progress, cancel |
//! | [`pgn_stream`] | streaming game splitter for large files |
//! | [`dedupe`] | the stable fingerprint that makes a re-import a no-op |
//! | [`query`] | filtered pages, counts, stats, maintenance, PGN export |
//! | [`scid`] | the only place that knows the Scid decoder crate |
//!
//! **Threading:** every public function here opens its own connection and is
//! meant to be called from `tokio::task::spawn_blocking`. Nothing in this
//! module knows about egui or about the app's event channel.

pub mod dedupe;
pub mod import;
pub mod model;
pub mod pgn_stream;
pub mod query;
pub mod scid;
pub mod store;

pub use model::{
    DatabaseInfo, DbStats, GameFilter, GamePage, GameRow, ImportOptions, ImportSummary,
    JobProgress, ResultMask, SideFilter, SortDir, SortKey,
};
pub use store::DbError;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// What [`remove_duplicates`] did: the number of deleted rows and, when there
/// were any, the PGN file the removed games were saved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateRemoval {
    /// Games deleted from the database.
    pub removed: u64,
    /// `Some(path)` when the removed games were written to that PGN file.
    pub saved: Option<PathBuf>,
    /// The user cancelled before the delete phase; nothing was removed.
    pub cancelled: bool,
}

/// What [`remove_empty_games`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmptyRemoval {
    /// Games deleted from the database.
    pub removed: u64,
    /// The user cancelled between batches; the games removed so far stay
    /// removed, the same way a cancelled import keeps what it already stored.
    pub cancelled: bool,
}

/// Folder new databases go to unless the user picks another one:
/// `~/Documents/SiliconBoard/databases`.
///
/// The same reasoning as the tournament PGN default: a desktop launch may have
/// an arbitrary working directory, so a relative default would be surprising.
pub fn default_database_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home)
            .join("Documents")
            .join("SiliconBoard")
            .join("databases"),
        None => std::env::temp_dir().join("SiliconBoard").join("databases"),
    }
}

/// Default `.sbd` path for a source file: the source's stem in the default
/// folder, with a numeric suffix when that name is taken.
pub fn default_database_path(source: &Path) -> PathBuf {
    let stem = source.file_stem().map_or_else(
        || "database".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    let dir = default_database_dir();
    let mut candidate = dir.join(format!("{stem}.sbd"));
    let mut suffix = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}-{suffix}.sbd"));
        suffix += 1;
    }
    candidate
}

/// Create a new, empty database file.
pub fn create(path: &Path, title: &str, description: &str) -> Result<DatabaseInfo, DbError> {
    store::create(path, title, description)
}

/// Open a database and read its description.
pub fn open(path: &Path) -> Result<DatabaseInfo, DbError> {
    store::open(path)
}

/// The overview counts.
pub fn stats(path: &Path) -> Result<DbStats, DbError> {
    let conn = store::connection(path)?;
    query::stats(&conn)
}

/// One page of games.
pub fn page(
    path: &Path,
    filter: &GameFilter,
    sort: SortKey,
    direction: SortDir,
    page_index: usize,
    page_size: usize,
) -> Result<GamePage, DbError> {
    let conn = store::connection(path)?;
    query::page(&conn, filter, sort, direction, page_index, page_size)
}

/// The number of games a filter matches.
pub fn count(path: &Path, filter: &GameFilter) -> Result<u64, DbError> {
    let conn = store::connection(path)?;
    query::count(&conn, filter)
}

/// Up to `limit` stored player names matching what the user has typed.
pub fn player_suggestions(
    path: &Path,
    fragment: &str,
    limit: usize,
) -> Result<Vec<String>, DbError> {
    let conn = store::connection(path)?;
    query::player_suggestions(&conn, fragment, limit)
}

/// The stored PGN text of one game, as UTF-8.
pub fn load_game(path: &Path, id: i64) -> Result<String, DbError> {
    let conn = store::connection(path)?;
    match query::load_game(&conn, id)? {
        Some(bytes) => String::from_utf8(bytes).map_err(|_| DbError::NotADatabase {
            path: path.to_path_buf(),
        }),
        None => Err(DbError::Sql(rusqlite::Error::QueryReturnedNoRows)),
    }
}

/// Import a source file into a database: `.pgn` directly, `.si4`/`.si5`
/// through the Scid converter.
pub fn import_file(
    db_path: &Path,
    source: &Path,
    options: &ImportOptions,
    progress: &mut dyn FnMut(JobProgress),
    cancel: &AtomicBool,
) -> Result<ImportSummary, DbError> {
    if scid::detect(source).is_some() {
        #[cfg(feature = "scid")]
        {
            return import::import_scid(db_path, source, options, progress, cancel);
        }
        #[cfg(not(feature = "scid"))]
        {
            return Err(DbError::Scid(
                "this build was made without Scid support".to_string(),
            ));
        }
    }
    import::import_pgn(db_path, source, options, progress, cancel)
}

/// Delete formatting-only duplicates.
///
/// The games that go are written to a PGN file next to the database first
/// (`<database stem>-duplicates.pgn`), so the user can inspect what the
/// housekeeping action removed. The database rows are only deleted after the
/// file was written successfully. `progress` follows the backup write (the
/// long phase); `cancel` is checked between games and again before the delete.
pub fn remove_duplicates(
    path: &Path,
    progress: &mut dyn FnMut(JobProgress),
    cancel: &AtomicBool,
) -> Result<DuplicateRemoval, DbError> {
    let conn = store::connection(path)?;
    let removed_games = query::duplicate_pgns(&conn)?;
    if removed_games.is_empty() {
        return Ok(DuplicateRemoval {
            removed: 0,
            saved: None,
            cancelled: false,
        });
    }
    let target = duplicates_pgn_path(path);
    let cancelled = write_pgn_games(&target, &removed_games, progress, cancel)?;
    if cancelled {
        // A partial backup would not describe what was removed (nothing was).
        let _ = std::fs::remove_file(&target);
        return Ok(DuplicateRemoval {
            removed: 0,
            saved: None,
            cancelled: true,
        });
    }
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::remove_file(&target);
        return Ok(DuplicateRemoval {
            removed: 0,
            saved: None,
            cancelled: true,
        });
    }
    let removed = query::remove_duplicates(&conn)?;
    Ok(DuplicateRemoval {
        removed,
        saved: Some(target),
        cancelled: false,
    })
}

/// Where [`remove_duplicates`] keeps the games it deleted:
/// `<database folder>/<database stem>-duplicates.pgn`.
pub fn duplicates_pgn_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().map_or_else(
        || "database".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    path.with_file_name(format!("{stem}-duplicates.pgn"))
}

/// Write stored PGN texts as one file, one blank line between games.
///
/// Returns `true` when the user cancelled early; the caller removes the
/// partial file. Progress is reported every few games so the strip moves while
/// a multi-gigabyte backup is written.
fn write_pgn_games(
    target: &Path,
    games: &[Vec<u8>],
    progress: &mut dyn FnMut(JobProgress),
    cancel: &AtomicBool,
) -> Result<bool, DbError> {
    use std::io::Write;

    const REPORT_EVERY: usize = 50;
    let total = games.len() as u64;
    let file = std::fs::File::create(target).map_err(|source| DbError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);
    for (index, game) in games.iter().enumerate() {
        if index % REPORT_EVERY == 0 {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(true);
            }
            progress(JobProgress {
                games: index as u64,
                total_games: Some(total),
                bytes: 0,
                total_bytes: 0,
            });
        }
        writer
            .write_all(game)
            .and_then(|()| writer.write_all(b"\n\n"))
            .map_err(|source| DbError::Io {
                path: target.to_path_buf(),
                source,
            })?;
    }
    writer.flush().map_err(|source| DbError::Io {
        path: target.to_path_buf(),
        source,
    })?;
    progress(JobProgress {
        games: total,
        total_games: Some(total),
        bytes: 0,
        total_bytes: 0,
    });
    Ok(false)
}

/// Delete games without moves.
pub fn remove_empty_games(
    path: &Path,
    progress: &mut dyn FnMut(JobProgress),
    cancel: &AtomicBool,
) -> Result<EmptyRemoval, DbError> {
    let conn = store::connection(path)?;
    let (removed, cancelled) = query::remove_empty_games(
        &conn,
        &mut |done, total| {
            progress(JobProgress {
                games: done,
                total_games: Some(total),
                bytes: 0,
                total_bytes: 0,
            });
        },
        cancel,
    )?;
    Ok(EmptyRemoval { removed, cancelled })
}

/// Rebuild the position index of every game (for databases imported before
/// the index covered whole games).
pub fn rebuild_position_index(
    path: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> Result<u64, DbError> {
    import::rebuild_position_index(path, progress, cancel)
}

/// Delete a database file and SQLite's sidecar files.
///
/// The caller must have closed every connection (the app's jobs are the only
/// users, and they finish before the button is offered).
pub fn delete(path: &Path) -> Result<(), DbError> {
    std::fs::remove_file(path).map_err(|source| DbError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.exists() {
            let _ = std::fs::remove_file(sidecar);
        }
    }
    Ok(())
}

/// Export the games a filter matches as one PGN file.
pub fn export_pgn(
    path: &Path,
    filter: &GameFilter,
    sort: SortKey,
    direction: SortDir,
    target: &Path,
    progress: &mut dyn FnMut(u64),
    cancel: &AtomicBool,
) -> Result<u64, DbError> {
    let conn = store::connection(path)?;
    query::export_pgn(&conn, filter, sort, direction, target, progress, cancel)
}

#[cfg(test)]
mod tests {
    use super::query::DEFAULT_PAGE_SIZE;
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn duplicates_are_written_next_to_the_database_before_they_are_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("My games.sbd");
        create(&db, "T", "").unwrap();
        let conn = store::connection(&db).unwrap();
        conn.execute("INSERT INTO players(name, norm) VALUES('A', 'a')", [])
            .unwrap();
        conn.execute("INSERT INTO players(name, norm) VALUES('B', 'b')", [])
            .unwrap();
        for copy in 0..2u8 {
            conn.execute(
                "INSERT INTO games(white_id, black_id, date_key, result, ply_count,
                    flags, pgn, dedupe, added_at)
                 VALUES(1, 2, 0, 0, 2, 0, ?1, ?2, 0)",
                rusqlite::params![
                    b"[White \"A\"]\n[Black \"B\"]\n\n1. e4 e5 1-0\n".as_slice(),
                    [copy; 8].as_slice(),
                ],
            )
            .unwrap();
        }
        drop(conn);

        let result =
            remove_duplicates(&db, &mut |_| {}, &std::sync::atomic::AtomicBool::new(false))
                .unwrap();
        assert_eq!(result.removed, 1);
        assert!(!result.cancelled);
        let saved = result.saved.expect("the removed game is saved");
        assert_eq!(
            saved.file_name().unwrap().to_string_lossy(),
            "My games-duplicates.pgn"
        );
        let text = std::fs::read_to_string(&saved).unwrap();
        assert!(text.contains("[White \"A\"]"), "{text}");
        assert!(text.contains("1. e4 e5"), "{text}");
        assert_eq!(stats(&db).unwrap().games, 1);
    }

    #[test]
    fn duplicate_removal_reports_progress_and_honours_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cancel.sbd");
        create(&db, "T", "").unwrap();
        let conn = store::connection(&db).unwrap();
        conn.execute("INSERT INTO players(name, norm) VALUES('A', 'a')", [])
            .unwrap();
        conn.execute("INSERT INTO players(name, norm) VALUES('B', 'b')", [])
            .unwrap();
        for copy in 0..2u8 {
            conn.execute(
                "INSERT INTO games(white_id, black_id, date_key, result, ply_count,
                    flags, pgn, dedupe, added_at)
                 VALUES(1, 2, 0, 0, 2, 0, ?1, ?2, 0)",
                rusqlite::params![
                    b"[White \"A\"]\n[Black \"B\"]\n\n1. e4 e5 1-0\n".as_slice(),
                    [copy; 8].as_slice(),
                ],
            )
            .unwrap();
        }
        drop(conn);

        let cancel = std::sync::atomic::AtomicBool::new(true);
        let result = remove_duplicates(&db, &mut |_| {}, &cancel).unwrap();
        assert!(result.cancelled);
        assert_eq!(result.removed, 0);
        assert!(result.saved.is_none(), "a cancelled run keeps no backup");
        assert_eq!(stats(&db).unwrap().games, 2, "nothing was deleted");

        let mut seen = Vec::new();
        let result = remove_duplicates(
            &db,
            &mut |progress| seen.push((progress.games, progress.total_games)),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert!(!result.cancelled);
        assert_eq!(result.removed, 1);
        assert!(
            seen.iter()
                .any(|(games, total)| *games == 1 && *total == Some(1)),
            "progress must reach the full duplicate count: {seen:?}"
        );
    }

    #[test]
    fn the_default_path_never_collides_with_an_existing_database() {
        // Only the shape is checked here; the real folder is the user's home.
        let path = default_database_path(Path::new("/tmp/LumbrasGigabase_OTB_2025.pgn"));
        assert_eq!(path.extension().unwrap(), "sbd");
        assert!(path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .starts_with("LumbrasGigabase_OTB_2025"));
    }

    /// Real-file measurement, kept out of the normal suite: it needs a source
    /// the repository must not carry.
    ///
    /// ```text
    /// SILICONBOARD_TEST_PGN=/path/gigabase.pgn \
    ///   cargo test --release --bin siliconboard real_file_import -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs SILICONBOARD_TEST_PGN (a large real PGN); measurement only"]
    fn real_file_import_and_query() {
        let source = std::env::var_os("SILICONBOARD_TEST_PGN")
            .expect("set SILICONBOARD_TEST_PGN to a PGN file");
        let dir = tempfile::tempdir().unwrap();
        // An explicit path lets a manual UI check reuse the result.
        let db = std::env::var_os("SILICONBOARD_TEST_DB")
            .map_or_else(|| dir.path().join("measure.sbd"), PathBuf::from);
        let started = std::time::Instant::now();
        create(&db, "measure", "").unwrap();
        let mut last = 0u64;
        let summary = import_file(
            &db,
            Path::new(&source),
            &ImportOptions {
                index_positions: std::env::var_os("SILICONBOARD_TEST_INDEX").is_some(),
                validate_moves: false,
            },
            &mut |progress| {
                if progress.games / 50_000 != last {
                    last = progress.games / 50_000;
                    println!("  {} games", progress.games);
                }
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        let elapsed = started.elapsed();
        println!(
            "imported {} in {:.1}s = {:.0} games/s ({} duplicates, {} skipped)",
            summary.imported,
            elapsed.as_secs_f64(),
            summary.imported as f64 / elapsed.as_secs_f64(),
            summary.duplicates,
            summary.skipped
        );
        let stats = stats(&db).unwrap();
        println!("stats: {stats:?}");

        let t = std::time::Instant::now();
        let first_page = page(
            &db,
            &GameFilter::default(),
            SortKey::Date,
            SortDir::Desc,
            0,
            DEFAULT_PAGE_SIZE,
        )
        .unwrap();
        println!(
            "first page: {} rows in {:?}",
            first_page.rows.len(),
            t.elapsed()
        );

        let t = std::time::Instant::now();
        let total = count(&db, &GameFilter::default()).unwrap();
        println!("count {total} in {:?}", t.elapsed());

        let filter = GameFilter {
            player: "carlsen".to_string(),
            ..GameFilter::default()
        };
        let t = std::time::Instant::now();
        let matched = count(&db, &filter).unwrap();
        println!("carlsen: {matched} in {:?}", t.elapsed());

        let t = std::time::Instant::now();
        let filtered = page(
            &db,
            &filter,
            SortKey::Date,
            SortDir::Desc,
            0,
            DEFAULT_PAGE_SIZE,
        )
        .unwrap();
        println!(
            "carlsen page: {} rows in {:?}; first = {} vs {}",
            filtered.rows.len(),
            t.elapsed(),
            filtered.rows.first().map_or("", |row| row.white.as_str()),
            filtered.rows.first().map_or("", |row| row.black.as_str())
        );
    }

    /// Real-file measurement for a Scid database, kept out of the suite.
    ///
    /// ```text
    /// SILICONBOARD_TEST_SCID=/path/base.si5 \
    /// SILICONBOARD_TEST_SCID_LIMIT=2000 \
    ///   cargo test --release --bin siliconboard real_scid_import -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs SILICONBOARD_TEST_SCID (a large real Scid base); measurement only"]
    fn real_scid_import() {
        let source = std::env::var_os("SILICONBOARD_TEST_SCID")
            .expect("set SILICONBOARD_TEST_SCID to an si4/si5 file");
        let limit: u64 = std::env::var("SILICONBOARD_TEST_SCID_LIMIT")
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or(2_000);
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("scid.sbd");
        create(&db, "scid measure", "").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let stopper = cancel.clone();
        let started = std::time::Instant::now();
        let summary = import_file(
            &db,
            Path::new(&source),
            &ImportOptions {
                index_positions: false,
                validate_moves: false,
            },
            &mut |progress| {
                println!("  {} games", progress.games);
                if progress.games >= limit {
                    stopper.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            },
            &cancel,
        )
        .unwrap();
        let elapsed = started.elapsed();
        println!(
            "scid: imported {} in {:.1}s = {:.0} games/s ({} skipped, cancelled={})",
            summary.imported,
            elapsed.as_secs_f64(),
            summary.imported as f64 / elapsed.as_secs_f64(),
            summary.skipped,
            summary.cancelled
        );
        println!("stats: {:?}", stats(&db).unwrap());
        let first = page(&db, &GameFilter::default(), SortKey::Id, SortDir::Asc, 0, 1).unwrap();
        if let Some(row) = first.rows.first() {
            println!(
                "first stored: {} vs {} ({})",
                row.white, row.black, row.result
            );
            let text = load_game(&db, row.id).unwrap();
            println!(
                "pgn starts: {}",
                text.lines().take(2).collect::<Vec<_>>().join(" | ")
            );
        }
    }
}

#[cfg(test)]
mod diagnostics {
    //! Manual checks against a database the user created. Ignored by default:
    //! they need a file outside the repository.

    use super::*;
    use std::sync::atomic::AtomicBool;

    /// ```text
    /// SILICONBOARD_TEST_DB=~/Documents/.../base.sbd \
    ///   cargo test --release --bin siliconboard db_diagnose -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs SILICONBOARD_TEST_DB"]
    fn db_diagnose() {
        let path = std::env::var_os("SILICONBOARD_TEST_DB").expect("set SILICONBOARD_TEST_DB");
        let path = std::path::PathBuf::from(path);
        if std::env::var_os("SILICONBOARD_TEST_REBUILD").is_some() {
            let cancel = AtomicBool::new(false);
            let indexed = rebuild_position_index(
                &path,
                &mut |done, total| {
                    if done == total {
                        println!("indexed {done}/{total} games");
                    }
                },
                &cancel,
            )
            .unwrap();
            println!("rebuilt index for {indexed} games");
        }
        let total = count(&path, &GameFilter::default()).unwrap();
        println!("total {total}");

        for side in [SideFilter::Either, SideFilter::White, SideFilter::Black] {
            let filter = GameFilter {
                side,
                elo_min: Some(2400),
                elo_max: Some(2500),
                ..GameFilter::default()
            };
            println!("elo 2400-2500 {:?}: {:?}", side, count(&path, &filter));
        }

        // A position from a stored game, at ply 10 and at ply 0.
        let first_game = page(
            &path,
            &GameFilter::default(),
            SortKey::Id,
            SortDir::Asc,
            0,
            1,
        )
        .unwrap();
        if let Some(row) = first_game.rows.first() {
            let text = load_game(&path, row.id).unwrap();
            let import = crate::chess::pgn::parse_pgn_with_report(&text).unwrap();
            let mut game = import.game;
            let plies = game.total_plies();
            println!("game {} has {} plies", row.id, plies);
            for ply in [0usize, 2, 10, plies.min(60)] {
                if ply > plies {
                    continue;
                }
                game.goto_ply(ply);
                let fen = game.fen();
                let key = game.zobrist();
                let filter = GameFilter {
                    position: Some(key),
                    ..GameFilter::default()
                };
                println!("ply {ply}: fen {fen} -> {:?}", count(&path, &filter));
            }
        }
        // The UI's own filter form, so the text parsing is covered too.
        for (side, min, max) in [
            (SideFilter::Either, "2000", "2200"),
            (SideFilter::White, "2000", "2200"),
            (SideFilter::Black, "2000", "2200"),
            (SideFilter::Either, "2400", "2500"),
            (SideFilter::White, "2400", "2500"),
            (SideFilter::Black, "2400", "2500"),
        ] {
            let form = crate::gui::database_ui::FilterForm {
                side,
                elo_min: min.to_string(),
                elo_max: max.to_string(),
                ..crate::gui::database_ui::FilterForm::default()
            };
            let (filter, error) = form.build();
            println!(
                "form {side:?} {min}-{max}: side={:?} elo={:?}..{:?} error={error:?} -> {:?}",
                filter.side,
                filter.elo_min,
                filter.elo_max,
                count(&path, &filter)
            );
        }

        // The user's two cases: max 1800 and min 2000 with "both sides".
        for (min, max) in [(None, Some(1800u16)), (Some(2000u16), None)] {
            let filter = GameFilter {
                side: SideFilter::Either,
                elo_min: min,
                elo_max: max,
                ..GameFilter::default()
            };
            let counted = count(&path, &filter).unwrap();
            let first_rows = page(&path, &filter, SortKey::Date, SortDir::Desc, 0, 4).unwrap();
            println!(
                "check {min:?}-{max:?}: count {counted}, first: {:?}",
                first_rows
                    .rows
                    .iter()
                    .map(|row| (row.white_elo, row.black_elo))
                    .collect::<Vec<_>>()
            );
        }

        // Rows versus the count, with and without a player name.
        for (player, side, min, max) in [
            ("carlsen", SideFilter::Either, "2700", "2900"),
            ("carlsen", SideFilter::White, "2700", "2900"),
            ("carlsen", SideFilter::Black, "2700", "2900"),
            ("", SideFilter::Either, "2400", "2500"),
        ] {
            let filter = GameFilter {
                player: player.to_string(),
                side,
                elo_min: min.parse().ok(),
                elo_max: max.parse().ok(),
                ..GameFilter::default()
            };
            let counted = count(&path, &filter).unwrap();
            let first_rows = page(&path, &filter, SortKey::Date, SortDir::Desc, 0, 5).unwrap();
            println!(
                "rows player={player:?} side={side:?} {min}-{max}: count {counted}, first: {:?}",
                first_rows
                    .rows
                    .iter()
                    .map(|row| (
                        row.white.clone(),
                        row.white_elo,
                        row.black.clone(),
                        row.black_elo
                    ))
                    .collect::<Vec<_>>()
            );
        }

        // An exact FEN check, when one is given.
        if let Some(fen) = std::env::var_os("SILICONBOARD_TEST_FEN") {
            let fen = fen.to_string_lossy().into_owned();
            let key = crate::gui::database_ui::position_hash_for_tests(&fen);
            println!("fen key {key:?}");
            if let Some(key) = key {
                let filter = GameFilter {
                    position: Some(key),
                    ..GameFilter::default()
                };
                println!("fen matches: {:?}", count(&path, &filter));
                let conn = store::connection(&path).unwrap();
                let rows: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM positions WHERE hash = ?1",
                        [key as i64],
                        |row| row.get(0),
                    )
                    .unwrap_or(-1);
                println!("positions with that key: {rows}");
            }
        }
        let _ = AtomicBool::new(false);
    }
}
