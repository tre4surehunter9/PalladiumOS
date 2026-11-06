#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# fastfetch 2.71.0 (system information) for PalladiumOS: -> /bin/fastfetch
# Needs cmake on the host.
. "$(dirname "$0")/common.sh"

# GitHub's source archive. The project publishes no checksum for it: this
# is the hash of the archive as first downloaded (2026-11-06).
fetch fastfetch-2.71.0 https://github.com/fastfetch-cli/fastfetch/archive/refs/tags/2.71.0.tar.gz \
    e71d93de41e8e4b2554c36d2ae51758112a21ecf3a8c98605a2c689251e62987

# Everything optional is off: PalladiumOS has no graphics, D-Bus, threads,
# or libraries to load at run time (programs are static).
OFF=""
for opt in VULKAN WAYLAND XCB_RANDR XRANDR DRM VADRM VAX11 VDPAU GIO DCONF EET DBUS SQLITE3 RPM EGL GLX \
    OPENCL FREETYPE ELF THREADS IMAGE_LOGO IMAGEMAGICK7 IMAGEMAGICK6 SIXEL CHAFA ZLIB LUA LIBZFS LTO; do
    OFF="$OFF -DENABLE_$opt=OFF"
done
# fastfetch includes Linux's userspace headers (linux/*.h), which musl
# doesn't ship: use the host's sanitized copy (linux-api-headers), kept
# apart from musl's own headers.
KHDR="$PORTS/linux-headers"
rm -rf "$KHDR" && mkdir -p "$KHDR"
cp -r /usr/include/linux /usr/include/asm /usr/include/asm-generic /usr/include/drm "$KHDR/"
# shellcheck disable=SC2086
CFLAGS="-isystem $KHDR" cmake -S . -B build -G "Unix Makefiles" \
    -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_C_COMPILER="$PCC" -DCMAKE_BUILD_TYPE=Release \
    -DIS_MUSL=ON -DBUILD_FLASHFETCH=OFF -DBUILD_TESTS=OFF -DSET_TWEAK=OFF -DINSTALL_LICENSE=OFF \
    $OFF >/dev/null
make -C build -j"$(nproc)" fastfetch
install -m 755 build/fastfetch "$BIN/fastfetch"
echo "fastfetch installed in $BIN"
