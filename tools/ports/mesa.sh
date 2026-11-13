#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Mesa for PalladiumOS: tools/ports/mesa.sh -> OpenGL for X programs, drawn
# in software (softpipe, Mesa's renderer that needs no LLVM) and put in the
# window with plain X calls (Mesa's "xlib" GLX: our X server has no GLX
# extension, and needs none). A static libGL.a (with its headers) in
# target/ports/mesa, for the programs built on it, and the demos glxgears
# and glxinfo (one program) in /usr/bin.
#
# Mesa builds with Meson and Ninja and its generators need Mako, MarkupSafe,
# PyYAML and packaging: those (pure Python, and Ninja from source) go in
# target/ports/host, for this build only.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
HOST="$PORTS/host"
PY="$HOST/python"
MESA="$PORTS/mesa"
if [ ! -f "$X/lib/libX11.a" ] || [ ! -x "$ROOT/target/musl/bin/palladium-c++" ]; then
    echo "run tools/ports/x11-apps.sh and tools/ports/cxx.sh first" >&2
    exit 1
fi

# ---- The build tools (on this machine).
mkdir -p "$HOST/bin" "$PY"
wheel() { # wheel FILE URL SHA256: a pure-Python wheel, unpacked into $PY
    f="$PORTS/src/$1"
    if [ ! -f "$f" ]; then
        curl -fL --proto '=https' -o "$f.part" "$2"
        mv "$f.part" "$f"
    fi
    echo "$3  $f" | sha256sum -c -
    python3 -I -m zipfile -e "$f" "$PY"
}
wheel meson-1.12.1-py3-none-any.whl \
    https://files.pythonhosted.org/packages/57/99/36cd25f9598eef70db97577440a70a360af505f6f01c2338a18d1556079c/meson-1.12.1-py3-none-any.whl \
    930bc7542cbd9f57009e182fd014eba48cf1a0180a7b9006d2d32d8f168d8b02
wheel mako-1.4.3-py3-none-any.whl \
    https://files.pythonhosted.org/packages/6d/a0/053d6af3e8f871e0073b4a36732d9e65be77a72e5434c31b94f6af78a6bb/mako-1.4.3-py3-none-any.whl \
    723296007c870bfd6b3f0c3230dba7198096e5269297ebf5e4eff9e7ffa39d4f
wheel packaging-26.3-py3-none-any.whl \
    https://files.pythonhosted.org/packages/63/34/ba1c580383c9eada3711951fef0795c80b829a078d72188184bcab9dd527/packaging-26.3-py3-none-any.whl \
    d7193f7c8e4e93f444fde0262bf90af30e16fa0ad0ad44cb553c87339b23cd1c
# (MarkupSafe and PyYAML: their pure-Python packages, from the source releases.)
fetch markupsafe-3.0.4 \
    https://files.pythonhosted.org/packages/38/9b/e422a865e1d5d57d0e509b4e0bf1c1a70a7f6382c29a5aa428df994c8bc8/markupsafe-3.0.4.tar.gz \
    2e9ad7dd851bf45fab9f75cbff4cb493fee9979e8d8c7c9c3ee119022518edd6
rm -rf "$PY/markupsafe" && cp -r src/markupsafe "$PY/"
fetch pyyaml-6.0.3 \
    https://files.pythonhosted.org/packages/05/8e/961c0007c59b8dd7729d542c61a4d537767a59645b82a0b521206e1e25c2/pyyaml-6.0.3.tar.gz \
    d76623373421df22fb4cf8817020cbb7ef15c725b9d5e45f17e189bfc384190f
rm -rf "$PY/yaml" && cp -r lib/yaml "$PY/"
# Ninja 1.13.2 (GitHub's tarball of the tag; Arch's checksum).
fetch ninja-1.13.2 https://github.com/ninja-build/ninja/archive/refs/tags/v1.13.2.tar.gz \
    974d6b2f4eeefa25625d34da3cb36bdcebe7fbce40f4c16ac0835fd1c0cbae17
if [ ! -x "$HOST/bin/ninja" ]; then
    python3 -I configure.py --bootstrap >bootstrap.log 2>&1 || { tail -20 bootstrap.log; exit 1; }
    install -m 755 ninja "$HOST/bin/ninja"
fi
# (A script file: Meson runs itself again by its own path.)
cat >"$HOST/bin/meson" <<EOF
#!/usr/bin/env python3
import sys
sys.path.insert(0, "$PY")
from mesonbuild.mesonmain import main
sys.exit(main())
EOF
chmod +x "$HOST/bin/meson"
PATH="$HOST/bin:$PATH"
PYTHONPATH="$PY"
export PATH PYTHONPATH

# ---- libxshmfence 1.3.3 (signed by Alan Coopersmith): Mesa's build wants
# it on Linux even without hardware drivers.
fetch libxshmfence-1.3.3 https://www.x.org/releases/individual/lib/libxshmfence-1.3.3.tar.xz \
    d4a4df096aba96fea02c029ee3a44e11a47eb7f7213c1a729be83e85ec3fde10
./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
    CPPFLAGS="-I$X/include" PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig" \
    >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
make -j"$(nproc)" install >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -20; exit 1; }

# ---- Mesa 26.2.7 (signed by Eric Engestrom).
fetch mesa-26.2.7 https://archive.mesa3d.org/mesa-26.2.7.tar.xz \
    04294f75d22e9f61cd1e050206bc48b3d39366e788f1803a3368e3d5a872ce18
# Its C11 threads declare three pthread functions weak (a glibc
# workaround): in a static link, weak references don't pull them from
# libc.a, so they stay 0 and the first recursive mutex jumps there. Plain
# references here.
sed -i 's|^#if defined(HAVE_FUNC_ATTRIBUTE_WEAK) \&\& !defined(__CYGWIN__)$|#if 0 /* PalladiumOS: static linking */|' \
    src/c11/impl/threads_posix.c
grep -q '^#if 0 /\* PalladiumOS: static linking' src/c11/impl/threads_posix.c || { echo "threads_posix.c not patched" >&2; exit 1; }
# softpipe's texture caches made on first use, not 768 per context.
python3 -I "$ROOT/tools/ports/mesa/softpipe-lazy-tex-cache.py"
# Its xlib libGL is always a shared library: here a static one.
python3 -I "$ROOT/tools/ports/mesa/static-libgl.py" src/gallium/targets/libgl-xlib/meson.build
cat >cross.ini <<EOF
[binaries]
c = '$PCC'
cpp = '$ROOT/target/musl/bin/palladium-c++'
ar = 'ar'
strip = 'strip'
pkg-config = 'pkg-config'

[properties]
pkg_config_libdir = ['$X/lib/pkgconfig', '$X/share/pkgconfig']

[host_machine]
system = 'linux'
cpu_family = 'x86_64'
cpu = 'x86_64'
endian = 'little'
EOF
rm -rf build
meson setup build --cross-file cross.ini --prefix="$MESA" --buildtype=release -Ddefault_library=static \
    -Dplatforms=x11 -Dglx=xlib -Dgallium-drivers=softpipe -Dvulkan-drivers= -Degl=disabled -Dgbm=disabled \
    -Dgles1=disabled -Dgles2=disabled -Dglvnd=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled \
    -Dlmsensors=disabled -Dzlib=disabled -Dzstd=disabled -Dexpat=disabled -Dshader-cache=disabled \
    -Dgallium-va=disabled -Dgallium-rusticl=false -Dteflon=false -Dvideo-codecs= -Dtools= -Dbuild-tests=false \
    -Dxmlconfig=disabled -Dperfetto=false -Dspirv-tools=disabled -Dmesa-clc=auto -Dprecomp-compiler=auto \
    >setup.log 2>&1 || { tail -40 setup.log; exit 1; }
# (Only libGL: not the DRI loader shim, a shared library for hardware drivers.)
ninja -C build src/gallium/targets/libgl-xlib/libGL.a >build.log 2>&1 ||
    { grep -B2 -A10 "error\|FAILED" build.log | head -60; exit 1; }
rm -rf "$MESA"
mkdir -p "$MESA/lib" "$MESA/include"
cp -r include/GL include/KHR "$MESA/include/"
# One libGL.a: the xlib target and every library of Mesa's it uses.
{
    echo "CREATE $MESA/lib/libGL.a"
    find build/src -name '*.a' | sort | sed 's/^/ADDLIB /'
    echo "SAVE"
    echo "END"
} | ar -M
ranlib "$MESA/lib/libGL.a"
echo "Mesa: $MESA/lib/libGL.a ($(du -h "$MESA/lib/libGL.a" | cut -f1))"

# ---- The demos glxgears and glxinfo (Mesa's demos 9.0.0, signed by Erik
# Faye-Lund; MIT), linked with it (as C++: Mesa's compiler is C++).
fetch mesa-demos-9.0.0 https://archive.mesa3d.org/demos/mesa-demos-9.0.0.tar.xz \
    3046a3d26a7b051af7ebdd257a5f23bfeb160cad6ed952329cdff1e9f1ed496b
XLIBS=$(PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig" pkg-config --static --libs \
    x11 xext xshmfence xcb-shm xcb-randr xcb-dri3 xcb-present xcb-sync xcb)
GLLIBS="-L$MESA/lib -lGL $XLIBS -lm -pthread"
CXX="$ROOT/target/musl/bin/palladium-c++"
# One program for both (one copy of Mesa on the disk: tools/ports/mesa/glx-demos.c).
INC="-I$MESA/include -I$X/include -Isrc/glad/include -Isrc/util"
"$PCC" -O2 -c $INC -Dmain=glxgears_main src/xdemos/glxgears.c -o glxgears.o
"$PCC" -O2 -c $INC -Dmain=glxinfo_main src/xdemos/glxinfo.c -o glxinfo.o
"$PCC" -O2 -c $INC -Dmain=test_gl_main "$ROOT/tools/ports/mesa/test_gl.c" -o test_gl.o
"$PCC" -O2 -c $INC src/util/glinfo_common.c src/glad/src/glad.c "$ROOT/tools/ports/mesa/glx-demos.c"
"$CXX" -static -s glx-demos.o glxgears.o glxinfo.o test_gl.o glinfo_common.o glad.o $GLLIBS -o glxgears 2>link.log ||
    { tail -20 link.log; exit 1; }
mkdir -p "$SYSROOT/bin"
install -m 755 glxgears "$SYSROOT/bin/glxgears"
ln -sf glxgears "$SYSROOT/bin/glxinfo"
ln -sf glxgears "$SYSROOT/bin/test_gl" # (the tests' texture check)
echo "glxgears and glxinfo installed in $SYSROOT/bin"
