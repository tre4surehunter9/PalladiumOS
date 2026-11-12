#!/bin/bash
# Copyright (c) 2026 tre4surehunter9
# tools/soak.sh [MINUTES] [CORES]: the soak test, before a release. Boots
# the usermode test kernel on CORES cores (4) and, after its usual tests,
# runs round after round for MINUTES minutes (30): the stress program with
# processes starting and ending around it, the C test programs (sockets,
# events, threads, terminals, shared memory, the network, sound), and DOOM
# under X. It stops at the first failure, or at any WARN line in the
# kernel log (a program's segfault, a disk error...), and shows it.
# The whole output: target/soak.log.
set -o pipefail
minutes=${1:-30}
cores=${2:-4}
cd "$(dirname "$0")/.." || exit 1
export PATH="$HOME/.cargo/bin:$PATH"
log=target/soak.log
echo "Soak test: $minutes minutes on $cores cores (log: $log)"
# (The test kernels' time limit: the usual tests, the soak, and room.)
PALLADIUM_SOAK_MINUTES=$minutes PALLADIUM_SMP=$cores PALLADIUM_TEST_TIMEOUT=$((minutes * 60 + 900)) \
    cargo test --test usermode 2>&1 | tee "$log" |
    grep --line-buffered -E "soak: round|\[failed\]|panicked|timed out|soak round|^error"
status=${PIPESTATUS[0]}
if [ "$status" = 0 ] && grep -q "soak: round" "$log"; then
    echo "Soak test passed: $(grep -c 'soak: round' "$log") rounds in $minutes minutes."
    exit 0
fi
echo "Soak test FAILED (see $log)"
exit 1
