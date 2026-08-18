#!/bin/sh
# Sweep RedoxFS's record level and measure filesystem throughput at each one (milestone 138).
#
# WHY THIS EXISTS. Milestone 38 measured that every 4 KiB file request moves 128 KiB, and that this
# single term is the whole remaining throughput gap. Milestone 138 lists three candidate fixes and
# says none of them can be argued until somebody measures throughput against the record level. This
# is that measurement, and it is a script rather than a paragraph so the next person can rerun it.
#
# WHAT IT DOES, AND THE ONE UGLY PART. RedoxFS stores the record level per node
# (`node.rs: pub record_level: Le<u32>`), set once at file creation from the crate constant
# `RECORD_LEVEL` (`vendor/redoxfs/src/lib.rs`). Nothing in `fs_proto` can ask for a different one, so
# the only way to create a file at another level today is to rebuild with a different constant. This
# script therefore EDITS THE VENDORED CONSTANT IN PLACE, builds, measures, and puts it back. It
# restores on any exit, including a signal. It refuses to start if that file is already dirty, so it
# can never restore over somebody else's edit.
#
# The image is regenerated on every `cargo xtask bench --release --real --smp` (it calls
# `mkredoxfs`), so each point is a whole filesystem built at that level, not a mixed one.
#
# USAGE
#
#     sh bench/record-level-sweep.sh [ROUNDS] [LEVELS...]
#     sh bench/record-level-sweep.sh 5 0 1 2 3 4 5     # the full sweep, five rounds each
#     sh bench/record-level-sweep.sh 1 0 5            # a quick two-point check
#
# Defaults: 5 rounds, levels 0 through 5. Level 5 is the tree's shipped value (128 KiB records);
# level 0 is one block (4 KiB).
#
# OUTPUT is one tab-separated row per round on stdout:
#
#     level<TAB>round<TAB>load<TAB>bench<TAB>ns_per_4k
#
# `load` is the host's one-minute load average at the start of the round. `fs_read` is the noise
# control and it is the right one: it reads `motd`, which is 69 bytes and lives INLINE in its node,
# so no record is fetched and its cost is independent of the level being swept. A round whose
# `fs_read` is far from its quiet value was taken on a loaded machine, and the analysis discards it
# rather than averaging it in (the discipline milestone 38 set; see notes/benchmarks.md).
#
# BUGS
#
# - It rebuilds the whole vendored engine per level, so the first round of each level pays a compile
#   the others do not. That is outside the guest's timed window and does not touch the numbers, but
#   it does mean the wall-clock cost of a full sweep is dominated by cargo.
# - It cannot sweep the TRANSFER size, only the record level, because a `fs_proto` request carries at
#   most one page. That limit is milestone 138's option 1 and this script cannot price it.
# - It does not check that the machine is quiet; it only records the load so you can tell afterwards.
set -eu

cd "$(dirname "$0")/.."

LIB=vendor/redoxfs/src/lib.rs
SAVED=$(mktemp -t record-level-sweep)

restore() {
    if [ -f "$SAVED" ]; then
        cp "$SAVED" "$LIB"
        rm -f "$SAVED"
    fi
}
trap restore EXIT HUP INT TERM

if ! git diff --quiet -- "$LIB"; then
    echo "record-level-sweep: $LIB has uncommitted changes; refusing to edit and restore over them" >&2
    exit 1
fi
cp "$LIB" "$SAVED"

ROUNDS=${1:-5}
[ $# -gt 0 ] && shift || true
LEVELS=${*:-0 1 2 3 4 5}

for level in $LEVELS; do
    # The constant, and only the constant. Anchored on the whole line so a comment mentioning
    # RECORD_LEVEL cannot be caught by it.
    cp "$SAVED" "$LIB"
    sed -i '' "s|^pub const RECORD_LEVEL: usize = .*;|pub const RECORD_LEVEL: usize = $level;|" "$LIB"
    grep -q "^pub const RECORD_LEVEL: usize = $level;" "$LIB" || {
        echo "record-level-sweep: could not set RECORD_LEVEL to $level in $LIB" >&2
        exit 1
    }

    round=0
    while [ "$round" -lt "$ROUNDS" ]; do
        round=$((round + 1))
        load=$(uptime | sed 's/.*averages*: *//' | awk '{print $1}' | tr -d ,)
        out=$(cargo xtask bench --release --real --smp 2>&1) || {
            echo "record-level-sweep: bench failed at level $level round $round" >&2
            echo "$out" >&2
            exit 1
        }
        # The table's last column is ns/iter, which for these rows is ns per 4 KiB transfer.
        echo "$out" | awk -v lvl="$level" -v rnd="$round" -v load="$load" '
            $1 ~ /^fs_(read|seq_write|seq_read|rand_read|rand_write|record_read|payload_fill)$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", lvl, rnd, load, $1, $NF
            }'
    done
done
