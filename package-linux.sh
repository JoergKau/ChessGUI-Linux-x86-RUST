#!/usr/bin/env bash
# package-linux.sh — build SiliconBoard and assemble the Linux release folder.
#
# The steps are the manual ones from the README, scripted:
#
#   1. `cargo build --release`
#   2. assemble `SiliconBoard-<version>-linux-<arch>/`: the binary in `bin/`,
#      the asset tree in `public/` (where the resolver looks when walking up
#      from the executable — unit-tested in src/assets.rs), the start-up PGN
#      in `PGN/`, a `.desktop` launcher, the window icon, LICENSE and
#      README.md,
#   3. build the distributable tarball
#      `SiliconBoard-<version>-linux-<arch>.tar.gz` and print its SHA-256
#      for the release listing.
#
# By default the freshly built app is also installed into `$HOME/.local`
# (like the macOS script installs into /Applications), so the daily-use app
# is always the one just built: binary, assets and launcher land under
# `~/.local/share/siliconboard` and `~/.local`, and the dock/menu icon
# works. Installing is the default because the user forgets the flag; a
# build left in the dev folder otherwise never shows an icon in the dock.
# `--no-install` (or `-n`) skips the install step and leaves the folder in
# the repo; the old `--install` (or `-i`) flags still mean the same; an
# unknown argument is rejected.
#
# The tarball is a build artefact; it is listed in .gitignore.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_ROOT"

INSTALL=1
for arg in "$@"; do
    case "$arg" in
        -i|--install) ;;
        -n|--no-install) INSTALL=0 ;;
        *) { echo "No such argument: $arg (use --no-install to skip the install)"; exit 1; } ;;
    esac
done

version="$(sed -n 's/^version = "\([^\"]*\)"$/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || { echo "No version found in Cargo.toml." >&2; exit 1; }
arch="$(uname -m)"

app_dir="$REPO_ROOT/SiliconBoard-$version-linux-$arch"

echo "▶ cargo build --release"
cargo build --release

echo "▶ Assembling $app_dir (v$version)"
rm -rf "$app_dir"
mkdir -p "$app_dir/bin"
cp target/release/siliconboard "$app_dir/bin/"
cp -R public "$app_dir/public"
# The start-up PGN rides along, so the installed app also opens
# PGN/WorldChamp1972.pgn at launch and selects game 1.
STARTUP_PGN="$REPO_ROOT/PGN/WorldChamp1972.pgn"
if [[ -f "$STARTUP_PGN" ]]; then
    mkdir -p "$app_dir/PGN"
    cp "$STARTUP_PGN" "$app_dir/PGN/"
else
    echo "  ▲ $STARTUP_PGN is missing — the folder starts without the start-up PGN." >&2
fi
cp LICENSE README.md "$app_dir/"

# A launcher for desktop environments. `Exec` uses the binary name, so it
# works both from the folder and after `--install` puts it on `$PATH`.
mkdir -p "$app_dir/share/applications" "$app_dir/share/icons/hicolor/512x512/apps"
cat > "$app_dir/share/applications/siliconboard.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=SiliconBoard
GenericName=Chess GUI
Comment=Play and analyse chess with UCI engines
Exec=siliconboard
Icon=siliconboard
Terminal=false
Categories=Game;BoardGame;
# Wayland matches the window by its app_id, "siliconboard" (set in main.rs);
# X11 by the WM_CLASS, which winit derives from the binary name.
StartupWMClass=siliconboard
EOF
if [[ -f "$REPO_ROOT/assets/icon/siliconboard-512.png" ]]; then
    cp "$REPO_ROOT/assets/icon/siliconboard-512.png" \
       "$app_dir/share/icons/hicolor/512x512/apps/siliconboard.png"
else
    echo "  ▲ assets/icon/siliconboard-512.png is missing — no menu icon." >&2
fi

# The distributable tarball: the folder plus LICENSE and README, with the
# SHA-256 printed for the release listing.
echo "▶ Distribution tarball"
tarball="$REPO_ROOT/SiliconBoard-$version-linux-$arch.tar.gz"
rm -f "$tarball"
tar -czf "$tarball" -C "$REPO_ROOT" "$(basename "$app_dir")"
echo "  SHA-256 of $tarball:"
sha256sum "$tarball" | awk '{print "  " $1}'

# Round-trip check: the tarball must unpack to the assembled folder.
verify="$(mktemp -d)"
tar -xzf "$tarball" -C "$verify"
diff -rq "$app_dir" "$verify/$(basename "$app_dir")" > /dev/null \
    || { echo "Tarball round-trip mismatch." >&2; exit 1; }
rm -rf "$verify"

if [[ "$INSTALL" == 1 ]]; then
    prefix="$HOME/.local"
    echo "▶ Installing into $prefix"
    # The real binary lives in share/siliconboard/bin, next to its assets
    # (the walk-up resolver and the PGN path both expect this layout); a
    # wrapper in ~/.local/bin points there and is what PATH and the
    # .desktop launcher use.
    install -D "$app_dir/bin/siliconboard" "$prefix/share/siliconboard/bin/siliconboard"
    rm -rf "$prefix/share/siliconboard/public" "$prefix/share/siliconboard/PGN"
    cp -R "$app_dir/public" "$prefix/share/siliconboard/public"
    if [[ -d "$app_dir/PGN" ]]; then
        cp -R "$app_dir/PGN" "$prefix/share/siliconboard/PGN"
    fi
    install -D "$app_dir/share/applications/siliconboard.desktop" \
        "$prefix/share/applications/siliconboard.desktop"
    if [[ -f "$app_dir/share/icons/hicolor/512x512/apps/siliconboard.png" ]]; then
        install -D "$app_dir/share/icons/hicolor/512x512/apps/siliconboard.png" \
            "$prefix/share/icons/hicolor/512x512/apps/siliconboard.png"
    fi
    # The wrapper is written last so it replaces an old real binary cleanly.
    install -Dm755 /dev/stdin "$prefix/bin/siliconboard" <<EOF
#!/usr/bin/env bash
export SILICONBOARD_PUBLIC_ROOT="\${SILICONBOARD_PUBLIC_ROOT:-$prefix/share/siliconboard/public}"
exec "$prefix/share/siliconboard/bin/siliconboard" "\$@"
EOF
    if command -v update-desktop-database > /dev/null; then
        update-desktop-database "$prefix/share/applications" 2> /dev/null || true
    fi
    if ! [[ ":$PATH:" == *":$prefix/bin:"* ]]; then
        echo "  ▲ $prefix/bin is not on your PATH — add it to use the launcher." >&2
    fi
    echo "  Installed. Start it from the menu or with: siliconboard"
else
    echo "▶ Folder ready (not installed): $app_dir"
    echo "  Run it with: $app_dir/bin/siliconboard"
    echo "  (The install step was skipped with --no-install.)"
fi
