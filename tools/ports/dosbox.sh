#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# DOSBox for PalladiumOS: tools/ports/dosbox.sh -> /usr/bin/dosbox (a PC
# with DOS, in an X window, with sound through /dev/dsp), on SDL 1.2
# (sdl12-compat, tools/ports/sdl.sh), and two DOS games, both MIT licensed,
# built from source with NASM: WORDLOS (Wordle) and Floppy Bird, in
# /usr/share/games/dos, which `dosbox` mounts as C:. NASM is built for the
# build machine first (target/ports/host).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -x "$X/bin/sdl-config" ] || [ ! -x "$ROOT/target/musl/bin/palladium-c++" ]; then
    echo "SDL or C++ isn't built: run tools/ports/cxx.sh and tools/ports/sdl.sh first" >&2
    exit 1
fi
HOST="$PORTS/host"
DOS="$SYSROOT/share/games/dos"

# NASM 3.02 (matching Arch's checksum), for the build machine.
fetch nasm-3.02 https://www.nasm.us/pub/nasm/releasebuilds/3.02/nasm-3.02.tar.xz \
    87336eba53b4acfe917424ab5d500d2b0054d9f5148d35c2273ccf2cfb712f0d
if [ ! -x "$HOST/bin/nasm" ]; then
    ./configure --prefix="$HOST" >configure.log 2>&1 || { tail -20 configure.log; exit 1; }
    make -j"$(nproc)" nasm >make.log 2>&1 || { tail -20 make.log; exit 1; }
    mkdir -p "$HOST/bin"
    install -m 755 nasm "$HOST/bin/nasm"
fi
NASM="$HOST/bin/nasm"
rm -rf "$DOS"
mkdir -p "$DOS"

# WORDLOS (CrociDB, MIT): Wordle for DOS.
fetch_git wordlos https://github.com/CrociDB/wordlos 455d70f2427ad002aa168ade19425bb18f01d44f
"$NASM" -f bin -o wordlos.com wordlos.asm
install -m 644 wordlos.com "$DOS/WORDLOS.COM"
install -m 644 LICENSE "$DOS/WORDLOS.LIC"
cd "$PORTS/src"

# Floppy Bird (Mihail Szabolcs, 2014, MIT), as a DOS .COM program.
fetch_git floppybird https://github.com/icebreaker/floppybird 511ffa5f5f7e6d5dbe9c2942557670ae6fc85a53
make com NASM="$NASM" >make.log 2>&1 || { tail -20 make.log; exit 1; }
install -m 644 "$(ls build/*.com | head -1)" "$DOS/FLOPBIRD.COM"
install -m 644 LICENSE "$DOS/FLOPBIRD.LIC"
cd "$PORTS/src"

# DOSBox 0.74-3 (matching Arch's checksum). Old C++: built permissive.
fetch dosbox-0.74-3 https://downloads.sourceforge.net/dosbox/dosbox-0.74-3.tar.gz \
    c0d13dd7ed2ed363b68de615475781e891cd582e8162b5c3669137502222260a
AM=$(ls -d /usr/share/automake-*/ | tail -1)
cp "${AM}config.sub" "${AM}config.guess" .
CC="$PCC" CXX="$ROOT/target/musl/bin/palladium-c++ -std=gnu++14 -fpermissive -Wno-error" \
    CPPFLAGS="-I$X/include" LDFLAGS="-L$X/lib -static" SDL_CONFIG="$X/bin/sdl-config" \
    LIBS="$("$X/bin/sdl-config" --libs)" \
    ./configure --host=x86_64-linux-musl --prefix=/usr --with-sdl-prefix="$X" --disable-sdltest \
    --disable-opengl --disable-alsa-midi --disable-dynamic-x86 \
    >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
install -m 755 -s src/dosbox "$SYSROOT/bin/dosbox.bin"

# Its settings: sound at 22050 Hz (the card plays any rate), the games'
# folder as C:.
cat > "$DOS/dosbox.conf" <<'CONF'
[sdl]
fullscreen=false
output=surface
[mixer]
rate=22050
blocksize=1024
prebuffer=40
[sblaster]
oplrate=22050
[gus]
gusrate=22050
[render]
scaler=normal2x
[autoexec]
mount c /usr/share/games/dos
c:
CONF
# dosbox: DOSBox with those settings. `dosbox GAME.COM` (a program in
# /usr/share/games/dos, any case) runs it and closes DOSBox after it (DOSBox
# itself would take a bare name for a file in the current directory).
cat > "$SYSROOT/bin/dosbox" <<'SCRIPT'
#!/bin/busybox sh
# dosbox: DOSBox, with /usr/share/games/dos as C: (WORDLOS.COM, FLOPBIRD.COM).
# `dosbox GAME.COM` runs one of them and closes DOSBox afterwards.
games=/usr/share/games/dos
if [ $# -eq 1 ]; then
    name=$(echo "$1" | tr a-z A-Z)
    if [ -f "$games/$name" ]; then
        exec /usr/bin/dosbox.bin -conf "$games/dosbox.conf" -c "$name" -c exit
    fi
fi
exec /usr/bin/dosbox.bin -conf "$games/dosbox.conf" "$@"
SCRIPT
chmod 755 "$SYSROOT/bin/dosbox"
# Floppy Bird's own setting (its dosbox.conf): 10000 cycles. DOSBox's
# default for a program without DOS's protected mode is about 3000, too few
# to draw a frame between two of the BIOS clock's ticks (18.2 a second),
# which the game waits for each frame: it ran slower than meant. (More
# can't make it too fast: the clock paces it.)
printf '[cpu]\ncycles=10000\n' > "$DOS/floppybird.conf"
# Shortcuts for the two games.
printf '#!/bin/busybox sh\nexec dosbox WORDLOS.COM\n' > "$SYSROOT/bin/wordlos"
cat > "$SYSROOT/bin/floppybird" <<'SCRIPT'
#!/bin/busybox sh
games=/usr/share/games/dos
exec /usr/bin/dosbox.bin -conf "$games/dosbox.conf" -conf "$games/floppybird.conf" -c FLOPBIRD.COM -c exit
SCRIPT
chmod 755 "$SYSROOT/bin/wordlos" "$SYSROOT/bin/floppybird"
echo "dosbox installed in $SYSROOT/bin, games in $DOS: $(ls "$DOS" | tr '\n' ' ')"
