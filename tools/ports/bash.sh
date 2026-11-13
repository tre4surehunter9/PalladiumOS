#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# GNU bash for PalladiumOS: tools/ports/bash.sh -> /usr/bin/bash (on the
# system disk; the kernel links /bin/bash to it, for #!/bin/bash scripts).
# The shell most scripts are written for: arrays, [[ ]], brace expansion,
# history and line editing (readline, built in), job control.
. "$(dirname "$0")/common.sh"

# bash 5.3 and its official patches 1-20 (ftp.gnu.org). Each file's GPG
# signature was checked: good, from Chet Ramey's key
# 7C0135FB088AAF6C66C650B9BB5869F064EA74AB (the one Arch's bash accepts);
# these are their checksums.
fetch bash-5.3 https://ftp.gnu.org/gnu/bash/bash-5.3.tar.gz \
    0d5cd86965f869a26cf64f4b71be7b96f90a3ba8b3d74e27e8e9d9d5550f31ba
while read -r n sum; do
    p="$PORTS/src/bash53-$n"
    if [ ! -f "$p" ]; then
        curl -fL --proto '=https' -o "$p.part" "https://ftp.gnu.org/gnu/bash/bash-5.3-patches/bash53-$n"
        mv "$p.part" "$p"
    fi
    echo "$sum  $p" | sha256sum -c - >/dev/null
    patch -p0 -s < "$p"
done <<'SUMS'
001 1f608434364af86b9b45c8b0ea3fb3b165fb830d27697e6cdfc7ac17dee3287f
002 e385548a00130765ec7938a56fbdca52447ab41fabc95a25f19ade527e282001
003 f245d9c7dc3f5a20d84b53d249334747940936f09dc97e1dcb89fc3ab37d60ed
004 9591d245045529f32f0812f94180b9d9ce9023f5a765c039b852e5dfc99747d0
005 cca1ef52dbbf433bc98e33269b64b2c814028efe2538be1e2c9a377da90bc99d
006 29119addefed8eff91ae37fd51822c31780ee30d4a28376e96002706c995ff10
007 c0976bbfffa1453c7cfdd62058f206a318568ff2d690f5d4fa048793fa3eb299
008 097cd723cbfb8907674ac32214063a3fd85282657ec5b4e544d2c0f719653fb4
009 eee30fe78a4b0cb2fe20e010e00308899cfc613e0774ebb3c8557a1552f24f8c
010 cf76f1cce2ea300c18bff9f002d21f280cc931acd17c28518110b93fe6e72569
011 0298df8f5ea2a31d3be43ed7d269c5b3c7c342dd5b570bea7f64d66dcbbe7531
012 d71379b39bebaedaf123414414e77fb458a0a43b9ad3116594c6df7ca6754573
013 042f9cda967e24bf4211944697441e93d06ff42b4b998629a98a1b249279f200
014 bd4360b401d38507e358783dcad8536a99c6789f0d3a5bd0cfb8c4a34144696c
015 55b79ceee2fc27f6767eed697e939a7eb2fe2a28c01556bd75f18d581014f46e
016 9ea29b266b7d24cb34d0ff3f1c4631e4d527bfe2d1ef15d17cdb924bf31ef767
017 443b927b45c1558ca72052410f8b8f6e5152b617ed707061a2781d4375b0d1c3
018 ae715d76c50341d7d7095e9a8d2eeed1ca9546152c2ac7289206f90cf30ac697
019 a25c581e4d0057dea3833918438a930e2e86ee4c6dc17fe15267b7f04cbc4e3d
020 df217ed3a9122aa2286d9b67bbe348661b6a9db262b580c29150dae55d532896
SUMS

# Cross-compiling: what configure can't find out by running programs.
export bash_cv_getcwd_malloc=yes bash_cv_job_control_missing=present \
    bash_cv_sys_named_pipes=present bash_cv_func_sigsetjmp=present \
    bash_cv_printf_a_format=yes bash_cv_unusable_rtsigs=no
./configure --host=x86_64-linux-musl --prefix=/usr CC="$PCC" CC_FOR_BUILD=gcc \
    CFLAGS="-O2" LDFLAGS="-static" --without-bash-malloc --disable-nls \
    --enable-readline --with-installed-readline=no --with-curses=no \
    >configure.log 2>&1 || { tail -25 configure.log; exit 1; }
# (-rdynamic: for loadable builtins' symbols; a static program has no
# dynamic symbol table.)
sed -i 's/-rdynamic//g' Makefile
make -j"$(nproc)" >make.log 2>&1 || { grep -B2 -A6 "error" make.log | tail -30; exit 1; }

mkdir -p "$SYSROOT/bin"
install -m 755 -s bash "$SYSROOT/bin/bash"
echo "bash installed in $SYSROOT/bin ($(./bash --version 2>/dev/null | head -1 || echo 'cross-built'))"
