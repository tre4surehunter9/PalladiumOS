#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# C++ for PalladiumOS: tools/ports/cxx.sh builds GCC's C++ library
# (libstdc++, with libsupc++) for musl, from the GCC release that matches
# the build machine's g++ (its headers rely on that compiler's builtins),
# into target/musl (beside musl), and writes target/musl/bin/palladium-c++:
# the build machine's g++ with musl, static-PIE as palladium-cc, and this
# libstdc++. Only libstdc++-v3 is built (not GCC itself).
. "$(dirname "$0")/common.sh"

MUSL="$ROOT/target/musl"
GXX_VERSION=$(g++ -dumpfullversion)
case "$GXX_VERSION" in
16.2.*) ;;
*)
    echo "cxx.sh is pinned to GCC 16.2 (the build machine's g++ is $GXX_VERSION):" >&2
    echo "update its GCC version and checksum to match" >&2
    exit 1
    ;;
esac

# GCC 16.2.0 (signed by Richard Biener).
fetch gcc-16.2.0 https://ftp.gnu.org/gnu/gcc/gcc-16.2.0/gcc-16.2.0.tar.xz \
    e6738e29597f733270731aa90600f37ffdc045079dfc27ec7e8192cc81085c3e
GCC_SRC="$PWD"
INC="$MUSL/include/c++/16.2.0"
# Built as in a GCC build tree (obj/libstdc++-v3), beside obj/libgcc with
# the POSIX threads layer a GCC build for Linux generates there (configure's
# threads check looks for it; without, no std::thread or std::mutex).
rm -rf build-libstdcxx obj
mkdir -p obj/libgcc obj/libstdc++-v3
cp libgcc/gthr-posix.h obj/libgcc/gthr-default.h
cd obj/libstdc++-v3
# The compilers for building it: palladium-cc, and g++ with musl (the same
# specs) and without the build machine's C++ headers (its own are used).
"$GCC_SRC/libstdc++-v3/configure" --host=x86_64-linux-musl --build="$(gcc -dumpmachine)" \
    --prefix="$MUSL" --with-gxx-include-dir="$INC" \
    --disable-multilib --disable-shared --enable-static --disable-nls --disable-libstdcxx-pch \
    --disable-werror --enable-libstdcxx-time=yes --enable-threads=posix --enable-tls \
    CC="$PCC" CXX="g++ -nostdinc++ -specs $MUSL/lib/palladium.specs" \
    CFLAGS="-O2" CXXFLAGS="-O2" >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B3 -A8 "error" make.log | tail -40; exit 1; }
make install >install.log 2>&1 || { tail -20 install.log; exit 1; }
rm -f "$MUSL/lib/"libstdc++*.la "$MUSL/lib/"libsupc++.la

# _dl_find_object, which the build machine's libgcc_eh (exceptions) needs
# and musl lacks: tools/ports/cxx/dl_find_object.c, in libdlfo.a.
"$PCC" -O2 -c "$ROOT/tools/ports/cxx/dl_find_object.c" -o dl_find_object.o
rm -f "$MUSL/lib/libdlfo.a"
ar rcs "$MUSL/lib/libdlfo.a" dl_find_object.o

# palladium-c++: as palladium-cc, for C++. libstdc++'s headers come before
# musl's (its <cstdlib> and the like wrap the C headers with #include_next).
cat > "$MUSL/bin/palladium-c++" <<WRAPPER
#!/bin/sh
# Compile C++ programs for PalladiumOS (static-PIE, musl libc, libstdc++).
exec "\${REALGXX:-g++}" -nostdinc++ -isystem "$INC" -isystem "$INC/x86_64-linux-musl" -isystem "$INC/backward" \\
    "\$@" -specs "$MUSL/lib/palladium.specs" -L "$MUSL/lib" -Wl,-u,_dl_find_object,-ldlfo
WRAPPER
chmod +x "$MUSL/bin/palladium-c++"
echo "libstdc++ $GXX_VERSION for musl installed in $MUSL; compiler: $MUSL/bin/palladium-c++"
