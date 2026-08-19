#!/bin/sh
# Measure file throughput THROUGH A MOUNTED SMB SHARE, at each of the file contract's transfer
# sizes (milestone 55).
#
# WHY THIS EXISTS, and it is the whole reason it is a second script rather than a flag on its
# sibling. bench/transfer-size-sweep.sh measures milestone 138 step 3 against `fs_proto`, by a
# client that speaks the file contract directly, and it found 8.02x on a sequential write. Nothing
# a customer runs speaks that contract. A Mac's bytes arrive through TCP, the socket contract, a
# reassembly buffer and an SMB2 state machine, and only then become an `fs::WRITE`. So a speedup on
# the contract is a claim about the contract until somebody measures the path a backup takes, and
# this is that measurement.
#
# WHAT IT DOES. `fs_proto::fs::TRANSFER_PAGES` is how many contiguous pages a client and the FS
# server share, and setting it to 1 reproduces the contract exactly as it stood before step 3 (the
# SMB adapter derives its own chunk size from it, which is the change milestone 55 made). This
# script therefore EDITS THAT CONSTANT IN PLACE, builds, runs the aarch64 kernel suite with the
# throughput leg turned on, and puts it back. It restores on any exit, including a signal, and
# refuses to start if that file is already dirty, so it can never restore over somebody else's
# edit. That is its sibling's discipline, unchanged.
#
# The leg lives in xtask's SMB prober (`smb_throughput_leg`): a host process, over a real forwarded
# TCP connection, writing 1 MiB and reading it back in `smb_proto::MAX_TRANSACT`-sized messages.
# 1 MiB is `fs_proto::fixture::throughput::TOTAL`, so a row here and a row from the in-guest
# benchmark are the same work seen through two different depths of the stack.
#
# USAGE
#
#     sh bench/smb-throughput.sh [ROUNDS] [PAGES...]
#     sh bench/smb-throughput.sh 3 1 16      # the before and the after, three rounds each
#
# Defaults: 3 rounds, 1 and 16 pages (4 KiB and 64 KiB).
#
# OUTPUT is one tab-separated row per direction per round on stdout:
#
#     pages<TAB>round<TAB>load<TAB>direction<TAB>mib_per_s
#
# BUGS
#
# - **The ceiling is SMB's, not the file contract's.** This server negotiates 64 KiB for
#   `MaxReadSize` and `MaxWriteSize` (`smb_proto::MAX_TRANSACT`), so a real client never asks for
#   more than that however large `TRANSFER_PAGES` becomes, and a point past 16 pages measures
#   nothing new on this path. Raising it means raising both numbers.
# - **A full kernel suite per point**, which is minutes, because the leg runs inside the boot that
#   the SMB gate already stages and there is no smaller boot that has a mounted share in it. The
#   timed window is only the leg; everything else is wall clock spent to get there.
# - It does not check that the machine is quiet; it records the load so you can tell afterwards.
# - The write leg's bytes stay on the fixture image (16 MiB, copy-on-write). A run that has been
#   repeated many times against a stale image can meet STATUS_DISK_FULL; the image regenerates from
#   `crates/nifefs`, so deleting it is the fix.
set -eu

cd "$(dirname "$0")/.."

LIB=crates/fs_proto/src/lib.rs
SAVED=$(mktemp -t smb-throughput)

restore() {
    if [ -f "$SAVED" ]; then
        cp "$SAVED" "$LIB"
        rm -f "$SAVED"
    fi
}
trap restore EXIT HUP INT TERM

if ! git diff --quiet -- "$LIB"; then
    echo "smb-throughput: $LIB has uncommitted changes; refusing to edit and restore over them" >&2
    exit 1
fi
cp "$LIB" "$SAVED"

ROUNDS=${1:-3}
[ $# -gt 0 ] && shift || true
PAGES=${*:-1 16}

for pages in $PAGES; do
    # The constant, and only the constant. Anchored on the whole line so a comment mentioning
    # TRANSFER_PAGES cannot be caught by it.
    cp "$SAVED" "$LIB"
    sed -i '' "s|^    pub const TRANSFER_PAGES: usize = .*;|    pub const TRANSFER_PAGES: usize = $pages;|" "$LIB"
    grep -q "^    pub const TRANSFER_PAGES: usize = $pages;" "$LIB" || {
        echo "smb-throughput: could not set TRANSFER_PAGES to $pages in $LIB" >&2
        exit 1
    }

    round=0
    while [ "$round" -lt "$ROUNDS" ]; do
        round=$((round + 1))
        load=$(uptime | sed 's/.*averages*: *//' | awk '{print $1}' | tr -d ,)
        out=$(NIFE_SMB_THROUGHPUT=1 cargo xtask test --arch aarch64 2>&1) || {
            echo "smb-throughput: the suite failed at $pages pages, round $round" >&2
            echo "$out" | tail -40 >&2
            exit 1
        }
        echo "$out" | awk -v pg="$pages" -v rnd="$round" -v load="$load" '
            # smb-throughput: write 12.34 MiB/s (...)
            $1 == "smb-throughput:" {
                printf "%s\t%s\t%s\t%s\t%s\n", pg, rnd, load, $2, $3
            }'
    done
done
