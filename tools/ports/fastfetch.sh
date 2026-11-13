#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# fastfetch 2.71.0 (system information) for PalladiumOS: -> /usr/bin/fastfetch
# Needs cmake on the host, and X (x11-apps.sh: xcb, Xlib, XRandR) for the
# display, which it reads from the X server when a desktop is running.
. "$(dirname "$0")/common.sh"

# GitHub's source archive. The project publishes no checksum for it: this
# is the hash of the archive as first downloaded (2026-11-06).
fetch fastfetch-2.71.0 https://github.com/fastfetch-cli/fastfetch/archive/refs/tags/2.71.0.tar.gz \
    e71d93de41e8e4b2554c36d2ae51758112a21ecf3a8c98605a2c689251e62987

X="$PORTS/x11"
if [ ! -f "$X/lib/libXrandr.a" ]; then
    echo "run tools/ports/x11-apps.sh first" >&2
    exit 1
fi
# X's RandR (through xcb, or Xlib) for the Display module, linked in
# (BINARY_LINK_TYPE=static: it normally loads them with dlopen, which a
# static program can't). Everything else optional is off: no Wayland,
# D-Bus, GPU libraries...
OFF=""
for opt in VULKAN WAYLAND DRM VADRM VAX11 VDPAU GIO DCONF EET DBUS SQLITE3 RPM EGL GLX \
    OPENCL FREETYPE ELF THREADS IMAGE_LOGO IMAGEMAGICK7 IMAGEMAGICK6 SIXEL CHAFA ZLIB LUA LIBZFS LTO; do
    OFF="$OFF -DENABLE_$opt=OFF"
done
# fastfetch includes Linux's userspace headers (linux/*.h).
KHDR=$(linux_headers)
# shellcheck disable=SC2086
rm -rf build
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig" PKG_CONFIG_PATH="" \
CFLAGS="-isystem $KHDR -I$X/include" cmake -S . -B build -G "Unix Makefiles" \
    -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_C_COMPILER="$PCC" -DCMAKE_BUILD_TYPE=Release \
    -DIS_MUSL=ON -DBUILD_FLASHFETCH=OFF -DBUILD_TESTS=OFF -DSET_TWEAK=OFF -DINSTALL_LICENSE=OFF \
    -DBINARY_LINK_TYPE=static -DCMAKE_EXE_LINKER_FLAGS="-static -L$X/lib" \
    -DCMAKE_C_STANDARD_LIBRARIES="$(PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig" pkg-config --static --libs xrandr xcb-randr x11 xcb)" \
    $OFF >cmake.log 2>&1 || { tail -30 cmake.log; exit 1; }
grep -E "XCB_RANDR|XRANDR|link type" cmake.log | head -5
make -C build -j"$(nproc)" fastfetch >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -30; exit 1; }
mkdir -p "$SYSROOT/bin"
install -m 755 -s build/fastfetch "$SYSROOT/bin/fastfetch"
rm -f "$BIN/fastfetch" # (it was in /bin before it read X)
echo "fastfetch installed in $SYSROOT/bin"
