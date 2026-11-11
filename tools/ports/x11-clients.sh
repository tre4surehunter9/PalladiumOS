#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# X programs for PalladiumOS: tools/ports/x11-clients.sh -> /bin/rxvt (a
# terminal) and /bin/evilwm (a window manager). Both need only libX11,
# which tools/ports/tinyx.sh builds into target/ports/x11.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ]; then
    echo "libX11 not found: run tools/ports/tinyx.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH
# libX11 is static: what it needs comes after it.
XLIBS="-lX11 -lxcb -lXau -lXdmcp"

# rxvt 2.7.10 (its sources moved off SourceForge: MacPorts' copy, the same
# file as MacPorts' and Arch's checksums). Old C; no utmp/wtmp/lastlog
# (PalladiumOS keeps none).
fetch rxvt-2.7.10 https://distfiles.macports.org/rxvt/rxvt-2.7.10.tar.gz \
    616ad56502820264e6933d07bc4eb752aa6940ec14add6e780ffccf15f38d449
# It includes X11/Intrinsic.h (the X Toolkit's) only for the Xlib headers:
# a shim, instead of building libXt.
mkdir -p shim/X11
cat > shim/X11/Intrinsic.h <<'SHIM'
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xresource.h>
#include <X11/Xfuncproto.h>
typedef unsigned long Pixel;
SHIM
# Its default fonts (7x14, 6x13): the X server's built-in "fixed" (6x13),
# by that name.
sed -i 's/"7x14", "6x10", "6x13", "8x13"/"fixed", "6x10", "fixed", "8x13"/' src/defaultfont.h
# (Its config.sub predates musl: today's, from automake.)
AM=$(ls -d /usr/share/automake-*/ | tail -1)
cp "${AM}config.sub" "${AM}config.guess" autoconf/
./configure --host=x86_64-linux-musl CC="$PCC" CFLAGS="-O2 -std=gnu89 -Wno-error=implicit-int \
    -Wno-error=implicit-function-declaration -Wno-error=int-conversion -Wno-error=incompatible-pointer-types" \
    CPPFLAGS="-I$PWD/shim -I$X/include" LDFLAGS="-L$X/lib -static" LIBS="$XLIBS" \
    --x-includes="$X/include" --x-libraries="$X/lib" \
    --disable-utmp --disable-wtmp --disable-lastlog --enable-xterm-scroll >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
# (Its make carries on past errors: check for the program.)
make -j"$(nproc)" >make.log 2>&1
[ -x src/rxvt ] || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
install -m 755 -s src/rxvt "$BIN/rxvt"

# evilwm 1.5, without Xrandr (not built here); with shaped windows
# (libXext).
fetch evilwm-1.5 https://www.6809.org.uk/evilwm/evilwm-1.5.tar.gz \
    6104852413e6d50669361dcadda5a25d39e2b2b0c95a6384022c905957a2740f
sed -i '/^OPT_CPPFLAGS += -DRANDR/d; /^OPT_LDLIBS   += -lXrandr/d' Makefile
# The only fonts are the X server's built-in ones: "fixed" by default.
sed -i 's/#define DEF_FONT        "variable"/#define DEF_FONT        "fixed"/' evilwm.h
# Keys are looked up without XKB (TinyX has no XKB extension: with it, no
# key binding ever matched).
sed -i 's/XkbKeycodeToKeysym(display.dpy, e->keycode, 0, 0)/XLookupKeysym(e, 0)/' bind.c
grep -q 'XLookupKeysym(e, 0)' bind.c || { echo "evilwm: key lookup not patched" >&2; exit 1; }
# Its terminal (Ctrl+Alt+Enter) is rxvt.
sed -i 's/#define DEF_TERM        "x-terminal-emulator"/#define DEF_TERM        "rxvt"/; s/#define DEF_TERM        "xterm"/#define DEF_TERM        "rxvt"/' evilwm.h
make CC="$PCC" CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib -static" LDLIBS="-lxcb -lXau -lXdmcp" >make.log 2>&1 ||
    { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
install -m 755 -s evilwm "$BIN/evilwm"
echo "rxvt and evilwm installed in $BIN"
