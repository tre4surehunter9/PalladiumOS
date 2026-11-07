#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# TinyCC (tcc) for PalladiumOS: a C compiler that runs inside the OS.
#   -> /bin/tcc, and on the system disk (/usr): musl's headers and
#      libraries, and tcc's own headers and runtime (/usr/lib/tcc).
. "$(dirname "$0")/common.sh"

# tinycc's development branch ("mob"); releases are rare (0.9.27 is from 2017).
fetch_git tinycc https://repo.or.cz/tinycc.git c638ddfb593214813aa33536fb993b9a45d23793

# PalladiumOS change: link statically by default (no dynamic linker yet).
# (Programs load at tcc's usual 0x400000, as on Linux: the kernel lives in
# the higher half.)
sed -i 's/^    s->nocommon = 1;$/    s->nocommon = 1;\n    s->static_link = 1; \/* PalladiumOS: no dynamic linker *\//' libtcc.c
grep -q 's->static_link = 1; /\* PalladiumOS' libtcc.c

./configure --cc="$PCC" --cpu=x86_64 --config-musl --enable-static \
    --prefix=/usr --sysincludepaths="{B}/include:/usr/include" \
    --libpaths="{B}:/usr/lib" --crtprefix=/usr/lib --elfinterp=/lib/ld-musl-x86_64.so.1 >/dev/null
# tcc itself is a static Linux program, so the build can run it on the
# host to compile its runtime library (libtcc1.a) -- against musl.
make -j"$(nproc)" CFLAGS="-O2" tcc
make -C lib XFLAGS="-B$PWD -I$PWD -nostdinc -I$PWD/include -I$ROOT/target/musl/include"

install -m 755 tcc "$BIN/tcc"

# The system disk's /usr: include/ and lib/ (musl), lib/tcc (tcc).
# (Only tcc's part of it: other ports put files there too.)
rm -rf "$SYSROOT/include" "$SYSROOT/lib" "$SYSROOT/src/tinycc"
mkdir -p "$SYSROOT/lib/tcc"
cp -r "$ROOT/target/musl/include" "$SYSROOT/include"
cp "$ROOT/target/musl/lib/"*.o "$ROOT/target/musl/lib/"*.a "$SYSROOT/lib/"
cp -r include "$SYSROOT/lib/tcc/include"
# libtcc1.a, plus runmain.o (tcc -run), bt-*.o (-bt backtraces), bcheck.o (-b)
cp libtcc1.a runmain.o bt-exe.o bt-log.o bcheck.o "$SYSROOT/lib/tcc/"
# tcc's own source, so tcc can compile itself inside PalladiumOS:
#   tcc -o tcc2 /usr/src/tinycc/tcc.c -I/usr/src/tinycc
mkdir -p "$SYSROOT/src/tinycc"
cp ./*.c ./*.h ./*.def config.h "$SYSROOT/src/tinycc/"
cp -r include lib "$SYSROOT/src/tinycc/"
rm -f "$SYSROOT/src/tinycc/lib/"*.o "$SYSROOT/src/tinycc/lib/Makefile"
echo "tcc installed in $BIN, system files in $SYSROOT ($(du -sh "$SYSROOT" | cut -f1))"
