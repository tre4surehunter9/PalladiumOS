#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# A file manager for X: tools/ports/xfm.sh -> /usr/bin/xfm (the X File
# Manager: folder windows with icons, drag and drop, an application
# window), with its icons in /usr/share/xfm and its configuration in
# /usr/lib/X11/xfm, on the system disk. On Xaw3d (the 3-D Athena widgets),
# built here into target/ports/x11.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libXmu.a" ]; then
    echo "the X Toolkit isn't built: run tools/ports/x11-apps.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# libXaw3d 1.6.6 (signed by Alan Coopersmith). Its options change its
# widgets' structures, so programs must be compiled with the same ones:
# its pkg-config flags give them (-DXAW_INTERNATIONALIZATION), which xfm's
# configure doesn't ask for (without, its text fields' fonts were
# overwritten by the library: a crash at start).
fetch libXaw3d-1.6.6 https://www.x.org/releases/individual/lib/libXaw3d-1.6.6.tar.xz \
    a41c3e37135ad616b1f28bbde70002afbf3cb59a30df34141f829d32eadc8646
./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
    CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib" >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

# xfm 1.5.4 (Debian's and Ubuntu's copy, matching their checksum). Old C.
# Its configure has no config.sub: built as for this machine (the static
# test programs run here too). Its own link flags put the libraries
# before the objects (static libraries need them after): emptied, the
# libraries come in LIBS. musl has no canonicalize_file_name: realpath
# (with NULL) is the same.
fetch xfm-1.5.4 https://ports.ubuntu.com/ubuntu-ports/pool/universe/x/xfm/xfm_1.5.4.orig.tar.gz \
    2c4c3f7c4c0473963a1d434303c0cd743f7c6bcb8a153d3aca74303c05f56cc2
mkdir -p shim
printf '#include <stdlib.h>\n#define canonicalize_file_name(p) realpath((p), 0)\n' > shim/compat.h
XFM_LIBS="-lXaw3d $(pkg-config --static --libs xmu xt sm ice xpm xext x11) -lz"
CC="$PCC -std=gnu89 -fcommon -Wimplicit-function-declaration -Wno-error=implicit-int -Wno-error=implicit-function-declaration -Wno-error=int-conversion -Wno-error=incompatible-pointer-types -Wno-error=return-mismatch" \
    CPPFLAGS="-I$X/include $(pkg-config --cflags xaw3d) -include $PWD/shim/compat.h" LDFLAGS="-L$X/lib -static" LIBS="$XFM_LIBS" \
    ./configure --prefix=/usr --sysconfdir=/usr/lib --datadir=/usr/share >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
make -j"$(nproc)" xfm_LDFLAGS= xfmtype_LDFLAGS= xfmmailcap_LDFLAGS= >make.log 2>&1 ||
    { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage && make install DESTDIR="$PWD/stage" xfm_LDFLAGS= xfmtype_LDFLAGS= xfmmailcap_LDFLAGS= >install.log 2>&1 ||
    { tail -20 install.log; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/share/X11/app-defaults"
for p in xfm xfmtype xfmmailcap; do install -m 755 -s "src/$p" "$SYSROOT/bin/$p"; done
rm -rf "$SYSROOT/share/xfm" "$SYSROOT/lib/X11/xfm"
cp -a stage/usr/share/xfm "$SYSROOT/share/"
cp -a stage/usr/lib/X11/xfm "$SYSROOT/lib/X11/"
install -m 644 stage/usr/lib/X11/app-defaults/Xfm "$SYSROOT/share/X11/app-defaults/Xfm"
# PalladiumOS's applications window and devices (tools/ports/xfm/), and
# the log's font: one that's there.
install -m 644 "$ROOT/tools/ports/xfm/Apps.xfm" "$ROOT/tools/ports/xfm/xfm_dev" "$SYSROOT/lib/X11/xfm/"
sed -i 's/^Xfm\*fmLog\*AsciiSink.font: .*/Xfm*fmLog*AsciiSink.font: fixed/' "$SYSROOT/share/X11/app-defaults/Xfm"
echo "xfm installed in $SYSROOT/bin"
