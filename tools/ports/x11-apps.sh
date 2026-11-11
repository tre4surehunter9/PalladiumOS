#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Classic X programs for PalladiumOS: tools/ports/x11-apps.sh -> xclock,
# xeyes, xcalc and xlogo in /usr/bin (on the system disk), with their
# resource files in /usr/share/X11/app-defaults.
#
# They're built on the X Toolkit and the Athena widgets, built here first
# (static, into target/ports/x11 with the rest of tinyx.sh's libraries):
# libICE, libSM, libXt, libXmu, libXaw; and libXfixes and libXi (XInput,
# for xeyes), libXrender, libXrandr (xev), libXcursor (xsetroot), libXtst (xdpyinfo).
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
# libXrender, libXrandr (xev), libXcursor (xsetroot) and libXtst (xdpyinfo).
fetch libXtst-1.2.6 https://www.x.org/releases/individual/lib/libXtst-1.2.6.tar.xz \
    f94f0b37c07d80d877f3eda2c2405cc7e62aa9842f6310354aa349e3d66a4fe5
lib --disable-specs --without-xmlto
fetch libXrender-0.9.13 https://www.x.org/releases/individual/lib/libXrender-0.9.13.tar.xz \
    3fe88c609df6782e1e7af2e4de36b2563d70cd70ff7dcb1cda561ad64912bb64
lib
fetch libXrandr-1.5.5 https://www.x.org/releases/individual/lib/libXrandr-1.5.5.tar.xz \
    72b922c2e765434e9e9f0960148070bd4504b288263e2868a4ccce1b7cf2767a
lib
fetch libXcursor-1.2.3 https://www.x.org/releases/individual/lib/libXcursor-1.2.3.tar.xz \
    fde9402dd4cfe79da71e2d96bb980afc5e6ff4f8a7d74c159e1966afb2b2c2c0
lib

# The programs. Static: everything a library needs comes after it.
APPLIBS="$(pkg-config --static --libs xaw7 xmu xtst xi xrandr xcursor xrender xfixes xt sm ice xpm xext x11)"
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
    # Its data (xedit's Lisp, for one) in /usr/lib/X11, as its install puts it.
    rm -rf stage && make install DESTDIR="$PWD/stage" >install.log 2>&1 || { tail -10 install.log; exit 1; }
    if [ -d stage/usr/lib/X11 ]; then
        mkdir -p "$SYSROOT/lib/X11"
        cp -a stage/usr/lib/X11/. "$SYSROOT/lib/X11/"
    fi
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

# More of X.Org's programs: an editor, messages, a load graph, a round
# clock, a magnifier, a font browser, a bitmap editor, a demo, screenshots
# (xwd, xwud), and tools that tell about windows and the server.
fetch xedit-1.2.5 https://www.x.org/releases/individual/app/xedit-1.2.5.tar.xz \
    733460cbf20a9d88e2a482ff1b288f51a97150cff807c7ffb98d2430546c8867
app xedit
fetch xmessage-1.0.7 https://www.x.org/releases/individual/app/xmessage-1.0.7.tar.xz \
    703fccb7a0b772d61d7e603c189b9739866aa97ba985c727275420f829a30356
app xmessage
fetch xload-1.2.2 https://www.x.org/releases/individual/app/xload-1.2.2.tar.xz \
    112a75e481783b52a20d37367f57983d60919f78059f0e107458dde236804131
app xload
fetch oclock-1.0.6 https://www.x.org/releases/individual/app/oclock-1.0.6.tar.xz \
    3ee85e5e2d211eca7bdb8b2fe0e3baf31998829af48198b5fa9bbee2aa0d24be
app oclock --without-xkb
fetch xmag-1.0.8 https://www.x.org/releases/individual/app/xmag-1.0.8.tar.xz \
    326d3c583d795bb53ac609d144e7f7fb1499baba7eaec14b8e6cd232ea069532
app xmag
fetch xfontsel-1.1.2 https://www.x.org/releases/individual/app/xfontsel-1.1.2.tar.xz \
    78ff018757c7b22137cf5d3526ef5e9388bc0cdead1f00daf2c898e2e274553f
app xfontsel
fetch bitmap-1.1.2 https://www.x.org/releases/individual/app/bitmap-1.1.2.tar.xz \
    5c6d2637244c575a9cc65a6b1ee15ced480ef0184821aceeab29650daf323c12
app bitmap
fetch ico-1.0.7 https://www.x.org/releases/individual/app/ico-1.0.7.tar.xz \
    56a96d6429c387e54aec23b522f548487d5db8296092f21bb289b67845069915
app ico
fetch xwd-1.0.10 https://www.x.org/releases/individual/app/xwd-1.0.10.tar.xz \
    4909eacc26c4cd5a9afcd1468aec26e7c646544639031bdad4f71173aa82f3c3
app xwd --without-xkb
fetch xwud-1.0.8 https://www.x.org/releases/individual/app/xwud-1.0.8.tar.xz \
    2e282648c34018fd870052b8d814ea4d35ad9fdb9bcfc8bdc5a06822c0f86e99
app xwud
fetch xev-1.2.7 https://www.x.org/releases/individual/app/xev-1.2.7.tar.xz \
    95167895924de58e34b1013b2b0c8476e90d0888c6c39e7ae9bc35e3a19dba04
app xev
fetch xprop-1.2.8 https://www.x.org/releases/individual/app/xprop-1.2.8.tar.xz \
    d689e2adb7ef7b439f6469b51cda8a7daefc83243854c2a3b8f84d0f029d67ee
app xprop
fetch xwininfo-1.1.7 https://www.x.org/releases/individual/app/xwininfo-1.1.7.tar.xz \
    bee14d594cc86cc59aae1015c1b452a71bf60c304131e2716ca1cf0df733b4ac
app xwininfo
fetch xdpyinfo-1.4.0 https://www.x.org/releases/individual/app/xdpyinfo-1.4.0.tar.xz \
    dc1de6e6e091ed46c4837b0ae9811e8182f7be0d2af638cab3e530ff081a48b6
app xdpyinfo --without-dga --without-xf86misc --without-xinerama --without-dmx --without-xtst --without-xkb --without-xf86vidmode --without-xcomposite --without-xinput --without-xrender
fetch xkill-1.0.7 https://www.x.org/releases/individual/app/xkill-1.0.7.tar.xz \
    5ff264bc4eebc0459255a92036dc8dc84b6d48caef8509f88a783beaadea750b
app xkill
fetch xsetroot-1.1.4 https://www.x.org/releases/individual/app/xsetroot-1.1.4.tar.xz \
    1315a3f7e9abe06357363b93461e272601f67225ce0bc075c430cce35073362b
app xsetroot

# libX11's data on the system disk (where it looks: /usr/share/X11): the
# locales (programs using fontsets, like the Athena widgets, need them),
# the error messages and the colour database.
rm -rf "$SYSROOT/share/X11/locale"
cp -a "$X/share/X11/locale" "$SYSROOT/share/X11/"
install -m 644 "$X/share/X11/XErrorDB" "$X/share/X11/Xcms.txt" "$SYSROOT/share/X11/"

echo "X programs installed in $SYSROOT/bin; app-defaults: $(ls "$APPDEF" | tr '\n' ' ')"
