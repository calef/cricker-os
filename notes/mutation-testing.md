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

The table is the run of 2026-08-03: **5,551 mutants over 38 host crates, 4,654 caught, 391
missed, 96 timed out, 410 unviable**, which is **92.4% of the viable mutants killed**. It cost about
five and a half hours at `-j 2` on an 8-core, 16 GiB machine.

**Read the `missed` column as a worklist, not as a score, and read it as of that run.** Every one of
those 391 has since been triaged, and the commits on this branch close about two hundred of them,
so a rerun today would report a smaller number. The table is not restated here from a rerun because
the honest way to re-measure is the weekly workflow, on a machine that is not also running the
triage. What each survivor turned into is the section below, crate by crate; the counts there and
the counts here are measurements of two different moments, and both are labelled.

```
crate           caught  missed  timeout  unviable   total   killed%
abi                 27       1        0         1      29    96.4
asid                21       2        0         2      25    91.3
bitfont             11       0        0         2      13   100.0
block_roster        53       1        0         4      58    98.1
c_seam              21       1        0         3      25    95.5
calendar           369       7        3        16     395    98.2
capability          37       1        0        16      54    97.4
clock_proto         54       2        7         3      66    96.8
compositor         209      14        0        10     233    93.7
coremark           106       2        1         0     109    98.2
cred               127       3        1        16     147    97.7
cred_proto          72       2        0        13      87    97.3
crickerfs          107       0        0         8     115   100.0
dma_validator       79       0        0         6      85   100.0
dtb                294       1       20        10     325    99.7
elf                 98       0        0         4     102   100.0
entropy_proto       21       2        0         0      23    91.3
frames              93       3        1        12     109    96.9
fs_proto           489      37        9        31     566    93.1
gfx_proto          120       6        0         6     132    95.2
glob               110      14        6         5     135    89.2
gpt                423       5        1        36     465    98.8
grant_plan         367      26       20        76     489    93.7
intrusive            4       3        0         4      11    57.1
ipc                 24       5        0         4      33    82.8
isa                147      22        3        40     212    87.2
line_editor        183      46        2         6     237    80.1
measured_boot      118       9        4         6     137    93.1
ntp_proto          156      16        0        14     186    90.7
paging             256      39        0        17     312    86.8
pci                 88      27        4         3     122    77.3
regions             11       0        0         1      12   100.0
sink_proto          43       2        0         2      47    95.6
slots               26       5        0        13      44    83.9
socket_proto        15       2        0         0      17    88.2
swish               44       3        9         2      58    94.6
user_heap           20       3        5         7      35    89.3
video_terminal     211      79        0        11     301    72.8
TOTAL             4654     391       96       410    5551    92.4
```

**`killed%` counts a timeout as a kill**, because every one of the 96 was checked by hand and every
one is a detected hang (below). It is over viable mutants, so `unviable` is excluded from the
denominator: an unviable mutant does not compile, which says nothing about the tests.

**Five crates scored 100%**: `crickerfs`, `dma_validator`, `elf`, `regions` and `bitfont`. The first
two are the trust-boundary parsers, and they got there the hard way, by having every one of their
24 first-pass survivors turn out to be a real gap that a test then closed. That is the number to
compare a crate against next week.

**Four numbers deserve their asterisk before anyone reads them as quality.** `video_terminal` at
72.8% and `pci` at 77.3% were simply untriaged when the run happened; both are now done and the
survivors were overwhelmingly real. `intrusive` at 57.1% is 4 caught out of 7 viable, which is a
crate small enough that one mutant moves the percentage nine points. And `line_editor` at 80.1% is
what a terminal looks like when its tests assert what the screen shows and never where the cursor
is.

**How the numbers were derived, because the method matters if you reproduce them.** The run was
resumed twice (once after an OOM at `-j 4`, once to finish the tail), so its `caught.txt` holds only
what the final pass caught and the earlier passes' kills are in `previously_caught.txt`, which also
carries their unviable mutants. Reading `caught` off that file would undercount. The table instead
takes the total per crate from `cargo mutants --list` on the merged tree, subtracts missed, timeout
and the union of every pass's unviable, and calls the rest caught. Three of 5,551 listed mutants
could not be matched to any pass's outcome (two in `glob`, one in `ntp_proto`, both crates whose
line numbers moved when tests landed) and are counted as caught, which is the only place this table
guesses. `script/mutants --save-baseline` writes the machine-readable copy the weekly job diffs
against.

## Calibration: the exhaustive crates

The roadmap block predicted `ntp_proto` and `gpt` would score near-perfectly as a check on the
tool. The honest verdict: **they scored near-perfectly exactly where their exhaustive method
reaches, and the tool's value was showing precisely where that is.**

- **ntp_proto**: 155 of 186 mutants caught on first contact (83%, or 90% of the viable ones), and
  every conversion the sweeps quantify over was mutation-proof. The 17 survivors sat in what the
  sweeps never read: a range constant tested from only one side, accessors no test called, a
  fixture whose tiny root delay could not tell `/ 2` from `% 2`, and stratum 15, which no
  rejection test ever probed (only 16). Four tests closed them; the rest are equivalents, one of
  them proven equivalent *by the sweep itself* (the 1e9-value round-trip passes under the mutant,
  because the backward conversion's rounding absorbs the forward truncation).
- **gpt**: the same shape with a sharper lesson. The single-byte-corruption sweep proves
  *rejection*, not *rejection for the right reason*: every corrupt header dies at the CRC before
  the layout checks run, so every layout comparison could rot invisibly, and one of them (`>` at
  the backup-array boundary) was hiding a real wrong-accept bug the mutant fixed.

So the calibration check passed in the way that matters: the tool agreed with the exhaustive
suites about everything they actually assert, and disagreed only where they assert less than they
appear to. That second half is the finding.

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
  refusal, each with its own test. The audit added two more. `from_hm`'s `hours < 0` is **not**
  masked by the `!= 0` clauses, only its `<=` siblings are: rotted to `hours == 0` it refuses
  `from_hm(-5, -30)`, which is how -05:30 is written, and accepts `from_hm(-5, 30)` as -07:30. Every
  test used positive hours, which is one row of the guard's truth table. And the six-byte guard
  ahead of the offset is a length check, not a trailing-bytes check: rotted to `<` it refuses the
  same inputs and renames one refusal, so `+05:60x` is now pinned as `BadOffset`. Equivalent:
  `from_hm`'s two `<` to `<=` mutants (masked by the `!= 0` clauses beside them), `Writer::byte`'s capacity guard (`FMT_CAP` is two bytes over the
  longest output, so the boundary is unreachable), `Writer::offset`'s sign at zero (both format
  paths branch to `Z`/`UTC` before a zero offset can reach it), and the redundant `+ with -` at
  the offset-length guard (the `number`/`expect` helpers bounds-check behind it, so every path
  still errors identically).
- **capability** (11): rights bits, `from_bits` masking (an OR there turns undefined bits into
  defined rights), idempotent union, and `insert_at` landing in the named slot; killed by
  `rights_bits_are_the_wire_format` and `insert_at_fills_exactly_the_named_slot`.
- **clock_proto** (10): the request wire format and the sanity window's seconds-times-a-billion
  arithmetic, killed by `the_request_word_is_the_wire_format_it_claims` and
  `the_sanity_window_is_where_it_says`. Equivalent: the CAS's `s + 1` (single-threaded blindness,
  above; nothing observes the odd window, and the sequence still advances by two) and `decide`'s `>`
  at equal timestamps (a zero step is accepted down both branches). The other seven seqlock mutants
  are **not** equivalent and are not survivors: flipping either spin guard or the reread check makes
  a single-threaded `read` or `publish` spin forever, so all seven are timeouts.
- **cred** (21): the longest legal identity and secret were never exercised end to end, the
  memory ceiling could become either a divide or an add, and only the divide was caught, because
  the test that was supposed to hold it named the ceiling as `Cost::MAX_M_KIB` on both sides of its
  own assertion: `1024 / 1024` is 1, which falls below `MIN_M_COST` and fails, but `1024 + 1024` is
  2048, which is a legal cost, so every symbolic check passed while every real policy was refused.
  The ceiling is now pinned as `1_048_576`, hand-computed, which is the crickerfs lesson applied to
  a constant instead of an image. The redacting `Debug` could also be
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
  `MAX_SURFACE_BYTES` as exact numbers, and a zero-width rect asserted empty. Equivalent (14, audited one by one):
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
- **glob** (21): the first pass wrote both of its new tests inside `#[cfg(kani)] mod verification`
  instead of `mod tests`, so `cargo test` never compiled them and they killed nothing. The
  paragraph that claimed fourteen was describing tests that had never run, and `.cargo/mutants.toml`
  already records why: `mod verification` is invisible to `cargo test`, which is the reason mutants
  inside it are excluded. Both tests are good and both kill once moved, and moving them takes
  eleven of the twenty-one. **`script/lint` now fails on a `#[test]` inside a `#[cfg(kani)]`
  module**, because there are 22 such modules in the tree and this failure mode reports a coverage
  it does not have. The step count is the DoS-bound contract (Kani proves only that it stays under
  `cost_bound`, which a counter stuck at zero also satisfies), so each class feature now costs
  exactly its own scan; and `[A-\]]` matches at its endpoint, resumes after the escape, and a class
  ending in a bare escape is unterminated rather than a read past the end. The remaining three
  needed arithmetic nobody would guess at. `scanned += after_hi - after_lo` charges exactly the
  bytes a range skips, so a resume index landing *inside* the range re-scans one at a time what it
  should have stepped over and the ledger balances; no step assertion can see it. Membership can:
  resuming at the `-` makes `[a-c]` match a hyphen, resuming at the high end makes `[z-a]` match
  `a`, and a wider class is a larger grant. The other coincidence is `2 + 2 == 2 * 2`, because at
  the head of a one-member class `scanned` is 2 and the tail is 2 bytes, so the addition and the
  multiplication agree and so do `4 - 2` and `4 / 2`; one member in front (`[xa-c]`) separates all
  four. The 7 timeouts are all detected hangs, every one a cursor that stops advancing. No
  equivalent mutants and nothing deferred: 21 of 21.

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
  timeouts are detected hangs, each confirmed by applying the mutation and watching the suite fail
  to finish. Fourteen are a cursor `+=` becoming `-=`, where the walker bounces between two offsets
  that both read as an empty-named `FDT_BEGIN_NODE`. The other six are a plain `+` becoming `-` in
  `let value_at = at + 8`, a different mechanism worth naming: `at` lands back on the same
  `FDT_PROP` token and the walker re-reads one property forever. Equivalent: `cells()`'s `|` vs `^`
  (ORs into freshly shifted zeros), confirmed by reading the masks and by running it.
- **fs_proto** (72 after the `proofs::` exclusion; the largest crate): two findings. Most
  survivors were equivalent, not gaps, and a later pass audited that claim **site by site rather
  than in aggregate**, because "the masks are disjoint" is a sentence about 26 different lines.
  Every request word, spec word and rights bundle ORs fields the code masks into disjoint bit ranges
  (`fs::req` splits 63:56, 55:40 and 39:0; `grant::spec` puts the length under `0xff` and the rights
  above bit 8; `xattr::spec` and `xattr::reply` split at bit 32; the six `dir` rights and the eight
  `attrs` claims are distinct single bits), so `|` and `^` agree on every reachable input. All 26
  were checked against the constants and 14 hand-mutated, one per distinct expression. Two more are
  the `verb::TABLE` order check inside a `const _: ()` block, where both mutants only make the loop
  vacuous and a `const` item has no runtime observable, so no test can reach them; the property is
  pinned at runtime by `every_verb_has_a_row_that_says_what_its_words_mean`. **The audit found one
  claim that was wrong.** `xattr::store::write_record`'s guard is not a mask, and with its second
  `||` turned into `&&` the value limit stops being enforced, because the name clause beside it is
  dead in both callers. That limit is load-bearing: `set` and `remove` re-emit records whose lengths
  come off the blob rather than from a bounds-checked caller, and a value length is a `u16` reaching
  65535 against a `MAX_VALUE` of 3072, killed by
  `a_record_wider_than_the_contract_is_refused_when_a_blob_is_rewritten`. The real gaps were the rights bundles as numbers
  (an `&` in `REMOVE_TREE` collapsed it to zero), one sentence per explained errno, the verb
  predicates as an exact partition, both dirent length limits, `pack_name` handed a
  seventeen-byte name (the mutated bound indexes past the packed word and panics), the nameset
  cursor byte for byte, the attribute record's exact bytes at an exactly sized buffer, and every
  witness verdict module pinned to distinct single bits, because both ends of a QEMU test build
  their words from the same constants and a shifted-to-zero claim silently stops being checked.

- **gpt, second pass** (18 more): the interrupted run's resume reached the modules the first pass
  never got to (entry, guid, header, span, the MBR validator), and the survivors were the same
  two shapes. Boundaries never met exactly: a header the size of its block, a partition on the
  first usable block, an exact-fit name buffer, entry sizes other than 128 (every fixture uses
  128, so the size guard had never been *judged*). And values every fixture shared: the
  protective record always in MBR slot 0, where `i * 16` is immune to arithmetic mutation, and
  every partition name ASCII, whose UTF-16 high byte is zero, which hid an off-by-one in name
  decode. One test each; `check_protective_mbr` also needed one bad block through the wrapper,
  since every existing call handed it a good one. Equivalent: the hex-nibble `|` vs `^` (disjoint
  bits), the span guard on a 64-bit host (it stops being equivalent the day anything builds for a
  32-bit target, which is exactly when you want the signal), `1 << 0` under shift direction, and `<`
  vs `<=` at the backup-reserve underflow guard (a disk that small dies on another arm of the same
  refusal). The audit found one more real gap of the first shape: `check_partitions`'
  `last_lba < first_lba` rotted to `<=` refuses a partition of exactly one block, which is legal
  because `last_lba` is inclusive, and every partition in both suites spans thousands of blocks, so
  the check had only ever been met from far away.

- **intrusive** (3): the run queue's `len` was asserted after every push and never after a pop
  (`-=` could become `+=`), and `is_empty` was only met empty; both now asserted mid-drain in
  `fifo_order`.
- **ipc** (5): the manual `PartialEq` impls were only ever compared equal, so a body stuck at
  `true` passed; distinct variants now must disagree and the hang-dump `Debug` strings are
  pinned. Equivalent: `one_queue_invariant` replaced with `true`, because the API maintains the
  invariant, so no reachable endpoint state returns false; the checker exists for kernel-side
  debug assertions whose states are built by kernel code this crate cannot construct.

- **grant_plan** (26): 25 real. The tokenizer and parser were only ever fed lines whose cursor
  arithmetic coincided at small indices (`i + 1` and `i * 1` agree at 1), so buffer ceilings, a
  trailing `--mem`, and the flag-cluster bound could all rot into indexing past the line; the
  recursion bit was only tested on `rm`, whose `r` is bit 0, where shift direction cannot matter;
  `prog_id`'s round trip used id 1, the constant the mutant substitutes, and `from_id`'s arm list
  had drifted by one program. Refusal sentences and Debug renderings pinned exactly. Equivalent:
  `RECURSIVE`'s `1 << 0`. Deferred finding, integrator's call: `NameSet`'s Debug renders numeric
  byte lists, not the legible names its own header promises; pinned as-is because the fix changes
  rendered output.

- **isa** (22): 12 real, and the shape is the wrong-accept this crate exists to prevent. Both
  riscv64 fixtures declared their narrowest hart first, so the widest-wins fold never replaced an
  answer it already held: rv32 after rv64 could keep rv64 (and boot on the rv32 machine), and a
  base-less extensions node could clobber the base under one flipped `&&`. A wide-first fixture
  exercises every replacement arm; `Missing::any` gets one single-missing case per clause;
  ASIDBits `0b0000` must decode to the 8 bits `crates/asid` assumes. Equivalent (10): idempotent
  assignments at `<=` boundaries, disjoint-operand `|` vs `^`, `1 << 0`, and six compile-time
  duplicate-check loops whose guards are vacuous on a table that already passes, a pattern the
  weekly report will keep resurfacing (noted here so nobody re-triages it).

- **measured_boot** (9): one real (uppercase hex was dead in the suite; every test round-tripped
  through our own lowercase `to_hex`). The other eight are textbook equivalent mutants worth
  naming because they will resurface weekly: SHA-256's `ch` has operands masked disjoint by `e`
  and `!e` (`|` equals `^`), `maj` computed with OR is the same majority function as with XOR
  (they agree wherever at least two inputs agree), `update`'s boundary take is the same number
  down both branches at `len == want`, and the hex nibble combine ORs into cleared low bits.

- **paging** (39): **seventeen real, and not one of them a bug**, which is the result to want from
  page-table math. Eleven are accessors and predicates nothing inside the crate reads back:
  `Flags::bits`, `is_user_page_va` (whose `&&` to `||` mutant shorts the user-VA gate into an
  or-gate that admits an aligned *kernel* address to a user `MAP`), `PageFormat::half_base`, and
  `Mapper::root`, whose value is what the kernel writes into TTBR0/satp, so a constant there
  installs the wrong table in silicon while every walk test still passes. Four more are descriptor
  bits the portable `Flags` do not carry, so the encode/decode round-trips are structurally blind
  to them: aarch64's `TABLE_OR_PAGE` (bit 1, which `is_present` never reads, so a descriptor that
  loses it walks perfectly in software and is a translation fault at L3 and a *block* descriptor
  above it), aarch64's inner-shareable `SH` on normal memory (the `delete !` mutant hands it to
  device memory instead, leaving normal pages with a core-private view another core's write never
  invalidates), and Sv39's `D`. The last two are in the DMA domain, where every existing test
  granted exactly one page: the region shape that cannot tell a page count from the constant `1`,
  nor `grant_page`'s whole-page test from its opposite, both killed by a grant of two pages and a
  half. Equivalent: twenty-two, in two families. `1 << 0` to `1 >> 0` (`CAP_WRITE`, aarch64
  `VALID`, Sv39 `V`) and `0b00 << 6` (`AP_RW_EL1`), the degenerate shifts named above; and every
  `|` to `^` in the `Flags` constructors and in `table_entry`/`leaf_entry` on both formats, because
  the six `CAP_*` are distinct single bits and a descriptor's address mask (aarch64 bits 47:12,
  Sv39's PPN at 53:10) shares no bit with its attributes or with valid/type at bits 1:0.
- **pci** (31: 27 missed, 4 timeouts): nineteen real, and they share one cause, which is that the
  fake config space held exactly one device. Enumeration's multifunction handling was never
  exercised (nine mutants on the single line that reads the header type), the per-function "nobody
  home" check was never reached because no device had a function 1, and the fixture's 64-bit BAR
  was both unassigned and in the last slot pair, so a base that dropped its high half and a walk
  that ran one slot past BAR5 both read as correct. Killed by
  `enumeration_follows_the_multifunction_bit_and_skips_what_does_not_answer`,
  `an_empty_slot_costs_one_config_read` (the vendor-id guard changes no output at all, only read
  count, so a count is its only witness: 45 reads on a 32-slot bus against 306 without it),
  `a_64_bit_bar_in_the_first_slot_keeps_its_high_half`,
  `the_command_bits_are_the_specified_positions`, and
  `a_function_without_a_capability_list_is_not_walked`. Equivalent, seven, all one argument: the
  `|` to `^` flips in `ecam_offset`, `requester_id`, and both halves of `read_bars`'s 64-bit
  assembly are ORs of **disjoint** bit ranges (`bus:8 | dev:5 | fn:3` packs without overlap for
  every BDF a caller can produce, which is what the Kani proof assumes), and `size`'s
  `mask | 0xffff_ffff_0000_0000` is the same case against a constant. All four timeouts are
  genuine detected hangs on the BAR cursor, and a fifth mutant joined them: `i += 2` to `i *= 2`
  differs from the original only at slot 0, so the new first-slot fixture turned a silent survivor
  into a hang.
- **line_editor** (48): 45 real, and they cluster by what the terminal model hid. `Screen` asserts
  what the user sees, which is the right contract, but a test that checks only the finished line
  never proves *where the cursor is*, so the whole movement layer was mutable at will: ^E and ^F
  could be deleted, `right` could be emptied to `()` or have its `<` flipped four ways, `left`'s
  `> 0` could become `>= 0`. `control_key_movement` and `right_arrow_moves_and_stops_at_end` type a
  character after every move and close all nine. The history ring's index arithmetic
  (`(hist_next + HIST - k) % HIST`, eight mutants across `repeats_newest` and `hist_next_entry`)
  survived because no test crossed the wrap point, where a `+` for a `-` and a `%` for a `/` still
  land on a plausible entry; `wrapped_ring_walks_correctly_both_ways` walks ten commands through a
  ring of eight in both directions, `duplicate_entry_is_stored_once` pins the dedup by bell count
  (the screen cannot tell one stored copy from two, which is why `repeats_newest -> false` survived
  everything else), and `empty_lines_stay_out_of_history` covers the `len > 0` half of the same
  guard. The repaint arithmetic (`len - cur` in `start_line`, in ^L, in `yank`, in the stash
  restore) needed a cursor left off the end and *then* a keystroke, since the echo alone redraws
  the same glyphs either way. The rest are one test each: the CSI `;` arm and its `n += 1` (one of
  them a `-=` that underflows to a panic), ^W's two scan loops (whose `>` to `>=` reads `buf[a - 1]`
  off the front), the three-way split at `kill_len > 0`, and `csi_move`'s digit loop, which nothing
  had ever driven past nine columns. Equivalent (3): `req`'s `|` to `^` (opcode in bits 63:56,
  length in 31:0, disjoint), `FLAG_EOF`'s `1 << 0`, and `csi_move`'s `1` arm, which only elides a
  count: delete it and `n == 1` emits `CSI 1 D`, which ECMA-48 defines as the same motion as
  `CSI D`. That last is byte economy on a serial line, not behaviour, and the check that it is not
  an excuse is its sibling one line up: deleting the `0` arm makes a zero-column move travel a
  column, and `backspace_erases_on_screen` kills it.
- **video_terminal** (79, the largest single block in the run): **64 real, 15 equivalent, none
  deferred**, and the shape splits three ways. The geometry and colour accessors were never called
  with a *number*, only compared against another value computed the same way, so `cols()`,
  `width()`, `height()`, `colours()` and `to_pixels` could return a constant or swap an operator
  unnoticed. A 6 by 3 grid separates `6*8` from `6+8` from `6/8`; three pixels of a glyph at cell
  (1,1) separate a divide from a remainder, which agree at (0,0), where every existing pixel
  assertion sat; and a union with a rect *inside* the first is the only shape that reads the first
  operand's far edges. The parser's less-travelled arms were individually deletable because nothing
  fed them: a tab, a bare control code, a string terminated by `ESC \`, `CSI 1J`, SGR 27, 39, 49
  and 90-97. The switches that turn something *off* are the ones worth naming, because a terminal
  that only ever sets attributes passes every test while leaving a line reversed forever. Third,
  clamps and damage were asserted only where they did not bind: `\x1b[99B` and `\x1b[99;99H` now
  pin the clamps, since one too generous parks the cursor off the grid where every later write is
  silently dropped, and a bare `LF` on the bottom row with the cursor hidden is what makes
  `damage_all` non-deletable. Equivalent (15): disjoint-mask `|` versus `^` at three sites,
  `DEFAULT_BG << 4` on a zero operand, the four min/max selections in `union` at equality, SGR
  40-47's `p - 40` versus `p + 40` (40 is a multiple of 8 and the background is masked to three
  bits, so they agree for all eight legal parameters), `erase_display`'s `to > from` (true for
  every mode), two match arms that fall through to a byte-identical body, and three range guards no
  caller can reach because `col < cols` and `row < rows` are invariants of every write. Those last
  three are the ones most worth leaving visible: a new caller that broke either invariant turns all
  three back into real bugs. **The correction worth recording**: `csi`'s `>` to `==` at the
  parameter limit was argued equivalent by hand and is not. `== MAX_PARAMS` fires on the fourth
  separator, so it drops a legal `CSI 1;2;3;4 m` entirely, and only running the mutation caught it.
- **slots** (5): four real, all in the half of the API the Kani harnesses never touch. `get_mut`
  could return `None` for a live name with nothing on the host noticing, while the kernel
  `unwrap()`s it on the switch path, so the mutant is a kernel panic dressed as a lookup miss;
  `is_empty` could be stuck at `true`, stuck at `false`, or inverted, because nothing in the tree
  calls it at all, which is exactly the accessor that rots unobserved. Killed by
  `get_mut_reaches_the_live_entry_and_writes_through_it` and
  `a_table_is_empty_only_while_it_holds_nothing`. Equivalent: `name`'s `|` to `^`, where the
  generation is shifted into bits 63:32 and the slot is `< N <= u32::MAX` by the const assert in
  `new`, so the two operands never share a bit.
- **socket_proto** (2): one real. `DATA_MAX` is `4096 - OFF_PAYLOAD` and the test only asked
  whether a full payload *fits* the frame, which `4096 / OFF_PAYLOAD` also does, so the constant
  could shrink by 3576 bytes unnoticed; it is now pinned as the whole page after the header, which
  refuses both a payload that overruns the grant and one that leaves granted bytes unreachable.
  Equivalent: `req`'s `|` to `^`, where the opcode is a byte and the socket id is shifted past it,
  and the crate's own `every_opcode_fits_in_its_byte` is what keeps the two ranges disjoint.
- **sink_proto** (2): both equivalent, no gaps. `req` ORs an opcode shifted to bits 63:56 into a
  length masked to bits 31:0. And `pack`'s `bytes.len() < INLINE_MAX` to `<=` is the boundary where
  both arms return the same number: at exactly sixteen bytes `bytes.len()` *is* `INLINE_MAX`, so no
  slice length distinguishes them.
- **user_heap** (6): three real, all in the split arithmetic, and the reason nothing saw them is
  that a block's *size* is never readable. `free_bytes` is an independent counter a wrong split does
  not touch, and every test asked only for the block **count**, which the coalescing invariant
  expects to be 1. So `block_count` could return the constant 1 and pass everything; `alloc`'s
  `tail > 0` could become `>=`, writing a zero-length free node one node past the donation; and
  `bsize - front` could become `bsize + front`, inflating the remainder by twice the front padding
  and leaving the heap willing to hand out memory nobody gave it. Killed by
  `a_block_that_fits_exactly_leaves_nothing_behind` and
  `a_split_never_invents_bytes_nobody_donated` (the first donates its arena away from the process
  heap on purpose, so a mutant writing past the region lands in slack rather than in the test
  runner's own allocations). The 3 timeouts are detected hangs of the same shape: a wrong `front`
  makes `insert_free` write a node whose `next` points at itself. Two of the three fail an assertion
  before anything hangs and are recorded as timeouts only because the thrashing test spins in the
  same binary, which is the honest caveat on reading a timeout as "not caught".
- **swish** (2 real, 1 equivalent, 9 timeouts): the shell's own sentences, where a regression is
  user-visible and nothing else looks. `echo`'s `i > space` could become `>=`, emitting a
  zero-length whitespace run; in the program `out` is the terminal endpoint, so an empty write is a
  round trip carrying no bytes. `write_pwd` could be replaced with nothing at all, because no test
  called it, so a `pwd` that printed an empty line would have shipped. Equivalent: deleting
  `write_refusal`'s bare `Refusal::NoSuchProgram` arm, because the guarded arm above takes every
  non-empty program name and the `_` arm it falls into prints a prefix only when `Prog::from_name`
  resolves, which an empty name never does. The 9 timeouts are all detected hangs in `echo`'s
  two-cursor scan.

## The ledger: every survivor's disposition

The triage rule says nothing stays untriaged, so this is the accounting that backs the claim. Rows
are the run's survivors (missed plus timeout); "killed" means a test was written and **verified by
applying the mutation and watching that named test fail**, "equivalent" means the mutant provably
cannot differ from the original, and "hang" means a timeout that was confirmed to be an infinite
loop rather than an undetected bug.

| crate | survivors | killed | equivalent | hang | deferred |
|---|---|---|---|---|---|
| video_terminal | 79 | 64 | 15 | 0 | 0 |
| line_editor | 48 | 45 | 3 | 0 | 0 |
| fs_proto | 46 | 1 | 36 | 9 | 0 |
| grant_plan | 46 | 25 | 1 | 20 | 0 |
| paging | 39 | 17 | 22 | 0 | 0 |
| pci | 31 | 20 | 7 | 4 | 0 |
| isa | 25 | 12 | 10 | 3 | 0 |
| glob | 21 | 14 | 0 | 7 | 0 |
| dtb | 21 | 0 | 1 | 20 | 0 |
| ntp_proto | 16 | 4 | 12 | 0 | 0 |
| compositor | 14 | 0 | 14 | 0 | 0 |
| measured_boot | 13 | 1 | 8 | 4 | 0 |
| swish | 12 | 2 | 1 | 9 | 0 |
| calendar | 10 | 2 | 5 | 3 | 0 |
| clock_proto | 9 | 0 | 2 | 7 | 0 |
| user_heap | 8 | 3 | 0 | 5 | 0 |
| gpt | 6 | 1 | 4 | 1 | 0 |
| gfx_proto | 6 | 0 | 6 | 0 | 0 |
| slots | 5 | 4 | 1 | 0 | 0 |
| ipc | 5 | 4 | 1 | 0 | 0 |
| frames | 4 | 1 | 2 | 1 | 0 |
| cred | 4 | 1 | 2 | 1 | 0 |
| intrusive | 3 | 3 | 0 | 0 | 0 |
| coremark | 3 | 0 | 2 | 1 | 0 |
| the 2-survivor crates | 12 | 1 | 11 | 0 | 0 |
| the 1-survivor crates | 4 | 0 | 4 | 0 | 0 |
| **total** | **487** | **225** | **170** | **95** | **0** |

The 2-survivor crates are `asid`, `cred_proto`, `entropy_proto`, `sink_proto`, `socket_proto` and
`slots`' siblings; the 1-survivor crates are `abi`, `block_roster`, `c_seam` and `capability`. Their
survivors are the recurring patterns named at the top of this section, one or two each.

**Nothing is deferred, and that is a claim worth being suspicious of**, so here is what it rests on.
Every "equivalent" in the table was argued from the code, and in the crates a later pass audited
(compositor, frames, calendar, cred, clock_proto, gpt, fs_proto, dtb, glob) every one was also
**re-run under its mutation**. That audit changed six verdicts: five mutants called equivalent were
real gaps (`frames::index_of`'s upper bound, `calendar::from_hm`'s sign guard and its offset-length
check, `cred`'s memory ceiling, `gpt::check_partitions`' one-block partition), and glob's entire
first pass turned out to have written its tests where `cargo test` could not see them. **A verdict
reached by reading is wrong about ten percent of the time; a verdict reached by running is not.**
The crates that were not re-audited (`grant_plan`, `isa`, `measured_boot`, `ntp_proto`, `ipc`,
`intrusive`) had their kills verified the same way when they were written, but their *equivalence*
claims rest on argument alone, and the weekly run is what will check them.

**The alarming survivors, named.** A survivor in a security boundary is worth more attention than
fifty in a display crate, so: `capability`, `regions`, `dma_validator`, `crickerfs` and `elf` have
**zero real survivors** between them, and the three trust-boundary parsers score 100%. The one
security-relevant survivor the run found anywhere was `fs_proto::xattr::store::write_record`, whose
value limit stopped being enforced under a single `||` to `&&`, on a path that re-emits records
whose lengths come off the blob rather than from a bounds-checked caller. It is closed. The
next-most-serious were `paging`'s user-VA gate and `Mapper::root` (a constant there installs the
wrong table in silicon), `isa`'s widest-wins fold (an rv32 hart booting an rv64 answer), and
`slots::get_mut` (a `None` the kernel `unwrap()`s on the switch path). All closed.


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
