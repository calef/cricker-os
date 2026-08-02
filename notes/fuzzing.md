# Fuzzing the parse surface

Milestone 42's second leg. The first two (advisories and licences, vendored integrity) ask whether
the code we did not write is what we think it is; this one asks whether the code we *did* write
survives bytes it did not write.

`script/fuzz` runs four coverage-guided fuzz targets over the parsers that read data from outside
this system. It found two bugs on its first day, and a third came out of reading the code while
designing a target. All three are fixed and pinned by host tests.

## What fuzzing finds that Kani does not

This project has **107 Kani harnesses across 19 crates**, and DECISIONS §14 says the proofs are the
thesis. Adding a fuzzer only earns its place if it answers a question the proofs cannot, so that is
the first thing to settle.

**Kani is exhaustive inside a bound.** `kani::any()` is every value at once, the program becomes a
formula, and `UNSATISFIABLE` means no input breaks the property. That is a stronger claim than any
fuzzer can make, for the inputs it covers. Its limit is structural rather than incidental: a solver
reasons completely about *fixed-size* things, so an unbounded loop over an arbitrarily long input has
to be unrolled to a limit, and past a certain unrolling the formula stops being solvable in useful
time. notes/verification.md records those bounds and their justifications.

**Fuzzing is unbounded and random.** It proves nothing. What it does is execute the real code on
millions of concrete inputs an hour, with coverage feedback steering toward inputs that reach new
branches, and it does not care how many loop iterations that takes.

So the two tools divide on one line, and the line is visible in this tree rather than theoretical:

| | Kani | Fuzzing |
|---|---|---|
| Coverage | every input, inside a bound | some inputs, no bound |
| Answers | "is this property true" | "does anything crash" |
| Loops | must be unrolled | free |
| Cost of a wrong bound | a silent hole | none |
| Verdict on success | a proof | "not yet" |

**The place where they meet is the bound**, and this tree has three worked examples of the bound
being the whole story:

1. **`crates/elf`.** The crate's own doc comment says the whole-parse totality proof *did not
   return*: an O(n^2) overlap loop over up to 64 program headers, unrolled 64 deep against symbolic
   slice offsets, is past what the solver can do. What is proved is `check_segment_bounds`, the leaf
   arithmetic, factored out so bounded model checking can reach it. The proved leaf and the unproved
   shell are exactly the split a fuzzer covers from the other side.
2. **`crates/dtb`.** Four harnesses, all on `be32`/`be64`, the leaf readers. The seven *walkers*
   above them are unbounded loops over a symbolic blob carrying a depth counter and two 16-entry
   per-depth arrays. Both of the bugs found on 2026-08-02 were in that unproved region, and one of
   them was an out-of-bounds index into one of those arrays.
3. **`crates/crickerfs`.** `Fs::parse` is proved total, with no bound on the image size at all
   (`the_validation_implies_reads_slice_is_in_bounds`). Fuzzing it for panics would be burning CI
   time to rediscover a theorem. What is *not* proved, and cannot be by a harness of that shape, is
   that the writer and the reader agree, and that is where its bug was.

**The honest summary: a proof says the property holds where it looked, and a fuzzer looks where the
proof could not.** Neither subsumes the other, and the third case above shows the boundary is not
even always "bounded versus unbounded": sometimes it is that nobody wrote the property down.

## Which parsers, and why those

**The rule that picked them: does this code read bytes from outside the trust boundary?** A parser
whose input this system produced is a correctness question. A parser whose input somebody else
produced is a security question, and it is the one where a panic has a consequence a user notices.

Four targets, deliberately fewer than one per crate. A fuzz target that finds nothing because it
fuzzes a total function is worse than no target: it costs CI time on every commit and it implies
coverage the project does not have.

| Target | Input comes from | Why it earns a target |
|---|---|---|
| `dtb_walk` | firmware (QEMU, OpenSBI, a board's ROM) | parsed **before anything else exists**, on both ISAs, from a pointer in a register; a panic here is a kernel that cannot boot and cannot say why. Kani reaches the leaf readers and not the walkers. |
| `elf_parse` | any binary a user asks to run | the only parser that **loads what it parses**: its output becomes page-table entries. The whole-parse totality proof is recorded as intractable. |
| `gpt_table` | a disk somebody else formatted | decides which LBA range is a filesystem. Heavily checked already, which is the point: the gap is *combinations* of hostile fields, which neither the proofs nor the single-byte mutation tests can build. |
| `crickerfs_roundtrip` | (structured) the writer's own output | not a panic hunt. Asserts that **what goes in comes out**, which is the property `write_image`'s truncation bug violated in 2026-08-01 and its NUL bug violated until 2026-08-02. |

### And the ones deliberately not fuzzed

Naming these matters as much as naming the four, because "we have fuzzers" is the kind of claim that
quietly grows to cover everything.

- **`crickerfs`'s `Fs::parse` on raw bytes.** Kani proves it total with no size bound. A fuzz target
  there would be a fuzz target that cannot find anything.
- **`fs_proto`, `sink_proto`, `socket_proto`.** These *are* a trust boundary (a malicious client on
  the other end of an endpoint), and they are unproved. But their decoders are written with
  `slice::get` throughout and are total by construction, and the real hazard at that boundary is not
  a byte string at all: it is the **double fetch**, where a server validates a length out of a
  shared page and then reads the page again, and the client changed it in between. A byte-string
  fuzzer cannot express a concurrent mutation. That is milestone 43's lens, and it needs a different
  tool.
- **`video_terminal`.** 63 escape sequences, no proofs, and a byte stream as input, so it looks like
  a good candidate. It is a candidate, and it is not in this first set because its input comes from a
  program *this system started*, one rung inside the boundary the other four sit on. Worth adding
  when the terminal serves output from something we did not build.
- **`grant_plan`.** Its parser was being rewritten in a concurrent lane while this was built.

## Why cargo-fuzz

DECISIONS §46 says taking a dependency is a decision, and that holds for a dev tool. Three candidates
were real: cargo-fuzz (libFuzzer), cargo-afl, and honggfuzz-rs.

**cargo-fuzz wins on one property the other two do not have here.** It is in-process and
coverage-guided with no external harness process, so a target is a plain Rust function over `&[u8]`
that the crates under test link into directly. Our parsers are `no_std`, pure, and take a byte slice,
which is the exact shape libFuzzer wants. AFL's fork-server model buys robustness against a target
that corrupts its own address space, which safe-Rust parsers cannot do, and pays for it in exec rate.
cargo-fuzz is also what rustc, `regex`, and most of the ecosystem use, so a crash reproduces for a
stranger reading this repository.

The costs, stated rather than glossed: it needs nightly (already pinned), it needs `rust-src` for
`-Zbuild-std` (already listed for the kernel), and it adds three crates (`libfuzzer-sys`,
`arbitrary`, `cc`) to a graph nothing ships. It also brought the licence exception below.

**The one licence decision.** `libfuzzer-sys` is `(MIT OR Apache-2.0) AND NCSA`, because it vendors
LLVM's libFuzzer C++ source. `deny.toml` carries it as a `[[licenses.exceptions]]` scoped to that one
crate rather than as an allow-list entry, so a second NCSA crate would still stop the build. NCSA is
OSI approved and FSF free and reads as BSD-3-Clause and MIT stapled together, and nothing that boots
links it.

## The three bugs

### 1. `dtb::Dtb::node_reg` indexed past its cell stack (out-of-bounds panic)

`node_reg` carries `#address-cells`/`#size-cells` down a **16-entry per-depth array**, and matched a
node at any depth `>= 2`. When the matched node's `reg` property arrived, it read `acells[depth - 1]`
with no upper guard, so a device tree nested **17 levels deep** with a matching name at the bottom
indexed `acells[16]` on a 16-element array. That is a panic in the parser the kernel runs against a
blob it did not write, before there is any way to report a failure.

Fixed by refusing to match past the stack, `(2..MAX_DEPTH).contains(&depth)`, which is what the
sibling `node_reg_compatible` already did. Clamping the index would have been worse: past `MAX_DEPTH`
the walk has stopped tracking cell counts, so the region it decoded would be arithmetic on the wrong
widths. Not finding the node is honest; finding it and reporting the wrong address is not.

**Found by reading the code while writing the fuzz target. Ten minutes of fuzzing did not find it**,
and that is recorded here rather than smoothed over. See BUGS below.

### 2. `dtb::Region::end` overflowed on a hostile memory map

`end()` was `self.start + self.size` on two `u64`s straight out of the blob.
`kernel/src/memory.rs`'s `place_bitmap` calls it on every RAM region the device tree declares, and
the kernel's own test builds are dev-profile builds with overflow checks on, so a `/memory` node
claiming `start = size = u64::MAX` is a panic on the boot path. In a release build it wraps instead,
which is worse: the frame allocator gets a memory map that is quietly wrong.

Fixed twice over, at the source and at the type. `decode_reg` now refuses a wrapping pair outright
(`Error::RegionOverflow`), because a boot path should be told its memory map is impossible rather
than work around it. And `end()` saturates, because `Region` is `pub` with `pub` fields and a type
anyone can construct has to hold on its own. That second argument is not new here;
`elf::Segment::page_range` already carries it in a comment.

**Found by the fuzzer**, in about ten minutes from a cold corpus, starting from a mutated copy of the
committed `qemu-virt.dtb`. Nobody had thought to look at `end()`.

### 3. `crickerfs::write_image` accepted a name it could not store

A name containing a NUL byte was written into the entry's NUL-padded name field, where every reader
stops at the first NUL. So `"a\0b"` was written and read back as `"a"`, and `read("a\0b")` answered
`None`: data written, not readable, nothing panicking, no test noticing. Two names agreeing up to
their first NUL become one entry, which is exactly the collision the `NameTooLong` refusal was
introduced to stop on 2026-08-01. That fix missed this case because nobody thought to write a name
with a NUL in it.

Fixed with `Error::NameHasNul`, the same shape as `NameTooLong`.

**Found by `crickerfs_roundtrip` in under a minute**, minimal input `[("\0", [])]`. No totality proof
could have found it, because nothing panicked: it is a *property* violation, and the property had
never been written down.

## The CI job, and its budget

`script/fuzz --time 60`, all four targets, in **a job of its own** on its own runner. Two decisions
worth defending.

**Why a time budget at all.** Fuzzing has no completion condition. A proof discharges or it does not;
a fuzzer runs until you stop it. So it cannot be a step inside a gate somebody waits on, because the
only knob is how long the wait is, and every second of it is paid by every commit forever. `ci.yml`
already has `cpu-matrix` as precedent for "its own runner, not a slower gate".

**Why sixty seconds.** Measured rather than felt. On the developer machine (Apple Silicon, and busy
with another lane's QEMU at the time), `dtb_walk` sustained **19,470 executions per second** over a
ten-minute run and 25,886 over a quiet one-minute run. So a minute is 1.2 to 1.5 million inputs per
target, and four to six million per CI run, from a seed corpus that is already past every magic-number
check. CI's runner is a different machine and will differ; the order of magnitude is what the budget
is chosen against.

**What the job is for, so it is not mistaken for something else: it is a regression tripwire, not a
search.** A minute per target from the committed seeds re-covers ground the parsers are already known
to survive and notices when a change breaks it. Finding something genuinely new is a long run on a
developer's machine, and that is deliberately not automated. A nightly job that finds a crash at 4am
with no persisted corpus produces an artifact nobody can reproduce and an alert nobody dispositions,
which is DECISIONS §35's wallpaper failure.

**The corpus is not cached between CI runs, on purpose.** Caching it would make a run's coverage
depend on which runs came before it, so a red run could not be reproduced from the repository alone.
Every run starts from exactly the committed seeds, which is the same starting point a developer gets.

## Corpus discipline

Three kinds of input, and they are kept apart because they have different reasons to exist.

**Seeds are committed, and they are read-only.** A fuzzer starting from `[]` spends its first minutes
rediscovering that a device tree begins `d0 0d fe ed`. With a sixty-second budget it would never get
past the magic check. But `fuzz/seeds/` holds exactly one file, because the seeds this project needs
**already exist in the tree**: `crates/dtb/tests/fixtures/` holds three real device trees dumped from
the boards we boot, and `crates/gpt/tests/fixtures/` holds two real disks formatted by `sgdisk` and
by Apple's Disk Utility. `script/fuzz` passes those directories to libFuzzer as extra corpus
arguments. Copying them under `fuzz/` would create a second copy that can drift from the first.

The exception is `fuzz/seeds/elf_parse/minimal_rx.elf`, 120 bytes, because nothing else in the tree is
a small ELF (our real binaries are over a megabyte). It is hand-assembled, and
[fuzz/seeds/README.md](../fuzz/seeds/README.md) carries the generator that produces it.

**A seed that stops parsing is not a seed, and nothing about a fuzz run would say so.** The target
returns immediately on anything the parser rejects, so a corpus of rejected inputs reports the same
"no crashes" a working one does. `crates/elf/tests/fuzz_seed.rs` holds that seed to actually parsing,
and to carrying this build's `e_machine`, which is a compile-time constant and would be wrong on a
riscv64 developer machine. The other three targets seed from fixtures that already have tests.

**Dictionaries are committed** (`fuzz/dictionaries/*.dict`), and they are the cheap half of a
grammar. A device tree's structure block is a stream of 32-bit tokens; a fuzzer that has to discover
by bit-flipping that `\x00\x00\x00\x01` opens a node spends its whole budget in the first three
branches. `crates/gpt`'s dictionary deliberately does *not* try to help with the CRC-32, which is
what the seeds are for.

**The working corpus is not committed** (`fuzz/corpus/`, gitignored). libFuzzer writes every input
that reaches a new edge, so minutes leave thousands of files and hours leave hundreds of megabytes.
It is machine-generated, machine-specific, and unbounded.

**Crash artifacts are not committed either**, and that is the discipline rather than an omission.
When a target finds a crash, **the input becomes a host test in the crate that owns the bug**, where
it runs in milliseconds on every `script/test` forever and where a reader meets it next to the code.
`crates/dtb/tests/hostile.rs` and `crickerfs`'s `a_name_with_a_nul_in_it_is_refused` are the two
written this way. A hand-built 200-byte blob with a docstring explaining what it attacks is worth
more than a 7,642-byte artifact named after its SHA-1.

## EXAMPLES

**Run everything, the way CI does:**

```sh
script/fuzz                     # four targets, 60 seconds each
```

**See what the targets are:**

```sh
script/fuzz --list
```

**Hunt properly, on a machine you are not using:**

```sh
script/fuzz --time 3600 dtb_walk        # one hour on one target
script/fuzz --time 0 elf_parse          # until it finds something, or you press ^C
```

**Reproduce a crash.** libFuzzer writes the failing input to `fuzz/artifacts/<target>/` and prints
the command. Re-running the target with a *file* argument replays that one input instead of fuzzing:

```sh
cargo fuzz run dtb_walk fuzz/artifacts/dtb_walk/crash-b93bbc15...
```

**See what the input actually was**, which matters for a structured target like
`crickerfs_roundtrip` where the bytes are not the data:

```sh
cargo fuzz fmt crickerfs_roundtrip fuzz/artifacts/crickerfs_roundtrip/crash-0ad4fab2...
# [
#     (
#         "\0",
#         [],
#     ),
# ]
```

**Shrink it before writing the test:**

```sh
cargo fuzz tmin dtb_walk fuzz/artifacts/dtb_walk/crash-b93bbc15...
```

**Then throw the artifact away and write the test.** That is the last step and it is the one that
matters: the artifact is a fact about one run, and the test is the thing that will still be checking
in a year. Delete `fuzz/artifacts/<target>/` when you are done.

**Prune a corpus that has grown** (fuzzing eats disk; a long run's corpus is not small):

```sh
cargo fuzz cmin dtb_walk         # keep the smallest input per edge
rm -rf fuzz/corpus fuzz/artifacts
```

## BUGS

**Ten minutes of fuzzing did not find the `node_reg` bug, and a reader did.** This is the most
important limitation on this page, because it is the one that contradicts what a fuzzer is assumed to
do. Reaching that panic needs a device tree **seventeen levels deep** whose *only* prefix match is at
the bottom, and the seed corpus is real device trees, which are three deep. A mutational fuzzer
explores outward from what it has; deep recursive structure is what it is worst at synthesizing. A
grammar-based generator (`arbitrary`-derived structure that emits well-formed trees) would reach it,
and is the obvious next step for `dtb_walk`.

**A green fuzz run means "not yet", never "correct".** Sixty seconds of no crashes is evidence about
sixty seconds. Nothing here is a proof, and nothing here should be quoted as one; the proofs are in
notes/verification.md and they say what they cover.

**Only panics and hangs are caught, plus whatever a target asserts.** A parser that returns the
*wrong answer* without panicking is invisible to `dtb_walk`, `elf_parse` and `gpt_table`, because
those three assert nothing beyond "it returned". `crickerfs_roundtrip` is the one target with a real
property, and it is the one that found a silent-corruption bug. That asymmetry is a hint about where
the next targets should go, not a fact about fuzzing.

**The needles in `dtb_walk` are a fixed list.** The kernel's real lookups (`intc`, `plic`, `pl031`,
`virtio_mmio`) plus the empty prefix. A `pub fn` that panics for some *other* prefix would not be
found. Letting the fuzzer choose the needle would mean structuring the input, which would stop the
committed `.dtb` fixtures from working as seeds; that trade is worth revisiting with a grammar.

**`node_prop` matches at any depth and `node_reg` now stops at 16.** The two lookups disagree about
how deep a device tree can be, which is a behavioural wart rather than a bug: `node_prop` keeps no
per-depth state, so it has nothing to overflow. Nothing in the tree nests past 4.

**Sanitizers are on and buy very little here.** The default is AddressSanitizer, and the four crates
under test contain no `unsafe`. It costs throughput and catches nothing these targets can produce;
it stays on because it costs nothing to leave a default alone, and because the first `unsafe` to
appear in one of these crates would be the one that needs it. The checks actually doing the work are
`debug-assertions` and `overflow-checks`, set in `fuzz/Cargo.toml`'s release profile, which is what
turned bug 2 from a silent wrap into a crash.

**The `crickerfs_roundtrip` target filters its own inputs**, skipping name sets the writer refuses.
That keeps the budget on the accepted region, and it means the target cannot notice if a *rejection*
regresses. The crate's host tests cover that instead.

**One block size.** `gpt_table` fixes 512 bytes because `Gpt::parse` takes the block size from the
header block's length and the fixtures are 512-byte disks. 4K-native disks exist and take a different
path through `array_blocks`.

**`gpt_table` barely reaches `check_backup`.** The input is one contiguous disk prefix, so the
harness passes the *primary* header and array as the backup, which fails on the first field
comparison (`my_lba != alternate_lba`) almost every time. Everything past that comparison, which is
seven more fields and a second `check_entry_array`, is effectively untested by this target. Reaching
it wants the input split into a head and a tail, which the committed `.head`/`.tail` fixture pair is
already shaped for and which would cost those fixtures their status as directly-usable seeds. Worth
doing; not done.

## See also

- notes/verification.md, the proofs and their bounds. Read it first; this note is its complement.
- notes/scripts.md, where `script/fuzz` sits in the front door.
- notes/device-tree.md, notes/elf.md, notes/gpt.md, notes/crickerfs.md for what each parser is for.
- `deny.toml` and `script/supply-chain`, milestone 42's first two legs.
