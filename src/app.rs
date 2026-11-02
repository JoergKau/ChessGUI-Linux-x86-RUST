//! Application shell: owns the game, the engines and the opening book, lays
//! out the panels, and performs the actions the widgets report.
//!
//! Threading: the egui thread never waits on anything. Slow work (file
//! dialogs, spawning an engine, reading a book or a PGN, writing a file) runs
//! on the Tokio runtime owned by [`SiliconBoardApp`] and reports back as an
//! [`AppEvent`], which [`SiliconBoardApp::drain_app_events`] picks up each frame.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Key};
use tokio::sync::mpsc;

use crate::assets::{AssetError, Assets};
use crate::chess::pgn::{self, PgnError, PgnImport};
use crate::chess::{BookError, GameState, PieceColor, PolyglotBook};
use crate::engine::{AnalysisStatus, EngineError, EngineHost, EngineProcess};
use crate::gui::board_view::{self, BoardAction, BoardView};
use crate::gui::eval_panel::{self, EvalCard, EvalSummary};
use crate::gui::menu_bar::{self, MenuAction, MenuState};
use crate::gui::{pgn_panel, theme};

/// How long engines get to quit on their own at app exit before they are killed.
const EXIT_GRACE: Duration = Duration::from_millis(600);

/// How long a started engine may take to answer `uci` before it is declared
/// not to be a UCI engine.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// A file was written.
    FileSaved {
        what: &'static str,
        outcome: Option<std::io::Result<PathBuf>>,
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
    events_tx: mpsc::UnboundedSender<AppEvent>,
    /// Receiver drained once per frame in [`SiliconBoardApp::logic`].
    events_rx: mpsc::UnboundedReceiver<AppEvent>,
    /// Textures, loaded on the first frame. `None` until then.
    assets: Option<Result<Assets, AssetError>>,
    /// The game: position, history and cursor.
    game: GameState,
    /// Board widget state (interaction, orientation).
    view: BoardView,
    /// Engine slots: A and B.
    engines: [Option<EngineHost>; 2],
    /// `true` while a file dialog or spawn for a slot is in flight.
    engine_loading: [bool; 2],
    /// Analysis switch from the Engine menu.
    analysis_enabled: bool,
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
    /// Last engine status seen per slot, to report start-up outcomes once.
    engine_status: [Option<AnalysisStatus>; 2],
    /// When each slot's engine was spawned, for the handshake timeout.
    engine_started: [Option<Instant>; 2],
    /// The FEN text in the top bar. Follows the position unless the user edits it.
    fen_input: String,
}

impl SiliconBoardApp {
    /// Create the application with the given Tokio runtime.
    pub fn new(runtime: tokio::runtime::Runtime) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            runtime,
            events_tx,
            events_rx,
            assets: None,
            game: GameState::new(),
            view: BoardView::default(),
            engines: [None, None],
            engine_loading: [false, false],
            analysis_enabled: false,
            book: None,
            book_moves: Vec::new(),
            synced_fen: String::new(),
            last_ply: 0,
            status: "Welcome — click or drag a piece to make a move.".to_string(),
            engine_status: [None, None],
            engine_started: [None, None],
            fen_input: GameState::new().fen(),
        }
    }

    // ── Per-frame work ─────────────────────────────────────────────────────

    /// Non-visual work for the frame: load assets once, apply background
    /// results, pump engine output, keep engines in sync, handle arrow keys.
    fn logic_frame(&mut self, ctx: &egui::Context) {
        if self.assets.is_none() {
            let loaded = Assets::load(ctx);
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
        self.report_engine_start_up();
        self.sync_position(false);
        self.handle_navigation_keys(ctx);
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
                self.status = format!("Engine {} is not a UCI engine: {} ({}).", letter, binary, reason);
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
                self.status = format!(
                    "Engine {} ready: {}",
                    letter,
                    now.name.as_deref().unwrap_or(&now.binary_name)
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
                        None => self.status = "Engine selection cancelled.".to_string(),
                        Some(Ok(process)) => {
                            let binary = process.binary().display().to_string();
                            if let Some(mut old) = self.engines[slot].take() {
                                let stale = old.shutdown();
                                self.quit_processes(vec![stale]);
                            }
                            let mut host = EngineHost::attach(process);
                            host.set_analysis(self.analysis_enabled, &self.game.fen());
                            self.engines[slot] = Some(host);
                            self.engine_status[slot] = None;
                            self.engine_started[slot] = Some(Instant::now());
                            self.status = format!("Loading engine {}: {}…", slot_letter(slot), binary);
                        }
                        Some(Err(err)) => {
                            log::error!("engine spawn failed: {}", err);
                            self.status = format!("Could not start engine {}: {}", slot_letter(slot), err);
                        }
                    }
                }
                AppEvent::BookLoaded { outcome } => match outcome {
                    None => self.status = "Book selection cancelled.".to_string(),
                    Some((name, Ok(book))) if book.is_empty() => {
                        self.status = format!("Opening book {} has no records.", name);
                    }
                    Some((name, Ok(book))) => {
                        self.status = format!("Opening book loaded: {} ({} records)", name, book.len());
                        self.book = Some((name, book));
                        self.sync_position(true);
                    }
                    Some((name, Err(err))) => {
                        self.status = format!("Could not load book {}: {}", name, err);
                    }
                },
                AppEvent::PgnOpened { outcome } => match outcome {
                    None => self.status = "Open cancelled.".to_string(),
                    Some(Ok((name, import))) => {
                        self.game = import.game;
                        self.view.clear_selection();
                        self.status = match import.ignored.summary() {
                            Some(note) => format!("Opened {} ({})", name, note),
                            None => format!("Opened {}", name),
                        };
                    }
                    Some(Err(err)) => {
                        log::error!("PGN open failed: {}", err);
                        self.status = err.to_string();
                    }
                },
                AppEvent::FileSaved { what, outcome } => match outcome {
                    None => self.status = format!("{} save cancelled.", what),
                    Some(Ok(path)) => self.status = format!("Saved {} to {}", what, path.display()),
                    Some(Err(err)) => {
                        log::error!("saving {} failed: {}", what, err);
                        self.status = format!("Could not save {}: {}", what, err);
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
        for host in self.engines.iter_mut().flatten() {
            host.set_position(&fen);
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
                self.view.clear_selection();
                for host in self.engines.iter_mut().flatten() {
                    host.new_game();
                }
                self.status = "New game.".to_string();
            }
            MenuAction::FlipBoard => self.view.flip(),
            MenuAction::OpenPgn => self.open_pgn(),
            MenuAction::SavePgn => {
                let text = pgn::to_pgn(&self.game);
                self.save_text("PGN", text, "game.pgn", "PGN", &["pgn"]);
            }
            MenuAction::CopyPgn => {
                ctx.copy_text(pgn::to_pgn(&self.game));
                self.status = "PGN copied to the clipboard.".to_string();
            }
            MenuAction::CopyFen => {
                ctx.copy_text(self.game.fen());
                self.status = "FEN copied to the clipboard.".to_string();
            }
            MenuAction::SaveFen => {
                let text = format!("{}\n", self.game.fen());
                self.save_text("FEN", text, "position.fen", "FEN", &["fen"]);
            }
            MenuAction::LoadEngine(slot) => self.load_engine(slot),
            MenuAction::ToggleAnalysis => self.set_analysis(!self.analysis_enabled),
            MenuAction::StartAnalysis => self.set_analysis(true),
            MenuAction::StopAnalysis => self.set_analysis(false),
            MenuAction::LoadFen => match GameState::from_fen(self.fen_input.trim()) {
                Ok(game) => {
                    self.game = game;
                    self.view.clear_selection();
                    self.status = "Position loaded from FEN.".to_string();
                }
                Err(err) => self.status = format!("Invalid FEN: {}", err),
            },
            MenuAction::LoadBook => self.load_book(),
        }
    }

    /// Apply the move (or selection change) the board widget requested.
    fn apply_board_action(&mut self, action: BoardAction) {
        match action {
            BoardAction::None => {}
            BoardAction::PieceSelected { from } => {
                self.status = format!("Selected {}.", from);
            }
            BoardAction::RequestMove { m } => match self.game.apply_move(m) {
                Ok(()) => {
                    let played = self
                        .game
                        .active_history()
                        .last()
                        .map(|ply| ply.san.to_string())
                        .unwrap_or_default();
                    self.status = if self.game.is_game_over() {
                        format!("Played {}. {}.", played, self.game.outcome())
                    } else {
                        format!("Played {}.", played)
                    };
                }
                Err(err) => self.status = format!("Move rejected: {}", err),
            },
            BoardAction::PromotionChoice { .. } => {
                self.status = "Promotion: pick Q / R / B / N on the board (Esc cancels).".to_string();
            }
        }
    }

    /// Show the file picker for an engine and spawn the chosen binary.
    fn load_engine(&mut self, slot: usize) {
        if self.engine_loading[slot] {
            return;
        }
        self.engine_loading[slot] = true;
        self.status = format!("Choose an engine for slot {}…", slot_letter(slot));
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Choose a UCI engine")
                .pick_file()
                .await;
            let outcome = match picked {
                None => None,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
                    Some(EngineProcess::spawn(&path).await)
                }
            };
            let _ = tx.send(AppEvent::EngineSpawned { slot, outcome });
        });
    }

    /// Show the file picker for a PolyGlot book and read it off the UI thread.
    fn load_book(&mut self) {
        self.status = "Choose a PolyGlot book…".to_string();
        let tx = self.events_tx.clone();
        self.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .add_filter("PolyGlot book", &["bin"])
                .pick_file()
                .await;
            let outcome = match picked {
                None => None,
                Some(handle) => {
                    let path = handle.path().to_path_buf();
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
                    Some((name, result))
                }
            };
            let _ = tx.send(AppEvent::BookLoaded { outcome });
        });
    }

    /// Show the file picker for a PGN file, then read and parse it.
    fn open_pgn(&mut self) {
        self.status = "Choose a PGN file…".to_string();
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
            let _ = tx.send(AppEvent::PgnOpened { outcome });
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
        self.status = format!("Choose where to save the {}…", what);
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
            let _ = tx.send(AppEvent::FileSaved { what, outcome });
        });
    }

    /// Switch analysis on (Start) or off (Abort) for every running engine.
    /// Engines stay loaded either way; nothing here quits a process.
    fn set_analysis(&mut self, enabled: bool) {
        self.analysis_enabled = enabled;
        let fen = self.game.fen();
        let mut running = 0usize;
        for host in self.engines.iter_mut().flatten() {
            if host.view().status != AnalysisStatus::Offline {
                running += 1;
                host.set_analysis(enabled, &fen);
            }
        }
        self.status = match (enabled, running) {
            (true, 0) => "Load an engine first.".to_string(),
            (true, _) => "Analysis started.".to_string(),
            (false, _) => "Analysis stopped.".to_string(),
        };
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
    fn eval_summary(&self) -> EvalSummary {
        let cards = [0usize, 1].map(|slot| {
            let view = self.engines[slot].as_ref().map(EngineHost::view);
            let pv_san = view
                .as_ref()
                .map(|v| self.game.uci_moves_to_san(&v.stats.pv))
                .unwrap_or_default();
            EvalCard { slot, view, pv_san }
        });
        EvalSummary {
            cards,
            white_to_move: self.game.turn() == PieceColor::White,
            outcome: self.game.outcome(),
            book_moves: self.book_moves.clone(),
            book_name: self.book.as_ref().map(|(name, _)| name.clone()),
            status: self.status.clone(),
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
            analysis_enabled: self.analysis_enabled,
            engine_loaded: [self.engines[0].is_some(), self.engines[1].is_some()],
            book_name: self.book.as_ref().map(|(name, _)| name.as_str()),
            ply_count: self.game.total_plies(),
        };
        let mut requested: Option<MenuAction> = None;
        let fen_input = &mut self.fen_input;

        egui::Panel::top("top_bar").show(ui, |ui| {
            if let Some(action) = menu_bar::show(ui, &menu_state, fen_input) {
                requested = Some(action);
            }
        });

        let follow = self.game.cur_ply() != self.last_ply;
        self.last_ply = self.game.cur_ply();
        egui::Panel::bottom("move_list")
            .resizable(true)
            .default_size(190.0)
            .show(ui, |ui| pgn_panel::show(ui, &mut self.game, follow));

        let summary = self.eval_summary();
        egui::Panel::right("evaluation")
            .resizable(true)
            .default_size(340.0)
            .min_size(280.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let Some(action) = eval_panel::show(ui, &summary) {
                            requested = Some(action);
                        }
                    });
            });

        let mut board_action = BoardAction::None;
        egui::CentralPanel::default().show(ui, |ui| match &self.assets {
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
                board_action = board_view::show(ui, &self.game, assets, &mut self.view);
            }
        });

        self.apply_board_action(board_action);
        if let Some(action) = requested {
            self.perform(action, &ctx);
        }
    }

    /// Stop every engine before the process exits, so none is left orphaned.
    fn on_exit(&mut self) {
        self.shutdown_engines();
    }
}

/// `A` for slot 0, `B` for slot 1.
fn slot_letter(slot: usize) -> char {
    char::from(b'A' + slot as u8)
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
