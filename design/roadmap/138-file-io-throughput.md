# 138. Close the read gap: a 4 KiB request must stop moving 128 KiB

**Status: PARTIAL.** Step 1 of four is **built and measured** (2026-08-18): the record level is
1, a 4 KiB read is **5.13x** faster and a 4 KiB write **3.01x**, and the per-request residual it
leaves is **206 us on a read (72% of it) and 690 us on a write (87%)**. See
notes/benchmarks.md and vendor/README.md divergence 5. Steps 2, 3 and 4 are not started.
Minted 2026-08-18 by calef, on milestone 38's measurement: *"Against
buffered Linux we are three orders of magnitude behind on reads. We need a milestone to optimize and
close the gap."*

**Gate: NONE.** Decided by calef 2026-08-18: take all four steps in the order below, measuring at
each. This gate read `DECISION` while the options were unpriced; the sweep (PR #338) and the
metadata identification (PR #348) priced them, and the decision followed. The original reasoning is
kept below because it is why the answer can be trusted: both candidate fixes are things two programs
agree on, which the *move fast on
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
measurement. The confined userspace block server is **at parity with Linux's block layer** (39.0 us
per 4 KiB against 39 to 53 us for Linux's own raw virtio reads on the same device). Every nife figure
is 39.0 us times a small integer plus a fixed walk, and nothing was fitted. **The 32x is most of the
remaining gap** and
it belongs to the vendored store's record size, not to the microkernel.

## The candidate fixes, none priced against the others

1. **A multi-page transfer on the file contract.** `fs_proto`'s transfer unit is one page because
   that is what a request can carry. Milestone 38 measured the neighbouring case: ext4 moves 64 KiB
   for about what it charges for 4 KiB, so sixteen times the payload arrives at the same price,
   600-900 MiB/s against 40-80. This is a wire change.
2. **A record level matched to the transfer unit.** Leaves the contract alone and changes what the
   store does. Cheaper to reach and it gives up compression's current terms: RedoxFS compresses a
   record with lz4 only when the record exceeds one block, which is always true today.

**All three must arrive priced together**, per `AGENTS.md`'s rule that an irreversible fork gets
options and their costs rather than a recommendation.

3. **Replace the store** (calef, 2026-08-18, raising it on the pull request that minted this block:
   *"we should consider if redoxfs is the problem and we should move to a different
   implementation"*). It belongs in the list, and the first draft of this block was wrong to name two
   options that both keep RedoxFS without saying that a third existed.

   **What the code says, read rather than assumed.** `RECORD_LEVEL` is 5 and `BLOCK_SIZE` is 4096, so
   `RECORD_SIZE` is 128 KiB. But **the record level is a per-node field in the on-disk format**
   (`node.rs`: `pub record_level: Le<u32>`), it is set once at file creation from that constant, and
   every read and write path honours the **node's** value rather than the constant
   (`transaction.rs`: `let record_level = node.data().record_level();`). Directories already get 0.

   So a smaller record for a file is **a creation-time choice the format already supports**, not a
   format change and not a fork of the vendored crate. That does not make it free, and the costs are
   named in option 2, but it means the 32x is a **parameter this store exposes** rather than a
   property it imposes. On the evidence available today, RedoxFS is not structurally the cause.

   **What would actually justify replacing it** is therefore something this milestone has not
   measured: a cost that survives after the record level is tuned. §46 puts a dependency of this size
   in the expensive column (*"adding one is a morning; removing one after a subsystem is built on it
   is a project"*), and RedoxFS is the case §46 itself cites for vendoring, where correctness is won
   by exposure rather than by reading a spec.

## The measurement was taken, and it moved two of the three

**Superseded 2026-08-18.** This section asked for a record-level sweep before any option was chosen.
It was run (PR #338) and the result is in notes/benchmarks.md; what follows is kept because the
question it asked is why the answer is trustworthy, and struck through in substance rather than
deleted so a reader can see what was asked.

**What it found**, all measured on milestone 38's own harness across twenty interleaved passes:
`cost = 208 us + 39.0 us x 2^level`, read residuals within 5% at every level. So a one-block record
buys **5.6x on reads and 3.0 to 3.8x on writes, not 32x**, because the record is only one of two
terms. Option 1 is worth more (16x) because it amortises both. Both together are 28x.

**And it corrected two things this block asserted.** Milestone 38's 46.2 us per block was an average
that charged the per-request walk to the blocks; the marginal cost is 39.0 us and the walk is a
separate 208 us. And this block said option 2 is "not a fork of the vendored crate": **it is one.**
`Node::new` takes no level and has no setter, and three call sites gate on `RECORD_LEVEL`, so
lowering the constant makes every record already stored at a higher level unreadable.

**The 208 us was then identified** (PR #348): five single-block reads per request, the *same* five
blocks every time, 99.6% repeat rate, 94% of the fixed term. That is the absence of a cache rather
than a property of RedoxFS, which is what rules out option 3.

## The question this section originally asked

**Nobody has measured throughput against record level, and it is the cheap experiment that decides
between all three options.** Sweep the per-file record level against the transfer size and the access
pattern, on the harness milestone 38 already built. If a lower level closes most of the 32x, option 2
is a small change to a constant at creation and options 1 and 3 are unnecessary. If it does not, the
number that survives is the argument for one of the others, and it is an argument nobody can make
today.

**And it should re-ask which workload this milestone is optimising**, because milestone 38 measured
4 KiB by convention rather than by need. A Time Machine backup writes **band files**, which are large
and sequential, and a 128 KiB record is plausibly right for those. It is possible that the customer
path wants the current setting and that the 4 KiB figure is the atypical case. That would not make
the gap uninteresting, but it would change what "close the gap" means and which milestone owns it.

## Decided 2026-08-18: all of them, then measure again

**calef:** *"it seems like we do them all. And then we measure and we figure out other optimization
options. Because disk performance is pretty critical to many real workloads."*

Four pieces, and the ordering is set by what each one unblocks rather than by size:

| | what | measured or modelled effect |
|---|---|---|
| **1** | **option 2**, the record level | **DONE 2026-08-18, measured**: 4 KiB read 2.68 -> **13.76 MiB/s**, write 1.63 -> **4.90**. The modelled 15.8 was level 0's figure; this shipped at level 1, which keeps lz4 and halves the space cost for 8.7% of the read speed. **And there is no one-way door after all**: the created level and the largest readable level are now separate constants, so nothing stored at any level 0 to 5 becomes unreadable and the next change cannot orphan this one's data |
| **2** | **the metadata cache**, the five blocks | on its own worth 15%; with a small record it is **4.7x**. The two are multiplicative and neither is worth much alone |
| **3** | **option 1**, multi-page transfer on the file contract | 64 KiB in one request: 75 MiB/s, and 98 with the cache. This is the customer path |
| **4** | **the block contract**, one request per 4 KiB today | the ~100 MiB/s ceiling behind all three. Recorded in notes/fs-server.md's BUGS and not yet a milestone |

**Then re-measure and re-decide.** The numbers above are a model calibrated against the sweep (it
reproduces the measured 837 us per 64 KiB as 832), not a prediction anybody should spend four
milestones on without checking at each step.

## Step 1, built and measured: the record is 8 KiB (2026-08-18)

**5.13x on a 4 KiB read and 3.01x on a 4 KiB write**, measured on milestone 38's harness over six
interleaved passes at levels 5, 1 and 0, on a machine quiet enough that the `fs_read` control varied
0.6% across every level and no normalisation was needed. 1,458,124 ns to 283,974 on a sequential
read; 2,399,611 to 796,930 on a sequential write. notes/benchmarks.md has the tables.

**Level 1 rather than level 0, verified rather than inherited.** The sweep recommended it and this
run checked the trade it named: level 0 reads 8.7% faster and gives up lz4 entirely, because RedoxFS
compresses a record only when it is larger than one block, and it pays roughly twice the space
overhead for that (+38% against +19% on text). Sequential writes are marginally *faster* at level 1.
The 8.7% is not the compression; it is the second block at 39.1 us, which the two-term model predicts
without knowing lz4 exists.

**The one-way door this table said to walk through is not there any more, and that is the part worth
reviewing.** The block priced step 1 as irreversible: lowering `RECORD_LEVEL` makes every record
stored at a higher level answer `ENOENT`, which is free today only because nothing is stored. It is
free *because* of the timing, and the timing is not a property anyone can hold on to. So the change
splits the constant instead: `RECORD_LEVEL` is the level a new file is **created** at (now 1) and a
new `RECORD_LEVEL_MAX` is the largest level this build can **read** (still upstream's 5), which is
what the two `BlockTrait::empty` guards compare against. Nothing at any level from 0 to 5 becomes
unreadable, a future change of the created level cannot orphan what this one wrote, and the guards
now compare against a maximum, which is half of what a genuine per-file level would need. It cost one
constant. See vendor/README.md divergence 5.

**The residual, which is what this step was asked to report.** It did not shrink at all, and that is
the finding: a read's fixed term is **205,698 ns**, unchanged, and it went from 14% of a request to
**72%** of one. A write's is **690,085 ns** and **87%**. Of the read's 206 us, ~195 is step 2's five
repeated block reads and ~13 us is the IPC round trip and the server's own work, which is the number
this milestone's four steps never touch. **The write residual is the transaction** (allocate, rewrite
the node, commit to the header ring, per 4 KiB request), and nothing on this list except step 3
addresses it. After step 1 it is the largest unaddressed term in the whole measurement.

**What step 2 is worth now, against measurement rather than the model.** A read is 283,974 ns; a
cache that removed all five repeated block reads would take it to about **89,000 ns**, which is
**3.2x again** and 16x against where milestone 138 started. The table below modelled 4.7x, and the
difference is that the model was built on level 0's numbers while this shipped at level 1. The
block's other claim survives intact and is now checked: the same cache *before* step 1 would have
been worth 15%, exactly as predicted, so **the two are multiplicative and step 1 is what makes step 2
worth building.**

**Crash consistency was re-run at the new geometry**, because a safety claim measured at one record
size is not automatically true at another. Same fault-point count, same properties, **0 silently
wrong** at both levels; eleven lying-device cases move from "refused at a read" to "recovered", which
is what a smaller record predicts. It also turned up a stale record: notes/fs-server.md's counts were
milestone 37's and the workload has grown since, so the table there was wrong before this lane
touched it. Corrected in place.

## The question underneath, which is worth more than any of the four

**Does this architecture have a disk-read liability that cannot be overcome?** calef, the same day,
and it is a thesis question rather than an optimization one: DECISIONS §14 claims a capability
microkernel that runs real workloads, and a structural inability to read a disk at speed would be the
strongest thing anyone could say against it.

**What is already answered.** The confined userspace block server measured **39.0 us per 4 KiB
against Linux's own raw virtio at 38.7 to 53.3 us on the same device at the same tier**. The extra
address-space crossing this design pays, client to FS server to block server, is **not measurable
against the device round trip**. `relay_rtt` prices a two-hop confined relay at about 2 us. So the
block layer is at parity and the architecture is not the cost there.

**What is not answered, and is the number to watch.** Every request pays a residual of about **13 us**
that no record level and no transfer size removes: the IPC round trip plus the server's own work.
That is the floor, and it puts a cached 4 KiB read at roughly **300 MiB/s** however good everything
else gets. ext4 buffered is 7,141 MiB/s, because it is a memcpy inside the kernel with **no
address-space crossing at all**.

So the honest statement of the frontier: **against uncached Linux this design reaches parity;
against the page cache it does not, and the reason is structural rather than lazy.** Every gap found
so far (a 128 KiB record, a page-sized file contract, a block-sized block contract, no cache) is an
implementation choice Linux also had to solve. The 13 us is the first thing that is not.

**And the way past it, if it is ever worth taking, is capability-shaped**: stop doing a round trip
per request and grant the client frames it can read directly, which is what Linux's `mmap` over the
page cache is. Frames are already capabilities here. Nobody has designed it and it is not this
milestone; it is named so that the residual is understood as a frontier rather than a wall.

**Each of the four steps should report that residual**, so the question gets answered by
accumulation rather than by one argument at the end.

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
- **This block asserts that RedoxFS is not the cause, on a reading of the code rather than on a
  measurement.** The per-node `record_level` field is real and is honoured on both paths; that a
  lower level actually closes the gap is a prediction, and the sweep above is what would test it.
  Nobody has run it.
- **`Transaction::write_node` compares before writing**, so rewriting a block with identical contents
  costs a read and no write. A benchmark that sends one constant page repeatedly measures the
  comparison rather than the store.
- **Step 1's 5.13x is a number about the contract as it is today, not a property of the store.** The
  sweep already showed that once a request carries 64 KiB, every record level from 0 to 4 costs the
  same, because the fixed term is per request and the block count is identical. So step 3 will
  make step 1's ratio meaningless as a ratio, and re-measuring it then is not optional.
- **The space cost of the 8 KiB record was not re-measured for step 1.** The +19% figure is the
  sweep's, taken on text, which is the payload most favourable to lz4; a backup workload is the
  incompressible case and would show only the pointer half. Nobody has measured that case.
- **`RECORD_LEVEL_MAX` keeps old images readable and does not migrate them.** A file created by an
  older build keeps its 128 KiB record forever: it reads correctly and it reads at the old price.
  There is no rewrite path and no `fsck` that would make one, which is the right answer today
  because no such image exists outside a test, and the wrong one the day somebody upgrades a
  populated disk.
- **The step-1 measurement and the crash re-run are both this lane's own**, on one machine, on one
  afternoon. The throughput figures reproduce the earlier sweep's two-term fit to within 3% from a
  different run on a different day, which is the strongest independent check available without a
  second machine, and it is not a second machine.
