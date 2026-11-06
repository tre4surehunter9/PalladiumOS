#!/bin/sh
# Rebuild the test programs (GNU as, Intel syntax). The .bin files are committed,
# so this is only needed after editing a .s file.
set -e
cd "$(dirname "$0")"
for src in *.s; do
    name="${src%.s}"
    as --64 -o "/tmp/$name.o" "$src"
    objcopy -O binary -j .text "/tmp/$name.o" "$name.bin"
    rm "/tmp/$name.o"
done
