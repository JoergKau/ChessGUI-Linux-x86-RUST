//! Application shell: owns the game, the engines and the opening book, lays
//! out the panels, and performs the actions the widgets report.
//!
//! Threading: the egui thread never waits on anything. Slow work (file
//! dialogs, spawning an engine, reading a book or a PGN, writing a file) runs
//! on the Tokio runtime owned by [`SiliconBoardApp`] and reports back as an
//! [`AppEvent`], which [`SiliconBoardApp::drain_app_events`] picks up each frame.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key};
use tokio::sync::mpsc;

use crate::assets::{AssetError, Assets, BoardTexture, BoardTheme, PieceId, PieceSet};
use std::collections::BTreeMap;

use crate::chess::clock::{Clock, TimeControl};
use crate::chess::game_state::{Ending, MoveLine};
use crate::chess::opening_book::BookPicker;
use crate::chess::pgn::{self, PgnError, PgnImport};
use crate::chess::players::{accepts_draw, GameMode, Seat};
use crate::chess::{BookError, GameState, PieceColor, PolyglotBook};
use crate::engine::protocol::GoControl;
use crate::engine::{AnalysisStatus, EngineError, EngineHost, EngineProcess, EngineWake, SearchGoal};
use crate::gui::board_view::{self, BoardAction, BoardView};
use crate::gui::eval_panel::{self, EvalCard, EvalSummary};
use crate::gui::menu_bar::{self, MenuAction, MenuState};
use crate::gui::options_dialog::{OptionsAction, OptionsDialog};
use crate::settings::{self, Settings, SettingsError};
use crate::gui::time_control::{self, ClockFace, DialogOutcome, PlayerBar, TimeControlDialog};
use crate::gui::{pgn_panel, theme};
use shakmaty::Move;

/// How long engines get to quit on their own at app exit before they are killed.
const EXIT_GRACE: Duration = Duration::from_millis(600);

/// Time an engine thinks for each move in a game without a clock.
const ENGINE_MOVE_MS: u64 = 1_000;

/// While a clock is running, the window is redrawn this often so the digits
/// keep moving and a flag is seen without input.
const CLOCK_REFRESH: Duration = Duration::from_millis(100);

/// How long the engine looks at a position when it is asked for a draw.
const EVALUATION_MS: u64 = 1_000;

/// Opening-book replies stop after this many plies, so the book covers the
/// opening only. Same default as en-croissant's game setting.
const BOOK_MAX_PLIES: usize = 40;

/// An engine that stalls again within this long after a restart is left
/// offline instead of being restarted, so a broken engine cannot loop.
const RESTART_WINDOW: Duration = Duration::from_secs(60);

/// How long a started engine may take to answer `uci` before it is declared
/// not to be a UCI engine.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long after the last panel/window resize the settings are written.
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);

/// While an engine is loaded, the window runs a frame this often so the
/// watchdog and the handshake timeout are checked even when the engine is
/// silent. Engine output itself wakes the window at once (see [`EngineWake`]),
/// so the analysis updates do not wait for this tick.
const ENGINE_TICK: Duration = Duration::from_millis(500);

/// Shortest gap between engine-driven repaints: about 30 frames per second.
/// A search emits `info` lines far faster than that, and drawing each one
/// spends frames for no visible gain.
const ENGINE_WAKE_INTERVAL: Duration = Duration::from_millis(33);

/// Default height of the move list when no size has been saved yet.
const DEFAULT_MOVE_LIST_HEIGHT: f32 = 190.0;

/// Smallest useful move list, so a dragged panel can always be grabbed again.
const MIN_MOVE_LIST_HEIGHT: f32 = 80.0;

/// How serious the message in the status line is. Warnings and errors are
/// drawn in colour so they cannot be missed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    /// Something that happened as expected.
    Info,
    /// Something the user should know, such as ignored PGN content.
    Warning,
    /// A failed action: it did not happen.
    Error,
}

/// Cache of one engine's principal variation as SAN.
struct PvCache {
    /// Displayed position the SAN was computed from.
    fen: String,
    /// UCI moves the SAN was computed from.
    uci: Vec<String>,
    /// Converted SAN line.
    san: Arc<Vec<String>>,
}

/// Cached SAN layouts for the move list, keyed by history revision and figurine setting.
type PgnGalleyCache = (u64, bool, Vec<Option<Arc<egui::Galley>>>);

/// A one-second summary of frame costs, used by the F-14 measurement.
#[derive(Copy, Clone, Debug, PartialEq)]
struct FrameSummary {
    /// Frames counted in the window.
    frames: u32,
    /// Mean CPU time spent in `logic` + `ui` for one frame.
    average_cost: Duration,
    /// Longest frame cost in the window.
    worst_cost: Duration,
    /// Mean wall-clock gap between frames.
    average_gap: Duration,
    /// Longest wall-clock gap between frames.
    worst_gap: Duration,
}

/// Measures the cost of each frame (the `logic` + `ui` work) while an engine
/// searches, and reports one summary per second. Enabled by `SILICONBOARD_PERF`;
/// off by default, so normal runs do not pay for it.
///
/// The gap between frames is logged too: with event-driven repaints it is the
/// engine's `info` rate, not the cost of drawing.
struct FrameTimer {
    /// End of the previous frame, for the wall-clock gap.
    last: Instant,
    /// Start of the current one-second window.
    window: Instant,
    /// Frames counted in the window.
    frames: u32,
    /// Sum of the frame costs in the window.
    total_cost: Duration,
    /// Longest frame cost in the window.
    worst_cost: Duration,
    /// Sum of the wall-clock gaps in the window.
    total_gap: Duration,
    /// Longest wall-clock gap in the window.
    worst_gap: Duration,
    /// Whether the previous recorded frame was active.
    was_active: bool,
}

impl FrameTimer {
    /// Start measuring from `now`.
    fn new_at(now: Instant) -> Self {
        Self {
            last: now,
            window: now,
            frames: 0,
            total_cost: Duration::ZERO,
            worst_cost: Duration::ZERO,
            total_gap: Duration::ZERO,
            worst_gap: Duration::ZERO,
            was_active: false,
        }
    }

    /// Record one frame that took `cost` at `now`. `active` is false while no
    /// engine searches; the window is then reset so idle time does not skew the
    /// numbers.
    fn record_at(&mut self, now: Instant, cost: Duration, active: bool) -> Option<FrameSummary> {
        if !active {
            self.last = now;
            self.window = now;
            self.frames = 0;
            self.total_cost = Duration::ZERO;
            self.worst_cost = Duration::ZERO;
            self.total_gap = Duration::ZERO;
            self.worst_gap = Duration::ZERO;
            self.was_active = false;
            return None;
        }
        // The first active frame only starts the window; the idle gap before it
        // is not a frame interval.
        if !self.was_active {
            self.was_active = true;
            self.window = now;
            self.last = now;
            return None;
        }
        let gap = now.saturating_duration_since(self.last);
        self.last = now;
        self.frames += 1;
        self.total_cost += cost;
        self.worst_cost = self.worst_cost.max(cost);
        self.total_gap += gap;
        self.worst_gap = self.worst_gap.max(gap);
        if now.saturating_duration_since(self.window) < Duration::from_secs(1) {
            return None;
        }
        let summary = FrameSummary {
            frames: self.frames,
            average_cost: self.total_cost / self.frames.max(1),
            worst_cost: self.worst_cost,
            average_gap: self.total_gap / self.frames.max(1),
            worst_gap: self.worst_gap,
        };
        self.window = now;
        self.frames = 0;
        self.total_cost = Duration::ZERO;
        self.worst_cost = Duration::ZERO;
        self.total_gap = Duration::ZERO;
        self.worst_gap = Duration::ZERO;
        Some(summary)
    }
}

/// Sends results of background work to the UI thread and wakes the window,
/// so a result is handled at once even when nothing else is happening. (The
/// window only runs a frame when asked; without the wake-up a result waits
/// for the next mouse move.)
#[derive(Clone)]
pub struct EventSender {
    /// Channel drained once per frame in [`SiliconBoardApp::drain_app_events`].
    tx: mpsc::UnboundedSender<AppEvent>,
    /// The egui context, used only to request a repaint.
    ctx: egui::Context,
}

impl EventSender {
    /// Queue `event` for the UI thread and ask for a frame to handle it.
    fn send(&self, event: AppEvent) {
        let _ = self.tx.send(event);
        self.ctx.request_repaint();
    }

    /// Ask the window for a frame. Engine readers call this when output
    /// arrives, so the analysis is shown as soon as it exists instead of on a
    /// polling timer.
    pub fn wake(&self) {
        self.ctx.request_repaint();
    }
}

/// A wake-up callback shared by both engine readers. It caps engine-driven
/// repaints at about 30 per second; important lines (handshake, `bestmove`,
/// exit) wake at once. One shared clock keeps two searching engines from
/// doubling the frame rate.
fn engine_wake_for(events: &EventSender) -> EngineWake {
    let sender = events.clone();
    let last = Arc::new(Mutex::new(Instant::now() - ENGINE_WAKE_INTERVAL));
    Arc::new(move |important: bool| {
        let wake = {
            let mut last = last.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if wakes_now(important, last.elapsed()) {
                *last = Instant::now();
                true
            } else {
                false
            }
        };
        if wake {
            sender.wake();
        }
    })
}

/// Whether an engine line should wake the window now. Handshake answers,
/// `bestmove`, watchdog answers and process exit always do; `info` lines wait
/// for the next [`ENGINE_WAKE_INTERVAL`] slot (at most 30 frames per second).
fn wakes_now(important: bool, since_last: Duration) -> bool {
    important || since_last >= ENGINE_WAKE_INTERVAL
}

/// Results of background work, delivered to the UI thread. `None` means the
/// user cancelled a file dialog, which leaves the state unchanged.
enum AppEvent {
    /// An engine was chosen and spawned (or the spawn failed) for `slot`.
    EngineSpawned {
        slot: usize,
        outcome: Option<Result<EngineProcess, EngineError>>,
    },
    /// An opening book was read (or failed to read).
    BookLoaded {
        outcome: Option<(String, Result<PolyglotBook, BookError>)>,
    },
    /// A PGN file was read and parsed.
    PgnOpened {
        outcome: Option<Result<(String, PgnImport), OpenPgnError>>,
    },
    /// File was written.
    FileSaved {
        what: &'static str,
        outcome: Option<std::io::Result<PathBuf>>,
    },
    /// A board theme was decoded off the UI thread and is ready to upload.
    BoardThemeLoaded {
        theme: BoardTheme,
        outcome: Result<egui::ColorImage, AssetError>,
    },
    /// A piece set was rasterised off the UI thread and is ready to upload.
    PieceSetLoaded {
        set: PieceSet,
        outcome: Result<Vec<(PieceId, egui::ColorImage)>, AssetError>,
    },
    /// The settings file was written.
    SettingsSaved {
        outcome: Result<(), SettingsError>,
    },
}

/// Why opening a PGN file failed.
#[derive(Debug)]
pub enum OpenPgnError {
    /// The file could not be read.
    Read {
        /// File that could not be read.
        path: PathBuf,
        /// Underlying I/O failure.
        source: std::io::Error,
    },
    /// The file is not a readable PGN game.
    Parse(PgnError),
}

impl std::fmt::Display for OpenPgnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenPgnError::Read { path, source } => {
                write!(f, "could not read `{}`: {}", path.display(), source)
            }
            OpenPgnError::Parse(source) => write!(f, "{}", source),
        }
    }
}

impl std::error::Error for OpenPgnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OpenPgnError::Read { source, .. } => Some(source),
            OpenPgnError::Parse(source) => Some(source),
        }
    }
}

/// The application state shared by every panel.
pub struct SiliconBoardApp {
    /// Tokio runtime for engines, file dialogs and background I/O.
    runtime: tokio::runtime::Runtime,
    /// Sender cloned into background tasks.
    events_tx: EventSender,
    /// Wakes the window when engine output arrives; shared by both readers.
    engine_wake: EngineWake,
    /// Receiver drained once per frame in [`SiliconBoardApp::logic`].
    events_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Textures, loaded on the first frame. `None` until then.
    assets: Option<Result<Assets, AssetError>>,
    /// The game: position, history and cursor.
    game: GameState,
    /// Who plays which side: analysis only, a human against engine A, or engine A against engine B.
    mode: GameMode,
    /// The game clock, when a time control is set. Runs only in a play mode.
    clock: Option<Clock>,
    /// The Game → Time control dialog, while it is open.
    time_dialog: Option<TimeControlDialog>,
    /// Whether an engine on the move plays an opening-book move when one exists.
    book_in_games: bool,
    /// Chooses among the book moves by weight.
    book_picker: BookPicker,
    /// For each slot being restarted after a stall: whether analysis was on.
    restarting: [Option<bool>; 2],
    /// When each slot was last restarted after a stall.
    restarted_at: [Option<Instant>; 2],
    /// How the game ended by resignation or agreement. A flag is read from the clock.
    ending: Option<Ending>,
    /// `true` from Start until Abort, a game end, or a new game. A play-mode
    /// game does nothing until then: no engine searches and no clock runs.
    game_started: bool,
    /// UCI option values the user chose, per slot. Sent again when the engine restarts.
    engine_settings: [BTreeMap<String, String>; 2],
    /// The engine options window, while it is open.
    options_dialog: Option<OptionsDialog>,
    /// The slot whose engine is evaluating a pending draw offer.
    draw_offer: Option<usize>,
    /// What is kept between runs. Changed here and written back on each change.
    settings: Settings,
    /// The window size last written to the settings, so it is saved only when it changes.
    saved_window_size: Option<settings::WindowSize>,
    /// Whether the game was live on the last frame, to notice it ending.
    game_was_live: bool,
    /// Where `settings` is kept. `None` when there is no home folder.
    settings_file: Option<PathBuf>,
    /// A settings write is in flight.
    settings_saving: bool,
    /// The settings changed while a write was in flight; save again after it.
    settings_save_pending: bool,
    /// When the debounced window/panel save is due.
    settings_save_due: Option<Instant>,
    /// Board widget state (interaction, orientation).
    view: BoardView,
    /// Engine slots: A and B.
    engines: [Option<EngineHost>; 2],
    /// `true` while a file dialog or spawn for a slot is in flight.
    engine_loading: [bool; 2],
    /// Whether analysis is switched on for each slot (Start, Abort or the card's toggle).
    analysis_on: [bool; 2],
    /// The loaded opening book and its file name.
    book: Option<(String, PolyglotBook)>,
    /// Book moves for the displayed position, heaviest first.
    book_moves: Vec<(String, u16)>,
    /// FEN the engines and book were last synchronised to.
    synced_fen: String,
    /// Cursor position on the previous frame, to drive move-list auto-scroll.
    last_ply: usize,
    /// Status line shown in the evaluation panel.
    status: String,
    /// Severity of the status line.
    status_level: StatusLevel,
    /// Last engine status seen per slot, to report start-up outcomes once.
    engine_status: [Option<AnalysisStatus>; 2],
    /// When each slot's engine was spawned, for the handshake timeout.
    engine_started: [Option<Instant>; 2],
    /// The FEN text in the top bar. Follows the position unless the user edits it.
    fen_input: String,
    /// Board images found in `public/images/board/`, for the View menu.
    board_themes: Vec<BoardTheme>,
    /// Piece sets found in `public/piece/`, for the View menu.
    piece_sets: Vec<PieceSet>,
    /// Per-slot SAN cache for the engine principal variations.
    pv_cache: [Option<PvCache>; 2],
    /// Cached move list plus the history revision it was built from.
    pgn_cache: Option<(u64, Vec<MoveLine>)>,
    /// Cached SAN layouts for the move list, keyed by revision and figurine setting.
    pgn_galleys: Option<PgnGalleyCache>,
    /// Frame-time measurement, when `SILICONBOARD_PERF` is set.
    frame_timer: Option<FrameTimer>,
    /// When this frame's `logic` began, so `ui` can report the frame cost.
    frame_started: Option<Instant>,
}

impl SiliconBoardApp {
    /// Create the application with the given Tokio runtime, and start the
    /// background loads: the start-up book, the settings and the engines saved
    /// in them.
    pub fn new(
        runtime: tokio::runtime::Runtime,
        ctx: egui::Context,
        settings: Settings,
        settings_file: Option<PathBuf>,
    ) -> Self {
        let mut app = Self::unloaded(runtime, ctx);
        app.settings_file = settings_file;
        // The list is needed before the saved theme is validated.
        app.board_themes = crate::assets::board_themes();
        app.piece_sets = crate::assets::piece_sets();
        // The settings were read before the window existed (for its size).
        app.apply_settings(settings);
        app.load_default_book();
        app
    }

    /// Take `settings`: the book switch, the time control, and the engines
    /// loaded last time, which are started again but not started searching.
    fn apply_settings(&mut self, settings: Settings) {
        self.book_in_games = settings.book_in_games;
        self.clock = settings
            .time_control
            .map(|control| Clock::new(control, self.game.turn()));
        self.saved_window_size = settings.window_size;
        let engines = settings.engines.clone();
        self.settings = settings;
        // A theme that is no longer on disk (or a malformed value) falls back
        // to the default board.
        if let Some(theme) = &self.settings.board_theme {
            if !self.board_themes.iter().any(|known| &known.file == theme) {
                log::warn!("saved board theme `{}` is not available; using the default", theme);
                self.settings.board_theme = None;
            }
        }
        if let Some(set) = &self.settings.piece_set {
            if !self.piece_sets.iter().any(|known| &known.name == set) {
                log::warn!("saved piece set `{}` is not available; using the default", set);
                self.settings.piece_set = None;
            }
        }
        for (slot, path) in engines.into_iter().enumerate() {
            if let Some(path) = path {
                self.spawn_engine_file(slot, path);
            }
        }
    }

    /// The application with no background loads started.
    fn unloaded(runtime: tokio::runtime::Runtime, ctx: egui::Context) -> Self {
        let (tx, events_rx) = mpsc::unbounded_channel();
        let events_tx = EventSender { tx, ctx };
        let engine_wake = engine_wake_for(&events_tx);
        Self {
            runtime,
            events_tx,
            engine_wake,
            events_rx,
            assets: None,
            game: GameState::new(),
            mode: GameMode::Analysis,
            clock: None,
            time_dialog: None,
            book_in_games: true,
            book_picker: BookPicker::default(),
            restarting: [None, None],
            restarted_at: [None, None],
            ending: None,
            game_started: false,
            engine_settings: [BTreeMap::new(), BTreeMap::new()],
            options_dialog: None,
            draw_offer: None,
            settings: Settings::default(),
            saved_window_size: None,
            game_was_live: false,
            settings_file: None,
            settings_saving: false,
            settings_save_pending: false,
            settings_save_due: None,
            view: BoardView::default(),
            engines: [None, None],
            engine_loading: [false, false],
            analysis_on: [false, false],
            book: None,
            book_moves: Vec::new(),
            synced_fen: String::new(),
            last_ply: 0,
            status: "Welcome — click or drag a piece to make a move.".to_string(),
            status_level: StatusLevel::Info,
            engine_status: [None, None],
            engine_started: [None, None],
            fen_input: GameState::new().fen(),
            board_themes: Vec::new(),
            piece_sets: Vec::new(),
            pv_cache: [None, None],
            pgn_cache: None,
            pgn_galleys: None,
            frame_timer: std::env::var_os("SILICONBOARD_PERF").map(|_| FrameTimer::new_at(Instant::now())),
            frame_started: None,
        }
    }

    // ── Per-frame work ─────────────────────────────────────────────────────

    /// Non-visual work for the frame: load assets once, apply background
    /// results, pump engine output, keep engines in sync, handle arrow keys.
    fn logic_frame(&mut self, ctx: &egui::Context) {
        self.frame_started = Some(Instant::now());
        if self.settings_save_due.is_some_and(|due| Instant::now() >= due) {
            self.settings_save_due = None;
            self.save_settings();
        }
        self.save_window_size(ctx);
        if self.assets.is_none() {
            let loaded = Assets::load_with(
                ctx,
                self.settings.board_theme.as_deref(),
                self.settings.piece_set.as_deref(),
            );
            if let Err(err) = &loaded {
                log::error!("asset load failed: {}", err);
            }
            self.assets = Some(loaded);
        }
        self.drain_app_events(ctx);
        let mut engines_changed = false;
        for host in self.engines.iter_mut().flatten() {
            engines_changed |= host.pump();
        }
        if engines_changed {
            ctx.request_repaint();
        }
        // Engine output wakes the window from the reader task, so a frame runs
        // as soon as there is new analysis. The tick below only keeps the
        // watchdog and the handshake timeout alive while an engine is silent.
        if (0..2).any(|slot| self.engine_live(slot)) {
            ctx.request_repaint_after(ENGINE_TICK);
        }
        self.report_stalled_engines();
        self.play_engine_moves();
        self.finish_draw_offer();
        self.play_book_reply(ctx);
        // A game that ends (or is replaced) switches its engines off, so each
        // card's button reads Start again and the engine rests (round 8, bugs 2-4).
        let live = self.live_play();
        if self.game_was_live && !live {
            for &slot in self.mode.engine_slots() {
                self.set_engine_analysis(slot, false);
            }
        }
        self.game_was_live = live;
        self.sync_clock(ctx);
        self.report_engine_start_up();
        self.sync_position(false);
        self.handle_navigation_keys(ctx);
    }

    /// How many engines are searching right now.
    fn searching_engines(&self) -> usize {
        self.engines
            .iter()
            .flatten()
            .filter(|host| host.view().status == AnalysisStatus::Thinking)
            .count()
    }

    /// Record this frame's `logic` + `ui` cost and log a one-second summary
    /// while an engine searches (F-14). `SILICONBOARD_PERF` turns it on.
    fn record_frame(&mut self) {
        let Some(started) = self.frame_started.take() else {
            return;
        };
        if self.frame_timer.is_none() {
            return;
        }
        let cost = started.elapsed();
        let searching = self.searching_engines();
        let summary = self
            .frame_timer
            .as_mut()
            .and_then(|timer| timer.record_at(Instant::now(), cost, searching > 0));
        let Some(summary) = summary else {
            return;
        };
        let nps: u64 = self
            .engines
            .iter()
            .flatten()
            .filter_map(|host| host.view().stats.nps)
            .sum();
        log::info!(
            "frame: {:.2} ms cost (worst {:.2} ms), {:.0} ms gap (worst {:.0} ms); {} frames, {} searching, {:.2} Mn/s",
            summary.average_cost.as_secs_f64() * 1e3,
            summary.worst_cost.as_secs_f64() * 1e3,
            summary.average_gap.as_secs_f64() * 1e3,
            summary.worst_gap.as_secs_f64() * 1e3,
            summary.frames,
            searching,
            nps as f64 / 1e6,
        );
    }

    /// Play the move of every engine whose move search has finished. The move
    /// was searched for the displayed position, so it is played only if it is
    /// still legal there, and only if the game has not been decided on the clock.
    fn play_engine_moves(&mut self) {
        for slot in 0..2 {
            let Some(uci) = self.engines[slot].as_mut().and_then(EngineHost::take_engine_move) else {
                continue;
            };
            if self.clock_flagged().is_some() {
                continue;
            }
            match self.game.apply_uci(&uci) {
                Ok(san) => {
                    self.view.clear_selection();
                    let mut text = format!("Engine {} played {}.", slot_letter(slot), san);
                    if let Some(result) = self.result_text() {
                        text.push_str(&format!(" {}.", result));
                    }
                    self.set_status(StatusLevel::Info, text);
                }
                Err(err) => {
                    log::error!("engine {} sent `{}`: {}", slot_letter(slot), uci, err);
                    self.set_status(
                        StatusLevel::Error,
                        format!("Engine {} sent {}, which is not legal here: {}", slot_letter(slot), uci, err),
                    );
                }
            }
        }
    }

    /// Follow each engine from spawn to ready. An engine that exits, or stays
    /// silent past `HANDSHAKE_TIMEOUT`, before answering `uci` is not a UCI
    /// engine: it is shut down and the status line says so. Later changes
    /// (Abort, exit) do not touch the status line.
    fn report_engine_start_up(&mut self) {
        for slot in 0..2 {
            let letter = slot_letter(slot);
            let timed_out = self.engine_started[slot].is_some_and(|t| t.elapsed() > HANDSHAKE_TIMEOUT);
            let failure = match self.engines[slot].as_mut() {
                Some(host) if host.view().status == AnalysisStatus::Starting => {
                    let reason = if host.has_exited() {
                        Some("it exited before answering `uci`")
                    } else if timed_out {
                        Some("it did not answer `uci` within 10 seconds")
                    } else {
                        None
                    };
                    reason.map(|reason| (host.shutdown(), host.view().binary_name, reason))
                }
                _ => None,
            };
            if let Some((process, binary, reason)) = failure {
                self.quit_processes(vec![process]);
                self.engine_status[slot] = Some(AnalysisStatus::Offline);
                self.engine_started[slot] = None;
                self.analysis_on[slot] = false;
                self.set_status(StatusLevel::Error, format!("Engine {} is not a UCI engine: {} ({}).", letter, binary, reason));
                continue;
            }

            let Some(host) = &self.engines[slot] else {
                self.engine_status[slot] = None;
                self.engine_started[slot] = None;
                continue;
            };
            let now = host.view();
            let before = self.engine_status[slot];
            if before == Some(now.status) {
                continue;
            }
            if before == Some(AnalysisStatus::Starting)
                && matches!(now.status, AnalysisStatus::Ready | AnalysisStatus::Thinking)
            {
                self.set_status(
                    StatusLevel::Info,
                    format!(
                        "Engine {} ready: {}",
                        letter,
                        now.name.as_deref().unwrap_or(&now.binary_name)
                    ),
                );
            }
            self.engine_status[slot] = Some(now.status);
        }
    }

    /// Apply every background result that arrived since the last frame.
    fn drain_app_events(&mut self, ctx: &egui::Context) {
        let mut received = false;
        while let Ok(event) = self.events_rx.try_recv() {
            received = true;
            match event {
                AppEvent::EngineSpawned { slot, outcome } => {
                    self.engine_loading[slot] = false;
                    match outcome {
                        None => self.set_status(StatusLevel::Info, "Engine selection cancelled.".to_string()),
                        Some(Ok(process)) => {
                            let binary = process.binary().display().to_string();
                            let path = process.binary().to_path_buf();
                            if let Some(mut old) = self.engines[slot].take() {
                                let stale = old.shutdown();
                                self.quit_processes(vec![stale]);
                            }
                            let restart = self.restarting[slot].take();
                            if restart.is_none() {
                                // A different engine: its own saved values apply, not the old engine's.
                                self.engine_settings[slot] =
                                    self.settings.engine_options.get(&binary).cloned().unwrap_or_default();
                                self.close_options_dialog(slot);
                                self.settings.engines[slot] = Some(path);
                                self.save_settings();
                            }
                            self.engines[slot] = Some(EngineHost::attach_with_settings(
                                process,
                                self.engine_settings[slot].clone(),
                            ));
                            self.engine_status[slot] = None;
                            self.engine_started[slot] = Some(Instant::now());
                            let playing = self.mode.engine_slots().contains(&slot);
                            let was_analysing = restart == Some(true);
                            self.set_engine_analysis(slot, playing || was_analysing);
                            let text = match restart {
                                Some(_) => format!("Engine {} restarted: {}…", slot_letter(slot), binary),
                                None => format!("Loading engine {}: {}…", slot_letter(slot), binary),
                            };
                            self.set_status(StatusLevel::Info, text);
                        }
                        Some(Err(err)) => {
                            self.restarting[slot] = None;
                            log::error!("engine spawn failed: {}", err);
                            self.set_status(StatusLevel::Error, format!("Could not start engine {}: {}", slot_letter(slot), err));
                        }
                    }
                }
                AppEvent::BookLoaded { outcome } => match outcome {
                    None => self.set_status(StatusLevel::Info, "Book selection cancelled.".to_string()),
                    Some((name, Ok(book))) if book.is_empty() => {
                        self.set_status(StatusLevel::Warning, format!("Opening book {} has no records.", name));
                    }
                    Some((name, Ok(book))) => {
                        self.set_status(StatusLevel::Info, format!("Opening book loaded: {} ({} records)", name, book.len()));
                        self.book = Some((name, book));
                        self.sync_position(true);
                    }
                    Some((name, Err(err))) => {
                        self.set_status(StatusLevel::Error, format!("Could not load book {}: {}", name, err));
                    }
                },
                AppEvent::PgnOpened { outcome } => match outcome {
                    None => self.set_status(StatusLevel::Info, "Open cancelled.".to_string()),
                    Some(Ok((name, import))) => {
                        let mut game = import.game;
                        game.goto_start();
                        self.game = game;
                        self.ending = None;
                        self.game_started = false;
                        self.reset_clock();
                        self.view.clear_selection();
                        match import.ignored.summary() {
                            Some(note) => self.set_status(
                                StatusLevel::Warning,
                                format!("Opened {} ({})", name, note),
                            ),
                            None => self.set_status(StatusLevel::Info, format!("Opened {}", name)),
                        }
                    }
                    Some(Err(err)) => {
                        log::error!("PGN open failed: {}", err);
                        self.set_status(StatusLevel::Error, err.to_string());
                    }
                },
                AppEvent::BoardThemeLoaded { theme, outcome } => match outcome {
                    Ok(image) => {
                        let handle = ctx.load_texture(&theme.file, image, egui::TextureOptions::LINEAR);
                        if let Some(Ok(assets)) = &mut self.assets {
                            assets.board = BoardTexture::from_handle(handle);
                        }
                        self.settings.board_theme = Some(theme.file.clone());
                        self.save_settings();
                        self.set_status(StatusLevel::Info, format!("Board theme: {}.", theme.label));
                        ctx.request_repaint();
                    }
                    Err(err) => {
                        log::error!("board theme {} failed: {}", theme.file, err);
                        self.set_status(
                            StatusLevel::Error,
                            format!("Could not load board theme {}: {}", theme.label, err),
                        );
                    }
                },
                AppEvent::PieceSetLoaded { set, outcome } => match outcome {
                    Ok(images) => {
                        if let Some(Ok(assets)) = &mut self.assets {
                            assets.pieces = crate::assets::upload_pieces(ctx, images);
                        }
                        self.settings.piece_set = Some(set.name.clone());
                        self.save_settings();
                        self.set_status(StatusLevel::Info, format!("Piece set: {}.", set.label));
                        ctx.request_repaint();
                    }
                    Err(err) => {
                        log::error!("piece set {} failed: {}", set.name, err);
                        self.set_status(
                            StatusLevel::Error,
                            format!("Could not load piece set {}: {}", set.label, err),
                        );
                    }
                },
                AppEvent::SettingsSaved { outcome } => {
                    self.settings_saving = false;
                    if let Err(err) = outcome {
                        log::error!("{}", err);
                        self.set_status(StatusLevel::Error, format!("Settings not saved: {}", err));
                    }
                    if self.settings_save_pending {
                        self.settings_save_pending = false;
                        self.save_settings();
                    }
                }
                AppEvent::FileSaved { what, outcome } => match outcome {
                    None => self.set_status(StatusLevel::Info, format!("{} save cancelled.", what)),
                    Some(Ok(path)) => self.set_status(StatusLevel::Info, format!("Saved {} to {}", what, path.display())),
                    Some(Err(err)) => {
                        log::error!("saving {} failed: {}", what, err);
                        self.set_status(StatusLevel::Error, format!("Could not save {}: {}", what, err));
                    }
                },
            }
        }
        if received {
            ctx.request_repaint();
        }
    }

    /// Push the displayed position to the engines and recompute the book
    /// suggestions, but only when the position actually changed (or `force`).
    fn sync_position(&mut self, force: bool) {
        let fen = self.game.fen();
        if !force && fen == self.synced_fen {
            return;
        }
        for slot in 0..2 {
            let goal = self.goal_for(slot);
            if let Some(host) = self.engines[slot].as_mut() {
                host.set_position(&fen, goal);
            }
        }
        self.book_moves = match &self.book {
            Some((_, book)) => self.game.book_suggestions(book),
            None => Vec::new(),
        };
        self.fen_input = fen.clone();
        self.synced_fen = fen;
    }

    /// ← / → step through the game; Home / End jump to the ends.
    fn handle_navigation_keys(&mut self, ctx: &egui::Context) {
        let (left, right, home, end) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
            )
        });
        if left {
            self.game.undo();
        }
        if right {
            self.game.redo();
        }
        if home {
            self.game.goto_start();
        }
        if end {
            self.game.goto_end();
        }
        if left || right || home || end {
            self.view.clear_selection();
        }
    }

    // ── Actions ────────────────────────────────────────────────────────────

    /// Perform a menu or FEN-bar action.
    fn perform(&mut self, action: MenuAction, ctx: &egui::Context) {
        match action {
            MenuAction::NewGame => {
                self.game = GameState::new();
                self.ending = None;
                self.game_started = false;
                self.reset_clock();
                self.view.clear_selection();
                for host in self.engines.iter_mut().flatten() {
                    host.new_game();
                }
                self.set_status(StatusLevel::Info, "New game.".to_string());
            }
            MenuAction::FlipBoard => self.view.flip(),
            MenuAction::SetMode(mode) => self.set_mode(mode),
            MenuAction::ToggleBookInGames => {
                self.book_in_games = !self.book_in_games;
                self.settings.book_in_games = self.book_in_games;
                self.save_settings();
                let text = if self.book_in_games {
                    "Book moves in games: on.".to_string()
                } else {
                    "Book moves in games: off.".to_string()
                };
                self.set_status(StatusLevel::Info, text);
                self.sync_position(true);
            }
            MenuAction::Resign => self.resign(),
            MenuAction::OpenEngineOptions(slot) => self.open_engine_options(slot),
            MenuAction::OfferDraw => self.offer_draw(),
            MenuAction::OpenTimeControl => {
                self.time_dialog = Some(TimeControlDialog::new(self.clock.as_ref().map(Clock::control)));
            }
            MenuAction::OpenPgn => self.open_pgn(),
            MenuAction::SavePgn => {
                let text = pgn::to_pgn(&self.game, self.current_ending());
                self.save_text("PGN", text, "game.pgn", "PGN", &["pgn"]);
            }
            MenuAction::CopyPgn => {
                ctx.copy_text(pgn::to_pgn(&self.game, self.current_ending()));
                self.set_status(StatusLevel::Info, "PGN copied to the clipboard.".to_string());
            }
            MenuAction::CopyFen => {
                ctx.copy_text(self.game.fen());
                self.set_status(StatusLevel::Info, "FEN copied to the clipboard.".to_string());
            }
            MenuAction::SaveFen => {
                let text = format!("{}\n", self.game.fen());
                self.save_text("FEN", text, "position.fen", "FEN", &["fen"]);
            }
            MenuAction::LoadEngine(slot) => self.load_engine(slot),
            MenuAction::ToggleEngine(slot) => self.toggle_engine(slot),
            MenuAction::StartAnalysis => self.set_analysis(true),
            MenuAction::StopAnalysis => self.set_analysis(false),
            MenuAction::LoadFen => match GameState::from_fen(self.fen_input.trim()) {
                Ok(game) => {
                    self.game = game;
                    self.ending = None;
                    self.game_started = false;
                    self.reset_clock();
                    self.view.clear_selection();
                    self.set_status(StatusLevel::Info, "Position loaded from FEN.".to_string());
                }
                Err(err) => self.set_status(StatusLevel::Error, format!("Invalid FEN: {}", err)),
            },
            MenuAction::LoadBook => self.load_book(),
            MenuAction::SetBoardTheme(index) => self.set_board_theme(index),
            MenuAction::SetPieceSet(index) => self.set_piece_set(index),
            MenuAction::ToggleFigurinesMoveList => self.toggle_figurines(false),
            MenuAction::ToggleFigurinesEval => self.toggle_figurines(true),
        }
    }

    /// Apply the move (or selection change) the board widget requested.
    fn apply_board_action(&mut self, action: BoardAction) {
        match action {
            BoardAction::None => {}
            BoardAction::PieceSelected { from } => {
                self.set_status(StatusLevel::Info, format!("Selected {}.", from));
            }
            BoardAction::RequestMove { m } => match self.game.apply_move(m) {
                Ok(()) => {
                    let played = self
                        .game
                        .active_history()
                        .last()
                        .map(|ply| ply.san.to_string())
                        .unwrap_or_default();
                    let text = match self.result_text() {
                        Some(result) => format!("Played {}. {}.", played, result),
                        None => format!("Played {}.", played),
                    };
                    self.set_status(StatusLevel::Info, text);
                }
                Err(err) => self.set_status(StatusLevel::Error, format!("Move rejected: {}", err)),
            },
            BoardAction::PromotionChoice { .. } => {
                self.set_status(StatusLevel::Info, "Promotion: pick Q / R / B / N on the board (Esc cancels).".to_string());
            }
        }
    }

    /// Replace an engine that has stopped answering `isready`. Its process is
    /// quit and the same file is started again, and the game continues from
    /// the displayed position. A second stall within [`RESTART_WINDOW`] leaves
    /// the engine offline instead.
    fn report_stalled_engines(&mut self) {
        for slot in 0..2 {
            let stalled = self.engine_live(slot) && self.engines[slot].as_ref().is_some_and(EngineHost::is_stalled);
            if !stalled {
                continue;
            }
            let Some(host) = self.engines[slot].as_mut() else {
                continue;
            };
            let binary = host.binary_path();
            let was_analysing = self.analysis_on[slot];
            let process = host.shutdown();
            self.close_options_dialog(slot);
            self.quit_processes(vec![process]);
            self.analysis_on[slot] = false;
            self.engine_status[slot] = None;
            self.engine_started[slot] = None;

            let recently_restarted = self.restarted_at[slot].is_some_and(|at| at.elapsed() < RESTART_WINDOW);
            if recently_restarted {
                self.restarted_at[slot] = None;
                self.set_status(
                    StatusLevel::Error,
                    format!(
                        "Engine {} stopped answering again. It is offline; load it to try once more.",
                        slot_letter(slot)
                    ),
                );
            } else {
                self.restarted_at[slot] = Some(Instant::now());
                self.engines[slot] = None;
                self.restart_engine(slot, binary, was_analysing);
                self.set_status(
                    StatusLevel::Warning,
                    format!("Engine {} stopped answering. Restarting it.", slot_letter(slot)),
                );
            }
        }
    }

    /// Open the options window for the engine in `slot`, with the options it declared.
    fn open_engine_options(&mut self, slot: usize) {
        let Some(host) = self.engines[slot].as_ref() else {
            self.set_status(StatusLevel::Warning, format!("Engine {} is not loaded.", slot_letter(slot)));
            return;
        };
        let options = host.options().to_vec();
        self.options_dialog = Some(OptionsDialog::new(
            slot,
            slot_letter(slot),
            options,
            &self.engine_settings[slot],
        ));
    }

    /// Set one option on the engine in `slot`, and keep the value for a restart.
    fn set_engine_option(&mut self, slot: usize, name: &str, value: &str) {
        self.engine_settings[slot].insert(name.to_string(), value.to_string());
        let file = match self.engines[slot].as_mut() {
            Some(host) => {
                host.set_option(name, value);
                Some(host.binary_path().display().to_string())
            }
            None => None,
        };
        if let Some(file) = file {
            self.settings
                .engine_options
                .entry(file)
                .or_default()
                .insert(name.to_string(), value.to_string());
            self.save_settings();
        }
        self.set_status(StatusLevel::Info, format!("Engine {}: {} set to {}.", slot_letter(slot), name, value));
    }

    /// Press a button option on the engine in `slot`.
    fn press_engine_button(&mut self, slot: usize, name: &str) {
        if let Some(host) = self.engines[slot].as_mut() {
            host.press_button(name);
        }
        self.set_status(StatusLevel::Info, format!("Engine {}: {} pressed.", slot_letter(slot), name));
    }

    /// Close the options window if it is for `slot`, e.g. when that engine is replaced.
    fn close_options_dialog(&mut self, slot: usize) {
        if self.options_dialog.as_ref().is_some_and(|dialog| dialog.slot() == slot) {
            self.options_dialog = None;
        }
    }

    /// Start the engine in `slot` again from `binary` (runs on the Tokio runtime).
    /// `was_analysing` tells the attach step whether analysis was on.
    fn restart_engine(&mut self, slot: usize, binary: PathBuf, was_analysing: bool) {
        self.restarting[slot] = Some(was_analysing);
        self.spawn_engine_file(slot, binary);
    }

    /// Start the engine file `path` in `slot` on the Tokio runtime. The result
    /// arrives as `EngineSpawned`.
    fn spawn_engine_file(&mut self, slot: usize, path: PathBuf) {
        log::info!("starting engine {} from {}", slot_letter(slot), path.display());
        self.engine_loading[slot] = true;
        let tx = self.events_tx.clone();
        let wake = self.engine_wake.clone();
        self.spawn(async move {
            let outcome = Some(EngineProcess::spawn_waking(&path, wake).await);
            tx.send(AppEvent::EngineSpawned { slot, outcome });
        });
    }

    /// Set an option back to the engine's default: the saved value is forgotten.
    fn reset_engine_option(&mut self, slot: usize, name: &str, default: &str) {
        self.engine_settings[slot].remove(name);
        let file = match self.engines[slot].as_mut() {
            Some(host) => {
                host.set_option(name, default);
                Some(host.binary_path().display().to_string())
            }
            None => None,
        };
        if let Some(file) = file {
            if let Some(saved) = self.settings.engine_options.get_mut(&file) {
                saved.remove(name);
            }
            self.save_settings();
        }
        self.set_status(
            StatusLevel::Info,
            format!("Engine {}: {} back to its default ({}).", slot_letter(slot), name, default),
        );
    }

    /// Show the file picker for an engine and spawn the chosen binary.
    fn load_engine(&mut self, slot: usize) {
        if self.engine_loading[slot] {
            return;
        }
        self.engine_loading[slot] = true;
        self.set_status(StatusLevel::Info, format!("Choose an engine for slot {}…", slot_letter(slot)));
        let tx = self.events_tx.clone();
        let wake = self.engine_wake.clone();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Choose a UCI engine")
                .pick_file()
                .await;
            let outcome = match picked {
                None => None,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
                    Some(EngineProcess::spawn_waking(&path, wake).await)
                }
            };
            tx.send(AppEvent::EngineSpawned { slot, outcome });
        });
    }

    /// Switch the board image. Decoding runs off the UI thread; the texture is
    /// uploaded when [`AppEvent::BoardThemeLoaded`] arrives.
    fn set_board_theme(&mut self, index: usize) {
        let Some(theme) = self.board_themes.get(index).cloned() else {
            return;
        };
        if self.settings.board_theme.as_deref() == Some(theme.file.as_str()) {
            return;
        }
        self.set_status(StatusLevel::Info, format!("Loading board theme: {}…", theme.label));
        let tx = self.events_tx.clone();
        let file = theme.file.clone();
        self.spawn(async move {
            let outcome = match crate::assets::board_theme_path(&file) {
                Some(path) => {
                    let error_path = path.clone();
                    let joined =
                        tokio::task::spawn_blocking(move || crate::assets::decode_board(&path)).await;
                    match joined {
                        Ok(result) => result,
                        Err(join) => Err(AssetError::OpenRead {
                            path: error_path,
                            source: std::io::Error::other(join),
                        }),
                    }
                }
                None => Err(AssetError::OpenRead {
                    path: PathBuf::from(&file),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "not a board image",
                    ),
                }),
            };
            tx.send(AppEvent::BoardThemeLoaded { theme, outcome });
        });
    }

    /// Switch the piece set. Rasterising the twelve SVGs runs off the UI
    /// thread; the textures are uploaded when [`AppEvent::PieceSetLoaded`]
    /// arrives.
    fn set_piece_set(&mut self, index: usize) {
        let Some(set) = self.piece_sets.get(index).cloned() else {
            return;
        };
        let current = self
            .settings
            .piece_set
            .as_deref()
            .unwrap_or(crate::assets::DEFAULT_PIECE_SET);
        if current == set.name {
            return;
        }
        self.set_status(StatusLevel::Info, format!("Loading piece set: {}…", set.label));
        let tx = self.events_tx.clone();
        let name = set.name.clone();
        self.spawn(async move {
            let outcome = match crate::assets::piece_set_path(&name) {
                Some(path) => {
                    let error_path = path.clone();
                    match tokio::task::spawn_blocking(move || {
                        crate::assets::rasterise_piece_set(&path)
                    })
                    .await
                    {
                        Ok(result) => result,
                        Err(join) => Err(AssetError::OpenRead {
                            path: error_path,
                            source: std::io::Error::other(join),
                        }),
                    }
                }
                None => Err(AssetError::OpenRead {
                    path: PathBuf::from(&name),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "not a complete piece set",
                    ),
                }),
            };
            tx.send(AppEvent::PieceSetLoaded { set, outcome });
        });
    }

    /// Flip one of the two figurine settings and save it. `eval` selects the
    /// engine PV; otherwise the move list.
    fn toggle_figurines(&mut self, eval: bool) {
        let on = {
            let setting = if eval {
                &mut self.settings.figurines_eval
            } else {
                &mut self.settings.figurines_move_list
            };
            *setting = !*setting;
            *setting
        };
        self.save_settings();
        let place = if eval { "engine evaluation" } else { "move list" };
        let state = if on { "on" } else { "off" };
        self.set_status(
            StatusLevel::Info,
            format!("Figurines in {place}: {state}."),
        );
    }

    /// Show the file picker for a PolyGlot book and read it off the UI thread.
    fn load_book(&mut self) {
        self.set_status(StatusLevel::Info, "Choose a PolyGlot book…".to_string());
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("PolyGlot book", &["bin"])
                .pick_file()
                .await;
            let outcome = match picked {
                None => None,
                Some(handle) => Some(read_book(handle.path().to_path_buf()).await),
            };
            tx.send(AppEvent::BookLoaded { outcome });
        });
    }

    /// Write the current settings to disk, off the UI thread. One writer at a
    /// time: changes made while a write is in flight are saved again when it
    /// finishes, so an older snapshot cannot overwrite a newer one.
    fn save_settings(&mut self) {
        let Some(path) = self.settings_file.clone() else {
            self.set_status(StatusLevel::Warning, "Settings are not saved: there is no home folder.".to_string());
            return;
        };
        if self.settings_saving {
            self.settings_save_pending = true;
            return;
        }
        self.settings_saving = true;
        let snapshot = self.settings.clone();
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let outcome = settings::save_sync(&path, &snapshot);
            tx.send(AppEvent::SettingsSaved { outcome });
        });
    }

    /// Debounce a settings write after a window or panel resize. A drag emits
    /// a size change every frame; only the final size is written.
    fn schedule_settings_save(&mut self) {
        self.settings_save_due = Some(Instant::now() + SETTINGS_SAVE_DEBOUNCE);
    }

    /// Keep the window size in the settings when the user changes it, so the
    /// next launch opens the same size.
    fn save_window_size(&mut self, ctx: &egui::Context) {
        let size = ctx.input(|i| i.viewport().inner_rect.map(|rect| rect.size()));
        let Some(size) = size else {
            return;
        };
        let size = settings::WindowSize {
            width: size.x,
            height: size.y,
        };
        if self.saved_window_size == Some(size) {
            return;
        }
        self.saved_window_size = Some(size);
        self.settings.window_size = Some(size);
        self.schedule_settings_save();
    }

    /// Keep the evaluation and move-list panel sizes in the settings when the
    /// user drags them, so the next launch opens the same layout. The sizes are
    /// rounded to whole points, and clamped to the allowed range, so a frame
    /// during a drag does not write a different value than the stored one.
    fn save_panel_sizes(&mut self, bottom_height: f32, right_width: f32) {
        let sizes = settings::PanelSizes {
            right_width: right_width.round().max(eval_panel::MIN_WIDTH),
            bottom_height: bottom_height.round().max(MIN_MOVE_LIST_HEIGHT),
        };
        if self.settings.panels == Some(sizes) {
            return;
        }
        self.settings.panels = Some(sizes);
        self.schedule_settings_save();
    }

    /// Read the workspace opening book at start-up, if it is there. The Engine
    /// menu can replace it. Reading runs off the UI thread.
    fn load_default_book(&mut self) {
        let Some(path) = default_book_path() else {
            return;
        };
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let outcome = Some(read_book(path).await);
            tx.send(AppEvent::BookLoaded { outcome });
        });
    }

    /// Show the file picker for a PGN file, then read and parse it.
    fn open_pgn(&mut self) {
        self.set_status(StatusLevel::Info, "Choose a PGN file…".to_string());
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("PGN", &["pgn"])
                .pick_file()
                .await;
            let outcome = match picked {
                None => None,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
                    Some(read_pgn(&path).await)
                }
            };
            tx.send(AppEvent::PgnOpened { outcome });
        });
    }

    /// Ask for a destination, then write `contents` there.
    fn save_text(
        &mut self,
        what: &'static str,
        contents: String,
        default_name: &'static str,
        filter: &'static str,
        extensions: &'static [&'static str],
    ) {
        self.set_status(StatusLevel::Info, format!("Choose where to save the {}…", what));
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_file_name(default_name)
                .add_filter(filter, extensions)
                .save_file()
                .await;
            let outcome = match picked {
                None => None,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
                    Some(tokio::fs::write(&path, contents).await.map(|()| path))
                }
            };
            tx.send(AppEvent::FileSaved { what, outcome });
        });
    }

    /// Whether the slot holds an engine that has not exited.
    fn engine_live(&self, slot: usize) -> bool {
        self.engines[slot]
            .as_ref()
            .is_some_and(|host| host.view().status != AnalysisStatus::Offline)
    }

    /// Switch analysis on or off for one slot and point it at the displayed
    /// position. The engine stays loaded either way.
    fn set_engine_analysis(&mut self, slot: usize, enabled: bool) {
        self.analysis_on[slot] = enabled;
        let fen = self.game.fen();
        let goal = self.goal_for(slot);
        if let Some(host) = self.engines[slot].as_mut() {
            host.set_analysis(enabled, &fen, goal);
        }
    }

    /// What `slot` should do with the displayed position. An engine is asked
    /// for a move only when it is on the move at the live end of a game that
    /// is still running. Browsing an earlier position never asks for a move.
    fn goal_for(&self, slot: usize) -> SearchGoal {
        let on_move = self.mode.seat(self.game.turn()) == Seat::Engine(slot);
        if self.live_play() && on_move {
            if self.book_choices().is_empty() {
                return SearchGoal::Move(self.engine_go());
            }
            // The book answers this move, so the engine does not search.
            return SearchGoal::Rest;
        }
        // In a play mode an engine thinks only when it is to move, as in
        // en-croissant: nothing is searched on the other side's time.
        if self.mode == GameMode::Analysis {
            SearchGoal::Analyse
        } else {
            SearchGoal::Rest
        }
    }

    /// Book moves the engine on the move plays instead of searching. Empty
    /// unless book play is on, a play mode is live, an engine is on the move,
    /// and the position is in the book.
    fn book_choices(&self) -> Vec<(Move, u16)> {
        let engine_to_move = matches!(self.mode.seat(self.game.turn()), Seat::Engine(_));
        if !self.book_in_games || !self.live_play() || !engine_to_move || self.game.total_plies() >= BOOK_MAX_PLIES {
            return Vec::new();
        }
        match &self.book {
            Some((_, book)) => self.game.book_moves(book),
            None => Vec::new(),
        }
    }

    /// Play a weighted-random book move for the engine on the move, if the
    /// book has one. One move per frame; the frame asks for another so a run
    /// of book moves keeps going.
    fn play_book_reply(&mut self, ctx: &egui::Context) {
        let choices = self.book_choices();
        let weights: Vec<u16> = choices.iter().map(|(_, weight)| *weight).collect();
        let Some(index) = self.book_picker.pick(&weights) else {
            return;
        };
        let Some(&(m, _)) = choices.get(index) else {
            return;
        };
        let Seat::Engine(slot) = self.mode.seat(self.game.turn()) else {
            return;
        };
        match self.game.apply_move(m) {
            Ok(()) => {
                let san = self
                    .game
                    .active_history()
                    .last()
                    .map(|ply| ply.san.to_string())
                    .unwrap_or_default();
                self.view.clear_selection();
                let mut text = format!("Engine {} played {} from the book.", slot_letter(slot), san);
                if let Some(result) = self.result_text() {
                    text.push_str(&format!(" {}.", result));
                }
                self.set_status(StatusLevel::Info, text);
                ctx.request_repaint();
            }
            Err(err) => {
                log::error!("book move {:?} rejected: {}", m, err);
                self.set_status(StatusLevel::Error, format!("The opening book gave an illegal move: {}", err));
            }
        }
    }

    /// The limits for an engine's next move: its clock when there is one, or
    /// a fixed think time in a game without a clock.
    fn engine_go(&self) -> GoControl {
        self.clock
            .as_ref()
            .map_or(GoControl::MoveTime(ENGINE_MOVE_MS), Clock::go_control)
    }

    /// `true` when the game is in a play mode, at the live end, and not over
    /// (checkmate, draw, resignation, agreed draw, or a flag). Only then do
    /// clocks run and moves get played.
    fn live_play(&self) -> bool {
        self.mode != GameMode::Analysis
            && self.game_started
            && self.game.cur_ply() == self.game.total_plies()
            && !self.game.is_game_over()
            && self.current_ending().is_none()
    }

    /// How the game ended by the game loop, if it has: a resignation, an agreed
    /// draw, or a flag. Checkmate and the other drawn positions are not included.
    fn current_ending(&self) -> Option<Ending> {
        self.ending
            .or_else(|| self.clock_flagged().map(|loser| Ending::OnTime { loser }))
    }

    /// `true` while a game against an engine is running: the human can resign or offer a draw.
    fn vs_engine_live(&self) -> bool {
        matches!(self.mode, GameMode::HumanVsEngine { .. }) && self.live_play()
    }

    /// The colour the human plays, in a game against an engine.
    fn human_colour(&self) -> Option<PieceColor> {
        match self.mode {
            GameMode::HumanVsEngine { human, .. } => Some(human),
            _ => None,
        }
    }

    /// The human resigns. The game ends, and the engine wins.
    fn resign(&mut self) {
        if !self.vs_engine_live() {
            self.set_status(StatusLevel::Warning, "No game against an engine is running.".to_string());
            return;
        }
        if let Some(loser) = self.human_colour() {
            self.ending = Some(Ending::Resigned { loser });
        }
        let text = format!("You resigned. {}.", self.current_ending().map_or_else(String::new, Ending::description));
        self.set_status(StatusLevel::Info, text);
        self.sync_position(true);
    }

    /// The human offers a draw. The engine evaluates the position for a moment,
    /// and accepts when the score is close to equal (see `finish_draw_offer`).
    fn offer_draw(&mut self) {
        if !self.vs_engine_live() {
            self.set_status(StatusLevel::Warning, "No game against an engine is running.".to_string());
            return;
        }
        let slot = self.mode.engine_slots()[0];
        let letter = slot_letter(slot);
        if !self.engine_live(slot) {
            self.set_status(StatusLevel::Error, format!("Engine {} is not running, so it cannot answer.", letter));
            return;
        }
        if !self.analysis_on[slot] {
            self.set_status(
                StatusLevel::Warning,
                format!("Engine {} is switched off, so it cannot judge the position. Switch it on.", letter),
            );
            return;
        }
        let fen = self.game.fen();
        if let Some(host) = self.engines[slot].as_mut() {
            host.set_position(&fen, SearchGoal::Evaluate(GoControl::MoveTime(EVALUATION_MS)));
        }
        self.draw_offer = Some(slot);
        self.set_status(StatusLevel::Info, format!("Engine {} is considering the draw…", letter));
    }

    /// Decide a pending draw offer once the engine's evaluation has arrived. The
    /// offer is dropped when the game ends, or when the engine stops evaluating
    /// (Abort, or the engine is switched off).
    fn finish_draw_offer(&mut self) {
        let Some(slot) = self.draw_offer else {
            return;
        };
        if !self.live_play() || !self.analysis_on[slot] {
            self.draw_offer = None;
            return;
        }
        let Some(result) = self.engines[slot].as_mut().and_then(EngineHost::take_evaluation) else {
            return;
        };
        self.draw_offer = None;
        let letter = slot_letter(slot);
        let white_to_move = self.game.turn() == PieceColor::White;
        let score = result.map(|score| eval_panel::white_point_of_view(score, white_to_move));
        if accepts_draw(score) {
            self.ending = Some(Ending::DrawAgreed);
            self.set_status(StatusLevel::Info, format!("Engine {} accepts the draw.", letter));
        } else if score.is_none() {
            self.set_status(
                StatusLevel::Warning,
                format!("Engine {} has no evaluation of this position, so it declines the draw.", letter),
            );
        } else {
            self.set_status(
                StatusLevel::Warning,
                format!("Engine {} declines the draw: it does not see the position as equal.", letter),
            );
        }
        // Back to the goals of the position: an engine to move searches again.
        self.sync_position(true);
    }

    /// The side that ran out of time, if any.
    fn clock_flagged(&self) -> Option<PieceColor> {
        self.clock.as_ref().and_then(Clock::flagged)
    }

    /// What ends the game, when it has ended: a flag, or the position itself.
    fn result_text(&self) -> Option<String> {
        if let Some(ending) = self.current_ending() {
            return Some(ending.description());
        }
        self.game.is_game_over().then(|| self.game.outcome())
    }

    /// Keep the clock in step with the game and detect a flag. The clock runs
    /// only while [`Self::live_play`] holds. Each ply played since the last
    /// call is one tap: the mover's increment is added and the turn passes.
    fn sync_clock(&mut self, ctx: &egui::Context) {
        let turn = self.game.turn();
        let playing = self.live_play();
        let Some(clock) = self.clock.as_mut() else {
            return;
        };
        let was_flagged = clock.flagged();
        if playing {
            if clock.side != turn {
                clock.tap();
            }
            clock.start();
            // Commits the running side; a side that has run out is flagged here.
            if clock.remaining_ms(clock.side) == 0 {
                clock.pause();
            }
        } else {
            clock.pause();
        }
        let flagged = clock.flagged();
        if playing && flagged.is_none() {
            ctx.request_repaint_after(CLOCK_REFRESH);
        }
        if let (Some(loser), None) = (flagged, was_flagged) {
            self.set_status(
                StatusLevel::Warning,
                format!("{} ran out of time. {} wins on time.", colour_name(loser), colour_name(loser.opposite())),
            );
            // A move search still running for the game now gets the Analyse goal,
            // so it stops, and its move is dropped when it arrives.
            self.sync_position(true);
        }
    }

    /// Start the clock again from full time, for the side to move. Called when
    /// the game is replaced or the mode changes, so no time carries over.
    fn reset_clock(&mut self) {
        let turn = self.game.turn();
        if let Some(clock) = self.clock.as_mut() {
            clock.reset(turn);
        }
    }

    /// Set the time control (`None` = no clock). Clocks start from full time.
    fn set_time_control(&mut self, control: Option<TimeControl>) {
        let text = match control {
            Some(control) => format!("Time control: {}.", control.label()),
            None => "No clock.".to_string(),
        };
        self.clock = control.map(|control| Clock::new(control, self.game.turn()));
        self.settings.time_control = control;
        self.save_settings();
        self.set_status(StatusLevel::Info, text);
        self.sync_position(true);
    }

    /// The two player bars in board order: the one at the top first. Each has
    /// the side's name and, in a play mode with a clock, the clock.
    fn player_bars(&self) -> [PlayerBar; 2] {
        let (top, bottom) = if self.view.white_on_bottom {
            (PieceColor::Black, PieceColor::White)
        } else {
            (PieceColor::White, PieceColor::Black)
        };
        [self.player_bar(top), self.player_bar(bottom)]
    }

    /// One side's bar. The name is the engine's own name (as its handshake
    /// gives it), `You` for the human, or the colour in analysis mode.
    fn player_bar(&self, color: PieceColor) -> PlayerBar {
        let name = match self.mode {
            GameMode::Analysis => colour_name(color).to_string(),
            _ => match self.mode.seat(color) {
                Seat::Human => "You".to_string(),
                Seat::Engine(slot) => self.engine_name(slot),
            },
        };
        let clock = match (&self.clock, self.mode) {
            (Some(clock), mode) if mode != GameMode::Analysis => Some(ClockFace {
                remaining_ms: clock.remaining_ms(color),
                base_ms: clock.control().base_ms,
                active: self.live_play() && clock.side == color,
                flagged: clock.flagged() == Some(color),
            }),
            _ => None,
        };
        PlayerBar { name, clock }
    }

    /// The name of the engine in `slot`: its handshake name, or `Engine A` before that.
    fn engine_name(&self, slot: usize) -> String {
        self.engines[slot]
            .as_ref()
            .and_then(|host| host.view().name)
            .unwrap_or_else(|| format!("Engine {}", slot_letter(slot)))
    }

    /// Switch the session to `mode`. The engines the mode plays with are
    /// switched on, so the game starts without a Start press. A missing engine
    /// is named in the status line. The clocks start from full time.
    fn set_mode(&mut self, mode: GameMode) {
        self.mode = mode;
        self.game_started = false;
        self.view.clear_selection();
        self.reset_clock();
        let mut missing = Vec::new();
        for &slot in mode.engine_slots() {
            if self.engine_live(slot) {
                self.set_engine_analysis(slot, true);
            } else {
                missing.push(slot_letter(slot).to_string());
            }
        }
        self.sync_position(true);
        if missing.is_empty() {
            let text = if mode == GameMode::Analysis {
                format!("{}.", mode.label())
            } else {
                format!("{}. Press Start to begin.", mode.label())
            };
            self.set_status(StatusLevel::Info, text);
        } else {
            self.set_status(
                StatusLevel::Warning,
                format!("{}: load engine {} first.", mode.label(), missing.join(" and ")),
            );
        }
    }

    /// A card's play/pause toggle: switch analysis for that engine only. In a
    /// play mode only the engines the mode plays with have a switch, so an idle
    /// loaded engine cannot be started into a game that does not use it.
    fn toggle_engine(&mut self, slot: usize) {
        let letter = slot_letter(slot);
        if !self.engine_live(slot) {
            self.set_status(StatusLevel::Warning, format!("Engine {} is not loaded.", letter));
            return;
        }
        if self.mode != GameMode::Analysis && !self.mode.engine_slots().contains(&slot) {
            self.set_status(
                StatusLevel::Warning,
                format!("Engine {} is not playing in this game.", letter),
            );
            return;
        }
        let enabled = !self.analysis_on[slot];
        self.set_engine_analysis(slot, enabled);
        let text = if enabled {
            format!("Analysis started for engine {}.", letter)
        } else {
            format!("Analysis stopped for engine {}.", letter)
        };
        self.set_status(StatusLevel::Info, text);
    }

    /// Start (`true`) or Abort (`false`) for every loaded engine. Engines stay
    /// loaded either way; nothing here quits a process.
    fn set_analysis(&mut self, enabled: bool) {
        let playing = self.mode != GameMode::Analysis;
        if playing {
            // Start begins the game; Abort pauses it. The engines are then
            // switched on or off below, with the game state already set.
            self.game_started = enabled;
        }
        let mut live = 0usize;
        for slot in 0..2 {
            if self.engine_live(slot) {
                live += 1;
                // In a play mode Start begins the game for the engines that play
                // it. A merely loaded engine is left switched off (round 9, bug 2).
                let playing_slot = !playing || self.mode.engine_slots().contains(&slot);
                self.set_engine_analysis(slot, enabled && playing_slot);
            } else {
                self.analysis_on[slot] = false;
            }
        }
        let text = match (enabled, playing, live) {
            (true, true, _) if !self.live_play() => {
                return self.set_status(
                    StatusLevel::Warning,
                    "The game cannot go on from here: it is over, or the last move is not shown.".to_string(),
                );
            }
            (true, true, _) => "Game started.".to_string(),
            (false, true, _) => "Game paused. Start continues it.".to_string(),
            (true, false, 0) => {
                return self.set_status(StatusLevel::Warning, "Load an engine first.".to_string());
            }
            (true, false, _) => "Analysis started.".to_string(),
            (false, false, _) => "Analysis stopped.".to_string(),
        };
        self.set_status(StatusLevel::Info, text);
    }

    /// Ask engines to quit, escalating to a kill, without blocking the UI.
    fn quit_processes(&self, processes: Vec<EngineProcess>) {
        for process in processes {
            self.spawn(async move {
                if let Err(err) = process.terminate().await {
                    log::warn!("engine did not accept quit: {}", err);
                }
            });
        }
    }

    /// Replace the status line.
    fn set_status(&mut self, level: StatusLevel, text: String) {
        self.status = text;
        self.status_level = level;
    }

    /// Run a future on the Tokio runtime, detached.
    fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.runtime.spawn(future);
    }

    /// Quit all engines before the window closes (`stop`, `quit`, then kill).
    fn shutdown_engines(&mut self) {
        let processes: Vec<EngineProcess> = self
            .engines
            .iter_mut()
            .flatten()
            .map(EngineHost::shutdown)
            .collect();
        if processes.is_empty() {
            return;
        }
        self.runtime.block_on(async move {
            for process in &processes {
                if let Err(err) = process.terminate().await {
                    log::warn!("engine did not accept quit at exit: {}", err);
                }
            }
            tokio::time::sleep(EXIT_GRACE).await;
        });
    }

    /// Snapshot of the evaluation panel for this frame.
    fn eval_summary(&mut self) -> EvalSummary {
        let fen = self.game.fen();
        let cards = [0usize, 1].map(|slot| {
            let view = self.engines[slot].as_ref().map(EngineHost::view);
            let pv_san = match view.as_ref() {
                Some(v) if !v.stats.pv.is_empty() => {
                    let fresh = self.pv_cache[slot]
                        .as_ref()
                        .is_some_and(|cache| cache.fen == fen && cache.uci == v.stats.pv);
                    if !fresh {
                        let san = Arc::new(self.game.uci_moves_to_san(&v.stats.pv));
                        self.pv_cache[slot] = Some(PvCache {
                            fen: fen.clone(),
                            uci: v.stats.pv.clone(),
                            san: Arc::clone(&san),
                        });
                    }
                    self.pv_cache[slot]
                        .as_ref()
                        .map(|cache| Arc::clone(&cache.san))
                        .unwrap_or_default()
                }
                _ => Arc::new(Vec::new()),
            };
            // A play-mode engine's button says whether the game is running, but
            // only the engines the mode plays with follow the game. An engine
            // that is merely loaded stays at play (round 9, bug 2).
            let playing_slot = self.mode == GameMode::Analysis || self.mode.engine_slots().contains(&slot);
            let running = self.engine_live(slot)
                && playing_slot
                && match self.mode {
                    GameMode::Analysis => self.analysis_on[slot],
                    _ => self.live_play(),
                };
            EvalCard {
                slot,
                view,
                pv_san,
                running,
                toggle_enabled: playing_slot,
            }
        });
        EvalSummary {
            cards,
            white_to_move: self.game.turn() == PieceColor::White,
            fullmove: self.game.fullmove_number(),
            outcome: self.result_text().unwrap_or_else(|| self.game.outcome()),
            book_moves: self.book_moves.clone(),
            book_name: self.book.as_ref().map(|(name, _)| name.clone()),
            book_in_games: self.book_in_games,
            figurines: self.settings.figurines_eval,
            status: self.status.clone(),
            status_level: self.status_level,
        }
    }
}

impl eframe::App for SiliconBoardApp {
    /// Background frame: loading, event draining, engine pumping.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.logic_frame(ctx);
    }

    /// Visual frame: menu and FEN bar on top, evaluation dashboard on the
    /// right, move list at the bottom, board in the centre.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let menu_state = MenuState {
            engine_loaded: [self.engines[0].is_some(), self.engines[1].is_some()],
            mode: self.mode,
            book_in_games: self.book_in_games,
            vs_engine_live: self.vs_engine_live(),
            options_ready: [0, 1].map(|slot| {
                self.engine_live(slot)
                    && self.engines[slot]
                        .as_ref()
                        .is_some_and(|host| host.view().status != AnalysisStatus::Starting)
            }),
            book_name: self.book.as_ref().map(|(name, _)| name.as_str()),
            figurines_move_list: self.settings.figurines_move_list,
            figurines_eval: self.settings.figurines_eval,
            board_themes: &self.board_themes,
            board_theme: Some(
                self.settings
                    .board_theme
                    .as_deref()
                    .unwrap_or(crate::assets::DEFAULT_BOARD_IMAGE),
            ),
            piece_sets: &self.piece_sets,
            piece_set: Some(
                self.settings
                    .piece_set
                    .as_deref()
                    .unwrap_or(crate::assets::DEFAULT_PIECE_SET),
            ),
            ply_count: self.game.total_plies(),
        };
        let human_to_move = match self.mode {
            GameMode::Analysis => true,
            _ => {
                self.game_started
                    && self.mode.seat(self.game.turn()) == Seat::Human
                    && self.current_ending().is_none()
            }
        };
        let bars = self.player_bars();
        let mut requested: Option<MenuAction> = None;
        let fen_input = &mut self.fen_input;

        egui::Panel::top("top_bar").show(ui, |ui| {
            if let Some(action) = menu_bar::show(ui, &menu_state, fen_input) {
                requested = Some(action);
            }
        });

        let follow = self.game.cur_ply() != self.last_ply;
        self.last_ply = self.game.cur_ply();
        let figurines_move_list = self.settings.figurines_move_list;
        let revision = self.game.revision();
        if self.pgn_cache.as_ref().is_none_or(|(rev, _)| *rev != revision) {
            self.pgn_cache = Some((revision, self.game.pgn_lines()));
        }
        let (cached_revision, lines) = self
            .pgn_cache
            .take()
            .unwrap_or((revision, Vec::new()));
        let refresh_layouts = self
            .pgn_galleys
            .as_ref()
            .is_none_or(|(rev, figs, _)| *rev != revision || *figs != figurines_move_list);
        if refresh_layouts {
            let mut galleys = vec![None; self.game.total_plies() + 1];
            for line in &lines {
                for (entry, color) in [
                    (&line.white, theme::WHITE_MOVE),
                    (&line.black, theme::BLACK_MOVE),
                ] {
                    let Some(entry) = entry else { continue };
                    let job = crate::gui::figurines::san_job(
                        ui,
                        &entry.san,
                        color,
                        figurines_move_list,
                    );
                    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
                    if let Some(slot) = galleys.get_mut(entry.ply) {
                        *slot = Some(galley);
                    }
                }
            }
            self.pgn_galleys = Some((revision, figurines_move_list, galleys));
        }
        let (galley_revision, galley_figurines, galleys) = self
            .pgn_galleys
            .take()
            .unwrap_or((revision, figurines_move_list, Vec::new()));
        let move_list_height = self
            .settings
            .panels
            .map_or(DEFAULT_MOVE_LIST_HEIGHT, |sizes| sizes.bottom_height);
        let move_list = egui::Panel::bottom("move_list")
            .resizable(true)
            .default_size(move_list_height)
            .min_size(MIN_MOVE_LIST_HEIGHT)
            .show(ui, |ui| {
                pgn_panel::show(
                    ui,
                    &mut self.game,
                    &lines,
                    &galleys,
                    follow,
                    figurines_move_list,
                )
            });
        self.pgn_cache = Some((cached_revision, lines));
        self.pgn_galleys = Some((galley_revision, galley_figurines, galleys));

        let summary = self.eval_summary();
        let dashboard_width = self
            .settings
            .panels
            .map_or(eval_panel::MIN_WIDTH, |sizes| sizes.right_width);
        let dashboard = egui::Panel::right("evaluation")
            .resizable(true)
            .default_size(dashboard_width)
            .min_size(eval_panel::MIN_WIDTH)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(action) = eval_panel::show(ui, &summary) {
                            requested = Some(action);
                        }
                    });
            });
        self.save_panel_sizes(
            move_list.response.rect.height(),
            dashboard.response.rect.width(),
        );

        if let Some(dialog) = self.options_dialog.as_mut() {
            let slot = dialog.slot();
            match dialog.show(&ctx) {
                OptionsAction::None => {}
                OptionsAction::Close => self.options_dialog = None,
                OptionsAction::Set { name, value } => self.set_engine_option(slot, &name, &value),
                OptionsAction::Reset { name, value } => self.reset_engine_option(slot, &name, &value),
                OptionsAction::Press { name } => self.press_engine_button(slot, &name),
            }
        }

        if let Some(dialog) = self.time_dialog.as_mut() {
            match dialog.show(&ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancel => self.time_dialog = None,
                DialogOutcome::Apply(control) => {
                    self.time_dialog = None;
                    self.set_time_control(control);
                }
            }
        }

        let mut board_action = BoardAction::None;
        egui::CentralPanel::default().show(ui, |ui| {
            // en-croissant layout: a player bar above and below the board. The top
            // bar belongs to the side at the top, so flipping the board swaps them.
            let area = ui.available_rect_before_wrap();
            ui.allocate_rect(area, egui::Sense::hover());
            let (top_bar, board_rect, bottom_bar) = time_control::split_board_area(area);
            time_control::paint_bar(ui, top_bar, &bars[0]);
            time_control::paint_bar(ui, bottom_bar, &bars[1]);
            ui.scope_builder(egui::UiBuilder::new().max_rect(board_rect), |ui| match &self.assets {
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label("Loading piece and board assets…");
                    });
                }
                Some(Err(err)) => {
                    ui.colored_label(theme::DANGER_RED, "Asset load failed:");
                    ui.label(err.to_string());
                }
                Some(Ok(assets)) => {
                    board_action = board_view::show(ui, &self.game, assets, &mut self.view, human_to_move);
                }
            });
        });

        self.apply_board_action(board_action);
        if let Some(action) = requested {
            self.perform(action, &ctx);
        }
        self.record_frame();
    }

    /// Stop every engine before the process exits, so none is left orphaned.
    fn on_exit(&mut self) {
        self.shutdown_engines();
        // The window is gone now, so the size is written here, synchronously.
        if let Some(path) = &self.settings_file {
            if let Err(err) = settings::save_sync(path, &self.settings) {
                log::error!("{}", err);
            }
        }
    }
}

/// `A` for slot 0, `B` for slot 1.
fn slot_letter(slot: usize) -> char {
    char::from(b'A' + slot as u8)
}

/// `White` or `Black`, as shown to the user.
fn colour_name(color: PieceColor) -> &'static str {
    match color {
        PieceColor::White => "White",
        PieceColor::Black => "Black",
    }
}

/// Opening book read at start-up when present, relative to the workspace.
const DEFAULT_BOOK: &str = "gm2001.bin";

/// The start-up book: `SILICONBOARD_BOOK` if set, else `books/gm2001.bin` in
/// the nearest folder above the executable or the working directory.
fn default_book_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("SILICONBOARD_BOOK").map(PathBuf::from) {
        return path.is_file().then_some(path);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(exe.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.extend(cwd.ancestors().map(Path::to_path_buf));
    }
    roots
        .into_iter()
        .map(|dir| dir.join("books").join(DEFAULT_BOOK))
        .find(|path| path.is_file())
}

/// Read a PolyGlot book on a blocking thread. Returns the file name and the result.
async fn read_book(path: PathBuf) -> (String, Result<PolyglotBook, BookError>) {
    let name = file_name_of(&path);
    let loader_path = path.clone();
    let joined = tokio::task::spawn_blocking(move || PolyglotBook::open(&loader_path)).await;
    let result = match joined {
        Ok(result) => result,
        Err(join_error) => Err(BookError::Open {
            path,
            source: std::io::Error::other(join_error),
        }),
    };
    (name, result)
}

/// File name for display, falling back to the full path.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Read and parse a PGN file (runs on the Tokio runtime).
async fn read_pgn(path: &Path) -> Result<(String, PgnImport), OpenPgnError> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|source| OpenPgnError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let import = pgn::parse_pgn_with_report(&text).map_err(OpenPgnError::Parse)?;
    Ok((file_name_of(path), import))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_wakes_are_limited_to_about_thirty_per_second() {
        assert!(!wakes_now(false, Duration::ZERO), "a second info line in the same frame waits");
        assert!(!wakes_now(false, ENGINE_WAKE_INTERVAL - Duration::from_millis(1)));
        assert!(wakes_now(false, ENGINE_WAKE_INTERVAL), "the next slot after 33 ms is allowed");
        assert!(
            wakes_now(true, Duration::ZERO),
            "bestmove, handshake and exit never wait for the slot"
        );
    }

    #[test]
    fn frame_timer_reports_one_second_of_frames() {
        let start = Instant::now();
        let cost = Duration::from_millis(2);
        let mut timer = FrameTimer::new_at(start);
        // The first active frame starts the window.
        assert!(timer
            .record_at(start + Duration::from_millis(16), cost, true)
            .is_none());
        let mut summary = None;
        for i in 2..=64u32 {
            summary = timer.record_at(start + Duration::from_millis(16 * u64::from(i)), cost, true);
            if i < 64 {
                assert!(summary.is_none(), "no summary before the window closes");
            }
        }
        let summary = summary.expect("one second of frames closes the window");
        assert_eq!(summary.frames, 63);
        assert_eq!(summary.average_cost, cost);
        assert_eq!(summary.worst_cost, cost);
        assert_eq!(summary.average_gap, Duration::from_millis(16));
        assert_eq!(summary.worst_gap, Duration::from_millis(16));
    }

    #[test]
    fn frame_timer_ignores_idle_time() {
        let start = Instant::now();
        let cost = Duration::from_millis(2);
        let mut timer = FrameTimer::new_at(start);
        // Ten seconds with no search: nothing is measured.
        assert!(timer
            .record_at(start + Duration::from_secs(10), cost, false)
            .is_none());
        // The first active frame after the pause only starts the window.
        assert!(timer
            .record_at(start + Duration::from_secs(11), cost, true)
            .is_none());
        let mut summary = None;
        for i in 1..=63u32 {
            summary = timer.record_at(
                start + Duration::from_secs(11) + Duration::from_millis(16 * u64::from(i)),
                cost,
                true,
            );
        }
        let summary = summary.expect("active frames are measured again after an idle gap");
        assert_eq!(summary.frames, 63);
        assert_eq!(summary.average_gap, Duration::from_millis(16));
    }

    #[test]
    fn start_up_book_is_the_workspace_gm2001_book() {
        let path = default_book_path().expect("books/gm2001.bin must be found from the workspace");
        assert!(path.ends_with("books/gm2001.bin"), "found {}", path.display());
    }
}


#[cfg(test)]
mod game_flow_tests {
    use super::*;
    use crate::chess::clock::TimeControl;
    use crate::chess::opening_book::PolyglotBook;
    use crate::engine::{AnalysisStatus, EngineHost, EngineProcess, SearchGoal};

    /// An app with Stockfish in both slots and no book. It never reads or
    /// writes the user's settings file.
    fn app_with_two_engines() -> SiliconBoardApp {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let mut app = SiliconBoardApp::unloaded(runtime, egui::Context::default());
        let binary = crate::engine::test_engine_path();
        for slot in 0..2 {
            let process = app.runtime.block_on(EngineProcess::spawn(&binary)).unwrap();
            app.engines[slot] = Some(EngineHost::attach_with_settings(process, BTreeMap::new()));
        }
        app
    }

    /// Run frames until `done` holds, or give up after 30 s.
    fn run_until(app: &mut SiliconBoardApp, ctx: &egui::Context, done: impl Fn(&SiliconBoardApp) -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            app.logic_frame(ctx);
            if done(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn engine_game_waits_for_start_then_only_the_side_to_move_thinks() {
        let mut app = app_with_two_engines();
        let ctx = egui::Context::default();
        app.set_time_control(Some(TimeControl::fischer(60_000, 0)));
        app.set_mode(GameMode::EngineVsEngine);
        assert!(
            run_until(&mut app, &ctx, |a| a
                .engines
                .iter()
                .flatten()
                .all(|h| h.view().status == AnalysisStatus::Ready)),
            "both engines must finish their handshake"
        );

        // Before Start nothing happens: no move, no clock, no search.
        let until = Instant::now() + Duration::from_millis(1_500);
        while Instant::now() < until {
            app.logic_frame(&ctx);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.game.total_plies(), 0, "no move before Start");
        assert_eq!(app.clock.as_ref().map(|c| c.remaining_ms(PieceColor::White)), Some(60_000));
        assert!(
            app.engines.iter().flatten().all(|h| h.view().status == AnalysisStatus::Ready),
            "no engine thinks before Start"
        );

        app.set_analysis(true);
        assert!(run_until(&mut app, &ctx, |a| a.game.total_plies() >= 4), "the game goes on after Start");
        assert!(
            app.clock.as_ref().is_some_and(|c| c.remaining_ms(PieceColor::White) < 60_000),
            "the clock runs once the game has started"
        );
    }

    #[test]
    fn the_engine_rests_while_the_human_is_to_move() {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let mut app = SiliconBoardApp::unloaded(runtime, egui::Context::default());
        app.mode = GameMode::HumanVsEngine {
            human: PieceColor::White,
            engine: 0,
        };
        app.game_started = true;
        assert!(matches!(app.goal_for(0), SearchGoal::Rest), "White is the human: engine A waits");
        app.game.apply_uci("e2e4").unwrap();
        assert!(matches!(app.goal_for(0), SearchGoal::Move(_)), "Black to move: engine A searches");
        app.game_started = false;
        assert!(matches!(app.goal_for(0), SearchGoal::Rest), "before Start the engine rests");
    }

    #[test]
    fn book_replies_stop_after_the_opening_depth() {
        let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let mut app = SiliconBoardApp::unloaded(runtime, egui::Context::default());
        let book = PolyglotBook::open(std::path::Path::new("../books/gm2001.bin"))
            .expect("the workspace fixture books/gm2001.bin must be present");
        app.book = Some(("gm2001.bin".to_string(), book));
        app.mode = GameMode::EngineVsEngine;
        app.game_started = true;
        assert!(!app.book_choices().is_empty(), "the book answers the first move");

        // Forty plies of knight moves that return to the start position.
        for _ in 0..10 {
            for uci in ["g1f3", "g8f6", "f3g1", "f6g8"] {
                app.game.apply_uci(uci).unwrap();
            }
        }
        assert_eq!(app.game.total_plies(), BOOK_MAX_PLIES);
        assert!(app.book_choices().is_empty(), "the book stops at the opening depth");
    }

    #[test]
    fn a_play_mode_switches_only_the_engine_that_plays() {
        let mut app = app_with_two_engines();
        app.mode = GameMode::HumanVsEngine {
            human: PieceColor::White,
            engine: 0,
        };
        app.game_started = true;
        let cards = app.eval_summary().cards;
        assert!(cards[0].running, "engine A plays this game");
        assert!(!cards[1].running, "engine B is only loaded, not playing");
        assert!(!cards[1].toggle_enabled, "engine B has no game switch here");

        // Black against engine B: only B follows the game.
        app.mode = GameMode::HumanVsEngine {
            human: PieceColor::Black,
            engine: 1,
        };
        let cards = app.eval_summary().cards;
        assert!(!cards[0].running);
        assert!(cards[1].running);

        // Both engines play each other.
        app.mode = GameMode::EngineVsEngine;
        let cards = app.eval_summary().cards;
        assert!(cards[0].running && cards[1].running);

        // Nothing is running before Start.
        app.game_started = false;
        let cards = app.eval_summary().cards;
        assert!(!cards[0].running && !cards[1].running);
    }

    #[test]
    fn a_play_engine_switches_off_when_its_game_ends() {
        let mut app = SiliconBoardApp::unloaded(
            tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap(),
            egui::Context::default(),
        );
        app.mode = GameMode::HumanVsEngine {
            human: PieceColor::White,
            engine: 0,
        };
        let ctx = egui::Context::default();
        app.game_started = true;
        app.analysis_on[0] = true;
        app.game_was_live = true;

        // The human resigns: the game is over, so the engine is switched off.
        app.ending = Some(Ending::Resigned {
            loser: PieceColor::White,
        });
        app.logic_frame(&ctx);
        assert!(!app.analysis_on[0], "a finished game leaves the engine switched off");
        assert!(!app.game_was_live);

        // A new game also waits for Start, with the engine switched off.
        app.game_started = true;
        app.ending = None;
        app.game_was_live = true;
        app.analysis_on[0] = true;
        app.game = GameState::new();
        app.game_started = false;
        app.logic_frame(&ctx);
        assert!(!app.analysis_on[0], "a new game waits for Start");
    }
}
