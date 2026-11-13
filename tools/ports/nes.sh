#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# A NES emulator for PalladiumOS: tools/ports/nes.sh -> /usr/bin/nes (a
# menu of the games, or `nes GAME.nes`):
# libretro's FCEUmm core (linked in, static) with a small SDL2 front end
# (tools/ports/nes/nes.c: an X window, sound through /dev/dsp, the
# keyboard), and three homebrew games under the GPL-3.0, from their
# releases, in /usr/share/games/nes: Thwaite and Concentration Room (Damian
# Yerrick) and Nova the Squirrel (NovaSquirrel). No commercial games.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -x "$X/bin/sdl2-config" ]; then
    echo "SDL2 isn't built: run tools/ports/sdl.sh first" >&2
    exit 1
fi
NES="$SYSROOT/share/games/nes"

# The menu's letters: misc-fixed 8x13 (public domain, font-misc-misc, as
# fonts.sh fetches it) made into a C array.
fetch font-misc-misc-1.1.3 https://www.x.org/releases/individual/font/font-misc-misc-1.1.3.tar.xz \
    79abe361f58bb21ade9f565898e486300ce1cc621d5285bec26e14b6a8618fed
FONT_H="$PORTS/src/nes-font"
mkdir -p "$FONT_H"
python3 "$ROOT/tools/ports/nes/bdf2c.py" 8x13.bdf "$FONT_H/font.h"

# FCEUmm (libretro, GPL-2.0) at a fixed commit (git checks it): the core
# as a static library.
fetch_git libretro-fceumm https://github.com/libretro/libretro-fceumm e0f3086a8104f8fbbe49a2615b10401504a9533f
make -f Makefile.libretro platform=unix STATIC_LINKING=1 CC="$PCC" -j"$(nproc)" >make.log 2>&1 ||
    { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
# (Its static archive comes out named fceumm_libretro.so.)
mv fceumm_libretro.so fceumm_libretro.a
CORE=fceumm_libretro.a
# libretro-common's file streams, which a static core leaves to the front
# end (RetroArch has them): built with ours.
LC=src/drivers/libretro/libretro-common
COMMON="$LC/compat/compat_posix_string.c $LC/compat/compat_strcasestr.c $LC/compat/compat_strl.c
    $LC/compat/fopen_utf8.c $LC/encodings/encoding_utf.c $LC/file/file_path.c $LC/file/file_path_io.c
    $LC/streams/file_stream.c $LC/streams/file_stream_transforms.c $LC/string/stdstring.c $LC/time/rtime.c
    $LC/vfs/vfs_implementation.c"
"$PCC" -O2 -D__LIBRETRO__ $("$X/bin/sdl2-config" --cflags) -I$LC/include -I"$FONT_H" \
    "$ROOT/tools/ports/nes/nes.c" $COMMON "$CORE" $("$X/bin/sdl2-config" --libs) -o nes 2>build.log ||
    { grep -B2 -A4 "error\|undefined" build.log | tail -20; exit 1; }
install -m 755 -s nes "$SYSROOT/bin/nes"

# The games (the ROMs their authors released; their sources: README).
rm -rf "$NES"
mkdir -p "$NES"
rom() { # rom FILE URL SHA256
    f="$PORTS/src/$1"
    if [ ! -f "$f" ]; then
        curl -fL --proto '=https' -o "$f.part" "$2"
        mv "$f.part" "$f"
    fi
    echo "$3  $f" | sha256sum -c -
    install -m 644 "$f" "$NES/$1"
}
rom thwaite.nes https://github.com/pinobatch/thwaite-nes/releases/download/v0.04/thwaite.nes \
    a2df24d9c9f72e56c2fdc4c703becc47a5700ad0158da8208247635ebeb3779c
rom croom.nes https://github.com/pinobatch/croom-nes/releases/download/v0.02a/croom.nes \
    2ce17df1ad66a8a0533c0a8739f5b5ebe275c264924bbe350c42c5ac0394f20e
rom nova.nes https://github.com/NovaSquirrel/NovaTheSquirrel/releases/download/v1.0.6a/nova.nes \
    e4780e90b9d1587489bfb797d2ca395be21371ea9262fa9f87f99324ec6960ab
cat > "$NES/README" <<'TEXT'
Homebrew NES games, free software under the GNU GPL version 3:
  thwaite.nes   Thwaite 0.04, Damian Yerrick: https://github.com/pinobatch/thwaite-nes
  croom.nes     Concentration Room 0.02a, Damian Yerrick: https://github.com/pinobatch/croom-nes
  nova.nes      Nova the Squirrel 1.0.6a, NovaSquirrel: https://github.com/NovaSquirrel/NovaTheSquirrel
Their sources are at those addresses (the release tags). Play: nes /usr/share/games/nes/GAME.nes
TEXT
echo "nes installed in $SYSROOT/bin, games in $NES"
