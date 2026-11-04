//! Session logging: the console plus one file for the whole run.
//!
//! The file is truncated at start-up, so a session never leaves a trail of
//! dated log files. It is `logs/siliconboard.log` in the working directory.
//! Set `SILICONBOARD_LOG_FILE` to choose another path.
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

fn session_log_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SILICONBOARD_LOG_FILE") {
        return Some(PathBuf::from(path));
    }
    Some(
        std::env::current_dir()
            .ok()?
            .join("logs")
            .join("siliconboard.log"),
    )
}

fn open_log(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .ok()
}
