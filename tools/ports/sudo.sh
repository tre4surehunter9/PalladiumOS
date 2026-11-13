#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# sudo for PalladiumOS: tools/ports/sudo.sh -> /usr/bin/sudo (setuid root),
# sudoedit and visudo, on the system disk. Static, with the sudoers plugin
# linked in (no loadable plugins), without PAM: passwords are checked
# against /etc/shadow (musl's crypt), as su does. The kernel writes
# /etc/sudoers at boot if there's none: root and the wheel group may run
# anything (edit it with visudo).
. "$(dirname "$0")/common.sh"

# sudo 1.9.17p2 (signed by Todd C. Miller).
fetch sudo-1.9.17p2 https://www.sudo.ws/dist/sudo-1.9.17p2.tar.gz \
    4a38a1ab3adb1199257edc2a7c4a2bd714665eb605b04368843b06dada2cfcfb
./configure --host=x86_64-linux-musl --prefix=/usr --sysconfdir=/etc CC="$PCC" LDFLAGS="-static" \
    --disable-shared --enable-static-sudoers --without-pam --with-passwd --disable-nls \
    --without-sendmail --without-ldap --disable-sasl --disable-zlib --disable-intercept \
    --with-noexec=no --disable-log-server --disable-log-client --disable-python \
    --with-rundir=/var/run/sudo --with-vardir=/var/db/sudo --with-editor=/bin/vi --with-env-editor \
    --with-secure-path=/bin:/usr/bin:/usr/local/bin --without-lecture \
    >configure.log 2>&1 || { tail -30 configure.log; exit 1; }
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A8 "error" make.log | tail -40; exit 1; }
mkdir -p "$SYSROOT/bin" "$SYSROOT/sbin"
# Setuid root: the bit goes onto the system disk with it (mke2fs keeps the
# modes; build.rs makes root the owner).
install -m 4755 -s src/sudo "$SYSROOT/bin/sudo"
ln -sf sudo "$SYSROOT/bin/sudoedit"
install -m 755 -s plugins/sudoers/visudo "$SYSROOT/bin/visudo"
echo "sudo installed in $SYSROOT/bin: $(ls -l "$SYSROOT/bin/sudo" | cut -d' ' -f1)"
