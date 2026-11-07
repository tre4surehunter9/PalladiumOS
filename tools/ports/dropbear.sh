#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Dropbear SSH for PalladiumOS: tools/ports/dropbear.sh -> /bin/dropbear (the
# server), /bin/dbclient (a client), /bin/dropbearkey, /bin/scp.
. "$(dirname "$0")/common.sh"

# The checksum in Dropbear's SHA256SUM file; the release is also GPG-signed
# by the Dropbear release key (F7347EF2EE2E07A267628CA944931494F29C6773).
fetch dropbear-2026.94 https://matt.ucc.asn.au/dropbear/releases/dropbear-2026.94.tar.bz2 \
    e098034a843699200c8c977a991fff73159735bf795d5f72ef672c41a6b1ae81

cat > localoptions.h <<'OPTIONS'
/* PalladiumOS: where things are, and what's left out. */
#define DEFAULT_PATH "/bin:/usr/bin"
#define DEFAULT_ROOT_PATH "/bin:/usr/bin"
#define DROPBEAR_PATH_SSH_PROGRAM "/bin/dbclient"
/* No ssh-agent or X11 to forward, no message of the day, no sftp-server. */
#define DROPBEAR_SVR_AGENTFWD 0
#define DROPBEAR_CLI_AGENTFWD 0
#define DO_MOTD 0
#define DROPBEAR_SFTPSERVER 0
OPTIONS

# Static, without the login records (utmp, wtmp, lastlog) PalladiumOS
# doesn't keep, or zlib. Passwords are checked with crypt() against
# /etc/shadow (PalladiumOS's hashes are SHA-256 crypt).
./configure --host=x86_64-linux-musl CC="$PCC" CFLAGS="-O2" LDFLAGS="-static" \
    --enable-static --disable-zlib --disable-lastlog --disable-utmp --disable-utmpx \
    --disable-wtmp --disable-wtmpx --disable-pututline --disable-pututxline \
    --disable-syslog --disable-harden >/dev/null
make PROGRAMS="dropbear dbclient dropbearkey scp" -j"$(nproc)" >/dev/null
install -m 755 dropbear dbclient dropbearkey scp "$BIN/"
echo "dropbear installed in $BIN"
