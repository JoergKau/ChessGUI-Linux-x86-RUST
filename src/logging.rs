//! Session logging: the console plus one file for the whole run.
//!
//! Each run writes its own file, `siliconboard-<UTC time>.log`, in
//! `~/Library/Logs/SiliconBoard/`. The newest five are kept. Set
//! `SILICONBOARD_LOG_FILE` to choose one file instead (it is truncated).
//!
//! The console keeps the `RUST_LOG` behaviour. The file defaults to
//! `info,siliconboard::engine=debug,siliconboard::app=debug`, which captures
//! engine commands, stderr and lifecycle messages without the noise of a
//! global trace. Set `SILICONBOARD_LOG` to override the file filter (for
//! example `SILICONBOARD_LOG=trace` when hunting a problem).

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use log::{LevelFilter, Log, Metadata, Record};

/// Whether records are written to the session file. The console is never
/// affected. Controlled by Engine → Session log.
static FILE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Initialise the console logger and the session file. The file is opened
/// (and truncated) either way so the menu can switch logging on later in the
/// same session. Returns the log path when the file could be opened.
pub fn init(enabled: bool) -> Option<PathBuf> {
    FILE_ENABLED.store(enabled, Ordering::Release);

    let console =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .format_timestamp_millis()
            .target(env_logger::Target::Stderr)
            .build();

    let path = session_log_path();
    let file = path.as_deref().and_then(open_log);
    let file_logger = file.map(|file| {
        env_logger::Builder::from_env(env_logger::Env::new().filter_or(
            "SILICONBOARD_LOG",
            "info,siliconboard::engine=debug,siliconboard::app=debug",
        ))
        .format_timestamp_millis()
        .target(env_logger::Target::Pipe(Box::new(file)))
        .build()
    });

    let logger = TeeLogger {
        console,
        file: file_logger,
    };
    log::set_max_level(LevelFilter::Trace);
    if log::set_boxed_logger(Box::new(logger)).is_err() {
        return None;
    }

    // A panic is exactly the kind of event the session log should contain.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        default_hook(info);
    }));

    path.filter(|path| path.is_file())
}

/// Write to the console and, when available, the session file. Each sink has
/// its own filter, so the file can be more verbose than the terminal.
struct TeeLogger {
    console: env_logger::Logger,
    file: Option<env_logger::Logger>,
}

impl Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.console.enabled(metadata)
            || (file_is_enabled()
                && self
                    .file
                    .as_ref()
                    .is_some_and(|file| file.enabled(metadata)))
    }

    fn log(&self, record: &Record<'_>) {
        if self.console.enabled(record.metadata()) {
            self.console.log(record);
        }
        if file_is_enabled() {
            if let Some(file) = &self.file {
                if file.enabled(record.metadata()) {
                    file.log(record);
                }
            }
        }
    }

    fn flush(&self) {
        self.console.flush();
        if let Some(file) = &self.file {
            file.flush();
        }
    }
}

/// Switch session-file logging on or off at runtime.
pub fn set_file_enabled(enabled: bool) {
    FILE_ENABLED.store(enabled, Ordering::Release);
    if !enabled {
        // Make sure the last records written before switching off reach disk.
        log::logger().flush();
    }
}

/// Whether session-file logging is currently on.
pub fn file_enabled() -> bool {
    file_is_enabled()
}

fn file_is_enabled() -> bool {
    FILE_ENABLED.load(Ordering::Acquire)
}

/// How many session logs are kept. Older ones are deleted at start-up.
const KEEP_LOGS: usize = 5;

/// The file for this run: `SILICONBOARD_LOG_FILE` when set, otherwise a new
/// file named by the start time in `~/Library/Logs/SiliconBoard/`. The working
/// directory is not used: a Finder-launched app starts in `/`.
fn session_log_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SILICONBOARD_LOG_FILE") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")?;
    let folder = PathBuf::from(home).join("Library/Logs/SiliconBoard");
    Some(folder.join(format!("siliconboard-{}.log", utc_stamp(std::time::SystemTime::now()))))
}

/// Opens this run's log. A file given by `SILICONBOARD_LOG_FILE` is truncated,
/// as before. Otherwise older session logs in the same folder are pruned first.
fn open_log(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
        if std::env::var_os("SILICONBOARD_LOG_FILE").is_none() {
            prune_logs(parent, KEEP_LOGS - 1);
        }
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .ok()
}

/// Delete all but the newest `keep` session logs in `folder`. The names carry a
/// UTC time stamp in fixed-width form, so sorting by name sorts by age.
fn prune_logs(folder: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("siliconboard-") && name.ends_with(".log"))
        })
        .collect();
    logs.sort();
    let surplus = logs.len().saturating_sub(keep);
    for path in logs.into_iter().take(surplus) {
        let _ = std::fs::remove_file(path);
    }
}

/// `YYYYMMDD-HHMMSS` in UTC for `time`, from the days since 1970 (no clock crate needed).
fn utc_stamp(time: std::time::SystemTime) -> String {
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from a day count (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        year,
        month,
        day,
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn time_stamps_are_utc_and_fixed_width() {
        assert_eq!(utc_stamp(UNIX_EPOCH), "19700101-000000");
        assert_eq!(utc_stamp(UNIX_EPOCH + Duration::from_secs(1_000_000_000)), "20010909-014640");
        assert_eq!(utc_stamp(UNIX_EPOCH + Duration::from_secs(951_782_400)), "20000229-000000", "leap day");
    }

    #[test]
    fn only_the_newest_session_logs_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for day in 1..=7 {
            std::fs::write(dir.path().join(format!("siliconboard-2026110{day}-120000.log")), "x").unwrap();
        }
        std::fs::write(dir.path().join("notes.txt"), "unrelated").unwrap();
        prune_logs(dir.path(), KEEP_LOGS - 1);
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "notes.txt",
                "siliconboard-20261104-120000.log",
                "siliconboard-20261105-120000.log",
                "siliconboard-20261106-120000.log",
                "siliconboard-20261107-120000.log",
            ],
            "the four newest logs remain, plus the file that is not a log"
        );
    }
}
