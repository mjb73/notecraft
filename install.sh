#!/bin/sh
# Install NoteCraft on Linux from the latest GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/mjb73/notecraft/main/install.sh | sh
#
# Installs into ~/.local (no root, no package manager) and prints how to run it. Set PREFIX to
# install somewhere else, or NOTECRAFT_VERSION to pin a release.
set -eu

REPO=${NOTECRAFT_REPO:-mjb73/notecraft}
PREFIX=${PREFIX:-$HOME/.local}
VERSION=${NOTECRAFT_VERSION:-}

if [ -n "${TMPDIR:-}" ] && [ -d "$TMPDIR" ]; then WORK=$TMPDIR; else WORK=$(mktemp -d); fi
WORK=$(mktemp -d "${WORK%/}/notecraft-install.XXXXXX")
trap 'rm -rf "$WORK"' EXIT INT TERM

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        echo "note: need curl or wget to download NoteCraft" >&2
        exit 1
    fi
}

case "$(uname -s)" in
    Linux) ;;
    *) echo "note: this script is for Linux. On macOS download the .dmg; on Windows the .zip." >&2; exit 1 ;;
esac

case "$(uname -m)" in
    x86_64 | amd64) ARCH=x86_64 ;;
    aarch64 | arm64) ARCH=aarch64 ;;
    *) echo "note: no prebuilt NoteCraft for $(uname -m) — build it with 'cargo install --path .'" >&2; exit 1 ;;
esac

if [ -z "$VERSION" ]; then
    # /releases/latest redirects to /releases/tag/vX.Y.Z; read the tag off the final URL.
    if command -v curl >/dev/null 2>&1; then
        FINAL=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
    else
        FINAL=$(wget -qS --spider "https://github.com/$REPO/releases/latest" 2>&1 | awk '/Location:/{print $2}' | tail -1)
    fi
    VERSION=$(printf '%s' "$FINAL" | sed -n 's#.*/tag/##p')
    [ -n "$VERSION" ] || { echo "note: could not work out the latest NoteCraft version" >&2; exit 1; }
fi

NUM=${VERSION#v}
ASSET="notecraft-$NUM-$ARCH-linux.tar.gz"
URL="https://github.com/$REPO/releases/download/$VERSION/$ASSET"

echo "installing NoteCraft $VERSION ($ARCH) into $PREFIX"
fetch "$URL" "$WORK/$ASSET"
fetch "$URL.sha256" "$WORK/$ASSET.sha256" 2>/dev/null || true
if [ -s "$WORK/$ASSET.sha256" ]; then
    ( cd "$WORK" && sha256sum -c "$ASSET.sha256" >/dev/null ) || { echo "note: checksum mismatch" >&2; exit 1; }
fi

tar -xzf "$WORK/$ASSET" -C "$WORK"
SRC="$WORK/notecraft-$NUM-$ARCH-linux"

mkdir -p "$PREFIX/bin" "$PREFIX/share/applications" \
         "$PREFIX/share/icons/hicolor/scalable/apps"
install -m 755 "$SRC/notecraft" "$PREFIX/bin/notecraft"
install -m 644 "$SRC/notecraft.desktop" "$PREFIX/share/applications/notecraft.desktop"
install -m 644 "$SRC/notecraft.svg" "$PREFIX/share/icons/hicolor/scalable/apps/notecraft.svg"

# Refresh the desktop database so the entry appears without a logout, where the tool exists.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$PREFIX/share/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo
echo "installed: $PREFIX/bin/notecraft"
case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "note: add $PREFIX/bin to PATH to run 'notecraft' from a terminal" ;;
esac
echo "Your notes will live in ~/Documents/NoteCraft (or set NOTECRAFT_VAULT)."
