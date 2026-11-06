#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# BusyBox 1.38.0: ~300 Unix tools in one program.
#   -> /bin/busybox, and links for each tool in /usr/bin (on the system
#      disk). PATH has /bin first, so PalladiumOS's own ls, cat, sh... stay
#      the defaults, and BusyBox adds the rest (vi, awk, sed, find, tar...).
#      Any of them also runs as `busybox NAME`.
. "$(dirname "$0")/common.sh"

# The checksum busybox.net publishes (busybox-1.38.0.tar.bz2.sha256).
fetch busybox-1.38.0 https://busybox.net/downloads/busybox-1.38.0.tar.bz2 \
    34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2

KHDR=$(linux_headers)
make defconfig >/dev/null
# Turn off what PalladiumOS can't support or musl can't build:
# - tc (traffic control) needs kernel headers newer interfaces removed
# - SHA hardware acceleration needs CPU instructions QEMU may lack
for opt in TC FEATURE_TC_INGRESS SHA1_HWACCEL SHA256_HWACCEL; do
    sed -i "s/^CONFIG_$opt=y/# CONFIG_$opt is not set/" .config
done
yes "" | make oldconfig >/dev/null
make -j"$(nproc)" CC="$PCC" HOSTCC=gcc EXTRA_CFLAGS="-isystem $KHDR" busybox
install -m 755 busybox "$BIN/busybox"

# A link per tool, on the system disk (built into target/usr.img).
rm -rf "$SYSROOT/bin" && mkdir -p "$SYSROOT/bin"
for tool in $(./busybox --list); do
    ln -s /bin/busybox "$SYSROOT/bin/$tool"
done
echo "busybox installed in $BIN, $(ls "$SYSROOT/bin" | wc -l) tool links in $SYSROOT/bin"
