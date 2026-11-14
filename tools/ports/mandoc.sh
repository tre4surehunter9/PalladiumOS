#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# mandoc for PalladiumOS: tools/ports/mandoc.sh -> man, apropos, whatis,
# makewhatis (all mandoc) in /usr/bin, and the manual pages of the ports
# already built (bash, nano, htop, less, vim, sox, ...) in /usr/share/man,
# with the index apropos searches. `man bash` shows a page in less.
# Run it after the ports whose pages it should take (it takes what's there).
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
if [ ! -f "$X/lib/libz.a" ]; then
    echo "zlib isn't built: run tools/ports/links.sh first" >&2
    exit 1
fi

# mandoc 1.14.6 (mandoc.bsd.lv): the checksum it publishes
# (mandoc-1.14.6.sha256); its SHA-512 matches Alpine's mandoc package.
fetch mandoc-1.14.6 https://mandoc.bsd.lv/snapshots/mandoc-1.14.6.tar.gz \
    8bf0d570f01e70a6e124884088870cbed7537f36328d512909eb10cd53179d9c
cat > configure.local <<CONF
PREFIX=/usr
BINDIR=/usr/bin
SBINDIR=/usr/bin
MANDIR=/usr/share/man
MANPATH_DEFAULT=/usr/share/man
OSNAME="PalladiumOS"
CC="$PCC"
CFLAGS="-O2 -I$X/include"
LDFLAGS="-L$X/lib"
STATIC=-static
UTF8_LOCALE=C.UTF-8
BINM_PAGER=less
CONF
./configure >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
make -j"$(nproc)" mandoc >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

M="$SYSROOT/share/man"
mkdir -p "$SYSROOT/bin"
install -m 755 -s mandoc "$SYSROOT/bin/mandoc"
for name in man apropos whatis makewhatis; do
    ln -sf mandoc "$SYSROOT/bin/$name"
done
# (Its own pages.)
for p in man.1:1 apropos.1:1 mandoc.1:1 man.7:7 mdoc.7:7 roff.7:7 makewhatis.8:8; do
    f=${p%:*}
    mkdir -p "$M/man${p#*:}" && cp "$f" "$M/man${p#*:}/$f"
done

# The ports' pages: page SOURCE SECTION [NAME] (if the port was built).
page() {
    src="$PORTS/src/$1"
    [ -f "$src" ] || return 0
    mkdir -p "$M/man$2"
    cp "$src" "$M/man$2/${3:-$(basename "${1%.*}").$2}"
}
page bash-5.3/doc/bash.1 1
page nano-9.3/doc/nano.1 1
page nano-9.3/doc/nanorc.5 5
page htop-3.5.3/htop.1 1
page vim-9.2.1310/runtime/doc/vim.1 1
page dash-0.5.12/src/dash.1 1
page lua-5.4.7/doc/lua.1 1
page lua-5.4.7/doc/luac.1 1
page links-2.30/links.1 1
page mpg123-1.33.7/man1/mpg123.1 1
page mpg123-1.33.7/man1/out123.1 1
page sox_ng-14.8.1/sox_ng.1 1 sox.1
page sox_ng-14.8.1/soxi_ng.1 1 soxi.1
page rxvt-2.7.10/doc/rxvt.1 1
page fastfetch-2.71.0/build/fastfetch.1 1
page sudo-1.9.17p2/docs/sudo.mdoc 8 sudo.8
page sudo-1.9.17p2/docs/sudoers.mdoc 5 sudoers.5
for f in dbclient.1 dropbearkey.1 dropbearconvert.1 dropbear.8; do
    page "dropbear-2026.94/manpages/$f" "${f##*.}"
done
for f in ogg123/ogg123.1 oggdec/oggdec.1 oggenc/man/oggenc.1 ogginfo/ogginfo.1 vcut/vcut.1 vorbiscomment/vorbiscomment.1; do
    page "vorbis-tools-1.4.3/$f" 1
done
# IceWM's, and the X programs' (made by their builds).
for f in "$PORTS"/src/icewm-4.1.0/build/man/*.1 "$PORTS"/src/x*/man/*.1 "$PORTS"/src/oclock-*/man/*.1 "$PORTS"/src/ico-*/man/*.1 "$PORTS"/src/bitmap-*/man/*.1; do
    [ -f "$f" ] && mkdir -p "$M/man1" && cp "$f" "$M/man1/"
done
# The index apropos and whatis search (mandoc runs here too: it's static).
ln -sf mandoc makewhatis
./makewhatis "$M"
echo "mandoc installed in $SYSROOT/bin; $(find "$M" -type f -name '*.[1-8]' | wc -l) manual pages"
