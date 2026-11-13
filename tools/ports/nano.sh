#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# GNU nano for PalladiumOS: tools/ports/nano.sh -> /usr/bin/nano, a small,
# friendly editor for the terminal (the console, xterm, SSH): the keys it
# takes are listed at the bottom of its screen (^X: exit, ^O: save...).
# With its syntax colouring files in /usr/share/nano (on by default: see
# /etc/nanorc), without translations or documentation. On ncurses
# (xterm.sh).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libncursesw.a" ]; then
    echo "ncurses isn't built: run tools/ports/xterm.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# nano 9.3, as nano-editor.org publishes it: this checksum is of the
# tarball whose GPG signature (nano-9.3.tar.xz.asc) is good from its
# maintainer's key, Benno Schulenberg's 168E6F4297BFD7A79AFD4496514BBE2EB8E1961F
# (the one Arch's nano package accepts too).
fetch nano-9.3 https://www.nano-editor.org/dist/v9/nano-9.3.tar.xz \
    a9affc52c9596ee0682bcdd4ceb3c0b235eb221d83d733dfd9b0dff45897cb0d
./configure --host=x86_64-linux-musl --prefix=/usr --sysconfdir=/etc CC="$PCC" \
    CPPFLAGS="-I$X/include -I$X/include/ncursesw" LDFLAGS="-L$X/lib -static" \
    NCURSESW_CFLAGS="-I$X/include/ncursesw" NCURSESW_LIBS="-lncursesw" \
    --disable-nls --disable-libmagic --disable-speller \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/share/nano"
install -m 755 -s src/nano "$SYSROOT/bin/nano"
rm -rf "$SYSROOT/share/nano" && mkdir -p "$SYSROOT/share/nano"
cp syntax/*.nanorc "$SYSROOT/share/nano/"
echo "nano installed in $SYSROOT/bin; $(ls "$SYSROOT/share/nano" | wc -l) syntax files"
