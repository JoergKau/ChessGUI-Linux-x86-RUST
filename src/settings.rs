//! Settings that survive a restart: the time control, whether engines play
//! book moves, the session-log switch, the UCI option values chosen for each
//! engine file, the board theme, the piece set, the two figurine switches, the
//! window and panel sizes, and the last tournament configuration.
//!
//! The file is JSON, kept under `~/Library/Application Support/SiliconBoard/`.
//! `SILICONBOARD_SETTINGS` overrides the path. A missing file means the
//! defaults. A file that cannot be read or parsed is reported, and the
//! defaults are used until the user changes something.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::chess::clock::TimeControl;
use crate::tournament::TournamentConfig;

/// Everything kept between runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The time control for play modes, or `None` for no clock.
    pub time_control: Option<TimeControl>,
    /// Whether engines in a play mode answer with opening-book moves.
    pub book_in_games: bool,
    /// Whether the app writes the session log (`logs/siliconboard.log`). On by
    /// default; Engine → Session log switches it.
    pub session_log: bool,
    /// Option values per engine file (the path as text), by option name.
    pub engine_options: BTreeMap<String, BTreeMap<String, String>>,
    /// The engine file last loaded into each slot (A, B). Loaded again at start-up.
    pub engines: [Option<PathBuf>; 2],
    /// The window's inner size in points, restored at the next launch.
    pub window_size: Option<WindowSize>,
    /// The evaluation and move-list panel sizes, restored at the next launch.
    pub panels: Option<PanelSizes>,
    /// Board image file name from `public/images/board/`, e.g. `blue.png`.
    pub board_theme: Option<String>,
    /// Piece set folder from `public/piece/`, e.g. `cburnett`.
    pub piece_set: Option<String>,
    /// Draw move-list piece letters as figurines (`♘f3`). On by default.
    pub figurines_move_list: bool,
    /// Draw engine PV piece letters as figurines. Off by default.
    pub figurines_eval: bool,
    /// The last tournament configuration, so the dialog opens where the user
    /// left it.
    pub tournament: Option<TournamentConfig>,
}

/// The window's inner size in points.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowSize {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

/// The resizable panels around the board, in points. The board takes what is
/// left of the window.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelSizes {
    /// Width of the evaluation dashboard on the right.
    pub right_width: f32,
    /// Height of the move list at the bottom.
    pub bottom_height: f32,
}

impl Default for Settings {
    /// No clock, book play on, no saved engine options.
    fn default() -> Self {
        Self {
            time_control: None,
            book_in_games: true,
            session_log: true,
            engine_options: BTreeMap::new(),
            engines: [None, None],
            window_size: None,
            panels: None,
            board_theme: None,
            piece_set: None,
            figurines_move_list: true,
            figurines_eval: false,
            tournament: None,
        }
    }
}

/// Why the settings could not be read or written.
#[derive(Debug)]
pub enum SettingsError {
    /// No home folder to put the settings in, and no override given.
    NoLocation,
    /// The file exists but could not be read.
    Read {
        /// The settings file.
        path: PathBuf,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// The file is not valid settings JSON.
    Parse {
        /// The settings file.
        path: PathBuf,
        /// Why it did not parse.
        source: serde_json::Error,
    },
    /// The file could not be written.
    Write {
        /// The settings file.
        path: PathBuf,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingsError::NoLocation => f.write_str("no home folder for the settings file"),
            SettingsError::Read { path, source } => write!(f, "could not read `{}`: {}", path.display(), source),
            SettingsError::Parse { path, source } => write!(f, "`{}` is not valid settings: {}", path.display(), source),
            SettingsError::Write { path, source } => write!(f, "could not write `{}`: {}", path.display(), source),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SettingsError::NoLocation => None,
            SettingsError::Read { source, .. } | SettingsError::Write { source, .. } => Some(source),
            SettingsError::Parse { source, .. } => Some(source),
        }
    }
}

/// Where the settings file lives: `SILICONBOARD_SETTINGS` if set, else
/// `~/Library/Application Support/SiliconBoard/settings.json`.
pub fn settings_path() -> Result<PathBuf, SettingsError> {
    if let Some(path) = std::env::var_os("SILICONBOARD_SETTINGS") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").ok_or(SettingsError::NoLocation)?;
    Ok(PathBuf::from(home)
        .join("Library/Application Support/SiliconBoard")
        .join("settings.json"))
}

/// Read the settings at `path`. A missing file gives the defaults.
///
/// This runs at start-up, before the window exists, because the window size is
/// one of the settings. The file is small, so one synchronous read is fine.
pub fn load_sync(path: &Path) -> Result<Settings, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|source| SettingsError::Parse {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(source) => Err(SettingsError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Write `settings` to `path`, creating the folder if needed. The app runs this
/// on the Tokio runtime while it is open, and synchronously at exit.
pub fn save_sync(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    let write_error = |source| SettingsError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(write_error)?;
    }
    std::fs::write(path, to_json(settings)).map_err(write_error)
}

/// Parse settings JSON. Fields that are missing take their defaults.
pub fn parse(text: &str) -> Result<Settings, serde_json::Error> {
    serde_json::from_str(text)
}

/// Settings as pretty-printed JSON.
pub fn to_json(settings: &Settings) -> String {
    // Serializing plain data cannot fail; the fallback keeps this total.
    serde_json::to_string_pretty(settings).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chess::clock::TimeControl;

    #[test]
    fn settings_round_trip_through_json() {
        let mut settings = Settings {
            time_control: Some(TimeControl::fischer(300_000, 3_000)),
            book_in_games: false,
            ..Settings::default()
        };
        settings
            .engine_options
            .entry("/opt/sf/stockfish".into())
            .or_default()
            .insert("Hash".into(), "256".into());
        let text = to_json(&settings);
        assert_eq!(parse(&text).unwrap(), settings);
    }

    #[test]
    fn session_logging_is_on_by_default_and_round_trips() {
        let settings = Settings::default();
        assert!(settings.session_log, "session logging is on unless turned off");
        assert!(parse(&to_json(&settings)).unwrap().session_log);
        assert!(!parse(r#"{"session_log":false}"#).unwrap().session_log);
    }

    #[test]
    fn the_window_size_round_trips() {
        let settings = Settings {
            window_size: Some(WindowSize {
                width: 1440.0,
                height: 900.0,
            }),
            ..Settings::default()
        };
        let restored = parse(&to_json(&settings)).unwrap();
        assert_eq!(restored.window_size, settings.window_size);
        assert_eq!(Settings::default().window_size, None, "no size saved yet");
    }

    #[test]
    fn the_panel_sizes_round_trip() {
        let settings = Settings {
            panels: Some(PanelSizes {
                right_width: 540.0,
                bottom_height: 220.0,
            }),
            ..Settings::default()
        };
        let restored = parse(&to_json(&settings)).unwrap();
        assert_eq!(restored.panels, settings.panels);
        assert_eq!(Settings::default().panels, None, "no sizes saved yet");
    }

    #[test]
    fn the_piece_set_round_trips() {
        let settings = Settings {
            piece_set: Some("staunty".into()),
            ..Settings::default()
        };
        let restored = parse(&to_json(&settings)).unwrap();
        assert_eq!(restored.piece_set.as_deref(), Some("staunty"));
        assert_eq!(Settings::default().piece_set, None, "the default is cburnett");
    }

    #[test]
    fn figurine_defaults_follow_the_move_list_and_not_the_evaluation() {
        let settings = Settings::default();
        assert!(settings.figurines_move_list, "the move list uses figurines by default");
        assert!(!settings.figurines_eval, "the engine PV is plain SAN by default");
        let restored = parse(&to_json(&settings)).unwrap();
        assert!(restored.figurines_move_list);
        assert!(!restored.figurines_eval);
    }

    #[test]
    fn the_board_theme_round_trips() {
        let settings = Settings {
            board_theme: Some("wood2.jpg".into()),
            ..Settings::default()
        };
        let restored = parse(&to_json(&settings)).unwrap();
        assert_eq!(restored.board_theme.as_deref(), Some("wood2.jpg"));
        assert_eq!(Settings::default().board_theme, None, "no theme chosen yet");
    }

    #[test]
    fn the_tournament_configuration_round_trips() {
        let config = crate::tournament::TournamentConfig {
            white: crate::tournament::EngineSpec::from_path(PathBuf::from("/opt/stockfish")),
            black: crate::tournament::EngineSpec::from_path(PathBuf::from("/opt/triumviratus")),
            games: 20,
            ..crate::tournament::TournamentConfig::default()
        };
        let settings = Settings {
            tournament: Some(config),
            ..Settings::default()
        };
        let restored = parse(&to_json(&settings)).unwrap();
        assert_eq!(restored.tournament, settings.tournament);
        assert_eq!(Settings::default().tournament, None, "no tournament saved yet");
    }

    #[test]
    fn engine_files_are_remembered_per_slot() {
        let settings = Settings {
            engines: [Some(PathBuf::from("/opt/sf/stockfish")), None],
            ..Settings::default()
        };
        let restored = parse(&to_json(&settings)).unwrap();
        assert_eq!(restored.engines[0], Some(PathBuf::from("/opt/sf/stockfish")));
        assert_eq!(restored.engines[1], None);
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings = parse("{}").unwrap();
        assert_eq!(settings, Settings::default());
        assert!(settings.book_in_games, "book play is on unless turned off");
    }

    #[test]
    fn garbage_is_a_parse_error_not_defaults() {
        assert!(parse("not json at all").is_err());
    }

    #[test]
    fn a_missing_file_loads_as_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_sync(&dir.path().join("absent.json")).unwrap();
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn save_then_load_restores_the_settings_and_creates_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let settings = Settings {
            time_control: Some(TimeControl::sudden_death(60_000)),
            ..Settings::default()
        };
        save_sync(&path, &settings).unwrap();
        assert_eq!(load_sync(&path).unwrap(), settings);
    }

    #[test]
    fn an_unreadable_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ broken").unwrap();
        match load_sync(&path) {
            Err(SettingsError::Parse { path: reported, .. }) => assert_eq!(reported, path),
            other => panic!("expected a parse error, got {:?}", other),
        }
    }
}
