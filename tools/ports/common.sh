# Copyright (c) 2026 tre4surehunter9
# Shared helpers for tools/ports/*.sh: download a pinned, checksum-verified
# source tarball and build it with palladium-cc (see tools/build-musl.sh).
# Programs are installed into target/ports/bin, which build.rs puts in /bin.
set -eu

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
PCC="$ROOT/target/musl/bin/palladium-cc"
PORTS="$ROOT/target/ports"
BIN="$PORTS/bin"
mkdir -p "$PORTS/src" "$BIN"

if [ ! -x "$PCC" ]; then
    echo "palladium-cc not found: run tools/build-musl.sh first" >&2
    exit 1
fi

# fetch NAME-VERSION URL SHA256: download (once), verify, unpack fresh into
# $PORTS/src/NAME-VERSION, and cd there.
fetch() {
    dir=$1 url=$2 sum=$3
    tarball="$PORTS/src/$(basename "$url")"
    if [ ! -f "$tarball" ]; then
        echo "Downloading $url"
        curl -fL --proto '=https' -o "$tarball.part" "$url"
        mv "$tarball.part" "$tarball"
    fi
    echo "$sum  $tarball" | sha256sum -c -
    rm -rf "$PORTS/src/$dir"
    tar -xf "$tarball" -C "$PORTS/src"
    cd "$PORTS/src/$dir"
}
