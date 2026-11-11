#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Tcl/Tk for PalladiumOS: tools/ports/tcltk.sh -> /usr/bin/tclsh and
# /usr/bin/wish (Tk: windows, buttons, canvases... in X, drawn with Xlib),
# with their script libraries in /usr/lib/tcl8.6 and /usr/lib/tk8.6, on
# the system disk. Static: no loadable extensions (load), and Tcl's
# bundled packages (shared libraries) aren't built.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ]; then
    echo "libX11 not found: run tools/ports/tinyx.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# Tcl and Tk 8.6.18 (matching MacPorts' checksums).
fetch tcl8.6.18 https://downloads.sourceforge.net/project/tcl/Tcl/8.6.18/tcl8.6.18-src.tar.gz \
    14f9af32b1767ff718477a8f974ad03c34341097e6b43f4ce54644ee974e268e
cd unix
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" --disable-shared --enable-static \
    --disable-load --disable-dll-unloading >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" binaries libraries >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage
make install-binaries install-libraries install-tzdata install-msgs DESTDIR="$PWD/stage" >install.log 2>&1 ||
    { tail -20 install.log; exit 1; }
TCL_UNIX="$PWD"

fetch tk8.6.18 https://downloads.sourceforge.net/project/tcl/Tcl/8.6.18/tk8.6.18-src.tar.gz \
    95cd528a80f5e4bdb557af9b14a7197d6860793a3894e25e7c9fad2ed05d4c3c
cd unix
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" --disable-shared --enable-static \
    --with-tcl="$TCL_UNIX" --disable-xft --disable-xss --with-x \
    --x-includes="$X/include" --x-libraries="$X/lib" \
    LDFLAGS="-static" LIBS="$(pkg-config --static --libs xext x11)" >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
make -j"$(nproc)" binaries libraries >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage
# (And Tk's demos: the widget tour, /usr/lib/tk8.6/demos/widget.)
make install-binaries install-libraries install-demos DESTDIR="$PWD/stage" >install.log 2>&1 || { tail -20 install.log; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/lib"
install -m 755 -s "$TCL_UNIX/tclsh" "$SYSROOT/bin/tclsh"
install -m 755 -s wish "$SYSROOT/bin/wish"
rm -rf "$SYSROOT/lib/tcl8.6" "$SYSROOT/lib/tcl8" "$SYSROOT/lib/tk8.6"
cp -a "$TCL_UNIX/stage/usr/lib/tcl8.6" "$SYSROOT/lib/"
cp -a "$TCL_UNIX/stage/usr/lib/tcl8" "$SYSROOT/lib/" # its modules (msgcat, http...)
cp -a stage/usr/lib/tk8.6 "$SYSROOT/lib/"
# (Their headers and static libraries aren't needed on PalladiumOS.)
rm -f "$SYSROOT/lib/tcl8.6"/*.a "$SYSROOT/lib/tk8.6"/*.a
echo "tclsh and wish installed in $SYSROOT/bin"
