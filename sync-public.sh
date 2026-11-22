#!/bin/zsh
# Refresh the bundled asset tree (public/) in the repository from the
# canonical shared tree in the workspace root (../public by default).
# The repository copy holds only what SiliconBoard reads: board themes
# without thumbnails/.orig/horsey overlays, the twelve-SVG piece sets, the
# figurine font and the CC0 ECO openings table.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
CANONICAL="${SILICONBOARD_CANONICAL_PUBLIC:-$REPO_ROOT/../public}"

[[ -d "$CANONICAL/images/board" || -d "$CANONICAL/piece" ]] || {
  print -u2 "No canonical public/ tree at $CANONICAL (set SILICONBOARD_CANONICAL_PUBLIC)."
  exit 1
}

# Board themes: the app lists PNG/JPEG images; thumbnails, .orig files and
# the horsey board-side overlays (current-premove, last-move, …) are skipped.
mkdir -p "$REPO_ROOT/public/data/openings" "$REPO_ROOT/public/font" "$REPO_ROOT/public/images/board" "$REPO_ROOT/public/piece"
rsync -a --delete \
  --exclude='*.thumbnail.*' --exclude='*.orig.*' --exclude='horsey.*-*.png' \
  "$CANONICAL/images/board/" "$REPO_ROOT/public/images/board/"

# Piece sets: complete sets only are shown at runtime; commit them all.
rsync -a --delete "$CANONICAL/piece/" "$REPO_ROOT/public/piece/"

# Figurine font and the ECO openings table (with its README).
cp -f "$CANONICAL/font/lichess-chess.ttf" "$REPO_ROOT/public/font/"
cp -f "$CANONICAL/data/openings/openings.tsv" "$REPO_ROOT/public/data/openings/"
[[ -f "$CANONICAL/data/openings/README.md" ]] && cp -f "$CANONICAL/data/openings/README.md" "$REPO_ROOT/public/data/openings/"

print "Synced. Repo copy:"
du -sh "$REPO_ROOT/public"
