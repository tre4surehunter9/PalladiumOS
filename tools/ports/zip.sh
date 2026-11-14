#!/bin/sh
# Copyright (c) 2026 tre4surehunter9
# Info-ZIP's zip 3.0 and unzip 6.0 for PalladiumOS: tools/ports/zip.sh ->
# /usr/bin/zip, unzip, zipinfo, zipnote, zipsplit, zipcloak, funzip, unzipsfx.
# Old releases, with the patches Alpine Linux applies (security fixes:
# CVEs, zip bombs; large files; Unicode names). BusyBox's unzip stays
# `busybox unzip`.
. "$(dirname "$0")/common.sh"

# patches NAME COMMIT: apply Alpine's patches for NAME (aports at COMMIT),
# each checked against the SHA-512 Alpine's APKBUILD pins for it, in order.
patches() {
    name=$1 commit=$2
    dir="$PORTS/src/$name-alpine-patches"
    mkdir -p "$dir"
    while read -r sum file; do
        [ -f "$dir/$file" ] || curl -fL --proto '=https' -s -o "$dir/$file" \
            "https://gitlab.alpinelinux.org/alpine/aports/-/raw/$commit/main/$name/$file"
        echo "$sum  $dir/$file" | sha512sum -c - >/dev/null
        patch -p1 -s < "$dir/$file"
    done
}

# unzip 6.0 (Alpine's copy: its SHA-512 is the one Alpine's unzip pins).
fetch unzip60 https://dev.alpinelinux.org/archive/unzip/unzip60.tgz \
    036d96991646d0449ed0aa952e4fbe21b476ce994abc276e49d30e686708bd37
patches unzip b7410ac718d5eeae21001de5cae9159f597bb517 <<'SUMS'
d2d20e546c74e64b444b9d7053bc49119666e693e6ca32dfeed35245b86cdcae4430e49736b4c19ad6c0ea34e369b6015d7084afc5a61a015c506282cf6e0f32 08-allow-greater-hostver-values.patch
9d2914f22fb0075a2b6f72825c235f46eafd8d47b6fb6fcc8303fc69336e256b15923c002d2615bb6af733344c2315e4a8504d77bae301e10c11d4736faa2c81 10-unzip-handle-pkware-verify.patch
0fbc26fa0eb1097e1c5fa5d9a7b29766a9b5a0b6c16681ea2ef093c8752940bc9c0eac08dc1420cc2cc77f9280589a323deb331ccd9c04031b96a683bebd4eac 13-remove-build-date.patch
57699582e9056af0817dcb67f8db67e6a1ff8208c137fbebcf559429e5f12b471b75d7e1ef938e5bbb5416074a51ac7342e4ce8057f4bbdcb0bf079b8d7832af 20-unzip-uidgid-fix.patch
70545de519f522b0c0f3df516b019dd248f55f3e4e107a19ade5bd1774cc25fe1d012dad5ddde5915a807352dbbe5909a92136085308d24bbde3dd1324dcc4d1 21-fix-warning-messages-on-big-files.patch
3c7f525687b198aaa8547a8b30e744f7f184943624279d5c70170d5b9bb3f0c0f27f3e69bc808dd0d144690107bc76a10c06e160bf99c54fd5684246208b7cff unzip-6.0-exec-shield.patch
94560c730437ac2561d5e7550b91688dad1b828e1da96c9477e228e17b37e455ecdcd3a774e7db94dd902bbe12547d910602c0656b803768e5865b045d452dd7 unzip-6.0-format-secure.patch
b0b745cff474756447e699a13ff003871b33a4f7a24a91150e5a947eba5132fd90fbacf7580379fc13c5f638483b25cbc226f85b9cac9c7662b2f91927eb2bb3 unzip-6.0-heap-overflow-infloop.patch
e387dc533142f0f702c04092da297e8dfc9b51e4ec7001e6e657d93a9a0f6382b1b39196f239190b8d52b8ecfa46a965627e503aaecdab86e59272af84bbc2c6 unzip-6.0-timestamp.patch
13f9c54fcdde478c4afe391c8e7ef9c31b03228aaace5da38382612951cbfd60710fd3d931569297953be32b2c5906715aed4b1c05e28cc8fccbb27f38b57550 CVE-2014-8139.patch
028a97e781fb4e277df331fd40b848bbc002f1a5ceeb40e74477cf68d2f063ac2623e24afbeddfa0456940ecc7694fdb66ecd031cbcecad63079e8427fb731c9 CVE-2014-8140.patch
3dd21343d6e5ae7d19f2b2f9cf7310eac38dd7f598e1265e247559a48143c9dbffabd9fc0d7aff6d859ec9e646e85c2b7ee00a1b1a2e23bdf96192c22c58b058 CVE-2014-8141.patch
281c524a9adb1c0f1cb861548d96115f55152c1d76adca34bbaabcca410c5aaf5dd53d99360d7ea8ee9d0ab9eb62031cb40c5de4b5ecfd91535ac178cd3e7098 CVE-2014-9636.patch
9a62286acdbd5bf5f679d813017b93c25bdb06edaf48b2b53d3281ce3c30587158a777b07457c574d72350499f786dac6b4493092d7e08c17c07cb65ecc513b6 CVE-2014-9913.patch
8c4a4313072ff0d87eadb0f5472eb48f2802b835dd282305811a96de87a41fed48be60fbdd434e6b6359418f0559f7793deaa1d68161a0c0ead9f8574bb9f14c CVE-2016-9844.patch
6f757385a23fe6a034f676df6bf233243afa8743761e3d715e532d066fcd7dc8f8dcd6192be693258f3855837e5534490784378768abe7ce710fb869258d49b7 CVE-2018-1000035.patch
1edd66fbca3cfbfad7b19db0b1564b93f13b27b10ff157cf9907228184b56f1f4c87c2dd2a09afc1307ee86622f3aaeea46f8336c249249c8452304cf4d25272 CVE-2018-18384.patch
d1ea86ee591e6d73853798bfcafb368338129a698a65732715b5bff36a5f8c242de42b8d2ef87cd8c41c8cd271780bd9efcc664d7f9b4261a6f10b1c4a8cf792 large-symlinks.patch
bb54397ac6f84c9eae012b9e782f768ac4c715e20f2060e7b17b1770921cd6edb31adde9ebd6d9b735f935d6dc08daa751115fd817390a3942bb84b80f38d489 CVE-2021-4217.patch
bff17d21399a2189ed497602a735eab55746a17e6d414d843068c0374ae09d8d5958c00731e9f35dbfbce6ec9f802cb83d1e7436363392a36a2e34b724d0d71c CVE-2022-0529-and-CVE-2022-0530.patch
cb51b1ff5c1bc4a3acc8d4bb60c92cd74dec1b76799f00f542e793b1407964c00cfbda8153703e40a64d1cf89705d6ba16a4c11e7ca9a304eb3a14355546e5eb zipbomb-manpage.patch
4f940afa1f6628a47faf6eb13116eab384bda05c841b0b286b18cafad9c4b567ef332a301b8fbdf07259acdf8f6bdb452487e086bce2a3f092daa4e9d9daefa6 zipbomb-part1.patch
e20e97722e0daf48b97df540added603325d356c6597634afd694af3972bb62952dd0f92c10d98f8c9f28eb9d089f6f5b022e0beb8c6224e32fd2cfaadffa200 zipbomb-part2.patch
7e11e29dde260f0245bc25eeb811d794515d1c523b42ea6004c7c6a2eda19b9de4dd7a8ecc03e5ff7d376e28a96c6f1b2b922d6b8b3963a9e4746231f3c257f4 zipbomb-part3.patch
27d45a25a6a51415af609a4fdefcb7c95a1105d511a6e18e2a7464e9d3773ba2ccb25f138a3cc6ddc6e5e9c558b633ee60d273cebf562c2a7d1e99d3f229d1ba zipbomb-part4.patch
48875d7e08d669637e26a7e800f8b2a3812d477e6f249c8d4962fdf93ba6d346f5b22b83d82cb65317b506dff84c441d42c0fe7d1c042a065619d39bdf25fdd0 zipbomb-part5.patch
a788d57fe0fb9ae6106381d2a8fe566aa35bb037012139dc7c283fe5eb316056835dffa9ea9778c15a5b39e50a75329a135a0dffdfc6a53d575ef2013b1d478a zipbomb-part6.patch
18837ceb96dd86d5b24435bbb04b36b062449b50e65da8ab6f4fbd1082cc4afea6c42a9f363fc790995ea5450e3d6c36e749aaa8fd4b7dd8415e304dadcb5290 zipbomb-part7.patch
c1383b1886ec433e22adb24b00fc5de0d7792cf727afef62d54940a4bb39f2cd6f465f28ef0b9ab68a6798f3df7bb227fa1ec162fd4d4c671c9e4b46a1c2ef42 zipbomb-switch.patch
1c93f72e96b19974d44556f2150ed28f825181b05aa41269e6df514e045c63b8c9318f2a414f9e50bf606dea26c8aac5ed9dd066978ba6bc79ccc39f770953bc gcc15.patch
SUMS
# (Alpine's options: Unicode names, large files, dates as YMD.)
D="-DACORN_FTYPE_NFS -DWILD_STOP_AT_DIR -DLARGE_FILE_SUPPORT -DUNICODE_SUPPORT -DUNICODE_WCHAR"
D="$D -DUTF8_MAYBE_NATIVE -DNO_LCHMOD -DDATE_FORMAT=DF_YMD -DNOMEMCPY -DNO_WORKING_ISPRINT"
make -f unix/Makefile CC="$PCC" LF2="-static" CF="-O2 -I. $D" prefix=/usr unzips >make.log 2>&1 \
    || { tail -20 make.log; exit 1; }
mkdir -p "$SYSROOT/bin" "$SYSROOT/share/man/man1"
for p in unzip funzip unzipsfx; do install -m 755 -s "$p" "$SYSROOT/bin/$p"; done
ln -sf unzip "$SYSROOT/bin/zipinfo"
for m in unzip funzip unzipsfx zipinfo; do cp "man/$m.1" "$SYSROOT/share/man/man1/"; done

# zip 3.0 (SourceForge: its SHA-512 is the one Alpine's zip pins).
fetch zip30 https://downloads.sourceforge.net/infozip/zip30.tar.gz \
    f0e8bb1f9b7eb0b01285495a2699df3a4b766784c1765a8f1aeedf63c0806369
patches zip 14226268fa7d58f8faa74d8971ce1503959b8345 <<'SUMS'
e71f7c6f6dd6f8f576018581b155f324eaf6810a2f7c5c402843bdfc3ce6772f09be166b33ff34dcb703774694b18584c8de1763f29895c1d563661679f43d71 10-zip-3.0-build.patch
2a6824066543684e02731bd36930e0ffa6ab48162c61d5e7af08e8ea0795401271ef28bdd0f6ace865d91fdb1a05b643813cdd458a9fdf554f1ca298ea7f50e3 20-zip-3.0-exec-stack.patch
fcb2d728d79160128064af90eb17a9c03e980d9619c4d930c4dbf79cb13842c7bb694036dcbf5cd9b412efc6fc0b86bb94a88b3f110b6b63bf6bda2b97fe0568 30-zip-3.0-pic.patch
f22649d1cbe94ffcacf622493400489393d6fac80067211e6ba12b85d7b062f76b041f318068731b267167e715b7749589bfe9205670a61c6e56e1ffdfaea29b 40-fix-zipnote.patch
1d2147e97e9e19ac403ec6fb3466059dba96edeee2ac2a809b75c652d50d01d9ce717c1d9812359c8d13330369e60f6421afdb9e54e3e19d6bcf0ddf2405b15e format-security.patch
fd952e9e3cd9535c94f17baeeb34a51ba7b34d604b96620c9f85e458e9a71269e357ec825eb2ee37f34a57d36332e3a79e776e72490a4a67fe42d0832ecc5d95 configure-gcc14.patch
SUMS
make -f unix/Makefile CC="$PCC" LOCAL_ZIP="-O2" LFLAGS1="-static" prefix=/usr generic >make.log 2>&1 \
    || { tail -20 make.log; exit 1; }
for p in zip zipnote zipsplit zipcloak; do install -m 755 -s "$p" "$SYSROOT/bin/$p"; done
for m in zip zipnote zipsplit zipcloak; do cp "man/$m.1" "$SYSROOT/share/man/man1/"; done
echo "zip and unzip installed in $SYSROOT/bin"
