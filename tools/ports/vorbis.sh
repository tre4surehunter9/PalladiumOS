#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Ogg Vorbis for PalladiumOS: tools/ports/vorbis.sh -> vorbis-tools on the
# system disk: ogg123 (plays .ogg files through /dev/dsp), oggenc and oggdec
# (WAV <-> Ogg Vorbis), ogginfo and vorbiscomment. Static, on libogg,
# libvorbis and libao, built here into $PORTS/vorbis (SoX uses them too).
# libao loads its outputs as plugins (dlopen); here its OSS output is built
# into the library instead (tools/ports/vorbis/ao_oss_static.c).
. "$(dirname "$0")/common.sh"

V="$PORTS/vorbis"
mkdir -p "$V"
lib() { # lib: configure, build and install into $V (static only)
    ./configure --host=x86_64-linux-musl --prefix="$V" --disable-shared --enable-static CC="$PCC" \
        CPPFLAGS="-I$V/include" LDFLAGS="-L$V/lib" PKG_CONFIG_PATH="$V/lib/pkgconfig" \
        PKG_CONFIG_LIBDIR="$V/lib/pkgconfig" "$@" >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
    make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
}

# The Xiph.Org releases (SHA-256 as in Xiph's SHA256SUMS lists; libvorbis
# is also signed by Ralph Giles), from the OSU OSL mirror of
# downloads.xiph.org.
M=https://ftp.osuosl.org/pub/xiph/releases
fetch libogg-1.3.6 $M/ogg/libogg-1.3.6.tar.xz \
    5c8253428e181840cd20d41f3ca16557a9cc04bad4a3d04cce84808677fa1061
lib
fetch libvorbis-1.3.7 $M/vorbis/libvorbis-1.3.7.tar.xz \
    b33cc4934322bcbf6efcbacf49e3ca01aadbea4114ec9589d1b1e9d20f72954b
lib --disable-oggtest --disable-docs --disable-examples
fetch libao-1.2.0 $M/ao/libao-1.2.0.tar.gz \
    03ad231ad1f9d64b52474392d63c31197b0bc7bd416e58b1c10a329a5ed89caf
# The OSS output built in: compiled with ao_null.c, and in the table of
# built-in drivers.
cp "$ROOT/tools/ports/vorbis/ao_oss_static.c" src/
grep -q ao_oss_static src/ao_null.c || echo '#include "ao_oss_static.c"' >>src/ao_null.c
grep -q '&ao_oss,' src/audio_out.c ||
    sed -i 's|^extern ao_functions ao_au;|&\nextern ao_functions ao_oss;|; s|^\t&ao_au,|&\n\t\&ao_oss,|' src/audio_out.c
lib --disable-alsa --disable-pulse --disable-esd --disable-arts --disable-nas --disable-roar \
    --disable-oss --disable-sndio --disable-wmm --disable-broken-oss

fetch vorbis-tools-1.4.3 $M/vorbis/vorbis-tools-1.4.3.tar.gz \
    a1fe3ddc6777bdcebf6b797e7edfe0437954b24756ffcc8c6b816b63e0460dde
# (C17: its getopt.c is old-style C, which C23, GCC 16's default, rejects.)
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" CFLAGS="-O2 -std=gnu17" CPPFLAGS="-I$V/include" \
    LDFLAGS="-static -L$V/lib" PKG_CONFIG_PATH="$V/lib/pkgconfig" PKG_CONFIG_LIBDIR="$V/lib/pkgconfig" \
    --disable-nls --without-curl --without-flac --without-speex --without-kate \
    >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
mkdir -p "$SYSROOT/bin"
for p in ogg123/ogg123 oggenc/oggenc oggdec/oggdec ogginfo/ogginfo vorbiscomment/vorbiscomment; do
    install -m 755 -s "$p" "$SYSROOT/bin/"
done
echo "vorbis-tools installed in $SYSROOT/bin"
