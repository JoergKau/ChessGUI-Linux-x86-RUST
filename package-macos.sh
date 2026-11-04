#!/bin/zsh
# package-macos.sh — build SiliconBoard and assemble the macOS .app bundle.
#
# F-15. The steps are the manual ones from the README, scripted:
#
#   1. `cargo build --release`
#   2. assemble SiliconBoard.app in the repository root: the binary in
#      Contents/MacOS (renamed to `SiliconBoard`), the icon in
#      Contents/Resources, and the asset tree in Contents/Resources/public
#      (where the asset resolver looks first in a bundle — unit-tested
#      in src/assets.rs),
#   3. write Contents/Info.plist with the version from Cargo.toml and the
#      About-panel copyright (macos_about.rs reads both from the bundle),
#   4. ad-hoc sign the bundle,
#   5. build the distributable zip: SiliconBoard-<version>-macos-arm64.zip
#      holds one folder with the signed app plus LICENSE and README.md,
#      and the SHA-256 is printed for the release listing.
#
# The .app is a build artefact; it is listed in .gitignore.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$REPO_ROOT"

APP="SiliconBoard.app"
CONTENTS="$REPO_ROOT/$APP/Contents"
PLIST="$CONTENTS/Info.plist"

version="$(sed -n 's/^version = "\([^\"]*\)"$/\1/p' Cargo.toml | head -1)"
[[ -n "$version" ]] || { print -u2 "No version found in Cargo.toml."; exit 1; }

print "▶ cargo build --release"
cargo build --release

print "▶ Assembling $APP (v$version)"
rm -rf "$REPO_ROOT/$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp target/release/siliconboard "$CONTENTS/MacOS/SiliconBoard"
cp assets/icon/SiliconBoard.icns "$CONTENTS/Resources/"
cp -R public "$CONTENTS/Resources/public"

cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>               <string>SiliconBoard</string>
    <key>CFBundleDisplayName</key>        <string>SiliconBoard</string>
    <key>CFBundleExecutable</key>         <string>SiliconBoard</string>
    <key>CFBundleIdentifier</key>         <string>org.chess-gui.SiliconBoard</string>
    <key>CFBundlePackageType</key>        <string>APPL</string>
    <key>CFBundleShortVersionString</key> <string>$version</string>
    <key>CFBundleVersion</key>            <string>$version</string>
    <key>CFBundleIconFile</key>           <string>SiliconBoard.icns</string>
    <key>LSMinimumSystemVersion</key>     <string>11.0</string>
    <key>NSHighResolutionCapable</key>    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright © 2026 Joerg Kauschke. MIT License.</string>
</dict>
</plist>
EOF
plutil -lint "$PLIST"

print "▶ Ad-hoc signature"
codesign --force --deep --sign - "$APP"

# The distributable zip: the app plus a copy of README and LICENSE (so a
# user who only downloads the zip still gets the MIT text). The zip holds
# one folder, `SiliconBoard-<version>`, and the app is signed before it is
# zipped — a zip added to a signed app would break the seal.
print "▶ Distribution zip"
stage="$REPO_ROOT/target/dist/SiliconBoard-$version"
rm -rf "$(dirname "$stage")"
mkdir -p "$stage"
cp -R "$REPO_ROOT/$APP" "$stage/"
cp LICENSE README.md "$stage/"
zip_file="$REPO_ROOT/SiliconBoard-$version-macos-arm64.zip"
rm -f "$zip_file"
ditto -c -k --sequesterRsrc --keepParent "$stage" "$zip_file"
rm -rf "$(dirname "$stage")"
print "  SHA-256 of $zip_file:"
shasum -a 256 "$zip_file"

print "▶ Done: $REPO_ROOT/$APP"
print "        $zip_file"
