# Mutation testing: the baseline and the triage rule

Milestone 85. `script/mutants` runs [cargo-mutants](https://mutants.rs/) over the host crates:
rewrite one function at a time (replace a return value, delete a match arm, flip an operator),
rerun the mutated package's tests, and record whether anything noticed. Coverage answers "did this
line run under a test"; a mutation run answers "would any test notice if this line were wrong",
which is the property a test suite exists for.

The tool is pinned in `.cargo-mutants-version` (the `.cargo-deny-version` discipline, and with an
extra tooth here: cargo-mutants changes which mutants it *generates* between versions, so an
unpinned tool moves the weekly numbers with nothing in the tree having changed). Exclusions live in
`.cargo/mutants.toml`, each with its reason; config, not a code dependency, per DECISIONS §46. The
weekly `mutation testing` workflow reruns the same command four-way sharded and publishes the
per-crate table against `.cargo/mutants-baseline.txt`. A report, not a gate, until the weekly
numbers prove stable enough that a new survivor deserves to fail something.

## The triage rule

Every survivor becomes exactly one of three things, and nothing stays untriaged:

1. **A test worth writing.** The mutant found a property no test asserts; assert it. This is the
   product working as intended.
2. **A recorded exclusion.** The function cannot be meaningfully tested on the host, or the mutant
   is semantically equivalent to the original. It goes in `.cargo/mutants.toml` with the reason
   next to it; an exclusion without a reason is a hole, not a decision.
3. **An honest deferral, recorded here.** A real gap that a test could close but whose test is not
   worth its cost yet. Named in this note's table so the weekly report's number has a ledger behind
   it, and nothing is silently accepted.

## The baseline

The machine-readable copy (what the weekly report diffs against) is `.cargo/mutants-baseline.txt`,
written by `script/mutants --save-baseline`; the table below is the same numbers with the story
attached. A **timeout** here is almost always a detected hang, not an undetected bug: the classic
case is `+=` to `-=` on a walker's cursor, which loops forever and trips cargo-mutants'
auto-timeout. It is still listed per crate because a timeout that is *not* a hang would be
invisible inside a merged "caught" number.

PLACEHOLDER-BASELINE

## Calibration: the exhaustive crates

PLACEHOLDER-CALIBRATION

## Survivors and where each one went

Grouped by crate; every survivor from the baseline run is accounted for in one of the three
buckets. "Killed by" names the test written for it.

### Patterns that recur, named once

- **Bit-constant survivors** (`1 << n` becoming `1 >> n`): every crate that defines a wire format
  as shifted constants had them, and the fix is one test pinning the exact values. `1 << 0`
  mutated to `1 >> 0` is the degenerate case: both are 1, so that one mutant is *equivalent* and
  is recorded as such wherever it appears (abi 238, c_seam 145, capability 63 had the killable
  siblings).
- **Boundary survivors** (`>` becoming `>=` at a length or range check): the tests exercised a
  short value and an over-long value but never the exact limit, so the limit itself was never
  proven legal. The fix is a test at the boundary, and in one case (gpt, below) the mutant was
  not a missing test but a missing `=`.
- **debug_assert shielding**: a belt-and-braces release-mode guard sitting *behind* a
  `debug_assert!` of the same condition is unobservable under `cargo test`, which runs with debug
  assertions on: every input that would reach the guard's differing behaviour panics first, in
  both the original and the mutant. Those mutants are recorded equivalent-under-harness, not
  excluded in config, so they stay visible if the assertions ever move.
- **Single-threaded blindness**: mutants in seqlock/atomic orderings (clock_proto's `publish`)
  change nothing a single-threaded test can observe. Concurrency claims are argued in the code's
  comments and, where they are pure, proved; a unit test cannot carry them.

### gpt: one real bug, fourteen missing tests

The `>` to `>=` survivor at `Gpt::parse`'s backup boundary was a genuine wrong-accept:
`block_count - backup_reserved` is the backup array's first block (it is exactly what
`backup_entry_lba` computes), and equality put one usable block inside the backup entry array,
where a partition would overwrite it. **The mutant was the fix**; parse now refuses equality, and
the boundary has a test on both sides. No test could see the difference before because
`real_disks.rs` corrupts one byte at a time, so every corrupt header dies at the CRC before the
layout checks run, and every table `create` makes is tight on both boundaries. The other fourteen
gpt survivors were killed by forging CRC-valid headers with one layout lie each (`table.rs`).

This is also the honest asterisk on the exhaustive-suite calibration: an exhaustive sweep proves
*rejection*, not *rejection for the right reason*. A mutant that weakens check B survives any
input that also trips check A.

### The hand-triaged crates

- **abi** (5): rights bits and the fault slot, killed by `rights_are_distinct_single_bits` and
  `the_fault_slot_is_inside_the_cspace`; `1 << 0` equivalent as above.
- **asid** (2): both in `free`'s release-mode range guard, equivalent-under-harness
  (debug_assert shielding, above).
- **block_roster** (3): the header-only page, killed by
  `a_header_only_page_is_an_empty_roster_not_a_short_one`; `capacity_of`'s `<` to `<=` is
  equivalent (`len == HEADER_BYTES` yields capacity 0 down both arms).
- **c_seam** (5): verdict bits, killed by `the_verdict_bits_are_distinct_single_bits`; the
  `1 << 0` sibling equivalent.
- **calendar** (18): two real parser edges (a fraction scan that could read one past the end;
  an offset-colon check whose index could rot to `i - 3`, which lands on the seconds colon in
  every well-formed input, so `+05300` parsed), killed by `parser_edges_the_mutation_run_found`;
  the absolute weekday, unix zero without `-0`, the three `Formatted` impls, and one message per
  refusal, each with its own test. Equivalent: `from_hm`'s sign-comparison mutants (masked by the
  `!= 0` clauses beside them), `Writer::byte`'s capacity guard (`FMT_CAP` is two bytes over the
  longest output, so the boundary is unreachable), `Writer::offset`'s sign at zero (both format
  paths branch to `Z`/`UTC` before a zero offset can reach it), and the redundant `+ with -` at
  the offset-length guard (the `number`/`expect` helpers bounds-check behind it, so every path
  still errors identically).
- **capability** (11): rights bits, `from_bits` masking (an OR there turns undefined bits into
  defined rights), idempotent union, and `insert_at` landing in the named slot; killed by
  `rights_bits_are_the_wire_format` and `insert_at_fills_exactly_the_named_slot`.
- **clock_proto** (10): the request wire format and the sanity window's seconds-times-a-billion
  arithmetic, killed by `the_request_word_is_the_wire_format_it_claims` and
  `the_sanity_window_is_where_it_says`. Equivalent: the seqlock CAS mutants (single-threaded
  blindness, above) and `decide`'s `>` at equal timestamps (a zero step is accepted down both
  branches).
- **cred** (21): the longest legal identity and secret were never exercised end to end, the
  memory ceiling's `1024 * 1024` could become a divide, and the redacting `Debug` could be
  replaced with one that prints nothing; killed by `the_longest_identity_and_secret_are_legal`,
  `the_cost_is_what_it_says_up_to_the_real_ceiling`, `a_store_is_empty_until_it_is_not`, and
  `debug_prints_the_redaction_and_nothing_secret`. Equivalent: `MAX_P_COST`'s exact value (any
  `p` large enough to notice it already fails the `m_kib < p * 8` check first).

- **cred_proto** (6 after the `proofs::` exclusion): the request word pinned as one exact number
  with opcode `SEAL` (every prior test used opcode 1, so an `op` returning the constant 1
  passed), the smallest page the layout fits accepted at both ends, and `wipe`'s bound asserted
  from both sides. Equivalent: the two `|` to `^` mutants in `req` (the three fields are masked
  into disjoint bit ranges, and `x | y == x ^ y` whenever `x & y == 0`).
- **coremark** (2): both equivalent by arithmetic. The list tie-break's `>` to `>=` shifts an
  equal u16 past an equal u16, which is bytewise identity, so the published-CRC pin cannot see
  it; the fsm counter tops out at 256, so bits 16 and up are zero down both shift directions.

- **compositor** (66, the largest cluster): the pattern generators mixed bits no test compared
  against a known answer, so every `&` could become `|` and every shift could reverse. Killed by
  five tests: two hand-computed pixels per generator at coordinates whose bit patterns
  distinguish the operators, the surface checksum pinned to an independently computed FNV-1a
  value plus a read count, the window digest cross-checked row-major, stride and
  `MAX_SURFACE_BYTES` as exact numbers, and a zero-width rect asserted empty. Equivalent (13):
  min/max selections at equal operands, `|` vs `^` over disjoint masked bit fields, `intersect`'s
  early return (the arithmetic path returns EMPTY anyway), and a max-accumulate's `>` at equality.

- **crickerfs** (12) and **dma_validator** (12): all 24 were real gaps, none equivalent, which
  fits both crates' role as trust-boundary parsers. Two recurring causes: layout constants with
  no independent pin (every test compared an image against the constant it was built from, so
  both sides moved together; the documented values are now hand-computed in the tests), and
  boundaries never hit exactly (a file ending exactly at the image end now round-trips; one byte
  under is truncated). dma_validator's ring tests had all used batch indices where `slot * 2`,
  `slot + 2`, `idx % 8` and `idx / 8` coincide; a batch starting at index 11 separates all four,
  with poisoned slots to catch a walk landing anywhere but the declared next. Six of the subtlest
  kills were verified by applying the mutation by hand and watching the test fail.

- **frames** (5): three real (a stuck `Some(true)` from `is_used`, `index_of` refusing the base
  frame, a zero-size `mark_used` rounding up to one frame), killed by
  `the_base_frame_and_the_empty_range_are_exact`. Equivalent: the alloc hint (an optimization; a
  scan starting on the just-used frame finds the same next free one) and `alloc_contiguous`'s
  early return (no run of zero or of more than `total` ever matches, so the scan reproduces it).

- **elf** (10): all real, none equivalent. Every fixture set PF_R, so `is_readable`'s mask could
  become any operator and the validator's execute-only branch was dead in the suite; an
  execute-only segment now asserts both sides, the header-table bounds get their exact edges, and
  `u16le` is pinned on bytes whose halves differ (every field in the old fixtures had a zero high
  byte, so reading the wrong neighbour byte read the same).
- **entropy_proto** (3): `op` pinned with a non-GET opcode (`GET` is 1, so a body replaced by the
  constant 1 passed every round trip). Equivalent: `|` vs `^` over disjoint masked operands, and
  `want`'s `>` at `n == MAX_BYTES`, where both branches return the same 8.

- **gfx_proto** (22): the test pattern's channel math had no pinned pixel, so a wrong buffer
  could only be wrong the same way on both sides; five hand-computed pixels, a one-bit-change
  digest test (an FNV whose xor became or collides exactly where it matters), and the errno's
  minus sign. Equivalent (7): OR-vs-XOR in `req` and the `rect` packing, where every field is
  masked into disjoint bits.
- **glob** (14): the step count is the DoS-bound contract (Kani proves the bound, nothing pinned
  a count), so each class feature now costs exactly its own scan, asserted as differences between
  near-identical matches; and the escape-inside-a-range parse had no test at all, so `[A-\]]`
  now matches at its endpoint, resumes correctly after the escape, and a class ending in a bare
  escape is unterminated rather than a read past the end.

- **dtb** (52, second largest): one recurring cause. Every test tree declared the fixtures' 2/2
  cell layout, so the `#address-cells` match arms could be deleted, inheritance could stop at the
  default, and a `reg` could decode with its own node's widths instead of its parent's, all
  invisibly. Closed in two passes, nineteen `hostile.rs` tests in all: the first eleven covered
  the 1-cell layout, inheritance, parent-vs-own decode, the compatible walker's root guard, prop
  lookup, a reservation block with entries (including one at address zero, which an `&&` rotted
  to `||` reads as the terminator), initrd's widths and exclusive end, and the header comparisons
  met exactly. **A second pass audited the first against the survivor list and found thirteen not
  actually killed**, most of them the `reserved_memory_regions` cluster nothing had ever called
  off the fixtures; eight more tests closed those, three verified by applying the mutation and
  watching the one test fail. The audit is the honest caveat on hand-triage: reasoning about
  which test kills which mutant is fallible, and the weekly rerun is the check on it. The 20
  timeouts are detected hangs (cursor `+=` becoming `-=` in walkers). Equivalent: `cells()`'s `|`
  vs `^` (ORs into freshly shifted zeros).
- **fs_proto** (72 after the `proofs::` exclusion; the largest crate): two findings. Most
  survivors were equivalent, not gaps: every request word, spec word, and rights bundle ORs
  fields masked into disjoint bit ranges, where `|` and `^` cannot differ; the note's rule is to
  record the masks, not to test the untestable. The real gaps were the rights bundles as numbers
  (an `&` in `REMOVE_TREE` collapsed it to zero), one sentence per explained errno, the verb
  predicates as an exact partition, both dirent length limits, `pack_name` handed a
  seventeen-byte name (the mutated bound indexes past the packed word and panics), the nameset
  cursor byte for byte, the attribute record's exact bytes at an exactly sized buffer, and every
  witness verdict module pinned to distinct single bits, because both ends of a QEMU test build
  their words from the same constants and a shifted-to-zero claim silently stops being checked.

PLACEHOLDER-TRIAGE-DELEGATED

## Scope and honest caveats

- **Scope is the main workspace's host crates.** The exclusions (and their reasons) are in
  `.cargo/mutants.toml`: the bare-metal crates cannot compile for the host, `supervision_proto`,
  `swap_proto` and `virtio` compile but cannot execute a line without a kernel underneath, and
  `xtask` is the build system, whose tests are the gates it runs.
- **`fs_server` and `tools/redoxfs_host` are not mutated.** Each is its own workspace (kept out of
  ours so upstream RedoxFS never meets our clippy/fmt gates), and cargo-mutants works one workspace
  at a time. `fs_server`'s pure logic is small and host-tested, but a run there mutates against a
  suite whose heavy half lives under QEMU (`script/test`'s redoxfs leg), so its score would
  overstate the gap. Deferred, on the record, not forgotten.
- **A survivor count is not a quality score across crates.** Crates differ in how much of their
  surface is host-assertable; compare a crate to its own last week, not to its neighbours.
- **Timeouts are auto-derived** by cargo-mutants from each package's baseline build and test time,
  so a mutant that makes a loop spin forever is recorded as `timeout`, not hung. The baseline's
  timeouts were checked and are detected hangs (cursor arithmetic in walkers), which is the tests
  noticing, not missing; a timeout on a mutant that could NOT hang would be triaged as a survivor.
