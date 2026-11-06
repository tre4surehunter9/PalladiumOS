#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# MicroPython 1.29.0 (the unix port) for PalladiumOS: -> /bin/micropython
. "$(dirname "$0")/common.sh"

# The official release tarball (a GitHub release asset, submodules
# included). No checksum is published for it: this is the hash of the
# file as first downloaded (2026-11-06).
fetch micropython-1.29.0 https://github.com/micropython/micropython/releases/download/v1.29.0/micropython-1.29.0.tar.xz \
    d925a7c664e79a2bdf3dfcb285ba5e2237041cc35a0bd4ee573b6c5711efeca0

# mpy-cross precompiles the built-in Python modules: a host tool.
make -C mpy-cross -j"$(nproc)" CC=gcc

# PalladiumOS has no threads, libffi (it needs dynamic loading) or TLS
# library yet; -Werror is dropped so new compiler warnings can't stop
# the build.
make -C ports/unix -j"$(nproc)" VARIANT=standard CC="$PCC" CWARN="-Wall" \
    MICROPY_PY_THREAD=0 MICROPY_PY_FFI=0 MICROPY_PY_SSL=0 MICROPY_SSL_MBEDTLS=0 MICROPY_PY_BTREE=0 \
    MICROPY_STANDALONE=0
install -m 755 ports/unix/build-standard/micropython "$BIN/micropython"
echo "micropython installed in $BIN"
