//! Chess board rendering and interaction — Phase 4.
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
//!    (`crate::assets::PieceTextures`),
//! 5. a floating copy of a piece being dragged at the cursor's square,
//! 6. a promotion modal when a pawn-to-back-rank move is pending.
//!
//! Interaction state machine (a single, linear-ish flow):
//!
//! ```text
//!                ┌─────────────── own piece ────────────┐
//!                ↓                                      │
//! ┌──────────────────────────┐  click on dest ▓  ┌─────────│────┐
//! │        Idle              │────────►        │ Selected │────│► RequestMove
//! └─────┘                    │                            │    │
//!     │ click+drag on piece  │  click elsewhere ─► Idle   │    │
//!     ↓                       │                            │    │
//! ┌──────────────────────────┐                  ┌─────────▼──┐ │
//! │       Dragging           │ drag stop on ────► Playing  │ │
//! └──────────────────────────┘  dest            └────────────┘ │
//!                                                              │
//!          └──── click on promotion dest ──► Promotion modal ──┘
//! ```
//!
//! The widget itself never mutates [`GameState`]; it returns a
//! [`BoardAction`] snapshot and the caller decides what to do.

use eframe::egui::{
    self, Align2, Color32, FontId, Image, Painter, Pos2, Rect, Sense, TextureId, Ui, Vec2,
    load::SizedTexture,
};
use shakmaty::{Board, File, Move, Position, Rank, Square};

use crate::assets::{Assets, PieceId, PieceTextures};
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
    /// The user is dragging a piece. `from` keeps the origin square; the
    /// piece is drawn at the *cursor's* square while dragging.
    Dragging { from: Square },
    /// A promotion modal is pending; the user must pick a promotion piece
    /// before the widget returns `RequestMove`.
    Promoting { from: Square, to: Square },
}

impl Interaction {
    /// If the user has picked a source square (Selected / Dragging /
    /// Promoting), it's reflected here. `Idle` returns `None`.
    pub fn from_square(self) -> Option<Square> {
        match self {
            Interaction::Idle => None,
            Interaction::Selected { from } | Interaction::Dragging { from } => Some(from),
            Interaction::Promoting { from, .. } => Some(from),
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
    /// A promotion is pending; the caller should render a modal and call
    /// `interaction = Interaction::Idle` after resolution.
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
    let square = (avail_rect.width().min(avail_rect.height()) / 8.0).floor();
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

    paint_board_texture(ui, board_rect, assets.board.handle());
    paint_coordinates(ui.painter(), view.white_on_bottom, square, board_rect);
    paint_overlays(ui, game_state, view, square, board_rect, &board);

    let mut action = BoardAction::None;
    for row in 0..8u8 {
        for col in 0..8u8 {
            let sq = coords_to_square(view.white_on_bottom, row, col);
            let sq_rect = square_rect(board_rect, square, row, col);
            let response = ui.allocate_rect(sq_rect, Sense::click().union(Sense::drag()));

            if let Some(piece) = board.piece_at(sq) {
                let color: PieceColor = piece.color.into();
                let kind = PieceKind::from_role(piece.role);
                let id = PieceId { color, kind };
                if let Some(tex) = textures.get(id) {
                    // Hidden origin piece during a drag — the cursor-side
                    // mirror in Interaction::Dragging takes over.
                    let hide_origin = matches!(
                        view.interaction,
                        Interaction::Dragging { from: s } if s == sq
                    );
                    if !hide_origin {
                        draw_piece(ui.painter(), sq_rect.center(), tex.id(), square);
                    }
                }
            }

            if let Some(a) = handle_interaction(&response, sq, game_state, view, &board) {
                action = a;
            }
        }
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
    Square::from_coords(
        File::new(file_idx as u32),
        Rank::new(rank_idx as u32),
    )
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
    let make_font = || FontId::proportional(font_size);

    for col in 0..8u8 {
        let file_idx = match white_on_bottom {
            true => col,
            false => 7 - col,
        };
        let file_ch = char::from(b'a' + file_idx);
        let rect = Rect::from_min_size(
            Pos2::new(
                board_rect.min.x + col as f32 * square,
                board_rect.min.y + board_rect.height() - font_size * 1.5,
            ),
            Vec2::new(square, font_size * 1.5),
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            file_ch.to_string(),
            make_font(),
            COORDINATE_TEXT,
        );
    }

    for row in 0..8u8 {
        let rank_num = match white_on_bottom {
            true => 8 - row,
            false => row,
        } + 1;
        let rect = Rect::from_min_size(
            Pos2::new(
                board_rect.min.x,
                board_rect.min.y + row as f32 * square,
            ),
            Vec2::new(font_size * 1.5, square),
        );
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            rank_num.to_string(),
            make_font(),
            COORDINATE_TEXT,
        );
    }
}

/// Paint the selected square, the last move, the checked king, and legal
/// destinations (dots for quiet, rings for captures).
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
                ui.painter()
                    .rect_filled(square_rect(board_rect, square, row, col), 0.0, LAST_MOVE_FILL);
            }
        }
    }

    // 2. Checked king.
    if game_state.is_check() {
        if let Some(king_sq) = board.king_of(game_state.turn().into()) {
            if let Some((row, col)) = square_to_coords(view.white_on_bottom, king_sq) {
                ui.painter()
                    .rect_filled(square_rect(board_rect, square, row, col), 0.0, CHECK_FILL);
            }
        }
    }

    // 3. Selected square + legal destinations (drives the mouse cursor).
    if let Some(from) = view.interaction.from_square() {
        if let Some((row, col)) = square_to_coords(view.white_on_bottom, from) {
            ui.painter()
                .rect_filled(square_rect(board_rect, square, row, col), 0.0, SELECTED_FILL);
        }
        for m in game_state.legal_moves_from(from) {
            let Some((row, col)) = square_to_coords(view.white_on_bottom, m.to()) else {
                continue;
            };
            let rect = square_rect(board_rect, square, row, col);
            let painter = ui.painter();
            if m.is_capture() {
                // Ring around the destination square (draw a stroke inside
                // the bounds by insetting the rect).
                let inset = square * CAPTURE_RING_THICKNESS;
                let inner = Rect::from_min_max(
                    Pos2::new(rect.min.x + inset, rect.min.y + inset),
                    Pos2::new(rect.max.x - inset, rect.max.y - inset),
                );
                painter.rect_filled(inner, 0.0, CAPTURE_RING);
            } else {
                let radius = square * QUIET_DOT_RADIUS;
                painter.circle_filled(rect.center(), radius, QUIET_MOVE_DOT);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Piece drawing
// ─────────────────────────────────────────────────────────────────────────────

fn draw_piece(painter: &Painter, center: Pos2, texture: TextureId, square: f32) {
    let half = square / 2.0;
    let rect = Rect::from_center_size(center, Vec2::splat(square));
    // `egui::paint_texture_at` is a free function imported through the
    // crate root. We set up a minimal `SizedTexture` + `ImageOptions` so
    // the piece renderer mirrors the egui `Image` widget's behaviour.
    let options = egui::ImageOptions::default();
    let sized = SizedTexture::new(texture, Vec2::splat(square));
    egui::paint_texture_at(painter, rect, &options, &sized);
}

// ─────────────────────────────────────────────────────────────────────────────
// Interaction
// ─────────────────────────────────────────────────────────────────────────────

/// Map a pointer position (relative to a board-square rect) to a shakmaty
/// [`Square`]. `white_on_bottom` determines whether the top of the screen
/// holds rank 8 or rank 1.
///
/// Returns the square whose rect contains `pointer`, or `None` if it is
/// outside the square's bounds (i.e. `pointer` isn't on a board square).
fn cursor_to_square(
    board_rect: &Rect,
    pointer: Pos2,
    white_on_bottom: bool,
) -> Option<Square> {
    if !board_rect.contains(pointer) {
        return None;
    }
    let square = board_rect.width() / 8.0;
    let col = ((pointer.x - board_rect.min.x) / square).floor() as u8;
    let row = ((pointer.y - board_rect.min.y) / square).floor() as u8;
    if col >= 8 || row >= 8 {
        return None;
    }
    Some(coords_to_square(white_on_bottom, row, col))
}

/// Consume one [`Response`] for a board square.
///
/// The widget is a `select.half-move` flow: click an own piece to select,
/// click a destination to play (or click another own piece to re-select),
/// drag a piece to its destination, or trigger the promotion modal.
fn handle_interaction(
    response: &egui::Response,
    sq: Square,
    game_state: &GameState,
    view: &mut BoardView,
    board: &Board,
) -> Option<BoardAction> {
    // ── Drag start (rename: "picked up" ──)
    if response.drag_started() {
        if board.piece_at(sq).is_some() {
            view.interaction = Interaction::Dragging { from: sq };
            return Some(BoardAction::PieceSelected { from: sq });
        }
    }
    // ── Dragged piece dropped onto a destination ──
    if let Interaction::Dragging { from } = view.interaction {
        if response.drag_stopped() {
            if let Some(pointer) = response.interact_pointer_pos() {
                // Determine the destination square from the cursor position.
                let to = cursor_to_square(&response.rect, pointer, view.white_on_bottom);
                if let Some(to) = to {
                    let legal = game_state.legal_moves_from(from);
                    if let Some(m) = legal.iter().find(|m| m.to() == to) {
                        let choices = game_state.promotion_choices(from, to);
                        if choices.is_empty() {
                            view.interaction = Interaction::Idle;
                            view.last_move = Some((from, to));
                            return Some(BoardAction::RequestMove { m: *m });
                        } else {
                            view.interaction = Interaction::Promoting { from, to };
                            return Some(BoardAction::PromotionChoice { from, to });
                        }
                    }
                }
                // Dropped on empty non-legal — reset.
                view.interaction = Interaction::Idle;
                return Some(BoardAction::None);
            }
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
                if let Some(m) = legal.iter().find(|m| m.to() == sq) {
                    let choices = game_state.promotion_choices(from, sq);
                    if choices.is_empty() {
                        view.interaction = Interaction::Idle;
                        view.last_move = Some((from, sq));
                        return Some(BoardAction::RequestMove { m: *m });
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
            _ => {}
        }
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Unused helpers, kept for future phases
// ─────────────────────────────────────────────────────────────────────────────

#[allow(dead_code)]
fn _unused_helper(_x: u32) -> u32 {
    0
}
