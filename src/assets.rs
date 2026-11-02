//! SiliconBoard asset pipeline — Phase 1 BIS edition.
//!
//! This module owns every GPU texture the renderer ever touches. As of the
//! Phase 1 BIS refactor it is responsible for:
//!
//! 1. The full board image (`public/images/board/blue.png` — a single
//!    high-resolution PNG decoded once at startup). The Phase 5 board-picker
//!    will re-use the same loader with a different filename per session.
//! 2. The twelve cburnett chess-piece sprites (`public/piece/cburnett/<w|b><K|Q|R|B|N|P>.svg`)
//!    which we **rasterise at load time** through the `usvg`/`resvg`/`tiny-skia`
//!    stack. That gives us vector-sharpness at any board scale on Apple
//!    Retina displays without paying a per-frame SVG-decoding cost.
//!
//! [`Assets`] ties the two together; [`BoardTexture`] and [`PieceTextures`]
//! stay independently cloneable so the future Phase 4/5 widgets can hold
//! their own borrow.

use crate::chess::{PieceColor, PieceKind};

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{ColorImage, Context, TextureHandle, TextureOptions};

/// Width × height (in pixels) of every rasterised piece sprite.
///
/// 512 is generously above what any sane chess-board square will ever
/// reach on screen (a 4K Apple display squeezed into an 8×8 grid is
/// already in the 300-px range). Bigger rasterisations cost memory; smaller
/// risk looking soft.
pub const PIECE_RASTER_SIZE: u32 = 512;

/// Path of the board image used for the v0.1 first release. The Phase-5
/// board-picker will replace this constant with a user-selected file.
pub const DEFAULT_BOARD_IMAGE: &str = "blue.png";

// ─────────────────────────────────────────────────────────────────────────────
// Public types
// ─────────────────────────────────────────────────────────────────────────────

/// Compound identifier for a chess-piece sprite. Pairs a [`PieceColor`]
/// (white/black) with a [`PieceKind`] (K/Q/R/B/N/P) so we can map cleanly
/// to filenames like `wK.svg` / `bP.svg`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PieceId {
    pub color: PieceColor,
    pub kind: PieceKind,
}

impl PieceId {
    /// Returns the canonical on-disk filename for the piece, e.g. `wK.svg`
    /// (white king) or `bP.svg` (black pawn).
    pub const fn filename(&self) -> &'static str {
        match (self.color, self.kind) {
            (PieceColor::White, PieceKind::King) => "wK.svg",
            (PieceColor::White, PieceKind::Queen) => "wQ.svg",
            (PieceColor::White, PieceKind::Rook) => "wR.svg",
            (PieceColor::White, PieceKind::Bishop) => "wB.svg",
            (PieceColor::White, PieceKind::Knight) => "wN.svg",
            (PieceColor::White, PieceKind::Pawn) => "wP.svg",
            (PieceColor::Black, PieceKind::King) => "bK.svg",
            (PieceColor::Black, PieceKind::Queen) => "bQ.svg",
            (PieceColor::Black, PieceKind::Rook) => "bR.svg",
            (PieceColor::Black, PieceKind::Bishop) => "bB.svg",
            (PieceColor::Black, PieceKind::Knight) => "bN.svg",
            (PieceColor::Black, PieceKind::Pawn) => "bP.svg",
        }
    }
}

/// GPU texture for the chess board image. Wrapped around a single
/// [`TextureHandle`] owned by egui/Metal.
#[derive(Clone)]
pub struct BoardTexture {
    handle: TextureHandle,
}

impl BoardTexture {
    /// Reference to the underlying egui texture handle.
    pub fn handle(&self) -> &TextureHandle {
        &self.handle
    }
}

/// GPU textures for all twelve pieces. Cheap to clone because the inner
/// [`TextureHandle`]'s storage on the GPU is held via an `Arc`.
#[derive(Clone)]
pub struct PieceTextures {
    /// Inner cache: `PieceId → egui::TextureHandle`.
    textures: HashMap<PieceId, TextureHandle>,
}

impl PieceTextures {
    /// Look up a piece texture by [`PieceId`].
    pub fn get(&self, id: PieceId) -> Option<&TextureHandle> {
        self.textures.get(&id)
    }
}

/// Bundle of every asset SiliconBoard needs at startup. Returned by
/// [`Assets::load`] and stored on the application root. Cloning is cheap
/// (just bumps a few reference counts on egui's GPU side).
#[derive(Clone)]
pub struct Assets {
    /// The single board image used by the current session.
    pub board: BoardTexture,
    /// All twelve piece sprites, rasterised from SVG.
    pub pieces: PieceTextures,
}

impl Assets {
    /// Discover the workspace `public/` root and load every asset.
    ///
    /// # Errors
    /// Returns an [`AssetError`] if the asset directory cannot be located,
    /// if `public/images/board/blue.png` is missing or unreadable, or if
    /// any of the twelve cburnett SVGs fails to parse or rasterise.
    pub fn load(ctx: &Context) -> Result<Self, AssetError> {
        let asset_root = resolve_asset_root();
        Self::load_from(ctx, &asset_root)
    }

    /// Load assets from an explicit directory. Useful for tests and for
    /// future "open resource bundle from a custom path" flows.
    pub fn load_from(ctx: &Context, asset_root: &Path) -> Result<Self, AssetError> {
        // (1) Board texture.
        let board_path = asset_root.join("images").join("board").join(DEFAULT_BOARD_IMAGE);
        let board = load_board(ctx, &board_path)?;

        // (2) Piece textures.
        let piece_root = asset_root.join("piece").join("cburnett");
        let pieces = load_pieces(ctx, &piece_root, PIECE_RASTER_SIZE)?;

        Ok(Self { board, pieces })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Internal loaders
// ─────────────────────────────────────────────────────────────────────────────

/// Load `blue.png` from `board_dir` and upload it to the GPU.
fn load_board(ctx: &Context, path: &Path) -> Result<BoardTexture, AssetError> {
    let bytes = std::fs::read(path).map_err(|source| AssetError::OpenRead {
        path: path.to_path_buf(),
        source,
    })?;
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|source| AssetError::Decode {
            path: path.to_path_buf(),
            source,
        })?
        .to_rgba8();

    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Err(AssetError::Empty {
            path: path.to_path_buf(),
        });
    }

    let color_image = ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        decoded.as_raw(),
    );
    let handle = ctx.load_texture(
        path.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("board"),
        color_image,
        TextureOptions::LINEAR,
    );

    log::debug!(
        "Loaded board texture {:?} ({}x{} px, {} bytes) into egui atlas id = {:?}",
        path.file_name(),
        width,
        height,
        decoded.len(),
        handle.id(),
    );

    Ok(BoardTexture { handle })
}

/// Load all twelve pieces from `piece_root` (a directory containing
/// `wK.svg`, `wQ.svg`, …, `bP.svg`). Each SVG is parsed with `usvg`,
/// rasterised to a fixed `target_px` × `target_px` RGBA8 buffer on a
/// `tiny_skia::Pixmap`, and uploaded to the GPU with `TextureOptions::LINEAR`.
fn load_pieces(
    ctx: &Context,
    piece_root: &Path,
    target_px: u32,
) -> Result<PieceTextures, AssetError> {
    let mut textures = HashMap::with_capacity(12);

    for &color in &[PieceColor::White, PieceColor::Black] {
        for &kind in &[
            PieceKind::King,
            PieceKind::Queen,
            PieceKind::Rook,
            PieceKind::Bishop,
            PieceKind::Knight,
            PieceKind::Pawn,
        ] {
            let id = PieceId { color, kind };
            let path = piece_root.join(id.filename());
            let handle = load_piece_svg(ctx, &path, target_px)?;
            textures.insert(id, handle);
        }
    }

    Ok(PieceTextures { textures })
}

/// Load a single cburnett SVG, rasterise it, and upload it to the GPU.
///
/// Steps:
/// 1. Read the SVG file.
/// 2. Parse with `usvg::Tree::from_data(...)`.
/// 3. Compute the scale factor needed to map the source viewBox onto a
///    `target_px × target_px` pixmap.
/// 4. Allocate a `tiny_skia::Pixmap` and ask `resvg` to render into it.
/// 5. Pull the RGBA8 buffer back via `take_demultiplied()` (egui wants
///    *un*premultiplied alpha) and wrap it in a `ColorImage`.
fn load_piece_svg(
    ctx: &Context,
    path: &Path,
    target_px: u32,
) -> Result<TextureHandle, AssetError> {
    let bytes = std::fs::read(path).map_err(|source| AssetError::OpenRead {
        path: path.to_path_buf(),
        source,
    })?;

    let tree = usvg::Tree::from_data(&bytes, &usvg::Options::default())
        .map_err(|source| AssetError::SvgParse {
            path: path.to_path_buf(),
            source,
        })?;

    let src_size = tree.size();
    let src_w = src_size.width().max(1.0);
    let src_h = src_size.height().max(1.0);

    // Pick a uniform scale so the SVG fits exactly into the target square.
    let scale = (target_px as f32 / src_w).min(target_px as f32 / src_h);

    let mut pixmap = tiny_skia::Pixmap::new(target_px, target_px).ok_or(AssetError::Alloc {
        path: path.to_path_buf(),
        width: target_px,
        height: target_px,
    })?;

    let transform = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // egui/ColorImage wants *non*-premultiplied RGBA8.
    let data = pixmap.take_demultiplied();
    if data.len() != (target_px as usize) * (target_px as usize) * 4 {
        return Err(AssetError::ChannelMismatch {
            path: path.to_path_buf(),
            expected_bytes: (target_px as usize) * (target_px as usize) * 4,
            actual_bytes: data.len(),
        });
    }

    let color_image = ColorImage::from_rgba_unmultiplied(
        [target_px as usize, target_px as usize],
        &data,
    );
    let handle = ctx.load_texture(
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("piece"),
        color_image,
        TextureOptions::LINEAR,
    );

    log::debug!(
        "Rasterised piece {:?} (src {}x{} → {}x{} px, {} bytes) into egui atlas id = {:?}",
        path.file_name(),
        src_w,
        src_h,
        target_px,
        target_px,
        data.len(),
        handle.id(),
    );

    Ok(handle)
}

// ─────────────────────────────────────────────────────────────────────────────
// Asset root resolution — Phase 1 BIS flavour
//
// We locate the workspace-level `public/` directory by walking up from the
// executable's path. `cargo run` lands at:
//
//   <repo>/siliconboard/target/{release,debug}/siliconboard
//   parent → target/{release,debug} → target → siliconboard → Chess-GUI/
//
// Bundling a `.app` later will need a Resources fallback; for v0.1 the
// walker is sufficient.
// ─────────────────────────────────────────────────────────────────────────────

/// Maximum number of parent directories to walk up while hunting for
/// `public/`. Six is generous — the exe lives four levels below the repo
/// root in `target/{release,debug}/siliconboard`.
const MAX_PARENT_WALK: usize = 8;

fn resolve_asset_root() -> PathBuf {
    let env_override = std::env::var("SILICONBOARD_PUBLIC_ROOT").ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    resolve_asset_root_from(env_override.as_deref(), exe_dir.as_deref())
}

/// Pure version of [`resolve_asset_root`]: the env override is tried first,
/// then the walk up from `exe_dir`, then `./public`.
fn resolve_asset_root_from(env_override: Option<&str>, exe_dir: Option<&Path>) -> PathBuf {
    // (1) Hard override via environment variable.
    if let Some(custom) = env_override {
        let p = PathBuf::from(custom);
        if looks_like_asset_root(&p) {
            return p;
        }
        log::warn!(
            "SILICONBOARD_PUBLIC_ROOT={:?} does not look like a SiliconBoard public/ tree, ignoring",
            p
        );
    }

    // (2) Walk up from the executable's folder looking for `public/`.
    if let Some(start) = exe_dir {
        let mut cursor: Option<&Path> = Some(start);
        let mut steps = 0usize;
        while let Some(candidate) = cursor {
            if steps > MAX_PARENT_WALK {
                break;
            }
            let proposed = candidate.join("public");
            if looks_like_asset_root(&proposed) {
                return proposed;
            }
            cursor = candidate.parent();
            steps += 1;
        }
    }

    // (3) Fallback — current working directory.
    PathBuf::from("public")
}

/// Returns `true` if `dir` looks like the canonical `public/` asset tree
/// (i.e. it contains both `images/board/` and `piece/cburnett/` with at
/// least one file each).
fn looks_like_asset_root(dir: &Path) -> bool {
    dir.join("images").join("board").is_dir()
        && dir.join("piece").join("cburnett").is_dir()
        && dir
            .join("images")
            .join("board")
            .read_dir()
            .map(|mut rd| rd.next().is_some())
            .unwrap_or(false)
        && dir
            .join("piece")
            .join("cburnett")
            .read_dir()
            .map(|mut rd| rd.next().is_some())
            .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Errors raised by the asset pipeline.
#[derive(Debug)]
pub enum AssetError {
    /// Could not open (or read) the asset file.
    OpenRead {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The PNG decoder rejected the bytes.
    Decode {
        path: PathBuf,
        source: image::ImageError,
    },
    /// `usvg::Tree::from_data` rejected the SVG.
    SvgParse {
        path: PathBuf,
        source: usvg::Error,
    },
    /// A zero-sized image — almost certainly corruption.
    Empty {
        path: PathBuf,
    },
    /// `tiny_skia::Pixmap::new` returned `None` (oversize dimension).
    Alloc {
        path: PathBuf,
        width: u32,
        height: u32,
    },
    /// The piece pixmap data did not match the configured dimensions.
    ChannelMismatch {
        path: PathBuf,
        expected_bytes: usize,
        actual_bytes: usize,
    },
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::OpenRead { path, source } => write!(
                f,
                "failed to read asset `{}`: {}",
                path.display(),
                source
            ),
            AssetError::Decode { path, source } => write!(
                f,
                "failed to decode PNG at `{}`: {}",
                path.display(),
                source
            ),
            AssetError::SvgParse { path, source } => write!(
                f,
                "failed to parse SVG at `{}`: {}",
                path.display(),
                source
            ),
            AssetError::Empty { path } => write!(
                f,
                "asset `{}` decoded to a zero-sized image",
                path.display()
            ),
            AssetError::Alloc {
                path,
                width,
                height,
            } => write!(
                f,
                "could not allocate a {}x{} pixmap for `{}`",
                width,
                height,
                path.display()
            ),
            AssetError::ChannelMismatch {
                path,
                expected_bytes,
                actual_bytes,
            } => write!(
                f,
                "pixmap byte count mismatch for `{}` (expected {}, got {})",
                path.display(),
                expected_bytes,
                actual_bytes
            ),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AssetError::OpenRead { source, .. } => Some(source),
            AssetError::Decode { source, .. } => Some(source),
            AssetError::SvgParse { source, .. } => Some(source),
            AssetError::Empty { .. }
            | AssetError::Alloc { .. }
            | AssetError::ChannelMismatch { .. } => None,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn piece_filename_is_svg() {
        assert_eq!(
            PieceId { color: PieceColor::White, kind: PieceKind::King }.filename(),
            "wK.svg"
        );
        assert_eq!(
            PieceId { color: PieceColor::Black, kind: PieceKind::Pawn }.filename(),
            "bP.svg"
        );
        // No leftover PNG filenames should be reachable through the public API.
        for &color in &[PieceColor::White, PieceColor::Black] {
            for &kind in &[
                PieceKind::King,
                PieceKind::Queen,
                PieceKind::Rook,
                PieceKind::Bishop,
                PieceKind::Knight,
                PieceKind::Pawn,
            ] {
                let f = PieceId { color, kind }.filename();
                assert!(f.ends_with(".svg"), "{:?} expected `.svg` got {}", kind, f);
            }
        }
    }

    #[test]
    fn every_piece_id_has_a_unique_filename() {
        let mut seen = std::collections::HashSet::new();
        for &color in &[PieceColor::White, PieceColor::Black] {
            for &kind in &[
                PieceKind::King,
                PieceKind::Queen,
                PieceKind::Rook,
                PieceKind::Bishop,
                PieceKind::Knight,
                PieceKind::Pawn,
            ] {
                let id = PieceId { color, kind };
                assert!(seen.insert(id), "{:?} has a duplicate filename", id);
            }
        }
    }

    /// Build a minimal fake `public/` tree (one file per required folder).
    fn fake_public_tree(root: &Path) -> PathBuf {
        let public = root.join("public");
        std::fs::create_dir_all(public.join("images").join("board")).unwrap();
        std::fs::create_dir_all(public.join("piece").join("cburnett")).unwrap();
        std::fs::write(public.join("images").join("board").join("blue.png"), b"x").unwrap();
        std::fs::write(public.join("piece").join("cburnett").join("wK.svg"), b"x").unwrap();
        public
    }

    #[test]
    fn looks_like_asset_root_rejects_garbage_dirs() {
        assert!(!looks_like_asset_root(Path::new("definitely/not/here")));
        assert!(!looks_like_asset_root(Path::new(".")));
    }

    #[test]
    fn env_override_wins_over_the_executable_walk() {
        let dir = tempfile::tempdir().unwrap();
        let override_root = fake_public_tree(&dir.path().join("override"));
        let exe_root = fake_public_tree(&dir.path().join("exe-side"));
        let exe_dir = dir.path().join("exe-side").join("target").join("debug");
        std::fs::create_dir_all(&exe_dir).unwrap();

        let chosen = resolve_asset_root_from(
            Some(override_root.to_str().unwrap()),
            Some(&exe_dir),
        );
        assert_eq!(chosen, override_root);
        assert_ne!(chosen, exe_root);
    }

    #[test]
    fn invalid_env_override_falls_back_to_executable_walk() {
        let dir = tempfile::tempdir().unwrap();
        let exe_root = fake_public_tree(dir.path());
        let exe_dir = dir.path().join("target").join("debug");
        std::fs::create_dir_all(&exe_dir).unwrap();

        let chosen = resolve_asset_root_from(Some("/definitely/not/a/public/tree"), Some(&exe_dir));
        assert_eq!(chosen, exe_root);
    }

    #[test]
    fn resolution_falls_back_to_cwd_public_when_nothing_matches() {
        let chosen = resolve_asset_root_from(None, None);
        assert_eq!(chosen, PathBuf::from("public"));
    }
}
