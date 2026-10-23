#!/bin/sh
# Build a NoteCraft .deb from an already-built binary.
#
#   packaging/linux/make-deb.sh <binary> <version> <arch> [<outdir>]
#   packaging/linux/make-deb.sh target/release/notecraft 1.0.0 amd64 dist
#
# `arch` is the Debian name (amd64, arm64). Used by the release workflow, and runnable by hand so a
# packager can reproduce exactly what was published. cargo-deb can also build this package from
# `[package.metadata.deb]` in Cargo.toml; this script is what CI uses because it needs nothing but
# /bin/sh, tar and either dpkg-deb or ar.
set -eu

BIN=${1:?usage: make-deb.sh <binary> <version> <arch> [outdir]}
VERSION=${2:?usage: make-deb.sh <binary> <version> <arch> [outdir]}
ARCH=${3:?usage: make-deb.sh <binary> <version> <arch> [outdir]}
OUT=${4:-dist}
HERE=$(cd "$(dirname "$0")/../.." && pwd)

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM
STAGE="$WORK/pkg"
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/share/applications" \
         "$STAGE/usr/share/icons/hicolor/scalable/apps" "$STAGE/usr/share/doc/notecraft"

install -m 755 "$BIN" "$STAGE/usr/bin/notecraft"
install -m 644 "$HERE/packaging/linux/notecraft.desktop" "$STAGE/usr/share/applications/"
install -m 644 "$HERE/assets/notecraft.svg" "$STAGE/usr/share/icons/hicolor/scalable/apps/"
install -m 644 "$HERE/LICENSE" "$STAGE/usr/share/doc/notecraft/copyright"

# The glibc version the binary actually needs, read off the binary rather than guessed: a package
# that declares a lower floor than the build used installs happily and then dies at load time with
# "version `GLIBC_2.xx' not found".
GLIBC=2.31
if command -v readelf >/dev/null 2>&1; then
    WANT=$(readelf --version-info "$BIN" 2>/dev/null \
        | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)
    [ -n "${WANT:-}" ] && GLIBC=$WANT
fi

SIZE=$(du -sk "$STAGE/usr" | cut -f1)

cat > "$STAGE/DEBIAN/control" <<EOF
Package: notecraft
Version: $VERSION-1
Architecture: $ARCH
Maintainer: Michael Bayes <mjbayes@pm.me>
Homepage: https://github.com/mjb73/notecraft
Section: utils
Priority: optional
Installed-Size: $SIZE
Depends: libc6 (>= $GLIBC), libgcc-s1
Recommends: libgl1, libxkbcommon0, libwayland-client0, libx11-6
Description: native notes, todos and ideas as plain markdown
 NoteCraft keeps notebooks, sections and pages as ordinary markdown files in a
 folder you choose: a block editor with read mode, tasks with due dates, tags,
 image attachments, screen snips, spellcheck, and HTML/PDF/text export.
 .
 No account, no database and no network access; the pages are yours, on disk.
EOF

# A .deb is an ar archive of exactly these three members, and the control member holds
# ./control at its root (not ./DEBIAN/control — that is the build tree, not the archive).
printf '2.0\n' > "$WORK/debian-binary"
tar -cJf "$WORK/control.tar.xz" -C "$STAGE/DEBIAN" .
tar -cJf "$WORK/data.tar.xz" -C "$STAGE" ./usr

mkdir -p "$OUT"
DEB="$OUT/notecraft_${VERSION}_${ARCH}.deb"
rm -f "$DEB"
if command -v dpkg-deb >/dev/null 2>&1; then
    dpkg-deb --root-owner-group --build "$STAGE" "$DEB" >/dev/null
else
    # No dpkg on this machine (Arch, say): write the same container with GNU ar.
    ( cd "$WORK" && ar rcD "$DEB" debian-binary control.tar.xz data.tar.xz )
fi
echo "$DEB"
