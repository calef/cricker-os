# 73. Name the aarch64 files aarch64, before x86_64 makes it worse

**Status: BUILT (2026-08-03), both ISAs**, for the five pairs. `crates/paging/src/aarch64.rs` is
**deferred to calef**, because the replacement is a name and names are his call; the proposal is in
the section below. Raised 2026-08-03 by calef, from the observation that files named when this was an
aarch64-only kernel never got renamed when RISC-V arrived and brought explicitly named twins.

**What landed**, and everything below this paragraph is the argument that produced it, kept as
written:

| was | is now |
|---|---|
| `kernel/link.ld`, `kernel/link-riscv.ld` | `kernel/link-aarch64.ld`, `kernel/link-riscv64.ld` |
| `bench/baseline.txt`, `bench/baseline-riscv.txt` | `bench/baseline-aarch64.txt`, `bench/baseline-riscv64.txt` |
| `scripts/qemu-runner.sh`, `scripts/qemu-runner-riscv.sh` | `scripts/qemu-runner-aarch64.sh`, `scripts/qemu-runner-riscv64.sh` |
| `crates/dtb/tests/fixtures/qemu-virt.dtb`, `qemu-virt-initrd.dtb` | `qemu-aarch64-virt.dtb`, `qemu-aarch64-virt-initrd.dtb` |
| `crates/dtb/tests/fixtures/qemu-riscv-virt.dtb`, `.dts` | `qemu-riscv64-virt.dtb`, `.dts` |
| `crates/dtb/tests/qemu_virt.rs`, `qemu_riscv_virt.rs` | `qemu_aarch64_virt.rs`, `qemu_riscv64_virt.rs` |

Two things the entry did not predict, both recorded because the next reader will meet them:

- **`qemu-virt-initrd.dtb` was a sixth file**, not in the table. It has no RISC-V twin, so the
  "suffix a file that has a named twin" rule had to be *checked* rather than applied, exactly as the
  `user/link.ld` paragraph below demands. The check says it is aarch64-only rather than shared (it is
  a `qemu-system-aarch64` dump), so it took the suffix. Leaving it would have produced
  `qemu-aarch64-virt.dtb` beside `qemu-virt-initrd.dtb`, which is the defect this milestone exists to
  remove, one directory deeper.
- **Both baseline files' first line was the literal `# bench/baseline.txt:`**, hardcoded in
  `xtask::run_bench`, so the RISC-V baseline has always claimed to be the aarch64 one. Renaming
  forced the issue: the header is now derived from the path being written, which fixes the RISC-V
  file too. This is the only edit in the milestone that is not a path following a rename.

**And an instruction that enumerated instead of stating its rule cost a round trip.** The first pass
was told "two spellings get fixed: `link-riscv.ld` and `qemu-riscv-virt.*`", followed it exactly, and
correctly left `baseline-riscv.txt` and `qemu-runner-riscv.sh` alone, which produced
`baseline-aarch64.txt` beside `baseline-riscv.txt`: a *new* inconsistency, created by the milestone
that exists to remove one. A list cannot be checked for completeness by the person applying it; a rule
can. The rule is in the section below, and it is the thing to carry forward.

**Inside `kernel/src/arch/` there is no problem**: the directory carries the ISA, so `aarch64/mmu.rs`
and `riscv64/mmu.rs` are both named. Everywhere else, only one side is.

## The five pairs

| RISC-V, named | aarch64, unnamed | references |
|---|---|---|
| `kernel/link-riscv.ld` | `kernel/link.ld` | 18 |
| `bench/baseline-riscv.txt` | `bench/baseline.txt` | 13 |
| `scripts/qemu-runner-riscv.sh` | `scripts/qemu-runner.sh` | 13 |
| `crates/dtb/tests/fixtures/qemu-riscv-virt.dtb`, `.dts` | `qemu-virt.dtb`, `.dts` | 4 |
| `crates/dtb/tests/qemu_riscv_virt.rs` | `crates/dtb/tests/qemu_virt.rs` | 2 |

The cost today is small and real: `scripts/qemu-runner.sh` reads as the runner and it is one of two, so
a reader looking for "the RISC-V one" finds it by suffix and then has to infer that the unsuffixed
file is the other ISA rather than something shared. **The cost after x86_64 is different in kind.** An
unnamed file among two named siblings is ambiguous; among three it is a claim that is actively false,
because "the default" will mean whichever ISA the reader started from. §19 names x86_64 as a declared
target, so this is a dated problem, not a hypothetical one.

## `crates/paging` left this milestone (2026-08-03)

It was raised here as an asymmetry: `aarch64.rs` beside `sv39.rs`, one an ISA and one a page-table
format. It is real, but it is not a rename, and milestone 77 carries it now. The short reason is that
calef expects a **second aarch64 configuration**, which turns "rename the file" into "make room for a
sibling on both sides", and that is a restructure with 174 call sites behind it.

Milestone 73 touched nothing under `crates/paging`, and one finding is worth carrying to 77: ARM has
no short format name the way RISC-V does. `Sv39` is a single token in the privileged spec and in
`satp`; the nearest ARM equivalent, "VMSAv8-64", names the architecture's memory system rather than
one translation scheme, and the configuration `aarch64.rs` actually implements (4 KiB granule,
48-bit VA) is expressed in `TCR_EL1` fields and never given a name of its own. That is the same
observation 77 starts from, arrived at independently.

## One that is not a pair

**Left alone by milestone 73**, deliberately: the decision below has not been made, and the name
depends on it.

`kernel/src/user/riscv_virtio_tests.rs` has no `virtio_tests.rs` twin; the shared virtio tests live
inside `tests.rs`. So this is a RISC-V-only test module rather than half of a pair, and the question
is whether the name should say "riscv-only test" or whether the aarch64 cases should be broken out to
match. Decide before renaming, because the answer changes the name.

## The scheme: suffix both sides (calef, 2026-08-03)

Every file in a pair carries its ISA as a hyphenated suffix. `kernel/link-aarch64.ld` beside
`kernel/link-riscv64.ld`, and so on for all five. Two alternatives were compared and lost, and both
reasons are worth keeping because they are about this tree rather than about taste.

**Naming by target triple was disqualified by a fact.** The obvious version, "match `targets/*.json`",
does not work: those are the **std overlay's** triples, for userspace. The kernel builds for
`aarch64-unknown-none-softfloat` and `riscv64imac-unknown-none-elf`, so `link-aarch64-unknown-cricker.ld`
would name a triple the kernel never compiles for, and naming it honestly gives
`link-riscv64imac-unknown-none-elf.ld`.

**A per-arch directory (`kernel/link/aarch64.ld`) was the close call.** It is the pattern rule 1 has
used since milestone 1, and `kernel/build.rs`'s own table shows why it is tempting, because the second
column already does it:

```rust
"aarch64" => ("link.ld",       "src/arch/aarch64/boot.s"),
"riscv64" => ("link-riscv.ld", "src/arch/riscv64/boot.s"),
```

It reads better for the linker scripts and worse for `scripts/`, where CLAUDE.md's convention is
hyphenated command names and `qemu-runner/aarch64.sh` stops looking like a thing you run. One rule for
all five pairs beats two rules split by file kind, which is the same argument that killed the two-tier
program-naming scheme: a convention with a branch is a convention someone gets wrong.

**The suffix is the ISA, so it is `riscv64` and never bare `riscv`.** `riscv` is the *family*;
`riscv64` is the thing the kernel compiles for, which every directory and target string in the tree
already agrees with (`kernel/src/arch/riscv64/`, `riscv64imac-unknown-none-elf`). Stated as a rule,
because it applies to every file this milestone touches rather than to a list someone has to keep
complete: **wherever a pair carries `-aarch64`, its twin carries `-riscv64`.** Four files were already
suffixed and all four were respelled (`link-riscv.ld`, `baseline-riscv.txt`, `qemu-runner-riscv.sh`,
`qemu-riscv-virt.*`).

**It does not reach a file with no twin.** `notes/riscv-port.md`, `notes/riscv-parity-scope.md`,
`notes/riscv-arch-tests.md` and `kernel/src/user/riscv_virtio_tests.rs` all name an ISA and none of
them is half of a pair, so none was touched. That is the same rule as the `user/link.ld` paragraph
below, in its other direction: suffix a file that has a named twin, not every file that mentions an
architecture.

A free win falls out: `kernel/src/user/tests.rs` globs `scripts/qemu-runner*.sh`, which matches both
files today only because one of them is unsuffixed. It becomes `qemu-runner-*.sh` and is exact.

## The fixture keeps hyphens and its test file takes underscores, and that is not a slip

At a glance the last row looks inconsistent: `qemu-riscv-virt.dts` becomes `qemu-riscv64-virt.dts`
while `qemu_riscv_virt.rs` becomes `qemu_riscv64_virt.rs`, two separators in one rename. **The tree is
18 for 18 on this split already**, so the milestone preserves a convention rather than introducing
one:

| kind | form | the files |
|---|---|---|
| fixtures and data files | hyphens, 8 of 8 | `qemu-riscv-virt.dtb`, `qemu-riscv-virt.dts`, `qemu-virt.dtb`, `qemu-virt-initrd.dtb`, `apple-64m.head`, `apple-64m.tail`, `sgdisk-64m.head`, `sgdisk-64m.tail` |
| Rust test files | `snake_case`, 10 of 10 | `hostile.rs`, `qemu_virt.rs`, `qemu_riscv_virt.rs`, `qemu_virt_dtb.rs`, `fuzz_seed.rs`, `real_disks.rs`, `mapping.rs`, `allocator.rs`, `table.rs`, `heap.rs` |

Two reasons, and both are better than "the table in CLAUDE.md says so".

**A `.rs` file's stem becomes a Cargo target name.** `cargo test --test qemu_riscv64_virt` is a name
you type at an identifier, so it takes Rust's convention the same way a crate or a module does.
Nothing in the tree ever types a fixture's name; the fixture is reached through `include_bytes!`.

**Device tree sources are hyphenated everywhere outside this repository.** Linux's
`arch/arm64/boot/dts/` is entirely `bcm2711-rpi-4-b.dts` in shape, and `dtc` users read that form
before they read ours. That is CLAUDE.md's own guard rail about form: a name whose shape a reader
already knows from outside costs them nothing, which is the same reason we do not respell
`supply-chain`.

This is the "one rule per domain, and each domain's own" table applied one level down, not a new
tier. The property it keys on is stable: a file either is a Cargo target or is bytes that a target
reads.

## DO NOT rename `user/link.ld`

There are three linker scripts, not two, and the third is a trap. `user/link.ld` is **genuinely
shared**: `user/build.rs` uses it unconditionally, with no `match` on the architecture, unlike
`kernel/build.rs` which selects per arch. Its lack of a suffix is CORRECT and means "shared", not
"aarch64 by default".

So the rule this milestone applies is not "suffix every unnamed file". It is **"suffix a file that has
a named twin"**, and a file with no twin has to be checked rather than assumed. A mechanical sweep for
`link.ld` renames `user/link.ld` and breaks both ISAs at once, which is also why the reference count of
18 in the table above overstates `kernel/link.ld`: some of those hits are the userspace script.

## Scope note

**Renames only. No behaviour, no content edits.** The proof obligation is the one milestone 69 met:
the tree must be byte-identical afterwards apart from the paths themselves. `link.ld` and
`qemu-runner.sh` have 18 and 13 referencing files, several of them build scripts and CI workflow
steps, so the risk is a missed reference that only fails on one ISA or only in CI. Grep for the bare
stem, not just the path.
