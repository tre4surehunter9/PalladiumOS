#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Games for X: tools/ports/games.sh -> /usr/bin/xboard (chess, against
# Fairy-Max, a small chess engine: /usr/bin/fairymax), xjewel (falling
# jewels), xsnow (snow and Santa on the desktop) and xroach (cockroaches
# that hide under your windows). On the X libraries tinyx.sh, fvwm95.sh
# and x11-apps.sh build.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libXaw7.a" ]; then
    echo "the X Toolkit isn't built: run tools/ports/x11-apps.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH
XLIBS="$(pkg-config --static --libs xpm xext x11)"
XAWLIBS="$(pkg-config --static --libs xaw7 xmu xt sm ice xpm xext x11)"
# Old C, built as gnu89; undeclared functions are reported (an undeclared
# function returning a pointer cuts it to 32 bits).
OLDC="$PCC -std=gnu89 -fcommon -O2 -Wimplicit-function-declaration -Wno-error=implicit-int -Wno-error=implicit-function-declaration -Wno-error=int-conversion -Wno-error=incompatible-pointer-types -Wno-error=return-mismatch"
mkdir -p "$SYSROOT/bin" "$SYSROOT/share/X11/app-defaults"

# cc OUT FILES...: an old C program, static, on Xlib.
cc() {
    out=$1
    shift
    $OLDC -I"$X/include" "$@" -L"$X/lib" -static $XLIBS -lm -o "$out" 2>build.log ||
        { grep -B2 -A6 "error" build.log | tail -20; exit 1; }
    if grep "implicit declaration of function" build.log | grep -v "built-in" >/dev/null; then
        echo "$out: undeclared functions:" >&2
        grep "implicit declaration of function" build.log | grep -v "built-in" >&2
    fi
    install -m 755 -s "$out" "$SYSROOT/bin/$out"
}

# Fairy-Max 5.0b (Debian's copy, matching its archive), the engine.
fetch fairymax-5.0b https://snapshot.debian.org/archive/debian/20200101T000000Z/pool/main/f/fairymax/fairymax_5.0b.orig.tar.gz \
    3e36ba168aa10179225f8dd6953d1c39c4cd4526572765b2e9c77717efe3e52f
$OLDC -DFAIRYDIR=\"/usr/share/fairymax\" fairymax.c -static -o fairymax 2>build.log ||
    { tail -20 build.log; exit 1; }
install -m 755 -s fairymax "$SYSROOT/bin/fairymax"
mkdir -p "$SYSROOT/share/fairymax"
install -m 644 data/fmax.ini "$SYSROOT/share/fairymax/fmax.ini"

# XBoard 4.6.2 (signed by its maintainer, Arun Persaud): the last one on
# the Athena widgets alone (later ones need cairo). Fairy-Max by default.
fetch xboard-4.6.2 https://ftp.gnu.org/gnu/xboard/xboard-4.6.2.tar.gz \
    2e57f2d2c6e90f704a2c5bc0c6e14840d88dc4f9a450ecf1b4c06516a00389df
# (Its config.sub predates musl: today's, from automake.)
AM=$(ls -d /usr/share/automake-*/ | tail -1)
cp "${AM}config.sub" "${AM}config.guess" .
CC="$OLDC" CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib -static" LIBS="$XAWLIBS" \
    ./configure --host=x86_64-linux-musl --prefix=/usr --with-Xaw --without-gtk --disable-nls \
    --with-x --x-includes="$X/include" --x-libraries="$X/lib" >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
# (Its own library list would put libXt before libXaw, whose VendorShell
# must come first: LIBS alone has them in order.)
make -j"$(nproc)" xboard_LDADD=-lm >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
install -m 755 -s xboard "$SYSROOT/bin/xboard"
rm -rf "$SYSROOT/share/xboard"
mkdir -p "$SYSROOT/share/xboard"
for d in bitmaps pixmaps sounds themes; do
    if [ -d "$d" ]; then cp -a "$d" "$SYSROOT/share/xboard/"; fi
done
# Its settings: Fairy-Max as the engine.
printf '%s\n' '-fcp fairymax' '-scp fairymax' > "$SYSROOT/share/xboard/xboard.conf"

# xjewel 1.6 (Debian's copy): its high scores in /tmp. (It uses open and
# close without their headers.)
fetch xjewel-1.6.orig https://snapshot.debian.org/archive/debian/20200101T000000Z/pool/main/x/xjewel/xjewel_1.6.orig.tar.gz \
    38188bf9baceee11804b44e46e82e5e767a5430d4d2b6131acc279a955ed01ce
cc xjewel -include fcntl.h -include unistd.h -DHSCORE_FILE=\"/tmp/xjewel.scores\" -Ibitmaps xw.c jewel.c logic.c panel.c xhscore.c hscore.c game.c intro.c help.c

# xsnow 1.42 (Debian's copy): the last one on Xlib alone (later ones are
# GTK).
fetch xsnow-1.42.orig https://snapshot.debian.org/archive/debian/20190101T000000Z/pool/main/x/xsnow/xsnow_1.42.orig.tar.gz \
    89d82ac37c8b86c9308a9a23a311ed4d07c242499fce8dc9607e49f21328dbf1
cc xsnow xsnow.c toon_root.c

# xroach 4.0 (Debian's copy).
fetch xroach-4.0 https://snapshot.debian.org/archive/debian/20200101T000000Z/pool/main/x/xroach/xroach_4.0.orig.tar.gz \
    1cbbfee918c9c3d4316b1cff0467082391786d72e460baa034a54817a1a6f10e
cc xroach xroach.c

echo "games installed in $SYSROOT/bin: xboard fairymax xjewel xsnow xroach"
