#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Download, verify and build musl libc for PalladiumOS C programs.
#
#   tools/build-musl.sh                  build musl and palladium-cc
#   tools/build-musl.sh --wrapper-only   just rewrite palladium-cc
#
# Installs into target/musl (headers, libc.a, crt files, and the musl-gcc
# wrapper). build.rs uses target/musl/bin/musl-gcc to compile the programs
# in userland-c/ when it exists; without it, C programs are skipped.
#
# musl needs no changes: PalladiumOS implements the Linux x86_64 syscall
# interface (including the `syscall` instruction) that musl targets.
set -eu

VERSION=1.2.5
SHA256=a9a118bbe84d8764da0ea0d28b3ab3fae8477fc7e4085d90102b8596fc7c75e4
URL="https://musl.libc.org/releases/musl-$VERSION.tar.gz"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PREFIX="$ROOT/target/musl"
WORK="$ROOT/target/musl-build"
TARBALL="$WORK/musl-$VERSION.tar.gz"

# --wrapper-only: just (re)write palladium-cc and its specs file.
WRAPPER_ONLY=0
[ "${1:-}" = "--wrapper-only" ] && WRAPPER_ONLY=1

if [ "$WRAPPER_ONLY" = 0 ]; then
mkdir -p "$WORK"
if [ ! -f "$TARBALL" ]; then
    echo "Downloading $URL"
    curl -fL --proto '=https' -o "$TARBALL.part" "$URL"
    mv "$TARBALL.part" "$TARBALL"
fi
echo "$SHA256  $TARBALL" | sha256sum -c -

rm -rf "$WORK/musl-$VERSION"
tar -xzf "$TARBALL" -C "$WORK"
cd "$WORK/musl-$VERSION"
# Static only: PalladiumOS has no dynamic linker (programs are static-PIE).
CC=gcc ./configure --prefix="$PREFIX" --disable-shared --enable-static
make -j"$(nproc)"
make install
fi

# palladium-cc: gcc with musl, producing static position-independent
# executables (PalladiumOS loads them at its user address and they
# relocate themselves with musl's rcrt1.o). musl-gcc's own specs file
# always links Scrt1.o, which doesn't relocate a static program.
cat > "$PREFIX/lib/palladium.specs" <<SPECS
%rename cpp_options old_cpp_options

*cpp_options:
-nostdinc -isystem $PREFIX/include -isystem include%s %(old_cpp_options)

*cc1:
%(cc1_cpu) -nostdinc -isystem $PREFIX/include -isystem include%s -fPIE

*link_libgcc:
-L$PREFIX/lib -L .%s

*libgcc:
libgcc.a%s %:if-exists(libgcc_eh.a%s)

*startfile:
$PREFIX/lib/rcrt1.o $PREFIX/lib/crti.o crtbeginS.o%s

*endfile:
crtendS.o%s $PREFIX/lib/crtn.o

*link:
-nostdlib %{!r:%{!shared:-static -pie --no-dynamic-linker -z text}}
SPECS
cat > "$PREFIX/bin/palladium-cc" <<WRAPPER
#!/bin/sh
# Compile C programs for PalladiumOS (static-PIE, musl libc).
exec "\${REALGCC:-gcc}" "\$@" -specs "$PREFIX/lib/palladium.specs"
WRAPPER
chmod +x "$PREFIX/bin/palladium-cc"
echo "musl $VERSION installed in $PREFIX (compiler: $PREFIX/bin/palladium-cc)"
