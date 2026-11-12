#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Vim for PalladiumOS: tools/ports/vim.sh -> /usr/bin/vim, in a terminal
# (the console, xterm, rxvt, SSH), with its runtime files (syntax
# colouring, file types, colour schemes) in /usr/share/vim, without the
# documentation, spelling dictionaries, tutor and translations (to keep
# the system disk small). On ncurses (xterm.sh). BusyBox's vi stays
# /usr/bin/vi. (No GUI: Vim dropped its Athena GUI in 2024, and GTK or
# Motif aren't here.)
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libncursesw.a" ]; then
    echo "ncurses isn't built: run tools/ports/xterm.sh first" >&2
    exit 1
fi
PKG_CONFIG_LIBDIR="$X/lib/pkgconfig:$X/share/pkgconfig"
PKG_CONFIG_PATH="$PKG_CONFIG_LIBDIR"
export PKG_CONFIG_LIBDIR PKG_CONFIG_PATH

# Vim 9.2.1310 (GitHub's tarball of the tag, matching Alpine's checksum).
fetch vim-9.2.1310 https://github.com/vim/vim/archive/v9.2.1310/vim-9.2.1310.tar.gz \
    ee7f73f33f6e3a23af2b94e9ebf04c0c7b632bd57721356241b0a60cbe2abef0
# Cross-compiling: what configure would find out by running programs.
export vim_cv_toupper_broken=no vim_cv_terminfo=yes vim_cv_tgetent=zero \
    vim_cv_getcwd_broken=no vim_cv_stat_ignores_slash=no vim_cv_memmove_handles_overlap=yes \
    vim_cv_tty_group=world vim_cv_tty_mode=0620 vim_cv_bcopy_handles_overlap=yes \
    vim_cv_memcpy_handles_overlap=yes
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" \
    CPPFLAGS="-I$X/include -I$X/include/ncursesw" LDFLAGS="-L$X/lib -static" \
    --with-features=huge --enable-gui=no --without-x \
    --with-tlib=ncursesw --disable-gpm --disable-sysmouse --disable-nls --disable-netbeans \
    --disable-canberra --disable-libsodium --without-wayland --disable-gtktest \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage && make install DESTDIR="$PWD/stage" >install.log 2>&1 || { tail -20 install.log; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/share"
install -m 755 -s src/vim "$SYSROOT/bin/vim"
rm -rf "$SYSROOT/share/vim"
cp -a stage/usr/share/vim "$SYSROOT/share/"
R=$(ls -d "$SYSROOT/share/vim/vim"[0-9]*)
rm -rf "$R/doc" "$R/spell" "$R/tutor" "$R/lang" "$R/print" "$R/pack/dist/opt/"*/doc
echo "vim installed in $SYSROOT/bin; runtime $(du -sh "$R" | cut -f1)"
