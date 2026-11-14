#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Python for PalladiumOS: tools/ports/python.sh -> /usr/bin/python3 (CPython,
# not Alpine's: no `alpine` needed) and its standard library in
# /usr/lib/python3.14. One static program: the C modules are compiled in
# (from Modules/Setup.stdlib, as configure found them), with OpenSSL (ssl,
# hashlib) and zlib from links.sh, ncurses (curses, and the interactive
# prompt's line editing) from xterm.sh, and SQLite, libffi, bzip2 and xz
# (sqlite3, ctypes, bz2, lzma) from pylibs.sh. (ctypes can't load other
# libraries: a static program has no dlopen.) Not here: the test suite,
# IDLE, tkinter.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
for f in libssl.a libz.a libncursesw.a; do
    [ -f "$X/lib/$f" ] || { echo "$f isn't built: run tools/ports/links.sh and xterm.sh first" >&2; exit 1; }
done
# SQLite, libffi, bzip2, xz: pylibs.sh.
PL="$PORTS/pylibs"
[ -f "$PL/lib/libsqlite3.a" ] || { echo "run tools/ports/pylibs.sh first (sqlite3, ctypes, bz2, lzma)" >&2; exit 1; }
# Cross-compiling needs this machine's Python of the same version.
BUILD_PY=$(command -v python3.14 || true)
if [ -z "$BUILD_PY" ]; then
    echo "python3.14 isn't installed on this machine (cross-compiling needs it)" >&2
    exit 1
fi

# Only the cross-built libraries (not this machine's: their headers and
# libraries aren't for PalladiumOS), and the modules without them off.
PKG_CONFIG_LIBDIR="$PL/lib/pkgconfig:$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH
# (Most built-in hashes too: they bring the same HACL* code, which clashes
# when all are in one program; hashlib has them from OpenSSL. Not BLAKE2:
# hashlib always takes that one from its own module.)
for m in _ctypes_test _dbm _gdbm _tkinter readline _uuid _zstd \
    _md5 _sha1 _sha2 _sha3 _hmac; do
    export "py_cv_module_$m=n/a"
done

# CPython 3.14.10 (python.org): its SHA-512 is the one Arch's and Alpine's
# python pin.
fetch Python-3.14.10 https://www.python.org/ftp/python/3.14.10/Python-3.14.10.tar.xz \
    de414a57119abef52784d8993e341d53aed635fc2b05c2607d9f98a2c5b72032
./configure --host=x86_64-linux-musl --build=x86_64-pc-linux-gnu --prefix=/usr CC="$PCC" \
    CPPFLAGS="-I$PL/include -I$X/include -I$X/include/ncursesw" LDFLAGS="-L$PL/lib -L$X/lib -static" \
    LIBS="-lssl -lcrypto -lz -lncursesw" \
    --with-build-python="$BUILD_PY" --disable-shared --without-ensurepip --disable-test-modules \
    --with-openssl="$X" --without-static-libpython \
    ac_cv_file__dev_ptmx=yes ac_cv_file__dev_ptc=no ac_cv_buggy_getaddrinfo=no \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
# Every module configure found (its state "yes" in the Makefile),
# compiled into the program.
{
    echo '*static*'
    awk 'NR == FNR { if (sub(/^MODULE_/, "") && sub(/_STATE=yes$/, "")) ok[$0] = 1; next }
         /^\*/ { next }
         /^[_a-zA-Z]/ { name = toupper($1); if (!(name in ok)) next } { print }' Makefile Modules/Setup.stdlib
} > Modules/Setup.local
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage && make DESTDIR="$PWD/stage" COMPILEALL_OPTS="-j0" install >install.log 2>&1 || { tail -20 install.log; exit 1; }

L=stage/usr/lib/python3.14
rm -rf "$L/test" "$L/idlelib" "$L/tkinter" "$L/turtledemo" "$L/ensurepip" "$L"/config-3.14-* \
    "$L"/__phello__
# (lib-dynload: empty, the modules are in the program; Python finds its
# platform files by it.)
rm -rf "$L/lib-dynload" && mkdir "$L/lib-dynload"
find "$L" -name '*.opt-1.pyc' -delete
find "$L" -name '*.opt-2.pyc' -delete
mkdir -p "$SYSROOT/bin" "$SYSROOT/lib" "$SYSROOT/share/man/man1"
rm -rf "$SYSROOT/lib/python3.14"
cp -a "$L" "$SYSROOT/lib/"
install -m 755 -s stage/usr/bin/python3.14 "$SYSROOT/bin/python3.14"
ln -sf python3.14 "$SYSROOT/bin/python3"
ln -sf python3.14 "$SYSROOT/bin/python"
cp Misc/python.man "$SYSROOT/share/man/man1/python3.1"
echo "python installed in $SYSROOT/bin; library $(du -sh "$SYSROOT/lib/python3.14" | cut -f1)"
