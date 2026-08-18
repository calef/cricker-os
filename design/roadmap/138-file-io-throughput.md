# 138. Close the read gap: a 4 KiB request must stop moving 128 KiB

**Status: NOT-STARTED.** Minted 2026-08-18 by calef, on milestone 38's measurement: *"Against
buffered Linux we are three orders of magnitude behind on reads. We need a milestone to optimize and
close the gap."*

**Gate: DECISION.** Both candidate fixes are things two programs agree on, which the *move fast on
what can be undone* tenet puts in the irreversible column, and milestone 38's own `BUGS` entry says
so: *"the fix is not in this server: it is either a multi-page transfer on the contract, or a record
level chosen to match the transfer unit, and both are decisions rather than patches."* The two must
be priced against each other before either is built.

**In brief.** Every 4 KiB file request moves **128 KiB**, in both directions. A read fetches a whole
RedoxFS record (32 blocks); a write reads the record, changes 4 KiB and writes a new copy, because
the store is copy-on-write. This milestone closes that, and only that.

## The measurement this exists to move

From milestone 38, all medians, ns per 4 KiB, at a matched virtualization tier:

| | nife | ext4 `O_DIRECT` | ext4 buffered | raw virtio |
|---|---|---|---|---|
| sequential read | 1,509,270 | 91,694 | **547** | 53,296 |
| sequential write | 2,566,304 | 63,688 | 2,068 | 42,104 |

**The architecture is not the problem, and that is the finding that makes this milestone tractable.**
The confined-server tax is about 1 us per request against a 1.5 to 3.4 ms operation: 0.07% of the
measurement. The confined userspace block server is **at parity with Linux's block layer** (46.2 us
per 4 KiB against 39 to 53 us for Linux's own raw virtio reads on the same device). Every nife figure
is 46.2 us times a small integer, and nothing was fitted. **The 32x is the entire remaining gap** and
it belongs to the vendored store's record size, not to the microkernel.

## The two candidate fixes, both irreversible, neither priced against the other

1. **A multi-page transfer on the file contract.** `fs_proto`'s transfer unit is one page because
   that is what a request can carry. Milestone 38 measured the neighbouring case: ext4 moves 64 KiB
   for about what it charges for 4 KiB, so sixteen times the payload arrives at the same price,
   600-900 MiB/s against 40-80. This is a wire change.
2. **A record level matched to the transfer unit.** Leaves the contract alone and changes what the
   store does. Cheaper to reach and it gives up compression's current terms: RedoxFS compresses a
   record with lz4 only when the record exceeds one block, which is always true today.

**Both must arrive priced together**, per `AGENTS.md`'s rule that an irreversible fork gets options
and their costs rather than a recommendation.

## What is out of scope, deliberately

**A cache is not this milestone.** There is no cache anywhere in the read path today, which milestone
38 established rather than assumed: sequential, random and record-aligned reads agree within 3%, and
`fs_test_client`'s claim to measure a "warm" read was wrong and is corrected. Adding one would move
the buffered-Linux comparison more than either fix above, and it is a larger design with its own
coherency and confinement questions. If it is wanted, it is its own milestone and this block should
not quietly grow into it.

## Why it matters

**It is on the customer path.** Milestone 55's Time Machine target is judged against these numbers,
and at 4 KiB they are not comfortable. §34's "primary filesystem" claim can now be argued from
measurements rather than asserted, and this is the measurement that most weakens it.

## BUGS

- **This block names a 32x and the reader may assume closing it closes the gap to buffered Linux. It
  does not.** Buffered Linux is 547 ns against our 1,509,270. Removing the 32x leaves roughly two
  orders of magnitude, and the rest is page cache, which is the out-of-scope paragraph above.
- **The payload entropy caveat travels with every number here.** RedoxFS lz4-compresses records, so
  an all-zero file reads and writes several times faster than an incompressible one. Milestone 38's
  figures use an incompressible payload, which is the conservative choice and the one a backup
  workload resembles; a re-measurement that quietly changes payload is not comparable.
- **`Transaction::write_node` compares before writing**, so rewriting a block with identical contents
  costs a read and no write. A benchmark that sends one constant page repeatedly measures the
  comparison rather than the store.
