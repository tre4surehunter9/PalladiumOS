#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# micro for PalladiumOS: tools/ports/micro.sh -> /usr/bin/micro, a terminal
# text editor with the keys of desktop editors (Ctrl-S save, Ctrl-Q quit,
# Ctrl-C/V copy and paste, the mouse), syntax colouring built in. Written
# in Go: built static (no libc: Go makes its own system calls) with Go's
# toolchain, fetched for this machine into target/ports/host/go.
. "$(dirname "$0")/common.sh"

HOST="$PORTS/host"
mkdir -p "$HOST"
# Go 1.27.4 for this machine (go.dev): the SHA-256 go.dev lists for it.
GO_TGZ="$PORTS/src/go1.27.4.linux-amd64.tar.gz"
if [ ! -x "$HOST/go/bin/go" ] || ! "$HOST/go/bin/go" version | grep -q go1.27.4; then
    if [ ! -f "$GO_TGZ" ]; then
        curl -fL --proto '=https' -o "$GO_TGZ.part" https://go.dev/dl/go1.27.4.linux-amd64.tar.gz
        mv "$GO_TGZ.part" "$GO_TGZ"
    fi
    echo "7226736c2339719cda28fbf3a54d71aa63bcefe376e98cef00034268dab24840  $GO_TGZ" | sha256sum -c -
    rm -rf "$HOST/go" && tar -xzf "$GO_TGZ" -C "$HOST"
fi
export GOROOT="$HOST/go" GOPATH="$HOST/gopath" GOCACHE="$HOST/gocache" PATH="$HOST/go/bin:$PATH"
# (Modules from Go's proxy, each checked against go.sum and Go's checksum
# database.)
export GOFLAGS="-mod=mod -trimpath" GOTOOLCHAIN=local

# micro 2.0.15 (GitHub's archive of the tag): its SHA-512 is the one
# Alpine's micro pins.
fetch micro-2.0.15 https://github.com/micro-editor/micro/archive/refs/tags/v2.0.15.tar.gz \
    612c775321c268c8f9e1767505ff378bca9b9ab66f5c41b69ecb2464ecf15084
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build -o micro \
    -ldflags "-s -w -X github.com/zyedidia/micro/v2/internal/util.Version=2.0.15 -X github.com/zyedidia/micro/v2/internal/util.CompileDate=2026-11-14" \
    ./cmd/micro >build.log 2>&1 || { tail -20 build.log; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/share/man/man1"
install -m 755 micro "$SYSROOT/bin/micro"
[ -f assets/packaging/micro.1 ] && cp assets/packaging/micro.1 "$SYSROOT/share/man/man1/micro.1"
echo "micro installed in $SYSROOT/bin ($(du -h micro | cut -f1))"
