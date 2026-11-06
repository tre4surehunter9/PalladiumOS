#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# dash 0.5.12 (the Debian Almquist shell) for PalladiumOS: -> /bin/dash
. "$(dirname "$0")/common.sh"

# (Debian's copy of the upstream release tarball: the dash site has no HTTPS.)
fetch dash-0.5.12 https://deb.debian.org/debian/pool/main/d/dash/dash_0.5.12.orig.tar.gz \
    6a474ac46e8b0b32916c4c60df694c82058d3297d8b385b74508030ca4a8f28a

# Cross-compile: helper programs that generate source run on the host.
./configure --host=x86_64-linux-musl CC="$PCC" CC_FOR_BUILD=gcc CFLAGS="-O2" >/dev/null
make -j"$(nproc)"
install -m 755 src/dash "$BIN/dash"
echo "dash installed in $BIN"
