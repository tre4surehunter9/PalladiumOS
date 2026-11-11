#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# fvwm95 for PalladiumOS: tools/ports/fvwm95.sh -> /usr/bin/fvwm95, the
# window manager with the Windows 95 look (a taskbar with a Start button),
# its modules and configuration in /usr/lib/X11/fvwm95 and its icons in
# /usr/lib/X11/pixmaps and mini-icons: all on the system disk (/bin is in
# the kernel's memory, kept for the smaller programs).
#
# It needs libXpm (its icons are XPM pixmaps), built here into
# target/ports/x11 beside the libraries tools/ports/tinyx.sh builds.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ]; then
    echo "libX11 not found: run tools/ports/tinyx.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# libXpm 3.5.20 (signed by Alan Coopersmith, X.Org's release manager, as
# libX11 is). No compressed-file support (it runs gzip through popen).
fetch libXpm-3.5.20 https://www.x.org/releases/individual/lib/libXpm-3.5.20.tar.xz \
    c064ead03e4dde78afdeb121fe0b644d25c550444cf903fbdc537f1a78f3ef74
./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
    CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib" --disable-open-zfile --disable-stat-zfile \
    >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

# xbitmaps 1.1.4: the standard X bitmaps (X11/bitmaps/gray...), which
# fvwm95 includes. Signed by Alan Coopersmith.
fetch xbitmaps-1.1.4 https://www.x.org/releases/individual/data/xbitmaps-1.1.4.tar.xz \
    895722f136e21e728c52f2d99fd2dae95018b9ddad1bac1f29d61bcd6593721d
./configure --prefix="$X" >/dev/null
make install >/dev/null

# fvwm95 2.0.43f, the last release (2003): SourceForge's file, matching
# SlackBuilds' checksum.
fetch fvwm95-2.0.43f https://downloads.sourceforge.net/project/fvwm95/fvwm95/2.0.43f/fvwm95-2.0.43f.tar.gz \
    fd455609ead6545d6dd74c40c20ebd955a904a0612704c9206b210f02647d2ee
# It includes the X Toolkit's headers for a few types only (it doesn't use
# libXt): a shim with those, as libXt defines them.
mkdir -p shim/X11
cat > shim/X11/Intrinsic.h <<'SHIM'
#ifndef PALLADIUM_INTRINSIC_SHIM
#define PALLADIUM_INTRINSIC_SHIM
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/Xresource.h>
#include <X11/Xfuncproto.h>
#include <X11/Xos.h>
typedef unsigned long Pixel;
typedef char Boolean;
typedef short Position;
typedef unsigned short Dimension;
typedef unsigned int Cardinal;
typedef char *String;
typedef struct _WidgetRec *Widget;
typedef void *XtPointer;
#define XtNumber(arr) ((Cardinal)(sizeof(arr) / sizeof(arr[0])))
#endif
SHIM
echo '#include <X11/Intrinsic.h>' > shim/X11/StringDefs.h
echo '#include <X11/Intrinsic.h>' > shim/X11/Shell.h
# Old C: K&R definitions, implicit declarations, globals defined in
# headers (-fcommon), built as gnu89 with
# GCC 14's new errors as warnings.
FVWM_CFLAGS="-I$PWD/shim -std=gnu89 -fcommon -Wno-error=implicit-int -Wno-error=implicit-function-declaration \
    -Wno-error=int-conversion -Wno-error=incompatible-pointer-types -Wno-error=return-mismatch"
# (Its configure is autoconf 2.13's: settings in the environment. It sets
# CFLAGS itself, so the flags old C needs go with the compiler.)
# libX11 is static: what it needs comes after it (configure puts LIBS
# after -lX11).
CC="$PCC $FVWM_CFLAGS" CFLAGS="-O2" LDFLAGS="-static -L$X/lib" LIBS="-lxcb -lXau -lXdmcp" ./configure --prefix=/usr \
    --x-includes="$X/include" --x-libraries="$X/lib" >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
# -rdynamic (for modules loaded at run time) means nothing to static
# programs, and palladium-cc refuses it.
find . -name Makefile -exec sed -i 's/-rdynamic//g' {} +
# Most modules' Makefiles name their libraries themselves (not LIBS):
# libX11's go after it there too.
find . -name Makefile -exec sed -i 's/-lX11\b/-lX11 -lxcb -lXau -lXdmcp/g' {} +
# The taskbar uses safemalloc without its declaration: in C89 that means it
# returns an int, and the 64-bit pointer is cut to 32 bits (a crash at
# start). The only such function returning a pointer (built with
# -Wimplicit-function-declaration to check).
sed -i 's|^extern char \*Module;|extern char *Module;\nchar *safemalloc(int);|' modules/FvwmTaskBar/Mallocs.c
grep -q '^char \*safemalloc(int);' modules/FvwmTaskBar/Mallocs.c || { echo "Mallocs.c not patched" >&2; exit 1; }
# The taskbar's plugins are shared libraries, which a static program can't
# load: not built.
sed -i '/^MODULES=/,/[^\\]$/d; s/^all: ${PROG} modules/all: ${PROG}/' modules/FvwmTaskBar/Makefile
make -j"$(nproc)" fvwm_icons=/usr/lib/X11/pixmaps >make.log 2>&1
[ -x fvwm/fvwm95 ] && [ -x modules/FvwmTaskBar/FvwmTaskBar ] ||
    { grep -B2 -A6 "error" make.log | tail -40; exit 1; }
# (Some modules don't build: FvwmIconMan, FvwmScroll (libXmu), FvwmScript.
# None of them is needed.)

# Install, on the system disk (/usr): fvwm95 and xpmroot in bin, its
# modules and configuration in lib/X11/fvwm95, its pixmaps and mini-icons
# where the configuration looks for them.
mkdir -p "$SYSROOT/bin"
rm -f "$BIN/fvwm95" "$BIN/xpmroot" # (where an earlier version put them)
install -m 755 -s fvwm/fvwm95 "$SYSROOT/bin/fvwm95"
install -m 755 -s xpmroot/xpmroot "$SYSROOT/bin/xpmroot" # sets the desktop's background
FD="$SYSROOT/lib/X11/fvwm95"
rm -rf "$FD" "$SYSROOT/lib/X11/pixmaps" "$SYSROOT/lib/X11/mini-icons"
mkdir -p "$FD" "$SYSROOT/lib/X11/pixmaps" "$SYSROOT/lib/X11/mini-icons"
for m in FvwmAuto FvwmBacker FvwmBanner FvwmButtons FvwmConsole FvwmForm FvwmIconBox \
    FvwmIdent FvwmPager FvwmSave FvwmSaveDesk FvwmTalk FvwmTaskBar FvwmWharf FvwmWinList; do
    [ -x "modules/$m/$m" ] && install -m 755 -s "modules/$m/$m" "$FD/$m"
done
install -m 644 "$ROOT/tools/ports/fvwm95/system.fvwm95rc" "$FD/system.fvwm95rc"
install -m 644 pixmaps/*.xpm "$SYSROOT/lib/X11/pixmaps/"
install -m 644 mini-icons/*.xpm "$SYSROOT/lib/X11/mini-icons/"
install -m 644 "$ROOT/tools/ports/fvwm95/desktop.xpm" "$SYSROOT/lib/X11/pixmaps/desktop.xpm"
echo "fvwm95 installed: $SYSROOT/bin/fvwm95, $FD ($(ls "$FD" | wc -l) files)"
