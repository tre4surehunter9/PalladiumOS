#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# DOOM for PalladiumOS: tools/ports/doom.sh -> /usr/bin/doom (in an X
# window): doomgeneric (id Software's DOOM source, GPL, made portable; its
# Xlib driver) with Freedoom's game data (Freedoom: Phase 1, free levels,
# graphics and sounds compatible with Ultimate DOOM), in
# /usr/share/games/doom. No sound.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libX11.a" ]; then
    echo "libX11 not found: run tools/ports/tinyx.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
export PKG_CONFIG_LIBDIR

# doomgeneric at a fixed commit (git checks it).
fetch_git doomgeneric https://github.com/ozkl/doomgeneric e257a09527062bfb986b6e5e0780839b6cbd5b71
cd doomgeneric
make -j"$(nproc)" CC="$PCC" CFLAGS="-O2 -DNORMALUNIX -DLINUX -D_DEFAULT_SOURCE -I$X/include" \
    LDFLAGS="-L$X/lib -static" LIBS="-lm $(pkg-config --static --libs x11)" >make.log 2>&1 ||
    { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
mkdir -p "$SYSROOT/bin" "$SYSROOT/share/games/doom"
install -m 755 -s doomgeneric "$SYSROOT/bin/doomgeneric"

# Freedoom 0.13.0 (matching its release's checksum, and Alpine's): Phase
# 1's WAD, and its licence. (A zip: fetch does tarballs.)
Z="$PORTS/src/freedoom-0.13.0.zip"
if [ ! -f "$Z" ]; then
    curl -fL --proto '=https' -o "$Z.part" https://github.com/freedoom/freedoom/releases/download/v0.13.0/freedoom-0.13.0.zip
    mv "$Z.part" "$Z"
fi
echo "3f9b264f3e3ce503b4fb7f6bdcb1f419d93c7b546f4df3e874dd878db9688f59  $Z" | sha256sum -c -
rm -rf "$PORTS/src/freedoom-0.13.0"
unzip -q -d "$PORTS/src" "$Z"
install -m 644 "$PORTS/src/freedoom-0.13.0/freedoom1.wad" "$PORTS/src/freedoom-0.13.0/COPYING.txt" "$SYSROOT/share/games/doom/"
cat > "$SYSROOT/bin/doom" <<'SCRIPT'
#!/bin/busybox sh
# doom: DOOM (doomgeneric) with Freedoom, in an X window. Settings and
# saved games are kept in your home directory.
cd "$HOME" 2>/dev/null || cd /tmp
exec doomgeneric -iwad /usr/share/games/doom/freedoom1.wad "$@"
SCRIPT
chmod 755 "$SYSROOT/bin/doom"
echo "doom installed in $SYSROOT/bin (doomgeneric, freedoom1.wad)"
