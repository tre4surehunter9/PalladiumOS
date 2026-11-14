#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# less for PalladiumOS: tools/ports/less.sh -> /usr/bin/less, the pager
# (scroll back and forth, search with /, q to quit); man shows pages in it.
# BusyBox's less stays `busybox less`. On ncurses (xterm.sh).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libncursesw.a" ]; then
    echo "ncurses isn't built: run tools/ports/xterm.sh first" >&2
    exit 1
fi

# less 710 (greenwoodsoftware.com): its GPG signature (less-710.sig) is
# good from Mark Nudelman's key AE27252BD6846E7D6EAE1DD6F153A7C833235259
# (the one Arch's less accepts, whose BLAKE2 checksum matches too).
fetch less-710 https://www.greenwoodsoftware.com/less/less-710.tar.gz \
    d1008fb78dcae1323ddab664bcb352a61f022b1b131bd8018548e021d975ec7a
./configure --host=x86_64-linux-musl --prefix=/usr --sysconfdir=/etc CC="$PCC" \
    CPPFLAGS="-I$X/include -I$X/include/ncursesw" LDFLAGS="-L$X/lib -static" \
    LIBS="-lncursesw" --with-regex=posix \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" less >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/share/man/man1" "$SYSROOT/share/man/man5"
install -m 755 -s less "$SYSROOT/bin/less"
cp less.nro "$SYSROOT/share/man/man1/less.1"
cp lesskey.nro "$SYSROOT/share/man/man5/lesskey.5"
echo "less installed in $SYSROOT/bin"
