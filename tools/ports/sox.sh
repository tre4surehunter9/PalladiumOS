#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# SoX for PalladiumOS: tools/ports/sox.sh -> /usr/bin/sox (sox_ng, the
# maintained continuation of SoX): converts, changes and plays sound files
# (WAV, AIFF, AU, raw, Ogg Vorbis...; effects: rate, vol, reverb, trim,
# fade, synth...; `-d` is the sound card: /dev/dsp, OSS), and soxi (a
# file's details). Static, on vorbis.sh's libogg and libvorbis. Not its
# `play` and `rec` names: PalladiumOS has its own play (`sox FILE -d`
# plays).
. "$(dirname "$0")/common.sh"

V="$PORTS/vorbis"
if [ ! -f "$V/lib/libvorbis.a" ]; then
    echo "run tools/ports/vorbis.sh first" >&2
    exit 1
fi

# sox_ng 14.8.1 (Codeberg's release tarball; every file of the git tag
# sox_ng-14.8.1, a4a14b4f, is in it unchanged).
fetch sox_ng-14.8.1 https://codeberg.org/sox_ng/sox_ng/releases/download/sox_ng-14.8.1/sox_ng-14.8.1.tar.gz \
    1dedd9ad574abc576bbda06743867fb778accf98a1eacde1a42913acbf072448
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" CPPFLAGS="-I$V/include" \
    LDFLAGS="-static -L$V/lib" PKG_CONFIG_PATH="$V/lib/pkgconfig" PKG_CONFIG_LIBDIR="$V/lib/pkgconfig" \
    --disable-shared --enable-static --without-libltdl --disable-openmp \
    --with-oss --with-oggvorbis --without-alsa --without-ao --without-pulseaudio --without-sndfile \
    --without-flac --without-opus --without-wavpack --without-amrwb --without-amrnb --without-magic \
    --without-png --without-ladspa --without-ffmpeg --without-mad --without-id3tag --without-lame --without-twolame \
    >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
mkdir -p "$SYSROOT/bin"
install -m 755 -s src/sox_ng "$SYSROOT/bin/sox_ng"
ln -sf sox_ng "$SYSROOT/bin/sox"
ln -sf sox_ng "$SYSROOT/bin/soxi"
echo "sox installed in $SYSROOT/bin: $(grep -E '^(OSS|Ogg|oggvorbis)' configure.log | tr '\n' ' ')"
