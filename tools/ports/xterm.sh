#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# xterm for PalladiumOS: tools/ports/xterm.sh -> /usr/bin/xterm (with its
# resources in /usr/share/X11/app-defaults), and ncurses, which it needs:
# the library (static, in target/ports/x11, for this and other programs)
# and a terminfo database in /usr/share/terminfo (the terminals programs
# meet here: xterm, rxvt, the console, screen...), compiled with this
# machine's tic.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libXaw7.a" ]; then
    echo "the X Toolkit isn't built: run tools/ports/x11-apps.sh first" >&2
    exit 1
fi
if ! command -v tic >/dev/null; then
    echo "tic (from ncurses) is needed on this machine" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# ncurses 6.6 (GNU's copy, signed by its maintainer, Thomas E. Dickey):
# wide characters, no programs, terminfo in /usr/share/terminfo.
fetch ncurses-6.6 https://ftp.gnu.org/gnu/ncurses/ncurses-6.6.tar.gz \
    355b4cbbed880b0381a04c46617b7656e362585d52e9cf84a67e2009b749ff11
./configure --host=x86_64-linux-musl --prefix="$X" CC="$PCC" --with-build-cc=gcc \
    --without-shared --without-debug --without-ada --without-cxx-binding --without-manpages \
    --without-progs --without-tests --enable-widec --disable-db-install \
    --with-default-terminfo-dir=/usr/share/terminfo --with-terminfo-dirs=/usr/share/terminfo \
    --enable-pc-files --with-pkg-config-libdir="$X/lib/pkgconfig" >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
make install >install.log 2>&1 || { tail -20 install.log; exit 1; }
# The terminals' descriptions, from ncurses' own list.
TI="$SYSROOT/share/terminfo"
rm -rf "$TI" && mkdir -p "$TI"
tic -x -o "$TI" -e xterm,xterm-256color,xterm-color,xterm-new,rxvt,rxvt-basic,rxvt-color,linux,vt100,vt102,vt220,ansi,screen,screen-256color,tmux,tmux-256color,dumb \
    misc/terminfo.src 2>tic.log || { tail -10 tic.log; exit 1; }

# xterm 412 (signed by Thomas E. Dickey; from his mirror: the main site
# refuses downloaders): no FreeType (bitmap fonts), not setuid.
fetch xterm-412 https://invisible-mirror.net/archives/xterm/xterm-412.tgz \
    b48bc780c1cbc14f6d89269f7e593712af6095876960d650be28b55e5dad65a6
XLIBS="$(pkg-config --static --libs xaw7 xmu xt sm ice xpm xext x11)"
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" \
    CPPFLAGS="-I$X/include -I$X/include/ncursesw" LDFLAGS="-L$X/lib -static" LIBS="-lncursesw $XLIBS" \
    --x-includes="$X/include" --x-libraries="$X/lib" --with-x \
    --disable-freetype --disable-setuid --disable-setgid --without-utempter --disable-luit \
    --enable-wide-chars --disable-imake --without-pkg-config \
    --with-app-defaults=/usr/share/X11/app-defaults >configure.log 2>&1 ||
    { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
mkdir -p "$SYSROOT/bin" "$SYSROOT/share/X11/app-defaults"
install -m 755 -s xterm "$SYSROOT/bin/xterm"
for f in XTerm UXTerm XTerm-color; do
    if [ -f "$f.ad" ]; then install -m 644 "$f.ad" "$SYSROOT/share/X11/app-defaults/$f"; fi
done
echo "xterm installed in $SYSROOT/bin; terminfo: $(ls "$TI" | tr '\n' ' ')"
