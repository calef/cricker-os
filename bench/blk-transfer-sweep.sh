#!/bin/sh
# Sweep the blk contract's transfer size and measure filesystem throughput at each one
# (milestone 138 step 4).
#
# WHY THIS EXISTS. Step 3 grew the file channel and found the residual moved rather than shrank:
# after it, a 4 KiB read is dominated by sixteen single-block trips through fs_proto::blk, and
# fs_seq_read measures 80.30 MiB/s against the ~100 MiB/s ceiling one blk round trip per block
# imposes. Step 4 is the change that grows the blk contract to carry more than one block per
# request, and this is the measurement that says by how much. It is the sibling of
# bench/transfer-size-sweep.sh and bench/record-level-sweep.sh in shape on purpose.
#
# WHAT IT DOES. `fs_proto::blk::TRANSFER_BLOCKS` is how many contiguous pages the FS server and the
# block server share, and therefore the most one blk request can carry. Every party's mapping, the
# block server's clamp and `IpcDisk`'s batching all derive from it, so setting it to 1 reproduces
# the contract exactly as it stood before step 4. This script therefore EDITS THAT CONSTANT IN
# PLACE, builds, measures, and puts it back. It restores on any exit, including a signal, and
# refuses to start if that file is already dirty, so it can never restore over somebody else's edit.
#
# The benchmark holds BYTES MOVED constant (`fixture::throughput::TOTAL`, 1 MiB) rather than the
# transfer count, exactly as the file-channel sweep does; the file channel's own transfer size
# (`fs::TRANSFER_PAGES`) is untouched here; only the blk channel's is swept, so what changes between
# points is how many blk round trips one file request fans out into, not how big the file request is.
#
# USAGE
#
#     sh bench/blk-transfer-sweep.sh [ROUNDS] [BLOCKS...]
#     sh bench/blk-transfer-sweep.sh 6 1 16          # the before and the after, six rounds each
#     sh bench/blk-transfer-sweep.sh 3 1 2 4 8 16    # where the curve bends
#
# Defaults: 5 rounds, 1 and 16 blocks (4 KiB and 64 KiB per blk request).
#
# OUTPUT is one tab-separated row per measured phase on stdout:
#
#     blocks<TAB>round<TAB>load<TAB>bench<TAB>value
#
# where `bench` is either a row name (`fs_seq_read`, ...) and the value is ns per request, or that
# name with `_mib_per_s` appended and the value is MiB/s, straight from the probe line.
#
# `fs_read` is the noise control and it is the right one here for the same reason it was in the
# other two sweeps: it reads `motd`, which is 69 bytes and lives INLINE in its node, so it moves no
# record and issues no blk request larger than one block whatever this constant is set to. A round
# whose `fs_read` is far from its quiet value was taken on a loaded machine, and the analysis
# discards it rather than averaging it in (the discipline milestone 38 set; see notes/benchmarks.md).
#
# BUGS
#
# - It rebuilds the kernel, the user programs and the FS server per point, because the constant is
#   in a crate all three depend on. That is outside the guest's timed window and does not touch the
#   numbers, but the wall-clock cost of a sweep is dominated by cargo.
# - It does not check that the machine is quiet; it only records the load so you can tell
#   afterwards.
# - It does not vary `fs::TRANSFER_PAGES` alongside `blk::TRANSFER_BLOCKS`; the two contracts are
#   swept independently on purpose (bench/transfer-size-sweep.sh is the file-channel sibling), so a
#   run at 64 KiB file requests and 4 KiB blk requests is a real, reachable point rather than a
#   confound.
set -eu

cd "$(dirname "$0")/.."

LIB=crates/fs_proto/src/lib.rs
SAVED=$(mktemp -t blk-transfer-sweep)

restore() {
    if [ -f "$SAVED" ]; then
        cp "$SAVED" "$LIB"
        rm -f "$SAVED"
    fi
}
trap restore EXIT HUP INT TERM

if ! git diff --quiet -- "$LIB"; then
    echo "blk-transfer-sweep: $LIB has uncommitted changes; refusing to edit and restore over them" >&2
    exit 1
fi
cp "$LIB" "$SAVED"

ROUNDS=${1:-5}
[ $# -gt 0 ] && shift || true
BLOCKS=${*:-1 16}

for blocks in $BLOCKS; do
    # The constant, and only the constant. Anchored on the whole line so a comment mentioning
    # TRANSFER_BLOCKS cannot be caught by it.
    cp "$SAVED" "$LIB"
    sed -i '' "s|^    pub const TRANSFER_BLOCKS: usize = .*;|    pub const TRANSFER_BLOCKS: usize = $blocks;|" "$LIB"
    grep -q "^    pub const TRANSFER_BLOCKS: usize = $blocks;" "$LIB" || {
        echo "blk-transfer-sweep: could not set TRANSFER_BLOCKS to $blocks in $LIB" >&2
        exit 1
    }

    round=0
    while [ "$round" -lt "$ROUNDS" ]; do
        round=$((round + 1))
        load=$(uptime | sed 's/.*averages*: *//' | awk '{print $1}' | tr -d ,)
        out=$(cargo xtask bench --release --real --smp 2>&1) || {
            echo "blk-transfer-sweep: bench failed at $blocks blocks, round $round" >&2
            echo "$out" >&2
            exit 1
        }
        echo "$out" | awk -v bl="$blocks" -v rnd="$round" -v load="$load" '
            $1 ~ /^fs_(read|seq_write|seq_read|rand_read|rand_write|record_read|payload_fill)$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", bl, rnd, load, $1, $NF
            }
            # The probe line carries MiB/s, which is the only figure comparable across points.
            # cargo xtask bench reprints the kernel line "bench-probe: X" as "  probe: X", so both
            # spellings are matched here.
            $1 ~ /^(bench-)?probe:$/ && $2 ~ /_mib_per_s$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", bl, rnd, load, $2, $NF
            }'
    done
done
