# 138. Close the read gap: a 4 KiB request must stop moving 128 KiB

**Status: BUILT.** All four steps are **built and measured**. Step 1 (2026-08-18): the record level
is 1, a 4 KiB read is **5.13x** faster and a 4 KiB write **3.01x**, and the per-request residual it
leaves is **206 us on a read (72% of it) and 690 us on a write (87%)**. Step 3 (2026-08-19), taken
before step 2 because that residual said to: a request carries **64 KiB**, a sequential write is
**8.02x** and a read **5.67x**, and the residual changed owner rather than shrinking, from the file
contract to the block contract. Step 4 (2026-08-19): the blk contract carries **16 blocks**, and the
win (1.16x to 1.55x) is far smaller than the block count predicts, because steps 1 and 3 already
shrank what one blk request can batch and the majority of a request's remaining cost is the
unbatchable tree walk step 2 targets. Step 2 (2026-08-19), last and largest: a 64-block cache over
that tree walk, **22.2x on a repeated inline read** and **1.37x to 1.64x on the throughput phases**.
**Combined, all four steps against milestone 38's original baseline: 70.9x on sequential read
throughput**, measured head to head rather than by multiplying separately-measured ratios. See
notes/benchmarks.md, notes/fs-server.md and vendor/README.md divergence 5.
Minted 2026-08-18 by calef, on milestone 38's measurement: *"Against
buffered Linux we are three orders of magnitude behind on reads. We need a milestone to optimize and
close the gap."*

**Why this needed no per-step gate, and why the decision that got it there can be trusted.** This
milestone's gate read `DECISION` while the options were unpriced; the sweep (PR #338) and the
metadata identification (PR #348) priced them, and calef's 2026-08-18 decision to take all four
steps in order, measuring at each, followed and needed nothing further. The original reasoning is
kept below because it is why the answer can be trusted: both candidate fixes are things two programs
agree on, which the *move fast on
what can be undone* tenet puts in the irreversible column, and milestone 38's own `BUGS` entry says
so: *"the fix is not in this server: it is either a multi-page transfer on the contract, or a record
level chosen to match the transfer unit, and both are decisions rather than patches."* The two had
to be priced against each other before either was built.

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

## The 208 us is the absence of a cache, counted rather than argued (2026-08-18)

The record-level sweep (notes/benchmarks.md) fit every read to `cost = 208 us + 39.0 us x 2^level`
and attributed the fixed term to "RedoxFS re-reading its own metadata, about 5.3 block reads". calef
asked the question that attribution left open: **is the 208 us inherent to RedoxFS's design, or is it
the absence of a cache that any store would also need?**

**It is the absence of a cache.** Every 4 KiB read of an ordinary file makes **exactly five
single-block reads** below the record, and they are **the same five block numbers on every request**:
`Transaction::read_tree_and_addr`'s walk of the node tree (L3 root, L2, L1, L0) and then the file's
own `Node` block. Measured at 5.00 per request over 256 requests per phase, sequential and random,
with 99.6% of them a block already read in the same phase, and unchanged at record levels 1, 2 and 5.
The first four are the tree spine and are shared by every file whose node id falls in the same 256,
so two different files differ in one block out of five.

At the sweep's measured 39.0 us per block-server round trip that is **195 us of the 208, 94% of it**.
The remaining ~13 us is the file-IPC round trip and the server's own work. The same number arrives
independently from a measurement already on the page: `fs_read` reads an inline `motd`, does exactly
these five reads and no record read at all, and costs 203 to 208 us.

**What it means for option 3.** It rules it out on this evidence, and now for a measured reason. The
walk is structural only in that the format fixes its depth; what makes it cost 195 us is that the
same five blocks are fetched off the device 256 times in a row. Every store that maps an id to a node
has a root-to-node path it would fetch too, so replacing RedoxFS buys a rewrite and arrives needing
the identical cache. The cache is small: the spine of a 65,536-node filesystem is 259 blocks, about
1 MiB, and the fifth block is a node the server could hold per open handle.

**What it does not settle**: that a cache is cheap to build. It says which milestone owns the 208 us,
not how to close it. See the out-of-scope section below, which this does not overturn.

## Decided 2026-08-18: all of them, then measure again

**calef:** *"it seems like we do them all. And then we measure and we figure out other optimization
options. Because disk performance is pretty critical to many real workloads."*

Four pieces, and the ordering is set by what each one unblocks rather than by size:

| | what | measured or modelled effect |
|---|---|---|
| **1** | **option 2**, the record level | **DONE 2026-08-18, measured**: 4 KiB read 2.68 -> **13.76 MiB/s**, write 1.63 -> **4.90**. The modelled 15.8 was level 0's figure; this shipped at level 1, which keeps lz4 and halves the space cost for 8.7% of the read speed. **And there is no one-way door after all**: the created level and the largest readable level are now separate constants, so nothing stored at any level 0 to 5 becomes unreadable and the next change cannot orphan this one's data |
| **2** | **the metadata cache**, the five blocks | **DONE 2026-08-19, measured**: a 64-slot write-through cache over the tree walk, **22.2x on a repeated inline read, 1.37x to 1.64x on the throughput phases**, taken after step 4. The block's 4.7x model and its two re-pricings (3.2x after step 1, 1.33x after step 3) all assumed the walk repeated *within* one file-level request; it does not (RedoxFS walks the tree once per `Server` call, not once per record), so the real payoff is across separate requests to the same handle, which every phase of the throughput bench makes and which the measured numbers price directly |
| **3** | **option 1**, multi-page transfer on the file contract | **DONE 2026-08-19, measured**: modelled 75 MiB/s, measured **80.30 on a read and 42.77 on a write**, 5.67x and 8.02x. The wire change is one constant: the length field was always 40 bits and the shared page was what bounded a transfer. This is the customer path |
| **4** | **the block contract**, one request per 4 KiB today | **DONE 2026-08-19, measured**: the blk channel carries 16 blocks, **1.16x to 1.55x**, far below the naive 16x because steps 1 and 3 already shrank each record to 2 blocks, so step 4 batches only a record's own body and cannot batch across the many records one 64 KiB request spans. Most of what remains per record (5 of 6 to 7 blk calls) is the tree walk, which step 2 then closes |

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

## Step 3, built and measured: a request carries 64 KiB (2026-08-19)

**Taken before step 2, deliberately**, because step 1's residual said to: a write's fixed term was
690 us per 4 KiB, 87% of the request, and step 2's read cache does not touch a write. A backup is
writes.

**8.02x on a sequential write and 5.67x on a read**, measured on milestone 38's harness over six
interleaved rounds at each transfer size (`sh bench/transfer-size-sweep.sh 6 1 16`), on a machine
whose `fs_read` control varied **0.3%** between the two points so no normalisation was needed.
`fs_seq_write` 5.33 MiB/s to **42.77**; `fs_seq_read` 14.16 to **80.30**; random write 4.34 to
**31.38**. notes/benchmarks.md has the table. The benchmark holds bytes moved constant at 1 MiB per
phase rather than the transfer count, so both points move the same file.

**The wire change is a constant, and that is the finding about the contract.**
`fs_proto::fs::TRANSFER_PAGES` is 16 instead of 1, so the region a client and the FS server share is
sixteen contiguous pages and a `READ` or `WRITE` may carry all of it. **Nothing in the packed request
word changed**: `fs::req` has carried a 40-bit length since milestone 32, so the packing was never
what limited a transfer to 4096 bytes. The page was. No new opcode, no second reply word, no
descriptor, no changed field, and the shape a reader of `fs_proto` meets is the one already there.

**A single-page client is untouched, and it is a property rather than a courtesy.** The serve loop
clamps twice, and which clamp a verb gets is the whole compatibility story: a length the **client**
chooses (`READ`, `WRITE`) may use the whole channel, and a length the **server** chooses (`READDIR`,
the attribute verbs, a rename's two names) stays inside the one page every client has always mapped.
A `READDIR` that filled 64 KiB would land in a single-page client's unmapped second page. `swish`,
the three caretakers, the sinks and the `std` PAL are unmodified and cannot tell this happened.

**What was refused, because milestone 138 authorises the step and not a shape.** A new opcode or a
`READV`-shaped scatter list: unnecessary, since the length field already fits, and a new concept on
this contract. A frame capability granted per request, which is the `mmap`-shaped answer: that is the
frontier this block names below, not a transfer size. A negotiated channel size at bind time: a new
verb, where every other agreement between a client and this wiring (the endpoint slot, the VA, the
role number) is already a compile-time constant both sides carry.

**The residual, and it changed owner rather than shrinking.** Step 1 reported that the fixed
per-request term became the whole problem. Step 3 inverts step 1's table exactly:

| 4 KiB sequential read | total | fixed term | block term |
|---|---|---|---|
| before step 3 | 275,860 ns | **204,076 (74%)** | 71,784 (26%) |
| after, per 64 KiB request | 778,354 ns | 204,076 (**26%**) | **574,278 (74%)** |

A read is now dominated by **sixteen single-block trips through `fs_proto::blk`**, which is step 4.
`fs_seq_read` measures 80.30 MiB/s against the ~100 MiB/s ceiling that contract imposes, so **step 4
stopped being a limitation recorded for later and became the binding constraint**;
notes/fs-server.md's `BUGS` entry says so and cites its own promotion trigger.

**The write residual moved, and by the most of anything in this milestone.** The 690 us transaction
is charged once per request, so it went from 690 us per 4 KiB to **43**. That is where the 8.02x
comes from, and it is what step 1 predicted a larger transfer would do.

**The two-term model was fitted a second time, from a different variable.** Varying the transfer size
gives a fixed term of **204,076 ns** and a per-block term of **35.9 us**, against step 1's 205,698
and 39.1 fitted by varying the record level on a different day. The fixed term agrees to **0.8%**,
and nothing was tuned to make it.

**Crash consistency was re-run and is unchanged**: 134 fault points, 13 commits, 0 silently wrong,
identical to step 1's. It is honest to say why it could not have changed: the injector drives
`Server` on the host, below the wire, and its workload already contained a 160 KiB
single-transaction write. What step 3 changed is that a **client** can now cause one; the
engine-level case was always covered.

**What step 2 is worth now, re-priced for the second time.** On a 64 KiB read the metadata cache is
worth about **1.33x**, not the 3.2x step 1 measured and not the block's modelled 4.7x, because the
five repeated block reads are ~195 us against a 778 us request instead of a 284 us one. On a 4 KiB
read it is still 3.2x. On writes it is still worth nothing. **Steps 2 and 3 target the same term**,
which the block's "multiplicative" note did not anticipate, so step 2's value is now a function of
which request size the workload uses: a third for milestone 55's backup, three times for a
small-file workload.

**What step 3 did not do, and it is one command.** The record level was not re-swept at 64 KiB. The
2026-08-18 sweep found that with a multi-page transfer levels 4 and 0 cost the **identical** 837 us
per 64 KiB, which predicts that step 1's 5.13x does not survive as a ratio at this transfer size.
`sh bench/record-level-sweep.sh 3 0 1 5` with `TRANSFER_PAGES` at 16 settles it, and until somebody
runs it the level shipped in step 1 is chosen on 4 KiB evidence for a system that no longer moves
4 KiB by default.

## Step 4, built and measured: the blk contract carries 16 blocks, and the win is smaller than the
## block count predicts (2026-08-19)

Step 3's own residual pointed here: after it, a read was 74% single-block trips through
`fs_proto::blk`, one per filesystem block, against the ~100 MiB/s ceiling notes/fs-server.md's
`BUGS` section had already named. `fs_proto::blk::TRANSFER_BLOCKS` goes from an unwritten 1 to 16
(the same number as `fs::TRANSFER_PAGES`, for the same reason): the region the FS server and the
block server share grows to 64 KiB of contiguous pages, `IpcDisk` batches contiguous whole-block
runs into one blk `CALL`, and the block server issues one virtio descriptor for the whole batch
instead of one per block. Nothing in the request word's packing changed, the same shape step 3's
change had: 56 bits below the opcode had never been used, and a block count fits in a handful of
them.

**10 interleaved rounds each** (`sh bench/blk-transfer-sweep.sh 10 1 16`), on a shared, noisy
machine (other lanes building and testing concurrently, `uptime` load 15 to 21 throughout, well
above earlier sweeps' 3.6 to 9). Median of 10 rounds, because 8 of 10 landed within 6% of the
established quiet `fs_read` baseline and the discipline is to discard load rather than average it
in, which with mostly-quiet rounds means the median already does that job:

| phase | 1 block/CALL | 16 blocks/CALL | speedup |
|---|---|---|---|
| `fs_seq_write` | 1,544,228 ns | **1,335,376** | **1.16x** |
| `fs_rand_write` | 2,101,313 | **1,539,488** | **1.37x** |
| `fs_seq_read` | 811,724 | **527,420** | **1.54x** |
| `fs_rand_read` | 848,706 | **546,541** | **1.55x** |
| `fs_record_read` | 843,448 | **545,040** | **1.55x** |

**Why this is nowhere near 16x, and it is a finding about steps 1 and 3, not a flaw here.** The
record level is 1 (step 1): an 8 KiB record, two blocks. RedoxFS walks its tree **once per
file-level `Server` call**, then loops over however many records that call's transfer spans, each
with its **own** `Disk::read_at`. A 64 KiB request (step 3) spans 8 records, so step 4 batches each
record's own body (2 blocks, one call instead of two) but cannot batch **across** records, because
each record's body is a separate call the engine issues on its own. Per record: 5 unbatchable
metadata reads + 1 data call (was 2); 8 x 6 = 48 calls per request, down from 8 x 7 = 56, a
1.17x call-count ratio that lines up with the measured write speedups almost exactly. Reads show a
larger ratio because a read's cost is almost entirely blk calls, so the same eight eliminated round
trips are a bigger fraction of it. **The majority of what remains, 5 of 6 to 7 calls per record, is
the tree walk**, which is exactly step 2's target and is why it is taken next rather than left for
later. notes/benchmarks.md has the full account, including the arithmetic that predicts the
absolute savings from the per-block cost this page already measured twice.

Crash consistency re-run: unchanged, 0 silently wrong, for the structural reason step 3's re-run
gave (the host-side model is below `IpcDisk`'s batching); the device-level crash injector keeps its
own one-block-per-`CALL` path unconditionally, so the real device test's coverage is unchanged by
this step rather than merely re-run.

## Step 2, built and measured: a 64-block cache over the tree walk, and it is the largest number in
## this milestone (2026-08-19)

`fs_server::CachedDisk` wraps `IpcDisk` in a small direct-mapped, write-through cache of
single-block reads, 64 slots (`CACHE_SLOTS`), about 257 KiB. Only a `buffer.len() == BLOCK` read
consults it; a record body (already batched by step 4) bypasses it. A write updates or invalidates
the written block's slot, only after the inner disk confirms the write landed. RedoxFS's
copy-on-write allocator never rewrites a live address in place, so the only way a cached address's
content can change is through that same write path, which is what makes a bare write-through cache
correct with no generation counter or fencing scheme. Six host tests cover hit/miss, write-through
freshness, short-write invalidation, slot-collision safety and multi-block bypass, in milliseconds,
no emulator.

**8 interleaved rounds each** (`sh bench/cache-slots-sweep.sh 8 1 64`), same machine, `uptime` load
15 to 19. Capacity 1 approximates "off" (the tree walk touches five *different* blocks per call, so
a one-slot cache thrashes and rarely survives to the next call) rather than being a true zero; both
`fs::TRANSFER_PAGES` and `blk::TRANSFER_BLOCKS` are at their shipped settings for both points, so
this isolates the cache's own marginal contribution:

| phase | 1 slot (~off) | 64 slots | speedup |
|---|---|---|---|
| `fs_read` (repeated inline read) | 210,490 ns | **9,474** | **22.2x** |
| `fs_seq_write` | 1,387,898 | **936,583** | **1.48x** |
| `fs_rand_write` | 1,487,736 | **1,087,904** | **1.37x** |
| `fs_seq_read` | 514,419 | **329,168** | **1.56x** |
| `fs_rand_read` | 542,605 | **331,732** | **1.64x** |
| `fs_record_read` | 560,088 | **341,504** | **1.64x** |

**`fs_read`'s 22.2x retires the "no cache anywhere" claim** milestone 38 demonstrated and both this
document and notes/fs-server.md stated as an architectural property: it was true of the build
measured then and it is not true of the build this tree ships now. `motd` lives inline in its node,
so its whole content rides on the tree walk with no separate record read, and a warm cache answers
it close to the bare IPC floor this document already estimated at ~13 us. See notes/fs-server.md's
own correction for what still holds (a different file's first access, or any file's first access in
a fresh session, stays fully uncached).

**Combined, all four steps against milestone 38's original baseline, measured head to head rather
than by multiplying separately-measured ratios**: `fs_seq_read` 1,509,270 ns per 4 KiB (2.68 MiB/s)
to 329,168 ns per 64 KiB (189.9 MiB/s), **70.9x**. The remaining gap to buffered Linux (7,141 MiB/s)
is the page-cache gap this milestone was never scoped to close; see "What is out of scope,
deliberately", below, and "The question underneath" for the frontier past it.

Crash consistency re-run: unchanged, 0 silently wrong. The host-side model is below `CachedDisk` too
(it drives `BlockDisk<Recording>` directly), and at the device level the property that matters is
argued rather than merely re-run: milestone 37's recovery mount is a fresh process, so it builds a
fresh, cold cache and can never observe anything the killed process's cache held.

**What was not measured.** The two caches (`blk::TRANSFER_BLOCKS` and `CACHE_SLOTS`) were swept
independently, each at the other's shipped value, not against each other; and 64 slots was sized
against the tree spine and the crash-test heap budget, not against a sweep of the capacity itself.
notes/benchmarks.md's `BUGS` entries for both steps name these as the first things to run next.

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
so far (a 128 KiB record, a page-sized file contract, a block-sized block contract, no metadata
cache) was an implementation choice Linux also had to solve, and all four are now closed steps. The
13 us is the first thing that is not, and it survives untouched by all four: it is the IPC round
trip and the server's own work, and no batching or caching of what travels over that round trip
removes the round trip itself.

**And the way past it, if it is ever worth taking, is capability-shaped**: stop doing a round trip
per request and grant the client frames it can read directly, which is what Linux's `mmap` over the
page cache is. Frames are already capabilities here. Nobody has designed it and it is not this
milestone; it is named so that the residual is understood as a frontier rather than a wall.

**Each of the four steps should report that residual**, so the question gets answered by
accumulation rather than by one argument at the end.

## What is out of scope, deliberately

**Superseded in part by step 2 (2026-08-19).** This section originally said "a cache is not this
milestone," written before calef's 2026-08-18 decision named the metadata cache as step 2 and before
step 4's measurement showed the tree walk was most of what remained. What is built is narrower than
what this paragraph warned against, and the distinction is the one worth keeping precise:

**A *data* cache is still not this milestone.** Nothing here caches a record body or gives a client
`mmap`-shaped access to bytes the server already fetched; every record read is still a round trip
(batched by step 4, never answered from memory), which is the property "The question underneath"
argues is the actual frontier past this milestone's four steps. That is the design with its own
coherency and confinement questions this paragraph warned about, and it remains somebody else's
milestone if it is ever wanted.

**A small, single-process *metadata* cache is step 2**, and it does not raise those questions: it
lives entirely inside one FS server's address space, one open file's tree spine is five blocks, the
cache holds 64, and RedoxFS's copy-on-write allocator is what makes a bare write-through cache
correct with no coherency protocol between processes to design. `fs_test_client`'s "warm read" claim
was wrong when milestone 38 corrected it and is right again now, for a different, stated reason: see
`fs_test_client.rs`'s own updated comment and notes/fs-server.md's correction.

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
- **This block asserted that RedoxFS is not the cause on a reading of the code rather than on a
  measurement, and that is now measured twice over.** The sweep ran on 2026-08-18 and the prediction
  held: a one-block record makes a 4 KiB read 5.6x faster and a write 3.0x faster
  (notes/benchmarks.md). The fixed term the sweep left over was then counted and is five repeated
  block reads, above. **The rest of this block still reads as though neither had happened**: the
  "measurement that should come before any of the three" section describes the sweep as unrun, and
  the "in brief" and comparison sections still quote milestone 38's 46.2 us per block, which the
  sweep corrected to a 39.0 us marginal cost plus a separate 208 us walk. Bringing the block into
  line with its own notes is work this lane identified and did not do.
- **`Transaction::write_node` compares before writing**, so rewriting a block with identical contents
  costs a read and no write. A benchmark that sends one constant page repeatedly measures the
  comparison rather than the store.
- **Step 1's record level was chosen on 4 KiB evidence and step 3 made 4 KiB the atypical request.**
  The 2026-08-18 sweep measured levels 4 and 0 at the identical 837 us per 64 KiB, so the level's
  effect at the shipped transfer size is plausibly nil and step 1's 5.13x is a ratio about a contract
  that no longer describes the default. Nothing here is wrong; the ranking of steps 1 and 2 was made
  against a transfer size that changed under them, and re-running the record sweep at
  `TRANSFER_PAGES = 16` is what would say so.
- **Nothing checks that a client asked for no more than it mapped** (step 3). The channel is 64 KiB
  and a client maps as much of it as it uses; one that asks for more gets a data abort on its own
  unmapped page, after the server has done the work. Rung four of `CLAUDE.md`'s ladder, marked as
  such where a reader meets the constant, and the way to rung one is the channel size on the wire,
  which is a new concept on this contract and was not taken.
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
- **Steps 2 and 4 were measured on a shared, noisy machine**, `uptime` load 15 to 21 throughout both
  sweeps, well above the 3.6 to 9 the earlier steps ran at. The `fs_read` control and the two-term
  model's internal agreement (a predicted ~296 us saving from step 4 matching a measured 284 to
  302 us across three independent phases) are the evidence these are real signals; they are not the
  single-tenant, quiet-machine conditions steps 1 and 3 had.
- **The two caches this milestone now has were never swept against each other.** `blk::TRANSFER_BLOCKS`
  (step 4) and `CACHE_SLOTS` (step 2) were each measured alone, at the other's shipped value.
  Whether a smaller blk batch with a larger metadata cache (or the reverse) reaches the same total
  for less DMA-region size or less heap is open; `bench/blk-transfer-sweep.sh` and
  `bench/cache-slots-sweep.sh` can both answer it, run together, and nobody has.
- **Neither `blk::TRANSFER_BLOCKS = 16` nor `CACHE_SLOTS = 64` was swept for where its own curve
  bends.** Both were chosen by symmetry or by sizing against a known working set (the file channel's
  own size; the tree spine's own size plus the crash-test heap budget), the same reasoning step 1's
  level and step 3's transfer size originally used before being swept properly. `sh
  bench/blk-transfer-sweep.sh N 1 2 4 8 16` and `sh bench/cache-slots-sweep.sh N 1 4 16 64 256` are
  the experiments and each is one command.
- **`CachedDisk`'s 64 slots were sized against a small test fixture and milestone 37's crash-test
  heap budget, not against a real deployment's node count.** notes/fs-server.md's own "same five
  blocks" finding says a 65,536-node filesystem's full tree spine is 259 blocks; 64 slots holds one
  open file's spine comfortably and thrashes once enough distinct files are open at once to collide
  across the tree's shared upper levels. Nobody has measured a multi-file workload against it.
