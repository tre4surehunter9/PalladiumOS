#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# git for PalladiumOS: tools/ports/git.sh -> /usr/bin/git and its helpers
# in /usr/libexec/git-core: clone, commit, branch, merge, log (in less)...
# with https:// remotes through libcurl (curl.sh) and OpenSSL (links.sh).
# Without Perl, Python, Tcl/Tk or gettext: no git-svn, git-gui or
# translations.
. "$(dirname "$0")/common.sh"

X="$PORTS/x11"
C="$PORTS/curl/usr"
if [ ! -f "$C/lib/libcurl.a" ]; then
    echo "libcurl isn't built: run tools/ports/curl.sh first" >&2
    exit 1
fi

# git 2.56.1 (kernel.org): its GPG signature (git-2.56.1.tar.sign, over the
# uncompressed tar) is good from Junio C Hamano's key
# 96E07AF25771955980DAD10020D04E5A713660A7 (the one Arch's git accepts).
fetch git-2.56.1 https://mirrors.edge.kernel.org/pub/software/scm/git/git-2.56.1.tar.xz \
    f0b0bf89264b9bd2e1c640363e7529643989da76281600473c50745cd34a607f
# (Cross-compiling: say what the target is instead of git asking uname.
# musl's regex lacks REG_STARTEND: git's own.)
set -- CC="$PCC" AR=ar prefix=/usr gitexecdir=/usr/libexec/git-core \
    uname_S=Linux uname_M=x86_64 uname_O=GNU/Linux uname_R=6.0 \
    CFLAGS="-O2 -I$X/include -I$C/include" LDFLAGS="-static -L$X/lib -L$C/lib" \
    CURLDIR="$C" CURL_LDFLAGS="-L$C/lib -lcurl -lssl -lcrypto -lz" CURL_CONFIG=/bin/false \
    OPENSSLDIR="$X" ZLIB_PATH="$X" NEEDS_LIBICONV= NO_REGEX=NeedsStartEnd \
    NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 NO_EXPAT=1 NO_INSTALL_HARDLINKS=1 \
    INSTALL_SYMLINKS=1 NO_GITWEB=1 NO_RUST=1
make -j"$(nproc)" "$@" all >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }
rm -rf stage && make "$@" DESTDIR="$PWD/stage" install >install.log 2>&1 || { tail -20 install.log; exit 1; }

mkdir -p "$SYSROOT/bin" "$SYSROOT/libexec" "$SYSROOT/share"
rm -rf "$SYSROOT/libexec/git-core" "$SYSROOT/share/git-core"
cp -a stage/usr/libexec/git-core "$SYSROOT/libexec/"
cp -a stage/usr/share/git-core "$SYSROOT/share/"
for f in git git-receive-pack git-upload-archive git-upload-pack git-shell; do
    [ -e "stage/usr/bin/$f" ] && cp -a "stage/usr/bin/$f" "$SYSROOT/bin/"
done
# Not here: serving repositories (git-daemon, git-http-backend), mailing
# patches by IMAP, and the old "dumb" HTTP fetcher (https:// cloning is
# git-remote-http): ~26 MB of static programs.
rm -f "$SYSROOT/libexec/git-core/git-daemon" "$SYSROOT/libexec/git-core/git-http-backend" \
    "$SYSROOT/libexec/git-core/git-imap-send" "$SYSROOT/libexec/git-core/git-http-fetch"
# (Links to programs not copied: scalar, git-cvsserver.)
find "$SYSROOT/libexec/git-core" -xtype l -delete
# (Strip the real programs: the rest are links to them.)
find "$SYSROOT/libexec/git-core" "$SYSROOT/bin/git" -type f -perm -u+x -exec sh -c 'file "$1" | grep -q ELF && strip "$1"' _ {} \;
# Its manual pages (man git, git help commit), released ready-made beside
# it: the same good signature from Junio C Hamano's key.
cd "$PORTS/src"
tarball="$PORTS/src/git-manpages-2.56.1.tar.xz"
if [ ! -f "$tarball" ]; then
    curl -fL --proto '=https' -o "$tarball.part" https://mirrors.edge.kernel.org/pub/software/scm/git/git-manpages-2.56.1.tar.xz
    mv "$tarball.part" "$tarball"
fi
echo "6ea093f15745a9ac85fe1b1d4fe488413d54e68d0fcb10bab62f047f362cf6de  $tarball" | sha256sum -c -
mkdir -p "$SYSROOT/share/man"
tar -xJf "$tarball" -C "$SYSROOT/share/man" --no-same-owner
echo "git installed in $SYSROOT ($(du -sh "$SYSROOT/libexec/git-core" | cut -f1) of helpers)"
