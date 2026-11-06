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
# Sources are unpacked inside the PalladiumOS repository: don't let their
# build scripts find *its* git history (MicroPython took its version
# from `git describe`).
GIT_CEILING_DIRECTORIES="$PORTS/src"
export GIT_CEILING_DIRECTORIES

if [ ! -x "$PCC" ]; then
    echo "palladium-cc not found: run tools/build-musl.sh first" >&2
    exit 1
fi

# fetch NAME-VERSION URL SHA256: download (once), verify, unpack fresh into
# $PORTS/src/NAME-VERSION, and cd there.
fetch() {
    dir=$1 url=$2 sum=$3
    case $url in
        *.tar.xz) tarball="$PORTS/src/$dir.tar.xz" ;;
        *) tarball="$PORTS/src/$dir.tar.gz" ;;
    esac
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

# fetch_git NAME URL COMMIT: clone (once) and check out exactly COMMIT (git
# verifies the content by its hash), into a fresh $PORTS/src/NAME.
fetch_git() {
    dir=$1 url=$2 commit=$3
    mirror="$PORTS/src/$dir.git"
    if [ ! -d "$mirror" ]; then
        echo "Cloning $url"
        git clone -q --bare "$url" "$mirror"
    fi
    if ! git -C "$mirror" cat-file -e "$commit^{commit}" 2>/dev/null; then
        git -C "$mirror" fetch -q origin '+refs/heads/*:refs/heads/*'
    fi
    rm -rf "$PORTS/src/$dir"
    git clone -q "$mirror" "$PORTS/src/$dir"
    cd "$PORTS/src/$dir"
    git -c advice.detachedHead=false checkout -q "$commit"
    echo "$dir at $(git rev-parse HEAD)"
}

# The files that go on the system disk, mounted at /usr in PalladiumOS
# (build.rs makes target/usr.img from it).
SYSROOT="$ROOT/target/sysroot"
