//! Chess board rendering and interaction (Phase 4, extended in Phase 5).
//!
//! The board widget is egui-native and refreshes per frame. It paints:
//!
//! 1. the blue-board texture from `public/images/board/blue.png` (v0.1
//!    hardcoded; the board picker is a backlog feature),
//! 2. overlays — the currently selected square, legal destinations (with
//!    different treatment for quiet vs. capture moves), the last played
//!    move's from/to, and a red highlight for a checked king. Square
//!    coordinates are not drawn: they were misplaced and are removed for now,
//! 3. every piece sprite from the cburnett SVG set rasterised at startup
//!    (`crate::assets::Assets::pieces`),
//! 5. a lifted copy of a piece being dragged at the cursor's square,
//! 6. a promotion modal (Q / R / B / N over the destination square) when a
//!    pawn-to-back-rank move is pending; ESC or a click outside cancels.
//!
//! Interaction state machine:
//!
//! ```text
//!  click own piece ─────┐          click dest ┌─────────────┐
//!              ┌────────▼───────┐   play     │  modal ↔    │
//!   Idle ─────►│ Selected{from} │──────► RequestMove      │
//!              └──────┬─────────┘                          │
//!    drag start       │ click own piece ──► re-select      │
//!              ┌──────▼─────────┐                          │
//!              │ Dragging{from, │  drag-stopped on         │
//!              │        cursor} │  dest ───► RequestMove   │
//!              └────────────────┘                          │
//!                     click promotion dest ──► Promoting ──┘
//! ```
//!
//! The widget itself never mutates [`GameState`]; it returns a
//! [`BoardAction`] snapshot and the caller decides what to do. Input is only
//! accepted while the newest position is shown: when the user browses the
//! history, the board is read-only (there is no variation tree to branch
//! into).
//!
//! # Castling input note
//! shakmaty's `Move::Castle { king, rook }.to()` returns the **rook** square
//! (e.g. `h1` for O-O), but a human clicks the *king's* destination (`g1`).
//! [`crate::chess::game_state::target_square`] maps a castle move onto the
//! king's destination so click-click, drag-drop, and the legal-move overlays
//! all behave correctly.

use eframe::egui::{
    self, Color32, Key, Painter, Pos2, Rect, Sense, TextureId, Ui, Vec2,
    load::SizedTexture,
};
use shakmaty::{Board, File, Move, Piece, Rank, Square};

use crate::assets::{Assets, PieceId, PieceTextures};
use crate::chess::game_state::target_square;
use crate::chess::{GameState, PieceColor, PieceKind};

// ─────────────────────────────────────────────────────────────────────────────
// Visual constants
// ─────────────────────────────────────────────────────────────────────────────

/// Translucent gold fill for the currently selected square.
const SELECTED_FILL: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xd5, 0x40, 0x55);

/// Softer gold for the from-and-to squares of the last move.
const LAST_MOVE_FILL: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xd5, 0x40, 0x30);

/// Slightly darker red tint for a checked king.
const CHECK_FILL: Color32 = Color32::from_rgba_unmultiplied_const(0xe0, 0x40, 0x40, 0x50);

/// Dotted centre indicator for a quiet legal move.
const QUIET_MOVE_DOT: Color32 = Color32::from_rgba_unmultiplied_const(0x20, 0x20, 0x20, 0x50);

/// Perimeter ring colour for a capture.
const CAPTURE_RING: Color32 = Color32::from_rgba_unmultiplied_const(0xc0, 0x40, 0x40, 0x60);

/// Radius of the quiet-move dot, as a fraction of the square's width.
const QUIET_DOT_RADIUS: f32 = 0.15;

/// Ring stroke width for a capture destination.
const CAPTURE_RING_THICKNESS: f32 = 0.03;

/// Backdrop behind each promotion-modal icon (semi-opaque dark so the
/// piece strokes read on any board theme).
const PROMO_BACKDROP: Color32 = Color32::from_rgba_unmultiplied_const(0x26, 0x26, 0x26, 0xd8);

/// Hover tint on a promotion-modal icon (drawn over [`PROMO_BACKDROP`]).
const PROMO_HOVER: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xd5, 0x40, 0x40);

/// Tint for the square the dragged piece currently hovers over.
const DRAG_HOVER_FILL: Color32 = Color32::from_rgba_unmultiplied_const(0xdd, 0xdd, 0xdd, 0x28);

// ─────────────────────────────────────────────────────────────────────────────
// Interaction state machine
// ─────────────────────────────────────────────────────────────────────────────

/// Which half of the click-drag-promote cycle the widget is in.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Interaction {
    /// Board is idle; the next click or drag selects a source piece.
    #[default]
    Idle,
    /// A piece is selected. A click on a legal destination plays; on
    /// another own piece it re-selects; anywhere else it deselects.
    Selected { from: Square },
    /// The user is dragging a piece. `from` keeps the origin; `cursor` is
    /// the square the pointer is currently over (a board-level resolver
    /// updates it each frame — see BUG-4-03).
    Dragging { from: Square, cursor: Square },
    /// A promotion modal is pending; the user must pick a promotion piece
    /// before the widget returns `RequestMove`. ESC / click-outside cancels.
    Promoting { from: Square, to: Square },
}

impl Interaction {
    /// If the user has picked a source square (Selected / Dragging /
    /// Promoting), it's reflected here. `Idle` returns `None`.
    pub fn source_square(self) -> Option<Square> {
        match self {
            Interaction::Idle => None,
            Interaction::Selected { from } | Interaction::Dragging { from, .. } => Some(from),
            Interaction::Promoting { from, .. } => Some(from),
        }
    }

    /// The square the dragged piece currently floats over (dragging only).
    pub fn drag_cursor(self) -> Option<Square> {
        match self {
            Interaction::Dragging { cursor, .. } => Some(cursor),
            _ => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-frame widget view state
// ─────────────────────────────────────────────────────────────────────────────

/// View-side state for one [`BoardView::show`] call.
#[derive(Debug, PartialEq, Eq)]
pub struct BoardView {
    /// Current interaction stage.
    pub interaction: Interaction,
    /// White at the bottom? Black is at the bottom after [`BoardView::flip`].
    pub white_on_bottom: bool,
    /// Cached legal moves for the selected square.
    legal: Option<LegalCache>,
}

/// Legal moves for one selected square at one recorded position.
#[derive(Debug, PartialEq, Eq)]
struct LegalCache {
    /// `GameState::cur_ply` of the position the moves were generated for.
    ply: usize,
    /// `GameState::revision` of the history at that point.
    revision: u64,
    /// The selected source square.
    from: Square,
    /// Legal moves from `from`.
    moves: Vec<Move>,
}

impl BoardView {
    /// Legal moves from `from`, cached until the displayed position or the
    /// selected square changes. The board repaints every frame, so this saves a
    /// full legal-move generation while a piece is selected.
    fn legal_moves(&mut self, game: &GameState, from: Square) -> &[Move] {
        let fresh = self.legal.as_ref().is_some_and(|cache| {
            cache.ply == game.cur_ply() && cache.revision == game.revision() && cache.from == from
        });
        if !fresh {
            self.legal = Some(LegalCache {
                ply: game.cur_ply(),
                revision: game.revision(),
                from,
                moves: game.legal_moves_from(from),
            });
        }
        &self
            .legal
            .as_ref()
            .expect("the cache was just filled")
            .moves
    }
}

impl Default for BoardView {
    /// Starts with White at the bottom, as in a standard board diagram.
    fn default() -> Self {
        Self {
            interaction: Interaction::Idle,
            white_on_bottom: true,
            legal: None,
        }
    }
}

impl BoardView {
    /// Flip the orientation.
    pub fn flip(&mut self) {
        self.white_on_bottom = !self.white_on_bottom;
    }

    /// Drop any selection, drag or pending promotion. Called when the
    /// displayed position changes under the user (new game, history jump).
    pub fn clear_selection(&mut self) {
        self.interaction = Interaction::Idle;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Output — the caller's next move
// ─────────────────────────────────────────────────────────────────────────────

/// What the widget wants the caller to do after this frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoardAction {
    /// Nothing happened this frame.
    None,
    /// The user picked a piece — the caller may update status labels.
    PieceSelected { from: Square },
    /// User completed a click/drag combination identifying a legal move.
    RequestMove { m: Move },
    /// A promotion just became pending; the modal is drawn by the widget on
    /// the *next* frame and resolves to [`BoardAction::RequestMove`] when
    /// the user picks Q/R/B/N.
    PromotionChoice {
        from: Square,
        to: Square,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Main widget entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Draw one [`BoardView`] inside the given `Ui` and return the pending
/// [`BoardAction`]. `input_allowed` is `false` when a person may not move
/// now (an engine is on the move); the board is then shown read-only.
pub fn show(
    ui: &mut Ui,
    game_state: &GameState,
    assets: &Assets,
    view: &mut BoardView,
    input_allowed: bool,
) -> BoardAction {
    let avail_rect = ui.available_rect_before_wrap();
    let square = (avail_rect.width().min(avail_rect.height()) / 8.0).floor().max(1.0);
    let board_px = square * 8.0;

    let board_min = Pos2::new(
        avail_rect.center().x - board_px / 2.0,
        avail_rect.center().y - board_px / 2.0,
    );
    let board_rect = Rect::from_min_size(board_min, Vec2::splat(board_px));

    // Reserve the board region so egui doesn't overlap widgets outside it.
    let _reserved = ui.allocate_rect(board_rect, Sense::hover());

    let board = game_state.board();
    let textures = &assets.pieces;
    // Browsing an earlier ply, or an engine being on the move, makes the board read-only.
    let interactive = input_allowed && game_state.cur_ply() == game_state.total_plies();
    if !interactive {
        view.clear_selection();
    }

    // ── Drag-cursor tracking (BUG-4-03: board-level pointer resolver) ──
    // While the user drags a piece, map the *pointer* position (not the
    // origin square's rect!) onto a board square every frame. This keeps
    // `Interaction::Dragging { cursor }` fresh so the drop landing zone
    // highlights correctly.
    if let Interaction::Dragging { from, .. } = view.interaction {
        if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
            let cursor = cursor_to_square(&board_rect, pointer, view.white_on_bottom)
                .unwrap_or(from);
            view.interaction = Interaction::Dragging { from, cursor };
        }
    }

    paint_board_texture(ui, board_rect, assets.board.handle());
    paint_overlays(ui, game_state, view, square, board_rect, board);
    paint_coordinates(ui, board_rect, square, view.white_on_bottom);

    let mut action = BoardAction::None;
    for row in 0..8u8 {
        for col in 0..8u8 {
            let sq = coords_to_square(view.white_on_bottom, row, col);
            let sq_rect = square_rect(board_rect, square, row, col);
            let response = ui.allocate_rect(sq_rect, Sense::click().union(Sense::drag()));

            // The piece being dragged is drawn under the pointer below, so
            // its origin square stays empty while the drag lasts.
            let lifted_from_here =
                matches!(view.interaction, Interaction::Dragging { from, .. } if from == sq);
            if let (Some(piece), false) = (board.piece_at(sq), lifted_from_here) {
                if let Some(tex) = texture_for(textures, piece) {
                    draw_piece(ui.painter(), sq_rect.center(), tex, square);
                }
            }

            if !interactive {
                continue;
            }
            if let Some(a) = handle_interaction(
                &response,
                sq,
                game_state,
                view,
                board,
                &board_rect,
            ) {
                action = a;
            }
        }
    }

    // Lifted piece: follows the pointer on top of the squares.
    if let Interaction::Dragging { from, .. } = view.interaction {
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if let (Some(pointer), Some(piece)) = (pointer, board.piece_at(from)) {
            if let Some(tex) = texture_for(textures, piece) {
                draw_piece(ui.painter(), pointer, tex, square);
            }
        }
    }

    // ── Promotion modal ──
    // Painted *after* the square loop so its four icon rects win pointer
    // priority over the squares underneath — a click on an icon therefore
    // does NOT land on the square's `clicked()`, which is what drives the
    // "click outside cancels" rule. A resolved choice overrides the action;
    // otherwise the `PromotionChoice` raised by this frame's drop survives.
    if matches!(view.interaction, Interaction::Promoting { .. }) {
        let resolution = show_promotion_modal(
            ui,
            game_state,
            textures,
            view.white_on_bottom,
            square,
            board_rect,
            view,
        );
        if resolution != BoardAction::None {
            action = resolution;
        }
    }

    // ESC cancels the pending promotion, mirroring lichess behaviour.
    if matches!(view.interaction, Interaction::Promoting { .. })
        && ui.input(|i| i.key_pressed(Key::Escape))
    {
        view.interaction = Interaction::Idle;
    }

    action
}

// ─────────────────────────────────────────────────────────────────────────────
// Layout helpers
// ─────────────────────────────────────────────────────────────────────────────

/// egui row (0 = top) + col (0 = left) for a shakmaty [`Square`].
fn square_to_coords(white_on_bottom: bool, sq: Square) -> Option<(u8, u8)> {
    let rank = sq.rank().to_u32() as u8;
    let file = sq.file().to_u32() as u8;
    if white_on_bottom {
        Some((7 - rank, file))
    } else {
        Some((rank, 7 - file))
    }
}

/// Inverse of [`square_to_coords`].
fn coords_to_square(white_on_bottom: bool, row: u8, col: u8) -> Square {
    assert!(row < 8 && col < 8, "row/col out of board bounds");
    let (rank_idx, file_idx) = if white_on_bottom {
        (7 - row, col)
    } else {
        (row, 7 - col)
    };
    Square::from_coords(File::new(file_idx as u32), Rank::new(rank_idx as u32))
}

fn square_rect(board_rect: Rect, square: f32, row: u8, col: u8) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            board_rect.min.x + col as f32 * square,
            board_rect.min.y + row as f32 * square,
        ),
        Vec2::splat(square),
    )
}

/// Map a pointer position onto a shakmaty [`Square`] via the *board* rect.
/// Row / col are clamped so a pointer one pixel past the edge still maps to
/// the last rank / file.
fn cursor_to_square(
    board_rect: &Rect,
    pointer: Pos2,
    white_on_bottom: bool,
) -> Option<Square> {
    if !board_rect.contains(pointer) {
        return None;
    }
    let square = board_rect.width() / 8.0;
    let col = (((pointer.x - board_rect.min.x) / square).floor() as i32).clamp(0, 7) as u8;
    let row = (((pointer.y - board_rect.min.y) / square).floor() as i32).clamp(0, 7) as u8;
    Some(coords_to_square(white_on_bottom, row, col))
}

/// `true` when `sq` holds a piece of `side_to_move`, i.e. one the user may
/// pick up. Opponent pieces can be looked at but not moved.
fn is_selectable(board: &Board, sq: Square, side_to_move: PieceColor) -> bool {
    board
        .piece_at(sq)
        .is_some_and(|piece| PieceColor::from(piece.color) == side_to_move)
}

/// Texture of the cburnett sprite for a shakmaty piece, if loaded.
fn texture_for(textures: &PieceTextures, piece: Piece) -> Option<TextureId> {
    let id = PieceId {
        color: piece.color.into(),
        kind: PieceKind::from_role(piece.role),
    };
    textures.get(id).map(|tex| tex.id())
}

// ─────────────────────────────────────────────────────────────────────────────
// Painting
// ─────────────────────────────────────────────────────────────────────────────

/// Paint the blue-board texture as a single rect across the given `board_rect`.
fn paint_board_texture(ui: &Ui, board_rect: Rect, handle: &egui::TextureHandle) {
    let options = egui::ImageOptions::default();
    let sized = SizedTexture::new(handle.id(), Vec2::splat(1.0));
    egui::paint_texture_at(ui.painter(), board_rect, &options, &sized);
}

/// Rank numbers in the top-left corner of the left-hand squares, and file
/// letters in the bottom-right corner of the bottom row, as en-croissant draws
/// them. They follow the orientation of the board.
fn paint_coordinates(ui: &Ui, board_rect: Rect, square: f32, white_on_bottom: bool) {
    let font = egui::FontId::proportional(13.0);
    let painter = ui.painter();
    for row in 0..8u8 {
        let rank = if white_on_bottom { 8 - row } else { row + 1 };
        let cell = square_rect(board_rect, square, row, 0);
        painter.text(
            cell.left_top() + Vec2::new(4.0, 2.0),
            egui::Align2::LEFT_TOP,
            rank.to_string(),
            font.clone(),
            crate::gui::theme::MUTED,
        );
    }
    for col in 0..8u8 {
        let file = if white_on_bottom { col } else { 7 - col };
        let cell = square_rect(board_rect, square, 7, col);
        painter.text(
            cell.right_bottom() - Vec2::new(4.0, 2.0),
            egui::Align2::RIGHT_BOTTOM,
            char::from(b'a' + file).to_string(),
            font.clone(),
            crate::gui::theme::MUTED,
        );
    }
}

/// Paint the selected square, the last move, the checked king, and legal
/// destinations (dots for quiet, rings for captures). Also tints the
/// square the dragged piece hovers over.
fn paint_overlays(
    ui: &Ui,
    game_state: &GameState,
    view: &mut BoardView,
    square: f32,
    board_rect: Rect,
    board: &Board,
) {
    // 1. Last move (from + to), taken from the displayed position's history.
    if let Some((from, to)) = game_state.last_move() {
        for sq in [from, to] {
            if let Some((row, col)) = square_to_coords(view.white_on_bottom, sq) {
                ui.painter().rect_filled(
                    square_rect(board_rect, square, row, col),
                    0.0,
                    LAST_MOVE_FILL,
                );
            }
        }
    }

    // 2. Checked king.
    if game_state.is_check() {
        if let Some(king_sq) = board.king_of(game_state.turn().into()) {
            if let Some((row, col)) = square_to_coords(view.white_on_bottom, king_sq) {
                ui.painter().rect_filled(
                    square_rect(board_rect, square, row, col),
                    0.0,
                    CHECK_FILL,
                );
            }
        }
    }

    // 3. Selected square + legal destinations. The legal moves are cached in
    // the view for as long as the position and selected square stay the same.
    if let Some(from) = view.interaction.source_square() {
        let white_on_bottom = view.white_on_bottom;
        if let Some((row, col)) = square_to_coords(white_on_bottom, from) {
            ui.painter().rect_filled(
                square_rect(board_rect, square, row, col),
                0.0,
                SELECTED_FILL,
            );
        }
        for &m in view.legal_moves(game_state, from) {
            let target = target_square(m);
            let Some((row, col)) = square_to_coords(white_on_bottom, target) else {
                continue;
            };
            let rect = square_rect(board_rect, square, row, col);
            let painter = ui.painter();
            if m.is_capture() {
                let inset = square * CAPTURE_RING_THICKNESS;
                let inner = Rect::from_min_max(
                    Pos2::new(rect.min.x + inset, rect.min.y + inset),
                    Pos2::new(rect.max.x - inset, rect.max.y - inset),
                );
                painter.rect_filled(inner, 0.0, CAPTURE_RING);
            } else {
                painter.circle_filled(rect.center(), square * QUIET_DOT_RADIUS, QUIET_MOVE_DOT);
            }
        }
    }

    // 4. Drag hover landing zone.
    if let Some(cursor) = view.interaction.drag_cursor() {
        if let Some((row, col)) = square_to_coords(view.white_on_bottom, cursor) {
            ui.painter().rect_filled(
                square_rect(board_rect, square, row, col),
                0.0,
                DRAG_HOVER_FILL,
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Piece drawing
// ─────────────────────────────────────────────────────────────────────────────

/// Paint one piece sprite of `square` pixels centred on `center`.
fn draw_piece(painter: &Painter, center: Pos2, texture: TextureId, square: f32) {
    let rect = Rect::from_center_size(center, Vec2::splat(square));
    let options = egui::ImageOptions::default();
    let sized = SizedTexture::new(texture, Vec2::splat(square));
    egui::paint_texture_at(painter, rect, &options, &sized);
}

// ─────────────────────────────────────────────────────────────────────────────
// Promotion modal
// ─────────────────────────────────────────────────────────────────────────────

/// The four promotion choices, in on-screen order **from the destination
/// square inward**: Queen, Rook, Bishop, Knight.
const PROMOTION_CHOICES: [PieceKind; 4] = [
    PieceKind::Queen,
    PieceKind::Rook,
    PieceKind::Bishop,
    PieceKind::Knight,
];

/// Compute the (row, col) on-screen position of each promotion choice.
///
/// The modal renders over the destination square and extends toward the
/// board interior (the direction from `to` toward `from`), matching lichess.
/// Pure function so it can be unit-tested for both orientations.
fn promotion_modal_layout(
    from: Square,
    to: Square,
    white_on_bottom: bool,
) -> [(u8, u8); 4] {
    let (from_row, _from_col) = square_to_coords(white_on_bottom, from).unwrap_or((0, 0));
    let (to_row, to_col) = square_to_coords(white_on_bottom, to).unwrap_or((0, 0));
    // Direction from `to` toward `from` in row space.
    let dir: i32 = if from_row > to_row { 1 } else { -1 };

    let mut out = [(0u8, 0u8); 4];
    for (k, slot) in out.iter_mut().enumerate() {
        let row_i = to_row as i32 + dir * k as i32;
        // The destination always sits on a board edge and `from` is one
        // square behind it, so rows 0..=3 in the dir direction are always
        // on the board. Clamp defensively anyway.
        *slot = ((row_i.clamp(0, 7) as u8), to_col);
    }
    out
}

/// Find the promotion legal move matching `kind`, or `None`.
fn promotion_move_for(game: &GameState, from: Square, to: Square, kind: PieceKind) -> Option<Move> {
    let role = kind.role();
    game
        .legal_moves_from(from)
        .into_iter()
        .find(|m| m.to() == to && matches!(m, Move::Normal { promotion: Some(r), .. } if *r == role))
}

/// Render and interact with the promotion modal. Returns the pending
/// [`BoardAction`] for this frame (an icon click resolves to a
/// `RequestMove`; any click elsewhere cancels to `Idle`).
fn show_promotion_modal(
    ui: &mut Ui,
    game_state: &GameState,
    textures: &crate::assets::PieceTextures,
    white_on_bottom: bool,
    square: f32,
    board_rect: Rect,
    view: &mut BoardView,
) -> BoardAction {
    let Interaction::Promoting { from, to } = view.interaction else {
        return BoardAction::None;
    };

    let layout = promotion_modal_layout(from, to, white_on_bottom);
    let promoting_color = game_state.turn();

    for (choice, &(row, col)) in PROMOTION_CHOICES.into_iter().zip(layout.iter()) {
        let icon_rect = square_rect(board_rect, square, row, col);
        let response = ui.allocate_rect(icon_rect, Sense::click());

        let painter = ui.painter();
        painter.rect_filled(icon_rect, 0.0, PROMO_BACKDROP);
        if response.hovered() {
            painter.rect_filled(icon_rect, 0.0, PROMO_HOVER);
        }
        let id = PieceId {
            color: promoting_color,
            kind: choice,
        };
        if let Some(tex) = textures.get(id) {
            draw_piece(painter, icon_rect.center(), tex.id(), square);
        }

        if response.clicked() {
            return match promotion_move_for(game_state, from, to, choice) {
                Some(m) => {
                    view.interaction = Interaction::Idle;
                    BoardAction::RequestMove { m }
                }
                None => {
                    // Should be impossible (the modal is only opened for
                    // promotion-capable dest squares), but stay safe.
                    view.interaction = Interaction::Idle;
                    BoardAction::None
                }
            };
        }
    }
    BoardAction::None
}

// ─────────────────────────────────────────────────────────────────────────────
// Interaction
// ─────────────────────────────────────────────────────────────────────────────

/// Consume one [`egui::Response`] for a board square.
///
/// `board_rect` is passed in so the drop-position resolver maps the pointer
/// through the *board*, not just this one square.
fn handle_interaction(
    response: &egui::Response,
    sq: Square,
    game_state: &GameState,
    view: &mut BoardView,
    board: &Board,
    board_rect: &Rect,
) -> Option<BoardAction> {
    let side = game_state.turn();

    // ── Drag start: pick up a piece (from Idle, or re-grab / re-target from
    // a previous selection) ──
    if response.drag_started() {
        let pick = match view.interaction {
            Interaction::Idle | Interaction::Selected { .. } => is_selectable(board, sq, side),
            Interaction::Promoting { .. } | Interaction::Dragging { .. } => false,
        };
        if pick {
            view.interaction = Interaction::Dragging {
                from: sq,
                cursor: sq,
            };
            return Some(BoardAction::PieceSelected { from: sq });
        }
    }

    // ── Drag drop ──
    if let (Interaction::Dragging { from, .. }, true) = (view.interaction, response.drag_stopped())
    {
        if let Some(pointer) = response.interact_pointer_pos() {
            let Some(to) = cursor_to_square(board_rect, pointer, view.white_on_bottom) else {
                // Dropped off the board — cancel.
                view.interaction = Interaction::Idle;
                return Some(BoardAction::None);
            };

            // BUG-4-02: dropping back on the origin keeps the piece selected
            // (lichess / Nibbler behaviour).
            if to == from {
                view.interaction = Interaction::Selected { from };
                return Some(BoardAction::PieceSelected { from });
            }

            let legal = view.legal_moves(game_state, from).to_vec();
            let matched = legal
                .iter()
                .copied()
                .find(|m| target_square(*m) == to);
            if let Some(m) = matched {
                let choices = GameState::promotion_choices_from(&legal, to);
                if choices.is_empty() {
                    view.interaction = Interaction::Idle;
                    return Some(BoardAction::RequestMove { m });
                } else {
                    view.interaction = Interaction::Promoting { from, to };
                    return Some(BoardAction::PromotionChoice { from, to });
                }
            }
            // Dropped on another own piece — switch the selection to it.
            if is_selectable(board, to, side) {
                view.interaction = Interaction::Selected { from: to };
                return Some(BoardAction::PieceSelected { from: to });
            }
            view.interaction = Interaction::Idle;
            return Some(BoardAction::None);
        }
    }

    // ── Click ──
    if response.clicked() {
        match view.interaction {
            Interaction::Idle => {
                if is_selectable(board, sq, side) {
                    view.interaction = Interaction::Selected { from: sq };
                    return Some(BoardAction::PieceSelected { from: sq });
                }
            }
            Interaction::Selected { from } => {
                if sq == from {
                    view.interaction = Interaction::Idle;
                    return Some(BoardAction::None);
                }
                let legal = view.legal_moves(game_state, from).to_vec();
                let matched = legal
                    .iter()
                    .copied()
                    .find(|m| target_square(*m) == sq);
                if let Some(m) = matched {
                    let choices = GameState::promotion_choices_from(&legal, sq);
                    if choices.is_empty() {
                        view.interaction = Interaction::Idle;
                        return Some(BoardAction::RequestMove { m });
                    } else {
                        view.interaction = Interaction::Promoting { from, to: sq };
                        return Some(BoardAction::PromotionChoice { from, to: sq });
                    }
                } else if is_selectable(board, sq, side) {
                    view.interaction = Interaction::Selected { from: sq };
                    return Some(BoardAction::PieceSelected { from: sq });
                }
                view.interaction = Interaction::Idle;
                return Some(BoardAction::None);
            }
            // A click on any square while the promotion modal is open
            // cancels — the modal icons (painted after the squares) swallow
            // their own clicks in `show_promotion_modal`, so clicks that
            // reach *this* handler are guaranteed to be outside the modal.
            Interaction::Promoting { .. } => {
                view.interaction = Interaction::Idle;
                return Some(BoardAction::None);
            }
            Interaction::Dragging { .. } => {}
        }
    }

    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests (pure functions — no egui runtime needed)
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(s: &str) -> Square {
        Square::from_ascii(s.as_bytes()).unwrap()
    }

    #[test]
    fn coordinates_round_trip_white_on_bottom() {
        for row in 0..8u8 {
            for col in 0..8u8 {
                let s = coords_to_square(true, row, col);
                assert_eq!(
                    square_to_coords(true, s),
                    Some((row, col)),
                    "white-on-bottom round trip failed at row {row} col {col}"
                );
            }
        }
    }

    #[test]
    fn coordinates_round_trip_black_on_bottom() {
        for row in 0..8u8 {
            for col in 0..8u8 {
                let s = coords_to_square(false, row, col);
                assert_eq!(
                    square_to_coords(false, s),
                    Some((row, col)),
                    "black-on-bottom round trip failed at row {row} col {col}"
                );
            }
        }
    }

    #[test]
    fn white_on_bottom_puts_rank1_at_bottom_row() {
        // rank 1 (row 7 on screen), file a (col 0).
        assert_eq!(square_to_coords(true, sq("a1")), Some((7, 0)));
        // rank 8 at the top row.
        assert_eq!(square_to_coords(true, sq("a8")), Some((0, 0)));
    }

    #[test]
    fn black_on_bottom_mirrors() {
        // Black-on-bottom: rank 8 at the bottom row, file a on the right.
        assert_eq!(square_to_coords(false, sq("a8")), Some((7, 7)));
        assert_eq!(square_to_coords(false, sq("h1")), Some((0, 0)));
    }

    #[test]
    fn cursor_to_square_clamps_and_rejects() {
        let board = Rect::from_min_size(Pos2::ZERO, Vec2::splat(800.0)); // 100 px per square
        // Centre of d4: col 3, row (rank 8 -> row 0; rank 4 -> row 4).
        assert_eq!(
            cursor_to_square(&board, Pos2::new(350.0, 450.0), true),
            Some(sq("d4"))
        );
        // One pixel past the right edge still maps into the last column.
        assert_eq!(
            cursor_to_square(&board, Pos2::new(800.0, 50.0), true),
            Some(sq("h8"))
        );
        // Outside the board entirely.
        assert_eq!(
            cursor_to_square(&board, Pos2::new(1000.0, 50.0), true),
            None
        );
    }

    #[test]
    fn castle_king_destination_used_as_target() {
        // Standard starting position: O-O from e1 is legal only after the
        // knights/bishops are out. Use a FEN with castling ready (small
        // army — each side has only king + rooks + pawns — but the castling
        // rights and empty paths are correct).
        let game = GameState::from_fen(
            "r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w KQkq - 0 1",
        )
        .unwrap();
        let e1 = sq("e1");
        let legal = game.legal_moves_from(e1);
        let king_side = legal.iter().find(|m| matches!(m, Move::Castle { rook, .. } if rook.file().to_u32() == 7));
        let queen_side = legal.iter().find(|m| matches!(m, Move::Castle { rook, .. } if rook.file().to_u32() == 0));
        assert!(king_side.is_some(), "O-O should be legal in this FEN");
        assert!(queen_side.is_some(), "O-O-O should be legal in this FEN");
        // Kingside O-O: rook target h1 but human clicks g1.
        assert_eq!(target_square(*king_side.unwrap()), sq("g1"));
        // Queenside O-O-O: rook target a1 but human clicks c1.
        assert_eq!(target_square(*queen_side.unwrap()), sq("c1"));
        // And a plain move keeps its own destination.
        let normal = *legal.iter().find(|m| !m.is_castle()).unwrap();
        assert_eq!(target_square(normal), normal.to());
    }

    #[test]
    fn promotion_modal_layout_white_bottom_white_promotes() {
        // White promotes a7 → a8 with white on the bottom: dest at row 0;
        // the modal extends downward (toward the origin on row 1).
        let layout = promotion_modal_layout(sq("a7"), sq("a8"), true);
        assert_eq!(
            layout,
            [(0, 0), (1, 0), (2, 0), (3, 0)],
            "Q at dest, then R, B, N introspect"
        );
    }

    #[test]
    fn promotion_modal_layout_black_promotes_upward() {
        // Black promotes e2 → e1 with white on the bottom: dest at row 7
        // (the bottom edge); the modal extends upward.
        let layout = promotion_modal_layout(sq("e2"), sq("e1"), true);
        assert_eq!(layout, [(7, 4), (6, 4), (5, 4), (4, 4)]);
    }

    #[test]
    fn promotion_modal_layout_flipped_board() {
        // Black-on-bottom orientation: white's rank-8 destination appears at
        // the BOTTOM row, so the modal extends upward.
        let layout = promotion_modal_layout(sq("a7"), sq("a8"), false);
        assert_eq!(layout, [(7, 7), (6, 7), (5, 7), (4, 7)]);
    }

    #[test]
    fn default_board_view_shows_white_at_the_bottom() {
        assert!(BoardView::default().white_on_bottom);
    }

    #[test]
    fn only_side_to_move_pieces_are_selectable() {
        let board = GameState::new().board().clone();
        assert!(is_selectable(&board, sq("e2"), PieceColor::White));
        assert!(!is_selectable(&board, sq("e7"), PieceColor::White));
        assert!(!is_selectable(&board, sq("e4"), PieceColor::White));
    }

    #[test]
    fn promotion_move_for_picks_the_right_role() {
        let game = GameState::from_fen("7k/P7/8/8/8/8/8/7K w - - 0 1").unwrap();
        let from = sq("a7");
        let to = sq("a8");
        assert!(game.promotion_choices(from, to).len() == 4);
        let m = promotion_move_for(&game, from, to, PieceKind::Knight)
            .expect("knight promotion should resolve");
        assert_eq!(m.to(), to);
    }

    /// A timing harness for the board hot path. It is ignored by default, so
    /// the normal suite stays fast. Run it on a release build:
    ///
    /// ```text
    /// cargo test --release --bin siliconboard board_paint -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "timing harness; run with --release --ignored --nocapture"]
    fn board_paint_stays_within_a_frame_budget() {
        use std::time::{Duration, Instant};

        let ctx = egui::Context::default();
        crate::gui::theme::apply(&ctx);
        let assets = Assets::load(&ctx).expect("the workspace public/ assets must load");
        let mut game = GameState::new();
        game.apply_uci("e2e4").unwrap();
        game.apply_uci("e7e5").unwrap();
        game.apply_uci("g1f3").unwrap();
        let mut view = BoardView {
            interaction: Interaction::Selected { from: sq("f3") },
            ..BoardView::default()
        };
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 900.0))),
            ..Default::default()
        };
        let paint = |view: &mut BoardView| {
            let _ = ctx.run_ui(input.clone(), |ui| {
                let _ = show(ui, &game, &assets, view, true);
            });
        };

        // Warm up the font atlas, the galley cache and the texture list.
        for _ in 0..20 {
            paint(&mut view);
        }
        const FRAMES: u32 = 2_000;
        let started = Instant::now();
        for _ in 0..FRAMES {
            paint(&mut view);
        }
        let per_frame = started.elapsed() / FRAMES;
        println!("board_view::show: {per_frame:?} per frame over {FRAMES} frames");
        assert!(
            per_frame < Duration::from_millis(4),
            "a board paint should stay far below a 60 fps frame ({per_frame:?})"
        );
    }
}
