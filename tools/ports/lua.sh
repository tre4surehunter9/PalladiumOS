#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Lua 5.4 for PalladiumOS: tools/ports/lua.sh -> /bin/lua, /bin/luac
. "$(dirname "$0")/common.sh"

fetch lua-5.4.7 https://www.lua.org/ftp/lua-5.4.7.tar.gz \
    9fbf5e28ef86c69858f6d3d34eccc32e911c1a28b4120ff3e84aaa70cfbf1e30

# The "posix" target: POSIX features (popen, mkstemp...), no dlopen.
make -C src posix CC="$PCC" AR="ar rcu" RANLIB=ranlib MYCFLAGS="-O2" -j"$(nproc)"
install -m 755 src/lua src/luac "$BIN/"
echo "lua installed in $BIN"
