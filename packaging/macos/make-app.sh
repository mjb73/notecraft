#!/bin/sh
# Build NoteCraft.app (and a .dmg from it) around an already-built binary.
#
#   packaging/macos/make-app.sh <path-to-notecraft-binary> <version> [<output-dir>]
#
# Kept as a plain shell script rather than a bundler crate so the release workflow and a local
# build do exactly the same thing. The binary is signed ad-hoc: without a Developer ID and a
# notarisation round trip, that is what makes a downloaded app start instead of being killed by
# Gatekeeper. See README "macOS" for what a user has to do on first launch.
set -eu

BIN=${1:?usage: make-app.sh <binary> <version> [outdir]}
VERSION=${2:?usage: make-app.sh <binary> <version> [outdir]}
OUT=${3:-dist}
HERE=$(cd "$(dirname "$0")/../.." && pwd)

APP="$OUT/NoteCraft.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/notecraft"
chmod 755 "$APP/Contents/MacOS/notecraft"

sed "s/__VERSION__/$VERSION/g" "$HERE/packaging/macos/Info.plist.in" > "$APP/Contents/Info.plist"
cp "$HERE/packaging/macos/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc signature. A real Developer ID + notarisation is a later step; this at least makes the
# bundle internally consistent so macOS does not report it as damaged.
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || \
    echo "note: codesign unavailable, leaving the bundle unsigned" >&2

# A dmg is the thing people expect to click. HFS+ keeps it readable on older macOS too, and the
# Applications symlink is what makes "drag NoteCraft to Applications" possible without opening a
# second Finder window — without it the volume shows only the app and nowhere to drop it.
if command -v hdiutil >/dev/null 2>&1; then
    rm -rf "$OUT/dmg" "$OUT/NoteCraft-$VERSION.dmg"
    mkdir -p "$OUT/dmg"
    cp -R "$APP" "$OUT/dmg/"
    ln -s /Applications "$OUT/dmg/Applications"
    hdiutil create -quiet -volname "NoteCraft $VERSION" -srcfolder "$OUT/dmg" \
        -ov -format UDZO "$OUT/NoteCraft-$VERSION.dmg"
    rm -rf "$OUT/dmg"
    echo "$OUT/NoteCraft-$VERSION.dmg"
else
    echo "$APP"
fi
