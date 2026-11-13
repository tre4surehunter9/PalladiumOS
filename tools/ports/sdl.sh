#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# SDL for PalladiumOS: tools/ports/sdl.sh builds SDL2 (static, X11 video,
# OSS sound through /dev/dsp, software rendering) and sdl12-compat (the SDL
# 1.2 API on SDL2, for older programs like DOSBox) into target/ports/x11,
# for the games and emulators built on them. Nothing is loaded at run time
# (programs are static): sdl12-compat uses a copy of libSDL2.a with its
# SDL_* symbols renamed SDL2x_* (tools/ports/sdl/sdl12-static.py).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ]; then
    echo "libX11 not found: run tools/ports/tinyx.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# SDL2 2.32.10 (signed by Sam Lantinga).
fetch SDL2-2.32.10 https://github.com/libsdl-org/SDL/releases/download/release-2.32.10/SDL2-2.32.10.tar.gz \
    5f5993c530f084535c65a6879e9b26ad441169b3e25d789d83287040a9ca5165
./configure --host=x86_64-linux-musl --prefix="$X" --disable-shared --enable-static CC="$PCC" \
    CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib" LIBS="$(pkg-config --static --libs xext x11)" \
    --enable-video-x11 --disable-x11-shared --enable-oss --disable-loadso \
    --disable-video-wayland --disable-video-kmsdrm --disable-video-vulkan --disable-video-opengl \
    --disable-video-opengles --disable-video-opengles1 --disable-video-opengles2 --disable-video-x11-xinput \
    --disable-video-x11-xfixes --disable-video-x11-xrandr --disable-video-x11-vm --disable-video-x11-xcursor \
    --disable-video-x11-xdbe --disable-video-x11-xshape \
    --disable-video-x11-scrnsaver --disable-alsa --disable-pulseaudio --disable-pipewire --disable-jack \
    --disable-sndio --disable-esd --disable-arts --disable-nas --disable-libsamplerate --disable-libudev \
    --disable-dbus --disable-ime --disable-ibus --disable-fcitx --disable-hidapi --disable-joystick \
    --disable-haptic --disable-sensor --disable-power >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -30; exit 1; }
make install >install.log 2>&1 || { tail -20 install.log; exit 1; }
grep -E "Video drivers|Audio drivers" configure.log | head -2

# Programs link SDL2 with the X libraries (sdl2.pc lists only X11 and
# Xext, not what they need in turn).
SDL2_LIBS="-L$X/lib -lSDL2 $(pkg-config --static --libs xext x11) -lm"
cat > "$X/bin/sdl2-config" <<CONFIG
#!/bin/sh
case "\$1" in
--version) echo 2.32.10 ;;
--cflags) echo "-I$X/include/SDL2 -I$X/include -D_REENTRANT" ;;
--libs|--static-libs) echo "$SDL2_LIBS" ;;
--prefix|--exec-prefix) echo "$X" ;;
*) echo "usage: sdl2-config --version --cflags --libs" >&2; exit 1 ;;
esac
CONFIG
chmod +x "$X/bin/sdl2-config"
sed -i "s|^Libs.private:.*|Libs.private: $(pkg-config --static --libs xext x11) -lm|" "$X/lib/pkgconfig/sdl2.pc"

# The copy for sdl12-compat: SDL_* renamed SDL2x_*.
nm --defined-only -g "$X/lib/libSDL2.a" 2>/dev/null | awk '$3 ~ /^SDL_/ {print $3, "SDL2x_" substr($3, 5)}' | sort -u > sdl2x.syms
objcopy --redefine-syms=sdl2x.syms "$X/lib/libSDL2.a" "$X/lib/libSDL2x.a"

# sdl12-compat 1.2.78 (signed by Sam Lantinga): libSDL.a and the SDL 1.2
# headers in include/SDL.
fetch sdl12-compat-1.2.78 https://github.com/libsdl-org/sdl12-compat/releases/download/release-1.2.78/sdl12-compat-1.2.78.tar.gz \
    bdc5277cf3a8ae18c7493a7d4d0867c53439a9e5a661133dcf1f2eb0b9ddca4d
python3 "$ROOT/tools/ports/sdl/sdl12-static.py" src/SDL12_compat.c
"$PCC" -O2 -D_REENTRANT -DSDL12COMPAT_STATIC_SDL2 -I"$X/include/SDL2" -I"$X/include" -c src/SDL12_compat.c -o SDL12_compat.o \
    2>build.log || { tail -20 build.log; exit 1; }
"$PCC" -O2 -Iinclude/SDL -c src/SDLmain/dummy/SDL_dummy_main.c -o SDL_dummy_main.o
rm -f "$X/lib/libSDL.a" "$X/lib/libSDLmain.a"
ar rcs "$X/lib/libSDL.a" SDL12_compat.o
ar rcs "$X/lib/libSDLmain.a" SDL_dummy_main.o
rm -rf "$X/include/SDL"
mkdir -p "$X/include/SDL"
cp include/SDL/*.h "$X/include/SDL/"
# sdl-config for programs that ask it (DOSBox's configure): SDL 1.2 on the
# renamed SDL2 and X.
SDL12_LIBS="-L$X/lib -lSDL -lSDL2x $(pkg-config --static --libs xext x11) -lm"
cat > "$X/bin/sdl-config" <<CONFIG
#!/bin/sh
case "\$1" in
--version) echo 1.2.78 ;;
--cflags) echo "-I$X/include/SDL -I$X/include -D_GNU_SOURCE=1 -D_REENTRANT" ;;
--libs|--static-libs) echo "$SDL12_LIBS" ;;
--prefix|--exec-prefix) echo "$X" ;;
*) echo "usage: sdl-config --version --cflags --libs" >&2; exit 1 ;;
esac
CONFIG
chmod +x "$X/bin/sdl-config"
echo "SDL2 and sdl12-compat installed in $X"
