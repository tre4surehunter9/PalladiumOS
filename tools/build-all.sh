#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Build musl (if needed) and every port in tools/ports, so their programs
# end up in /bin on the next kernel build.
#
#   tools/build-all.sh            musl (once), then lua, dash, fastfetch, tcc
#   tools/build-all.sh --rebuild  rebuild musl too
#   tools/build-all.sh --run      ...then `cargo run`
#
# A port that fails is reported at the end; the others still get built.
set -u

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

REBUILD=0
RUN=0
for arg in "$@"; do
    case $arg in
        --rebuild) REBUILD=1 ;;
        --run) RUN=1 ;;
        -h | --help)
            sed -n '3,10p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown option: $arg (try --help)" >&2
            exit 2
            ;;
    esac
done

# 1. musl libc and palladium-cc
if [ "$REBUILD" = 1 ] || [ ! -x target/musl/bin/palladium-cc ]; then
    echo "==> musl"
    tools/build-musl.sh || { echo "musl failed to build; stopping" >&2; exit 1; }
else
    echo "==> musl: already built (use --rebuild to build it again)"
fi

# 2. the ports
built=""
skipped=""
failed=""
for port in lua dash fastfetch tcc; do
    if [ "$port" = fastfetch ] && ! command -v cmake >/dev/null 2>&1; then
        echo "==> $port: skipped (needs cmake on the host)"
        skipped="$skipped $port"
        continue
    fi
    echo "==> $port"
    if tools/ports/$port.sh; then
        built="$built $port"
    else
        echo "==> $port FAILED" >&2
        failed="$failed $port"
    fi
done

echo
echo "Built:${built:- nothing}"
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
