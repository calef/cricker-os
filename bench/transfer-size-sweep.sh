#!/bin/sh
# Sweep the file contract's transfer size and measure filesystem throughput at each one
# (milestone 138 step 3).
#
# WHY THIS EXISTS. Milestone 38 found that every 4 KiB file request moves a whole RedoxFS record,
# and milestone 138 step 1 cut the record and then reported that it had not moved the residual at
# all: after it, 87% of a 4 KiB write is a fixed per-request term. Step 3 is the change that
# amortises that term over more payload, and this is the measurement that says by how much. It is a
# script rather than a paragraph so the next person can rerun it, and it is the sibling of
# bench/record-level-sweep.sh in shape on purpose.
#
# WHAT IT DOES. `fs_proto::fs::TRANSFER_PAGES` is how many contiguous pages the client and the FS
# server share, and therefore the most one request can carry. Every party's mapping, the server's
# clamp and the benchmark's transfer unit all derive from it, so setting it to 1 reproduces the
# contract exactly as it stood before step 3. This script therefore EDITS THAT CONSTANT IN PLACE,
# builds, measures, and puts it back. It restores on any exit, including a signal, and refuses to
# start if that file is already dirty, so it can never restore over somebody else's edit.
#
# The benchmark holds BYTES MOVED constant (`fixture::throughput::TOTAL`, 1 MiB) rather than the
# transfer count, so every point moves the same file and the MiB/s figures are comparable across
# the sweep. The ns/iter column is NOT: it is per request, and a request is a different size at
# each point. Read the `bench-probe: fs_throughput` lines for throughput and the rows for latency.
#
# USAGE
#
#     sh bench/transfer-size-sweep.sh [ROUNDS] [PAGES...]
#     sh bench/transfer-size-sweep.sh 6 1 16          # the before and the after, six rounds each
#     sh bench/transfer-size-sweep.sh 3 1 2 4 8 16    # where the curve bends
#
# Defaults: 5 rounds, 1 and 16 pages (4 KiB and 64 KiB).
#
# OUTPUT is one tab-separated row per measured phase on stdout:
#
#     pages<TAB>round<TAB>load<TAB>bench<TAB>value
#
# where `bench` is either a row name (`fs_seq_read`, ... and the value is ns per request) or that
# name with `_mib_per_s` appended (and the value is MiB/s, straight from the probe line).
#
# `fs_read` is the noise control and it is the right one here for the same reason it was in the
# record sweep: it reads `motd`, which is 69 bytes and lives INLINE in its node, so it moves no
# record and asks for no more than a page whatever this constant is set to. A round whose `fs_read`
# is far from its quiet value was taken on a loaded machine, and the analysis discards it rather
# than averaging it in (the discipline milestone 38 set; see notes/benchmarks.md).
#
# BUGS
#
# - It rebuilds the kernel, the user programs and the FS server per point, because the constant is
#   in a crate all three depend on. That is outside the guest's timed window and does not touch the
#   numbers, but the wall-clock cost of a sweep is dominated by cargo.
# - It does not check that the machine is quiet; it only records the load so you can tell
#   afterwards.
# - At 16 pages a phase is only 16 timed requests, because the bytes moved are held constant and
#   the fixture image bounds them at 1 MiB. Each request is long enough that the counter resolution
#   is not the problem, but the sample is small: take more rounds rather than trusting one.
set -eu

cd "$(dirname "$0")/.."

LIB=crates/fs_proto/src/lib.rs
SAVED=$(mktemp -t transfer-size-sweep)

restore() {
    if [ -f "$SAVED" ]; then
        cp "$SAVED" "$LIB"
        rm -f "$SAVED"
    fi
}
trap restore EXIT HUP INT TERM

if ! git diff --quiet -- "$LIB"; then
    echo "transfer-size-sweep: $LIB has uncommitted changes; refusing to edit and restore over them" >&2
    exit 1
fi
cp "$LIB" "$SAVED"

ROUNDS=${1:-5}
[ $# -gt 0 ] && shift || true
PAGES=${*:-1 16}

for pages in $PAGES; do
    # The constant, and only the constant. Anchored on the whole line so a comment mentioning
    # TRANSFER_PAGES cannot be caught by it.
    cp "$SAVED" "$LIB"
    sed -i '' "s|^    pub const TRANSFER_PAGES: usize = .*;|    pub const TRANSFER_PAGES: usize = $pages;|" "$LIB"
    grep -q "^    pub const TRANSFER_PAGES: usize = $pages;" "$LIB" || {
        echo "transfer-size-sweep: could not set TRANSFER_PAGES to $pages in $LIB" >&2
        exit 1
    }

    round=0
    while [ "$round" -lt "$ROUNDS" ]; do
        round=$((round + 1))
        load=$(uptime | sed 's/.*averages*: *//' | awk '{print $1}' | tr -d ,)
        out=$(cargo xtask bench --release --real --smp 2>&1) || {
            echo "transfer-size-sweep: bench failed at $pages pages, round $round" >&2
            echo "$out" >&2
            exit 1
        }
        echo "$out" | awk -v pg="$pages" -v rnd="$round" -v load="$load" '
            $1 ~ /^fs_(read|seq_write|seq_read|rand_read|rand_write|record_read|payload_fill)$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", pg, rnd, load, $1, $NF
            }
            # The probe line carries MiB/s, which is the only figure comparable across points.
            # cargo xtask bench reprints the kernel line "bench-probe: X" as "  probe: X", so both
            # spellings are matched here. The first draft matched only the kernel spelling and
            # silently produced no MiB/s rows, which looks exactly like a run with no throughput in
            # it. (An apostrophe in this comment would also end the awk program: it is inside single
            # quotes. That is why none of these comments contract a word.)
            $1 ~ /^(bench-)?probe:$/ && $2 ~ /_mib_per_s$/ {
                printf "%s\t%s\t%s\t%s\t%s\n", pg, rnd, load, $2, $NF
            }'
    done
done
