#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Build musl (if needed) and the ports in tools/ports, so their programs
# end up in /bin on the next kernel build.
#
#   tools/build-all.sh             build what's missing or out of date
#   tools/build-all.sh tcc lua     (re)build just these ports
#   tools/build-all.sh --rebuild   rebuild everything, musl included
#   tools/build-all.sh --run       ...then `cargo run`
#
# A port is up to date when its outputs exist and nothing it was built
# from changed: its script, tools/ports/common.sh, and musl's libc.a
# (recorded in target/ports/stamps after each successful build).
# A port that fails is reported at the end; the others still get built.
set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

ALL_PORTS="lua dash fastfetch tcc micropython busybox dropbear fonts tinyx x11-clients fvwm95 x11-apps xterm links tcltk vim xfm"
STAMPS=target/ports/stamps

# What each port produces (relative to target/).
outputs() {
    case $1 in
        lua) echo "ports/bin/lua ports/bin/luac" ;;
        dash) echo "ports/bin/dash" ;;
        fastfetch) echo "ports/bin/fastfetch" ;;
        tcc) echo "ports/bin/tcc sysroot/lib/tcc/libtcc1.a sysroot/src/tinycc/tcc.c" ;;
        micropython) echo "ports/bin/micropython" ;;
        busybox) echo "ports/bin/busybox sysroot/bin/vi" ;;
        dropbear) echo "ports/bin/dropbear ports/bin/dbclient ports/bin/dropbearkey ports/bin/scp" ;;
        fonts) echo "sysroot/share/fonts/X11/misc/fonts.dir sysroot/share/fonts/X11/75dpi/fonts.dir" ;;
        tinyx) echo "sysroot/bin/Xfbdev ports/x11/lib/libX11.a" ;;
        x11-clients) echo "sysroot/bin/rxvt sysroot/bin/evilwm" ;;
        x11-apps) echo "sysroot/bin/xclock sysroot/bin/xeyes sysroot/bin/xcalc sysroot/bin/xlogo" ;;
        vim) echo "sysroot/bin/vim" ;;
        xfm) echo "sysroot/bin/xfm sysroot/lib/X11/xfm/Apps.xfm" ;;
        tcltk) echo "sysroot/bin/tclsh sysroot/bin/wish sysroot/lib/tk8.6/demos/widget" ;;
        links) echo "sysroot/bin/links sysroot/share/ssl/cert.pem" ;;
        xterm) echo "sysroot/bin/xterm sysroot/share/terminfo/x/xterm ports/x11/lib/libncursesw.a" ;;
        fvwm95) echo "sysroot/bin/fvwm95 sysroot/bin/xpmroot sysroot/lib/X11/fvwm95/FvwmTaskBar" ;;
    esac
}

# Fingerprint of everything a port is built from.
fingerprint() {
    # (Its own files too, like tools/ports/fvwm95/'s configuration.)
    { cat "tools/ports/$1.sh" tools/ports/common.sh target/musl/lib/libc.a; [ -d "tools/ports/$1" ] && find "tools/ports/$1" -type f | sort | xargs cat; } 2>/dev/null | sha256sum | cut -d' ' -f1
}

up_to_date() {
    for f in $(outputs "$1"); do
        # (-L too: BusyBox's links point at /bin/busybox inside PalladiumOS)
        [ -e "target/$f" ] || [ -L "target/$f" ] || return 1
    done
    [ -f "$STAMPS/$1" ] && [ "$(cat "$STAMPS/$1")" = "$(fingerprint "$1")" ]
}

REBUILD=0
RUN=0
CHOSEN=""
for arg in "$@"; do
    case $arg in
        --rebuild) REBUILD=1 ;;
        --run) RUN=1 ;;
        -h | --help)
            sed -n '3,14p' "$0" | sed 's/^# \{0,1\}//'
            echo "Ports: $ALL_PORTS"
            exit 0
            ;;
        -*)
            echo "unknown option: $arg (try --help)" >&2
            exit 2
            ;;
        *)
            case " $ALL_PORTS " in
                *" $arg "*) CHOSEN="$CHOSEN $arg" ;;
                *)
                    echo "unknown port: $arg (ports: $ALL_PORTS)" >&2
                    exit 2
                    ;;
            esac
            ;;
    esac
done

# 1. musl libc and palladium-cc
if [ "$REBUILD" = 1 ] || [ ! -x target/musl/bin/palladium-cc ]; then
    echo "==> musl"
    tools/build-musl.sh || { echo "musl failed to build; stopping" >&2; exit 1; }
else
    echo "==> musl: up to date"
fi

# 2. the ports: the ones named (always rebuilt), or all of them (when needed)
mkdir -p "$STAMPS"
built=""
current=""
skipped=""
failed=""
for port in ${CHOSEN:-$ALL_PORTS}; do
    if [ -z "$CHOSEN" ] && [ "$REBUILD" = 0 ] && up_to_date "$port"; then
        echo "==> $port: up to date"
        current="$current $port"
        continue
    fi
    if [ "$port" = fastfetch ] && ! command -v cmake >/dev/null 2>&1; then
        echo "==> $port: skipped (needs cmake on the host)"
        skipped="$skipped $port"
        continue
    fi
    echo "==> $port"
    rm -f "$STAMPS/$port"
    if tools/ports/$port.sh; then
        fingerprint "$port" > "$STAMPS/$port"
        built="$built $port"
    else
        echo "==> $port FAILED" >&2
        failed="$failed $port"
    fi
done

echo
echo "Built:${built:- nothing}"
[ -n "$current" ] && echo "Up to date:$current"
[ -n "$skipped" ] && echo "Skipped:$skipped"
[ -n "$failed" ] && echo "Failed:$failed"
echo "Programs in target/ports/bin: $(ls target/ports/bin 2>/dev/null | tr '\n' ' ')"

if [ -n "$failed" ]; then
    exit 1
fi

# 3. optionally, boot PalladiumOS with them in /bin
if [ "$RUN" = 1 ]; then
    echo "==> cargo run"
    exec cargo run
fi
echo "Now run: cargo run"
