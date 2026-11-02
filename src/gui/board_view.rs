//! Chess board rendering and interaction — Phase 4 (complete).
//!
//! The board widget is egui-native and refreshes per frame. It paints:
//!
//! 1. the blue-board texture from `public/images/board/blue.png` (v0.1
//!    hardcoded; Phase 5 wires a board-picker),
//! 2. the `a-h` / `1-8` square coordinates along the bottom / left edge,
//! 3. overlays — the currently selected square, legal destinations (with
//!    different treatment for quiet vs. capture moves), the last played
//!    move's from/to, and a red highlight for a checked king,
//! 4. every piece sprite from the cburnett SVG set rasterised at startup
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
//! [`BoardAction`] snapshot and the caller decides what to do.
//!
//! # Castling input note
//! shakmaty's `Move::Castle { king, rook }.to()` returns the **rook** square
//! (e.g. `h1` for O-O), but a human clicks the *king's* destination (`g1`).
//! [`move_target_square`] maps a castle move onto the king's destination so
//! click-click, drag-drop, and the legal-move overlays all behave correctly.

use eframe::egui::{
    self, Align2, Color32, FontId, Image, Key, Painter, Pos2, Rect, Sense, TextureId, Ui, Vec2,
    load::SizedTexture,
};
use shakmaty::{Board, File, Move, Position, Rank, Square};

use crate::assets::{Assets, PieceId};
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

/// Colour of the coordinate labels (`a-h` / `1-8`).
const COORDINATE_TEXT: Color32 = Color32::from_rgba_unmultiplied_const(0xff, 0xff, 0xff, 0xc0);

/// Fraction of the square's height the coordinate glyphs occupy.
const COORDINATE_FONT_SCALE: f32 = 0.35;

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
    pub fn from_square(self) -> Option<Square> {
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
#[derive(Default)]
pub struct BoardView {
    /// Current interaction stage.
    pub interaction: Interaction,
    /// Last move's (from, to) pair, highlighted gold.
    pub last_move: Option<(Square, Square)>,
    /// White at the bottom? Typically flipped when the user is Black
    /// against an engine.
    pub white_on_bottom: bool,
}

impl BoardView {
    /// Flip the orientation. Idempotent.
    pub fn flip(&mut self) {
        self.white_on_bottom = !self.white_on_bottom;
    }

    /// Force the widget back to `Idle`. Useful when an engine plays a move
    /// while the user had a piece selected.
    pub fn reset(&mut self) {
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
/// [`BoardAction`].
pub fn show(
    ui: &mut Ui,
    game_state: &GameState,
    assets: &Assets,
    view: &mut BoardView,
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

    let board = game_state.position().board().clone();
    let textures = &assets.pieces;

    // ── Drag-cursor tracking (BUG-4-03: board-level pointer resolver) ──
    // While the user drags a piece, map the *pointer* position (not the
    // origin square's rect!) onto a board square every frame. This keeps
    // `Interaction::Dragging { cursor }` fresh so the piece paints under
    // the cursor and the drop landing zone highlights correctly.
    if let Interaction::Dragging { from, .. } = view.interaction {
        if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
            let cursor = cursor_to_square(&board_rect, pointer, view.white_on_bottom)
                .unwrap_or(from);
            view.interaction = Interaction::Dragging {
                from,
                cursor,
            };
        }
    }

    paint_board_texture(ui, board_rect, assets.board.handle());
    paint_coordinates(ui.painter(), view.white_on_bottom, square, board_rect);
    paint_overlays(ui, game_state, view, square, board_rect, &board);

    let mut action = BoardAction::None;
    for row in 0..8u8 {
        for col in 0..8u8 {
            let sq = coords_to_square(view.white_on_bottom, row, col);
            let sq_rect = square_rect(board_rect, square, row, col);
            let response = ui.allocate_rect(sq_rect, Sense::click().union(Sense::drag()));

            // Piece painting: hide the origin while dragging; draw a lifted
            // copy at the cursor square instead.
            if let Some(piece) = board.piece_at(sq) {
                let color: PieceColor = piece.color.into();
                let kind = PieceKind::from_role(piece.role);
                let id = PieceId { color, kind };
                if let Some(tex) = textures.get(id) {
                    dragging_painter(
                        ui.painter(),
                        view.interaction,
                        sq,
                        sq_rect,
                        tex.id(),
                        square,
                    );
                }
            }

            if let Some(a) = handle_interaction(
                &response,
                sq,
                game_state,
                view,
                &board,
                &board_rect,
            ) {
                action = a;
            }
        }
    }

    // ── Promotion modal (BUG-4-01) ──
    // Painted *after* the square loop so its four icon rects win pointer
    // priority over the squares underneath — a click on an icon therefore
    // does NOT land on the square's `clicked()`, which is what drives the
    // "click outside cancels" rule.
    if let (Interaction::Promoting { from, to }, resolution) = (
        view.interaction,
        show_promotion_modal(
            ui,
            game_state,
            textures,
            view.white_on_bottom,
            square,
            board_rect,
            view,
        ),
    ) {
        let _ = (from, to);
        action = resolution;
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

// ─────────────────────────────────────────────────────────────────────────────
// Move-target resolution (castling-aware)
// ─────────────────────────────────────────────────────────────────────────────

/// The square a human clicks for move `m`.
///
/// For most moves that's `Move::to()`, but for castling shakmaty reports the
/// *rook* square (h1 for O-O) whereas the user clicks the king's destination
/// (g1). This helper normalises both cases so the widget can match user
/// input against `game_state.legal_moves_from(from)`.
fn move_target_square(m: Move) -> Square {
    if let Move::Castle { king, rook } = m {
        // Kingside: rook sits to the right of the king (bigger file index).
        let kingside = rook.file().to_u32() > king.file().to_u32();
        let dest_file = if kingside { File::G } else { File::C };
        Square::from_coords(dest_file, king.rank())
    } else {
        m.to()
    }
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

/// Draw `a-h` along the bottom and `1-8` along the left.
fn paint_coordinates(
    painter: &Painter,
    white_on_bottom: bool,
    square: f32,
    board_rect: Rect,
) {
    let font_size = square * COORDINATE_FONT_SCALE;

    for col in 0..8u8 {
        let file_idx = if white_on_bottom { col } else { 7 - col };
        let file_ch = char::from(b'a' + file_idx);
        let rect = Rect::from_min_size(
            Pos2::new(
                board_rect.min.x + col as f32 * square,
                board_rect.min.y + board_rect.height() - square,
            ),
            Vec2::new(square, square),
        );
        painter.text(
            // Draw inside the bottom-right corner of the bottom row so the
            // labels do not collide with the a1 / h8 corner piece.
            Pos2::new(rect.right() - font_size * 0.9, rect.bottom() - font_size * 0.4),
            Align2::RIGHT_BOTTOM,
            file_ch.to_string(),
            FontId::proportional(font_size),
            COORDINATE_TEXT,
        );
    }

    for row in 0..8u8 {
        let rank_num = if white_on_bottom { 8 - row } else { row };
        let rect = Rect::from_min_size(
            Pos2::new(
                board_rect.min.x,
                board_rect.min.y + row as f32 * square,
            ),
            Vec2::new(square, square),
        );
        painter.text(
            Pos2::new(rect.left() + font_size * 0.35, rect.top() + font_size * 0.5),
            Align2::LEFT_TOP,
            (rank_num + 1).to_string(),
            FontId::proportional(font_size),
            COORDINATE_TEXT,
        );
    }
}

/// Paint the selected square, the last move, the checked king, and legal
/// destinations (dots for quiet, rings for captures). Also tints the
/// square the dragged piece hovers over.
fn paint_overlays(
    ui: &Ui,
    game_state: &GameState,
    view: &BoardView,
    square: f32,
    board_rect: Rect,
    board: &Board,
) {
    // 1. Last move (from + to).
    if let Some((from, to)) = view.last_move {
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

    // 3. Selected square + legal destinations.
    if let Some(from) = view.interaction.from_square() {
        if let Some((row, col)) = square_to_coords(view.white_on_bottom, from) {
            ui.painter().rect_filled(
                square_rect(board_rect, square, row, col),
                0.0,
                SELECTED_FILL,
            );
        }
        for m in game_state.legal_moves_from(from) {
            let target = move_target_square(m);
            let Some((row, col)) = square_to_coords(view.white_on_bottom, target) else {
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

fn draw_piece(painter: &Painter, center: Pos2, texture: TextureId, square: f32) {
    let rect = Rect::from_center_size(center, Vec2::splat(square));
    let options = egui::ImageOptions::default();
    let sized = SizedTexture::new(texture, Vec2::splat(square));
    egui::paint_texture_at(painter, rect, &options, &sized);
}

/// Paint a piece, taking dragging into account:
/// * origin square of the dragged piece → hidden (the cursor copy takes over),
/// * cursor square → the lifted piece is drawn there,
/// * otherwise → normal in-place rendering.
fn dragging_painter(
    painter: &Painter,
    interaction: Interaction,
    sq: Square,
    sq_rect: Rect,
    texture: TextureId,
    square: f32,
) {
    match interaction {
        Interaction::Dragging { from, cursor } if from == sq => {
            if cursor == sq {
                // Drag just started, pointer still on the origin.
                draw_piece(painter, sq_rect.center(), texture, square);
            }
            // else: hide the origin — the lifted copy is drawn by the
            // loop when it reaches the `cursor` square.
        }
        Interaction::Dragging { cursor, .. } if cursor == sq => {
            // Lifted piece at the cursor square.
            draw_piece(painter, sq_rect.center(), texture, square);
        }
        _ => {
            draw_piece(painter, sq_rect.center(), texture, square);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Promotion modal (BUG-4-01)
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

    /// Render + interact the promotion modal. Returns the pending
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
                    view.last_move = Some((from, to));
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
/// through the *board*, not just this one square (BUG-4-03).
fn handle_interaction(
    response: &egui::Response,
    sq: Square,
    game_state: &GameState,
    view: &mut BoardView,
    board: &Board,
    board_rect: &Rect,
) -> Option<BoardAction> {
    // ── Drag start: pick up a piece (from Idle, or re-grab / re-target from
    // a previous selection) ──
    if response.drag_started() {
        let pick = match view.interaction {
            Interaction::Idle | Interaction::Selected { .. } => board.piece_at(sq),
            Interaction::Promoting { .. } | Interaction::Dragging { .. } => None,
        };
        if pick.is_some() {
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

            let legal = game_state.legal_moves_from(from);
            let matched = legal
                .into_iter()
                .find(|m| move_target_square(*m) == to);
            if let Some(m) = matched {
                let choices = game_state.promotion_choices(from, to);
                if choices.is_empty() {
                    view.interaction = Interaction::Idle;
                    view.last_move = Some((from, to));
                    return Some(BoardAction::RequestMove { m });
                } else {
                    view.interaction = Interaction::Promoting { from, to };
                    return Some(BoardAction::PromotionChoice { from, to });
                }
            }
            // Dropped on an empty non-legal square — cancel. Re-selecting on
            // another own piece is handled by the click path below (egui
            // treats a quick drop as a click when the pointer barely moves).
            if board.piece_at(to).is_some() {
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
                if board.piece_at(sq).is_some() {
                    view.interaction = Interaction::Selected { from: sq };
                    return Some(BoardAction::PieceSelected { from: sq });
                }
            }
            Interaction::Selected { from } => {
                if sq == from {
                    view.interaction = Interaction::Idle;
                    return Some(BoardAction::None);
                }
                let legal = game_state.legal_moves_from(from);
                let matched = legal
                    .into_iter()
                    .find(|m| move_target_square(*m) == sq);
                if let Some(m) = matched {
                    let choices = game_state.promotion_choices(from, sq);
                    if choices.is_empty() {
                        view.interaction = Interaction::Idle;
                        view.last_move = Some((from, sq));
                        return Some(BoardAction::RequestMove { m });
                    } else {
                        view.interaction = Interaction::Promoting { from, to: sq };
                        return Some(BoardAction::PromotionChoice { from, to: sq });
                    }
                } else if board.piece_at(sq).is_some() {
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
        assert_eq!(move_target_square(*king_side.unwrap()), sq("g1"));
        // Queenside O-O-O: rook target a1 but human clicks c1.
        assert_eq!(move_target_square(*queen_side.unwrap()), sq("c1"));
        // And a plain move keeps its own destination.
        let normal = *legal.iter().find(|m| !m.is_castle()).unwrap();
        assert_eq!(move_target_square(normal), normal.to());
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
    fn promotion_move_for_picks_the_right_role() {
        let game = GameState::from_fen("7k/P7/8/8/8/8/8/7K w - - 0 1").unwrap();
        let from = sq("a7");
        let to = sq("a8");
        assert!(game.promotion_choices(from, to).len() == 4);
        let m = promotion_move_for(&game, from, to, PieceKind::Knight)
            .expect("knight promotion should resolve");
        assert_eq!(m.to(), to);
    }
}
