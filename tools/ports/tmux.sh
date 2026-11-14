#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# tmux for PalladiumOS: tools/ports/tmux.sh -> /usr/bin/tmux, a terminal
# multiplexer: several shells in one terminal (the console, xterm, SSH),
# split into panes, kept running when you detach (Ctrl-B d) and come back
# (tmux attach). On libevent (built here) and ncurses (xterm.sh).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libncursesw.a" ]; then
    echo "ncurses isn't built: run tools/ports/xterm.sh first" >&2
    exit 1
fi
L="$PORTS/libevent"

# libevent 2.1.13: its GPG signature (.asc) is good from Nick Mathewson's
# key 2133BC600AB133E1D826D173FE43009C4607B1FB (one of those Arch's
# libevent accepts).
fetch libevent-2.1.13-stable https://github.com/libevent/libevent/releases/download/release-2.1.13-stable/libevent-2.1.13-stable.tar.gz \
    f7e9383b8c0baa81b687e5b5eecc01beefaf1b19b64151d95ed61647fe7a315c
./configure --host=x86_64-linux-musl --prefix="$L" CC="$PCC" --disable-shared --enable-static \
    --disable-openssl --disable-samples --disable-libevent-regress --disable-debug-mode \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 && make install >install.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

# tmux 3.8 (GitHub release): its SHA-512 is the one Alpine's tmux pins.
fetch tmux-3.8 https://github.com/tmux/tmux/releases/download/3.8/tmux-3.8.tar.gz \
    e79c699c7e949dccd0a4a125e17b8d1261e311b16979dbc8eb34542f3966d82e
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" \
    CPPFLAGS="-I$L/include -I$X/include -I$X/include/ncursesw" LDFLAGS="-L$L/lib -L$X/lib -static" \
    LIBEVENT_CFLAGS="-I$L/include" LIBEVENT_LIBS="-L$L/lib -levent_core" \
    LIBTINFO_CFLAGS="-I$X/include/ncursesw" LIBTINFO_LIBS="-L$X/lib -lncursesw" \
    --enable-static --disable-utf8proc \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/share/man/man1"
install -m 755 -s tmux "$SYSROOT/bin/tmux"
cp tmux.1 "$SYSROOT/share/man/man1/tmux.1"
echo "tmux installed in $SYSROOT/bin"
