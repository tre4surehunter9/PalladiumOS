#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# A web browser for PalladiumOS: tools/ports/links.sh -> /usr/bin/links,
# Links 2 with graphics (links -g: in an X window, with pictures) and in
# text mode (in a terminal), HTTPS included. With what it needs, static,
# into target/ports/x11: OpenSSL (HTTPS), libpng and libjpeg (pictures);
# zlib comes from tinyx.sh. The certificates of the authorities HTTPS
# trusts (this machine's Mozilla list) go in /usr/share/ssl/cert.pem.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ] || [ ! -f "$X/lib/libz.a" ]; then
    echo "the X libraries aren't built: run tools/ports/tinyx.sh first" >&2
    exit 1
fi
CA=/etc/ssl/certs/ca-certificates.crt
if [ ! -f "$CA" ]; then
    echo "no certificate bundle at $CA (ca-certificates) on this machine" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# OpenSSL 3.5.9 (an LTS release; signed by OpenSSL's release key). No
# async jobs (they need makecontext, which musl hasn't), no loadable
# modules (its providers are built in: static). Its directory is
# /usr/share/ssl on PalladiumOS (cert.pem there).
fetch openssl-3.5.9 https://www.openssl.org/source/openssl-3.5.9.tar.gz \
    603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a
CC="$PCC" ./Configure linux-x86_64 no-shared no-module no-tests no-async no-docs no-apps \
    --prefix="$X" --libdir=lib --openssldir=/usr/share/ssl >configure.log 2>&1 ||
    { tail -20 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
make install_sw >install.log 2>&1 || { tail -20 install.log; exit 1; }

# libpng 1.6.59 (matching MacPorts' checksum) and libjpeg 10 (the IJG's;
# matching MacPorts' too).
fetch libpng-1.6.59 https://download.sourceforge.net/libpng/libpng-1.6.59.tar.xz \
    d80dd2a38a37f803cb9b6ac7b14bd6e74ddc3b654780a8380bdf93523fdb4389
./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
    CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib" >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
fetch jpeg-10 https://www.ijg.org/files/jpegsrc.v10.tar.gz \
    8b9eaa13242690ebd03e1728ab1edf97a81a78ed6e83624d493655f31ac95ab5
./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
    >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

# Links 2.30 (matching Arch's checksum): graphics with the X driver only.
# (Its configure is autoconf 2.13's: settings in the environment.)
fetch links-2.30 http://links.twibright.com/download/links-2.30.tar.bz2 \
    c4631c6b5a11527cdc3cb7872fc23b7f2b25c2b021d596be410dadb40315f166
CC="$PCC" CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib -static" \
    LIBS="-lssl -lcrypto -lpng16 -ljpeg -lz $(pkg-config --static --libs x11)" \
    ./configure --host=x86_64-linux-musl --prefix=/usr \
    --enable-graphics --with-x --x-includes="$X/include" --x-libraries="$X/lib" \
    --with-ssl="$X" --without-fb --without-directfb --without-svgalib --without-pmshell \
    --without-atheos --without-libtiff --without-librsvg --without-libavif --without-libwebp \
    --without-lzma --without-bzip2 --without-zstd --without-brotli --without-gpm \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
mkdir -p "$SYSROOT/bin" "$SYSROOT/share/ssl"
install -m 755 -s links "$SYSROOT/bin/links"
install -m 644 "$CA" "$SYSROOT/share/ssl/cert.pem"
echo "links installed in $SYSROOT/bin ($(grep -c 'BEGIN CERTIFICATE' "$SYSROOT/share/ssl/cert.pem") certificates)"
