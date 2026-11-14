#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Libraries for Python's sqlite3, ctypes, bz2 and lzma modules (python.sh),
# static, in target/ports/pylibs: SQLite, libffi, bzip2 and xz (liblzma).
# Also the sqlite3 shell, bzip2 and xz programs on the system disk.
. "$(dirname "$0")/common.sh"

P="$PORTS/pylibs"
rm -rf "$P" && mkdir -p "$P/include" "$P/lib"

# SQLite 3.54.1 (sqlite.org): its SHA3-256 matches the one on sqlite.org's
# download page.
fetch sqlite-autoconf-3540100 https://sqlite.org/2026/sqlite-autoconf-3540100.tar.gz \
    926ef8d3eab93fbf6e06011901273eec076b25594a4e1cdadd50c987f4f6d64e
./configure --host=x86_64-linux-musl --prefix="$P" CC="$PCC" --disable-shared --enable-static \
    --disable-readline --disable-editline >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 && make install >install.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -20; exit 1; }

# libffi 3.8.0 (GitHub release): its SHA-512 is the one Alpine's libffi pins.
fetch libffi-3.8.0 https://github.com/libffi/libffi/releases/download/v3.8.0/libffi-3.8.0.tar.gz \
    7da3e2d9a171eb0a038f592ecad3ff2bb2550f3496d87b3b29ad0cf4430c0db4
./configure --host=x86_64-linux-musl --prefix="$P" --libdir="$P/lib" CC="$PCC" --disable-shared --enable-static \
    --disable-docs --disable-multi-os-directory >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 && make install >install.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -20; exit 1; }

# bzip2 1.0.8 (sourceware.org): its GPG signature (.sig) is good from Mark
# Wielaard's key EC3CFE88F6CA0788774F5C1D1AA44BE649DE760A (the one Arch's
# bzip2 accepts).
fetch bzip2-1.0.8 https://sourceware.org/pub/bzip2/bzip2-1.0.8.tar.gz \
    ab5a03176ee106d3f0fa90e381da478ddae405918153cca248e682cd0c4a2269
make -j"$(nproc)" CC="$PCC" AR=ar RANLIB=ranlib CFLAGS="-O2 -D_FILE_OFFSET_BITS=64" libbz2.a bzip2 >make.log 2>&1 \
    || { tail -20 make.log; exit 1; }
cp libbz2.a "$P/lib/" && cp bzlib.h "$P/include/"
"$PCC" -O2 -static -o bzip2 bzip2.c libbz2.a

# xz 5.8.4 (the Tukaani project's GitHub release): its GPG signature (.sig)
# is good from Lasse Collin's key 3690C240CE51B4670D30AD1C38EE757D69184620
# (the one Arch's xz accepts). (Long after the 5.6.0/5.6.1 backdoor.)
fetch xz-5.8.4 https://github.com/tukaani-project/xz/releases/download/v5.8.4/xz-5.8.4.tar.xz \
    4ce24038fd4221e0d13bc1a2de7a4db56e90b92b3bf75321f6c14be73f65de4b
./configure --host=x86_64-linux-musl --prefix="$P" CC="$PCC" --disable-shared --enable-static \
    --disable-nls --disable-doc --disable-scripts --disable-lzmadec --disable-lzmainfo \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" LDFLAGS="-all-static" >make.log 2>&1 && make install >install.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -20; exit 1; }

# The programs, on the system disk.
mkdir -p "$SYSROOT/bin"
install -m 755 -s "$PORTS/src/bzip2-1.0.8/bzip2" "$SYSROOT/bin/bzip2"
install -m 755 -s "$P/bin/xz" "$SYSROOT/bin/xz"
install -m 755 -s "$P/bin/sqlite3" "$SYSROOT/bin/sqlite3"
echo "pylibs installed in $P (and sqlite3, bzip2, xz in $SYSROOT/bin)"
