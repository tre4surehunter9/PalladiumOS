#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# htop for PalladiumOS: tools/ports/htop.sh -> /usr/bin/htop, an
# interactive process viewer for the terminal: CPU and memory meters,
# processes sortable and searchable, killing them (F9), a tree (F5). It
# reads PalladiumOS's /proc as it does Linux's. On ncurses (xterm.sh).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libncursesw.a" ]; then
    echo "ncurses isn't built: run tools/ports/xterm.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# htop 3.5.3 (GitHub release). Its SHA-256 is the one the release
# publishes; its SHA-512 is the one Alpine's htop package pins.
fetch htop-3.5.3 https://github.com/htop-dev/htop/releases/download/3.5.3/htop-3.5.3.tar.xz \
    a8b164386494cb85bb255a415a3f5f80afe7a0c4491da5d113b3a0f951087e65
./configure --host=x86_64-linux-musl --prefix=/usr --sysconfdir=/etc CC="$PCC" \
    CPPFLAGS="-I$X/include -I$X/include/ncursesw" LDFLAGS="-L$X/lib -static" \
    --enable-unicode --enable-static --disable-sensors --disable-capabilities \
    --disable-delayacct --disable-hwloc \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
# (-rdynamic: for its crash backtraces' function names; a static program
# has no dynamic symbol table to export to.)
sed -i 's/ -rdynamic//' Makefile
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

mkdir -p "$SYSROOT/bin"
install -m 755 -s htop "$SYSROOT/bin/htop"
echo "htop installed in $SYSROOT/bin"
