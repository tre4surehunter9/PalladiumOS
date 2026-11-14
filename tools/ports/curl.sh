#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# curl for PalladiumOS: tools/ports/curl.sh -> /usr/bin/curl, to get (and
# send) things over HTTP(S), FTP and more. HTTPS through OpenSSL with the
# certificates in /usr/share/ssl/cert.pem (both from links.sh), zlib for
# compressed answers.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libssl.a" ] || [ ! -f "$X/lib/libz.a" ]; then
    echo "OpenSSL and zlib aren't built: run tools/ports/links.sh first" >&2
    exit 1
fi

# curl 8.23.0 (curl.se): its GPG signature (.asc) is good from Daniel
# Stenberg's key 27EDEAF22F3ABCEB50DB9A125CC908FDB71E12C2 (the one Arch's
# curl accepts).
fetch curl-8.23.0 https://curl.se/download/curl-8.23.0.tar.xz \
    8dadd7d2536a314f982ad05a651b713f426bf7d02cc2c76cffb9796f2218e6c5
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" \
    CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib -static" LIBS="-lssl -lcrypto -lz" \
    --disable-shared --enable-static --with-openssl="$X" --with-zlib="$X" \
    --with-ca-bundle=/usr/share/ssl/cert.pem --without-ca-path \
    --without-libpsl --without-brotli --without-zstd --without-nghttp2 --without-libidn2 \
    --without-libssh2 --disable-ldap --disable-docs --disable-manual \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

mkdir -p "$SYSROOT/bin"
install -m 755 -s src/curl "$SYSROOT/bin/curl"
# libcurl and its headers, for other ports (git's https://), in
# target/ports/curl.
rm -rf "$PORTS/curl" && make install DESTDIR="$PORTS/curl" >install.log 2>&1 || { tail -20 install.log; exit 1; }
echo "curl installed in $SYSROOT/bin"
