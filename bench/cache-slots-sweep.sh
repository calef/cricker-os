#!/bin/sh
# Sweep the FS server's metadata cache capacity and measure filesystem throughput at each one
# (milestone 138 step 2).
#
# WHY THIS EXISTS. Step 4 batches a record's own blocks into fewer blk round trips but leaves
# Transaction::read_tree_and_addr's five-block tree walk untouched: it is issued as five separate
# single-block Disk::read_at calls, one per level, fresh on every Server::read/write/... call, even
# when the last call resolved the very same node (notes/fs-server.md, "the same five blocks every
# time"). Step 2's CachedDisk (fs_server/src/lib.rs) answers a repeated single-block read from
# memory instead. Its payoff shows up ACROSS separate file-service requests to the same handle, not
# within one large transfer, because RedoxFS only walks the tree once per Server call regardless of
# how many records that one call's transfer spans. This sweep is the measurement that says how much,
# the sibling of bench/record-level-sweep.sh, bench/transfer-size-sweep.sh and
# bench/blk-transfer-sweep.sh in shape.
#
# WHAT IT DOES. `CACHE_SLOTS` (fs_server/src/bin/fs_server.rs) is how many single blocks
# `CachedDisk` holds. This script EDITS THAT CONSTANT IN PLACE, builds, measures, and puts it back.
# It restores on any exit, including a signal, and refuses to start if that file is already dirty.
#
# A capacity of 1 is not quite "no cache": if the exact same block is asked for twice in a row it
# still hits. But the tree walk touches five DIFFERENT blocks per call
# (L3, L2, L1, L0, the node itself), so a one-slot cache thrashes across a single walk and almost
# never survives to the next one, which is as close to "off" as this type can express without a
# second code path. Read the difference between capacity 1 and 64 as the cache's payoff; do not
# read capacity 1 alone as a true zero.
#
# USAGE
#
#     sh bench/cache-slots-sweep.sh [ROUNDS] [SLOTS...]
#     sh bench/cache-slots-sweep.sh 6 1 64          # off (approximately) and the shipped size
#     sh bench/cache-slots-sweep.sh 3 1 4 16 64     # where the curve bends
#
# Defaults: 5 rounds, 1 and 64 slots.
#
# OUTPUT is one tab-separated row per measured phase on stdout:
#
#     slots<TAB>round<TAB>load<TAB>bench<TAB>value
#
# where `bench` is either a row name (`fs_seq_read`, ...) and the value is ns per request, or that
# name with `_mib_per_s` appended and the value is MiB/s, straight from the probe line.
#
# `fs_read` is the noise control, for the same reason it is in the other sweeps: it reads `motd`,
# which lives inline in its node, so it still walks the tree (and so the cache still applies to it),
# but its cost is otherwise independent of anything this sweep varies.  A round whose `fs_read` is
# far from its quiet value was taken on a loaded machine, and the analysis discards it rather than
# averaging it in (the discipline milestone 38 set; see notes/benchmarks.md).
#
# BUGS
#
# - It rebuilds the kernel, the user programs and the FS server per point, because the constant sits
#   in a binary all three depend on transitively through the initrd image. That is outside the
#   guest's timed window and does not touch the numbers, but the wall-clock cost of a sweep is
#   dominated by cargo.
# - It does not check that the machine is quiet; it only records the load so you can tell
#   afterwards.
# - `fs_read` is not a pure control here the way it is in the transfer-size and blk-transfer sweeps:
#   because it also walks the tree, its own number moves a little with the cache too. It is still
#   the best load signal available, because whatever it moves for load-unrelated reasons is common
#   to every phase in the same round.
set -eu

cd "$(dirname "$0")/.."

BIN=fs_server/src/bin/fs_server.rs
SAVED=$(mktemp -t cache-slots-sweep)

restore() {
    if [ -f "$SAVED" ]; then
        cp "$SAVED" "$BIN"
        rm -f "$SAVED"
    fi
}
trap restore EXIT HUP INT TERM

if ! git diff --quiet -- "$BIN"; then
    echo "cache-slots-sweep: $BIN has uncommitted changes; refusing to edit and restore over them" >&2
    exit 1
fi
cp "$BIN" "$SAVED"

ROUNDS=${1:-5}
[ $# -gt 0 ] && shift || true
SLOTS=${*:-1 64}

for slots in $SLOTS; do
    # The constant, and only the constant. Anchored on the whole line so a comment mentioning
    # CACHE_SLOTS cannot be caught by it.
    cp "$SAVED" "$BIN"
    sed -i '' "s|^const CACHE_SLOTS: usize = .*;|const CACHE_SLOTS: usize = $slots;|" "$BIN"
    grep -q "^const CACHE_SLOTS: usize = $slots;" "$BIN" || {
        echo "cache-slots-sweep: could not set CACHE_SLOTS to $slots in $BIN" >&2
        exit 1
    }

    round=0
    while [ "$round" -lt "$ROUNDS" ]; do
        round=$((round + 1))
        load=$(uptime | sed 's/.*averages*: *//' | awk '{print $1}' | tr -d ,)
        out=$(cargo xtask bench --release --real --smp 2>&1) || {
            echo "cache-slots-sweep: bench failed at $slots slots, round $round" >&2
            echo "$out" >&2
            exit 1
        }
        echo "$out" | awk -v sl="$slots" -v rnd="$round" -v load="$load" '
            $1 ~ /^fs_(read|seq_write|seq_read|rand_read|rand_write|record_read|payload_fill)$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", sl, rnd, load, $1, $NF
            }
            $1 ~ /^(bench-)?probe:$/ && $2 ~ /_mib_per_s$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", sl, rnd, load, $2, $NF
            }'
    done
done
