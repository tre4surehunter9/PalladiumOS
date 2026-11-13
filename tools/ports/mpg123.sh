#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# mpg123 for PalladiumOS: tools/ports/mpg123.sh -> /usr/bin/mpg123 (plays
# MP3s, and MP1/MP2, through /dev/dsp: the OSS output, built in) and
# out123 (plays raw or WAV sound, or makes test tones). Static, without
# loadable output modules.
. "$(dirname "$0")/common.sh"

# mpg123 1.33.7 (signed by Thomas Orgis).
fetch mpg123-1.33.7 https://www.mpg123.de/download/mpg123-1.33.7.tar.bz2 \
    31d0e35a4ca567ec9b5ebda6c3062bb4435d6d3eacd6ef0d95cadd7854dc03ee
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" LDFLAGS="-static" \
    --disable-shared --enable-static --disable-modules --with-audio=oss --with-default-audio=oss \
    --with-cpu=x86-64 --disable-network --disable-components --enable-libmpg123 --enable-libout123 \
    --enable-libsyn123 --enable-programs \
    >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
mkdir -p "$SYSROOT/bin"
install -m 755 -s src/mpg123 "$SYSROOT/bin/mpg123"
install -m 755 -s src/out123 "$SYSROOT/bin/out123"
echo "mpg123 installed in $SYSROOT/bin"
