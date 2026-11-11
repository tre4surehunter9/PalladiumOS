#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Classic X programs for PalladiumOS: tools/ports/x11-apps.sh -> xclock,
# xeyes, xcalc and xlogo in /usr/bin (on the system disk), with their
# resource files in /usr/share/X11/app-defaults.
#
# They're built on the X Toolkit and the Athena widgets, built here first
# (static, into target/ports/x11 with the rest of tinyx.sh's libraries):
# libICE, libSM, libXt, libXmu, libXaw; and libXfixes and libXi (XInput,
# for xeyes).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ] || [ ! -f "$X/lib/libXpm.a" ]; then
    echo "the X libraries aren't built: run tools/ports/tinyx.sh and fvwm95.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH
CONF="--host=x86_64-linux-musl --prefix=$X --disable-shared --enable-static"

lib() {
    ./configure $CONF CC="$PCC" CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib" "$@" >configure.log 2>&1 ||
        { tail -20 configure.log; exit 1; }
    make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
}

# The libraries (signed by X.Org's release managers: Alan Coopersmith,
# Matt Turner; libXaw by its maintainer, Thomas E. Dickey).
fetch libICE-1.1.2 https://www.x.org/releases/individual/lib/libICE-1.1.2.tar.xz \
    974e4ed414225eb3c716985df9709f4da8d22a67a2890066bc6dfc89ad298625
lib --disable-docs --without-xmlto
fetch libSM-1.2.6 https://www.x.org/releases/individual/lib/libSM-1.2.6.tar.xz \
    be7c0abdb15cbfd29ac62573c1c82e877f9d4047ad15321e7ea97d1e43d835be
lib --disable-docs --without-xmlto --without-libuuid
# (makestrs, which writes StringDefs.h, runs on this machine; programs find
# their resource files in /usr/share/X11/app-defaults.)
fetch libXt-1.3.1 https://www.x.org/releases/individual/lib/libXt-1.3.1.tar.xz \
    e0a774b33324f4d4c05b199ea45050f87206586d81655f8bef4dba434d931288
lib CC_FOR_BUILD=gcc --disable-specs --without-xmlto \
    --with-xfile-search-path="/usr/share/X11/%L/%T/%N%C%S:/usr/share/X11/%T/%N%C%S:/usr/share/X11/%T/%N%S"
fetch libXmu-1.3.1 https://www.x.org/releases/individual/lib/libXmu-1.3.1.tar.xz \
    81a99e94c4501e81c427cbaa4a11748b584933e94b7a156830c3621256857bc4
lib --disable-docs --without-xmlto
fetch libXaw-1.0.16 https://www.x.org/releases/individual/lib/libXaw-1.0.16.tar.xz \
    731d572b54c708f81e197a6afa8016918e2e06dfd3025e066ca642a5b8c39c8f
lib --disable-xaw6 --disable-specs --without-xmlto
fetch libXfixes-6.0.2 https://www.x.org/releases/individual/lib/libXfixes-6.0.2.tar.xz \
    39f115d72d9c5f8111e4684164d3d68cc1fd21f9b27ff2401b08fddfc0f409ba
lib
fetch libXi-1.8.4 https://www.x.org/releases/individual/lib/libXi-1.8.4.tar.xz \
    ddd6057b89a91903bc2f884d55c8467440906945e482c36547260dff28661afc
lib --disable-docs --disable-specs --without-xmlto --without-asciidoc

# The programs. Static: everything a library needs comes after it.
APPLIBS="$(pkg-config --static --libs xaw7 xmu xi xfixes xt sm ice xpm xext x11)"
APPDEF="$SYSROOT/share/X11/app-defaults"
mkdir -p "$SYSROOT/bin" "$APPDEF"
app() {
    name=$1
    shift
    ./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" CPPFLAGS="-I$X/include" \
        LDFLAGS="-L$X/lib -static" LIBS="$APPLIBS" "$@" >configure.log 2>&1 ||
        { tail -20 configure.log; exit 1; }
    make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
    install -m 755 -s "$name" "$SYSROOT/bin/$name"
    # Its resource files (app-defaults/NAME, without the build's .ad files).
    if [ -d app-defaults ]; then
        for f in app-defaults/*; do
            case $f in
                *.ad | *.in | *.am | *Makefile*) ;;
                *) if [ -f "$f" ]; then install -m 644 "$f" "$APPDEF/"; fi ;;
            esac
        done
    fi
}

fetch xclock-1.2.1 https://www.x.org/releases/individual/app/xclock-1.2.1.tar.xz \
    e326cac20f7ad2af5412b9c4b70b1c3d392e08cd3cc6246e0a77b7c3935a4851
app xclock --without-xft --without-xkb
fetch xeyes-1.3.1 https://www.x.org/releases/individual/app/xeyes-1.3.1.tar.xz \
    5608d76b7b1aac5ed7f22f1b6b5ad74ef98c8693220f32b4b87dccee4a956eaa
app xeyes --without-xrender --without-present
fetch xcalc-1.1.3 https://www.x.org/releases/individual/app/xcalc-1.1.3.tar.xz \
    86e157b6174678675a4a912995dccd0ff4e2ce32b01bdd6f91bfe93333324980
app xcalc
fetch xlogo-1.0.7 https://www.x.org/releases/individual/app/xlogo-1.0.7.tar.xz \
    8a3f67b20407a53286a7de1a04dbda12d272234d59ef35e68ecfaf7633d83624
app xlogo --without-render

# libX11's data on the system disk (where it looks: /usr/share/X11): the
# locales (programs using fontsets, like the Athena widgets, need them),
# the error messages and the colour database.
rm -rf "$SYSROOT/share/X11/locale"
cp -a "$X/share/X11/locale" "$SYSROOT/share/X11/"
install -m 644 "$X/share/X11/XErrorDB" "$X/share/X11/Xcms.txt" "$SYSROOT/share/X11/"

echo "X programs installed in $SYSROOT/bin: xclock xeyes xcalc xlogo; app-defaults: $(ls "$APPDEF" | tr '\n' ' ')"
