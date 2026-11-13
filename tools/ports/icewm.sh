#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# IceWM for PalladiumOS: tools/ports/icewm.sh -> /usr/bin/icewm (the window
# manager startx starts: a taskbar with a menu, a clock and the windows'
# buttons, themes) and icewmbg (the desktop background), with its themes
# and PalladiumOS's menu in /usr/share/icewm. C++ (palladium-c++), on the
# X core fonts and XPM, PNG and JPEG images (libpng and libjpeg: links.sh's);
# with libXcomposite and libXdamage, built here (the rest of X is tinyx.sh's
# and x11-apps.sh's).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libXrender.a" ] || [ ! -x "$ROOT/target/musl/bin/palladium-c++" ] || [ ! -f "$X/lib/libjpeg.a" ]; then
    echo "run tools/ports/x11-apps.sh, tools/ports/links.sh and tools/ports/cxx.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

xlib() { # xlib NAME SHA256: an X.Org library (signed by Alan Coopersmith)
    fetch "$1" "https://www.x.org/releases/individual/lib/$1.tar.xz" "$2"
    ./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
        CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib" >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
    make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -20; exit 1; }
}
xlib libXcomposite-0.4.7 8bdf310967f484503fa51714cf97bff0723d9b673e0eecbf92b3f97c060c8ccb
xlib libXdamage-1.1.7 127067f521d3ee467b97bcb145aeba1078e2454d448e8748eb984d5b397bde24

# IceWM 4.1.0 (GitHub's tarball of the tag, matching Arch's checksum).
fetch icewm-4.1.0 https://github.com/ice-wm/icewm/archive/refs/tags/4.1.0.tar.gz \
    3ca0e7c29939e0a3d532dbe6e70a1e3b3bd7ce13fa7194a34ae1fe12a7aded32
# (Not icecursor, a cursor tool that needs Imlib2; it isn't installed.)
sed -i '/icecursor/d' src/CMakeLists.txt
# Static: every library at the end of the link (CMake's pkg-config gives
# the shared-library flags).
XLIBS="$(pkg-config --static --libs xcomposite xdamage xrender xcursor xfixes xpm libpng libjpeg sm ice xext x11) -lz -lm"
rm -rf build
mkdir build
cd build
cmake .. -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr \
    -DCMAKE_C_COMPILER="$PCC" -DCMAKE_CXX_COMPILER="$ROOT/target/musl/bin/palladium-c++" \
    -DCMAKE_EXE_LINKER_FLAGS="-static -L$X/lib" -DCMAKE_CXX_STANDARD_LIBRARIES="$XLIBS" \
    -DCMAKE_CXX_FLAGS="-I$X/include" -DCMAKE_C_FLAGS="-I$X/include" \
    -DCONFIG_IMLIB2=OFF -DCONFIG_XPM=ON -DCONFIG_LIBPNG=ON -DCONFIG_LIBRSVG=OFF -DCONFIG_NANOSVG=OFF \
    -DCONFIG_COREFONTS=ON -DCONFIG_XFREETYPE=OFF -DCONFIG_I18N=OFF -DENABLE_NLS=OFF -DCONFIG_FRIBIDI=OFF \
    -DXINERAMA=OFF -DCONFIG_XRANDR=OFF -DHAVE_XRES=OFF -DCONFIG_FDO_MENUS=OFF -DENABLE_ALSA=OFF \
    -DENABLE_AO=OFF -DCONFIG_GDK_PIXBUF_XLIB=OFF -DCONFIG_LIBJPEG=ON -DBUILD_TESTING=OFF \
    >cmake.log 2>&1 || { tail -30 cmake.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
rm -rf stage
make install DESTDIR="$PWD/stage" >install.log 2>&1 || { tail -20 install.log; exit 1; }
mkdir -p "$SYSROOT/bin"
for p in icewm icewmbg icewm-session icesh icehelp; do
    if [ -f "stage/usr/bin/$p" ]; then install -m 755 -s "stage/usr/bin/$p" "$SYSROOT/bin/$p"; fi
done
rm -rf "$SYSROOT/share/icewm"
cp -a stage/usr/share/icewm "$SYSROOT/share/"
# (Not its sounds or LED clock digits: no sound daemon, a text clock.
# IceWM.jpg stays: its splash picture at startup.)
rm -rf "$SYSROOT/share/icewm/sounds" "$SYSROOT/share/icewm/ledclock"
# PalladiumOS's menu, toolbar and settings (tools/ports/icewm/).
install -m 644 "$ROOT/tools/ports/icewm/"* "$SYSROOT/share/icewm/"
echo "icewm installed in $SYSROOT/bin; themes: $(ls "$SYSROOT/share/icewm/themes" | tr '\n' ' ')"
