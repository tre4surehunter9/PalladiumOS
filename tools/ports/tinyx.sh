#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# An X server for PalladiumOS: tools/ports/tinyx.sh -> /usr/bin/Xfbdev, TinyX's
# framebuffer X server (the small "kdrive" server, as TinyCore Linux uses).
# It draws on /dev/fb0, takes the keyboard from the console (medium raw
# mode) and the mouse from /dev/input/mice. Fonts: the ones on the system
# disk (tools/ports/fonts.sh: misc and Adobe's 75dpi), then libXfont's
# built-in "fixed" and "cursor".
#
# Its libraries are built first, static, into target/ports/x11 (where the X
# client libraries go too, see tools/ports/x11-clients.sh).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
mkdir -p "$X"
# Only these libraries, never this machine's.
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH
# palladium-cc only looks in musl's headers and libraries: add these.
CPPFLAGS="-I$X/include"
LDFLAGS="-L$X/lib"
export CPPFLAGS LDFLAGS
CONF="--host=x86_64-linux-musl --prefix=$X --disable-shared --enable-static"

fetch xorgproto-2026.1 https://www.x.org/releases/individual/proto/xorgproto-2026.1.tar.xz \
    f9bfe4a9ed8c8ab9d2a3b0d49797f046052dadd06b7a8b45dbffaffb137e8290
./configure $CONF CC="$PCC" >/dev/null
make install >/dev/null

fetch xtrans-1.5.2 https://www.x.org/releases/individual/lib/xtrans-1.5.2.tar.xz \
    5c5cbfe34764a9131d048f03c31c19e57fb4c682d67713eab6a65541b4dff86c
./configure $CONF CC="$PCC" >/dev/null
make install >/dev/null

fetch zlib-1.3.1 https://zlib.net/fossils/zlib-1.3.1.tar.gz \
    9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23
CC="$PCC" ./configure --prefix="$X" --static >/dev/null
make -j"$(nproc)" install >/dev/null

fetch libfontenc-1.1.8 https://www.x.org/releases/individual/lib/libfontenc-1.1.8.tar.xz \
    7b02c3d405236e0d86806b1de9d6868fe60c313628b38350b032914aa4fd14c6
./configure $CONF CC="$PCC" >/dev/null
make -j"$(nproc)" install >/dev/null

# libXfont 1.x (the "xfont" TinyX wants), without FreeType: the built-in
# bitmap fonts are enough.
fetch libXfont-1.5.4 https://www.x.org/releases/individual/lib/libXfont-1.5.4.tar.bz2 \
    1a7f7490774c87f2052d146d1e0e64518d32e6848184a18654e8d0bb57883242
./configure $CONF CC="$PCC" --disable-freetype --without-bzip2 --disable-devel-docs >/dev/null
make -j"$(nproc)" install >/dev/null

# The X client library (libX11 on libxcb) and libXext: for X programs, and
# their headers for TinyX (which predates XShm.h and dpms.h moving there).
fetch libXau-1.0.12 https://www.x.org/releases/individual/lib/libXau-1.0.12.tar.xz \
    74d0e4dfa3d39ad8939e99bda37f5967aba528211076828464d2777d477fc0fb
./configure $CONF CC="$PCC" >/dev/null
make -j"$(nproc)" install >/dev/null

fetch libXdmcp-1.1.5 https://www.x.org/releases/individual/lib/libXdmcp-1.1.5.tar.xz \
    d8a5222828c3adab70adf69a5583f1d32eb5ece04304f7f8392b6a353aa2228c
./configure $CONF CC="$PCC" --disable-docs >/dev/null
make -j"$(nproc)" install >/dev/null

fetch xcb-proto-1.17.0 https://www.x.org/releases/individual/proto/xcb-proto-1.17.0.tar.xz \
    2c1bacd2110f4799f74de6ebb714b94cf6f80fb112316b1219480fd22562148c
./configure --prefix="$X" PYTHON=python3 >/dev/null
make install >/dev/null

fetch libxcb-1.17.0 https://www.x.org/releases/individual/lib/libxcb-1.17.0.tar.xz \
    599ebf9996710fea71622e6e184f3a8ad5b43d0e5fa8c4e407123c88a59a6d55
./configure $CONF CC="$PCC" PYTHON=python3 --disable-devel-docs >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" install >/dev/null

# Its data (locales, the error database) is looked for in /usr/share/X11
# on PalladiumOS, and installed here for now.
fetch libX11-1.8.14 https://www.x.org/releases/individual/lib/libX11-1.8.14.tar.xz \
    bbc78c46217174655c2b32b138ad119f3ac15f50808b4d0b3a2677a4b1afc950
./configure $CONF CC="$PCC" CC_FOR_BUILD=gcc --datadir=/usr/share --disable-xf86bigfont \
    --disable-specs --without-xmlto >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage && make install DESTDIR="$PWD/stage" >/dev/null
cp -a "stage$X/." "$X/"
mkdir -p "$X/share" && cp -a stage/usr/share/. "$X/share/"

fetch libXext-1.3.7 https://www.x.org/releases/individual/lib/libXext-1.3.7.tar.xz \
    6c643c7035cdacf67afd68f25d01b90ef889d546c9fcd7c0adf7c2cf91e3a32d
./configure $CONF CC="$PCC" --disable-specs --without-xmlto >/dev/null
make -j"$(nproc)" install >/dev/null

fetch tinyx-feab72ca891bc04b18763763e15ee4e532369cdf \
    https://github.com/tinycorelinux/tinyx/archive/feab72ca891bc04b18763763e15ee4e532369cdf.tar.gz \
    82f69bbd7a979d30a25a80082ba6fdcb975740ba36836795a3763814aff0adca
# Old headers it includes for the server side only, now split up: shims
# that only TinyX sees.
mkdir -p shim/X11/extensions
cat > shim/X11/extensions/XTest.h <<'SHIM'
/* The server side of the old XTest.h: now in xorgproto. */
#include <X11/extensions/xtestconst.h>
#include <X11/extensions/xtestproto.h>
SHIM
CPPFLAGS="-I$PWD/shim $CPPFLAGS"
# The pointer drawn by the graphics device where the kernel can (a virtio
# GPU's hardware cursor): tools/ports/tinyx/hwcursor.c, in the card's table.
cp "$ROOT/tools/ports/tinyx/hwcursor.c" kdrive/fbdev/palladium-cursor.c
grep -q palladium-cursor.c kdrive/fbdev/fbdev.c || echo '#include "palladium-cursor.c"' >>kdrive/fbdev/fbdev.c
grep -q fbdevInitCursor kdrive/fbdev/fbdev.h ||
    sed -i 's|^#endif[[:space:]]*/\* _FBDEV_H_ \*/|Bool fbdevInitCursor(ScreenPtr pScreen);\nvoid fbdevEnableCursor(ScreenPtr pScreen);\nvoid fbdevDisableCursor(ScreenPtr pScreen);\nvoid fbdevFiniCursor(ScreenPtr pScreen);\n\n&|' kdrive/fbdev/fbdev.h
sed -i 's|^\t0,\t\t\t/\* initCursor \*/|\tfbdevInitCursor,\t/* initCursor */|; s|^\t0,\t\t\t/\* enableCursor \*/|\tfbdevEnableCursor,\t/* enableCursor */|; s|^\t0,\t\t\t/\* disableCursor \*/|\tfbdevDisableCursor,\t/* disableCursor */|; s|^\t0,\t\t\t/\* finiCursor \*/|\tfbdevFiniCursor,\t/* finiCursor */|' kdrive/fbdev/fbinit.c
grep -q fbdevInitCursor kdrive/fbdev/fbinit.c && grep -q fbdevInitCursor kdrive/fbdev/fbdev.h ||
    { echo "TinyX's fbdev not patched for the hardware cursor" >&2; exit 1; }
# (Its configure script is made here; xtrans's macros are in $X.)
ACLOCAL_PATH="$X/share/aclocal" autoreconf -fi >autoreconf.log 2>&1 || { tail -20 autoreconf.log; exit 1; }
# Old C (built as gnu17): INITARGS (an extension init's parameters) is
# void; KDRIVESERVER also in dix (it registers the built-in fonts); GCC 14's
# new errors for old idioms stay warnings.
TINYX_CFLAGS="-O2 -std=gnu17 -DKDRIVESERVER -DINITARGS=void -Wno-error=implicit-int -Wno-error=implicit-function-declaration \
    -Wno-error=int-conversion -Wno-error=incompatible-pointer-types -Wno-error=return-mismatch"
./configure $CONF CC="$PCC" CFLAGS="$TINYX_CFLAGS" LDFLAGS="$LDFLAGS -static" \
    --disable-xvesa --enable-xfbdev --disable-xdmcp --disable-xdm-auth-1 --disable-dpms \
    --with-default-font-path="/usr/share/fonts/X11/misc/,/usr/share/fonts/X11/75dpi/,built-ins" \
    --with-vendor-name="PalladiumOS" >configure.log 2>&1 ||
    { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -40; exit 1; }
# On the system disk, with the rest of X (/bin is in the kernel's memory).
mkdir -p "$SYSROOT/bin"
rm -f "$BIN/Xfbdev" # (where an earlier version put it)
install -m 755 -s kdrive/fbdev/Xfbdev "$SYSROOT/bin/Xfbdev"
echo "Xfbdev installed in $SYSROOT/bin"
