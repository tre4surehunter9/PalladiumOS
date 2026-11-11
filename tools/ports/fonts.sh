#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Fonts for X: tools/ports/fonts.sh -> /usr/share/fonts/X11/misc (the
# classic fixed-width fonts: fixed, 6x13, 7x14, 9x15...) and
# /usr/share/fonts/X11/75dpi (Adobe's Helvetica, Courier, Times...), on the
# system disk, where the X server looks (tools/ports/tinyx.sh).
#
# They come as BDF (text): bdftopcf (X.Org's, built here for this machine)
# compiles them to PCF, which the X server loads without parsing (BDF's
# Unicode fonts took it many seconds), gzipped. fonts.dir (what mkfontdir
# would make) is written here from each font's name, and fonts.alias gives
# the usual short names (fixed, variable, 6x13...).
. "$(dirname "$0")/common.sh"

FONTS="$SYSROOT/share/fonts/X11"
X="$PORTS/x11"

# bdftopcf 1.1.2, for this machine (signed by Alan Coopersmith), with the
# X protocol headers tinyx.sh installed.
fetch bdftopcf-1.1.2 https://www.x.org/releases/individual/util/bdftopcf-1.1.2.tar.xz \
    bc60be5904330faaa3ddd2aed7874bee2f29e4387c245d6787552f067eb0523a
PKG_CONFIG_PATH="$X/share/pkgconfig:$X/lib/pkgconfig" ./configure CC=gcc CPPFLAGS="-I$X/include" >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -20; exit 1; }
BDFTOPCF="$PWD/bdftopcf"

# dir SRC DEST: the BDF fonts in SRC, as gzipped PCF, into DEST, with
# fonts.dir and the licence.
install_fonts() {
    src=$1 dest=$2
    rm -rf "$dest" && mkdir -p "$dest"
    for f in "$src"/*.bdf; do
        "$BDFTOPCF" -t "$f" | gzip -9 -n > "$dest/$(basename "$f" .bdf).pcf.gz"
    done
    # fonts.dir: how many, then "FILE XLFD" (lower-case, as mkfontdir writes).
    tmp=$(mktemp)
    for f in "$src"/*.bdf; do
        name=$(sed -n 's/^FONT //p' "$f" | head -1 | tr 'A-Z' 'a-z')
        [ -n "$name" ] && echo "$(basename "$f" .bdf).pcf.gz $name" >> "$tmp"
    done
    { wc -l < "$tmp" | tr -d ' '; sort "$tmp"; } > "$dest/fonts.dir"
    rm -f "$tmp"
    install -m 644 "$src/COPYING" "$dest/COPYING"
}

# font-misc-misc 1.1.3 (public domain), without the four big Japanese and
# Korean fonts (12x13ja, 18x18ja, 18x18ko, k14).
fetch font-misc-misc-1.1.3 https://www.x.org/releases/individual/font/font-misc-misc-1.1.3.tar.xz \
    79abe361f58bb21ade9f565898e486300ce1cc621d5285bec26e14b6a8618fed
rm -f 12x13ja.bdf 18x18ja.bdf 18x18ko.bdf k14.bdf
install_fonts "$PWD" "$FONTS/misc"
# The short names. "fixed" is what X programs ask for when they ask for
# nothing in particular.
cat > "$FONTS/misc/fonts.alias" <<'ALIAS'
fixed        -misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso10646-1
variable     -adobe-helvetica-bold-r-normal--12-120-75-75-p-70-iso10646-1
5x7          -misc-fixed-medium-r-normal--7-70-75-75-c-50-iso10646-1
5x8          -misc-fixed-medium-r-normal--8-80-75-75-c-50-iso10646-1
6x9          -misc-fixed-medium-r-normal--9-90-75-75-c-60-iso10646-1
6x10         -misc-fixed-medium-r-normal--10-100-75-75-c-60-iso10646-1
6x12         -misc-fixed-medium-r-semicondensed--12-110-75-75-c-60-iso10646-1
6x13         -misc-fixed-medium-r-semicondensed--13-120-75-75-c-60-iso10646-1
6x13bold     -misc-fixed-bold-r-semicondensed--13-120-75-75-c-60-iso10646-1
7x13         -misc-fixed-medium-r-normal--13-120-75-75-c-70-iso10646-1
7x13bold     -misc-fixed-bold-r-normal--13-120-75-75-c-70-iso10646-1
7x14         -misc-fixed-medium-r-normal--14-130-75-75-c-70-iso10646-1
7x14bold     -misc-fixed-bold-r-normal--14-130-75-75-c-70-iso10646-1
8x13         -misc-fixed-medium-r-normal--13-120-75-75-c-80-iso10646-1
8x13bold     -misc-fixed-bold-r-normal--13-120-75-75-c-80-iso10646-1
9x15         -misc-fixed-medium-r-normal--15-140-75-75-c-90-iso10646-1
9x15bold     -misc-fixed-bold-r-normal--15-140-75-75-c-90-iso10646-1
9x18         -misc-fixed-medium-r-normal--18-120-100-100-c-90-iso10646-1
9x18bold     -misc-fixed-bold-r-normal--18-120-100-100-c-90-iso10646-1
10x20        -misc-fixed-medium-r-normal--20-200-75-75-c-100-iso10646-1
ALIAS

# font-adobe-75dpi 1.0.4 (Adobe's licence: use and copying with its notice,
# in COPYING).
fetch font-adobe-75dpi-1.0.4 https://www.x.org/releases/individual/font/font-adobe-75dpi-1.0.4.tar.xz \
    1281a62dbeded169e495cae1a5b487e1f336f2b4d971d92911c59c103999b911
install_fonts "$PWD" "$FONTS/75dpi"

echo "fonts installed in $FONTS: $(head -1 "$FONTS/misc/fonts.dir") misc, $(head -1 "$FONTS/75dpi/fonts.dir") 75dpi"
