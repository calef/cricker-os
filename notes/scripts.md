# The `script/` entry points

Every command you need to work on this repo lives in `script/`, one short file each, with the
same names GitHub's [Scripts to Rule Them All](https://github.com/github/scripts-to-rule-them-all)
pattern uses. The whole idea is muscle memory: clone any repo that follows the pattern, run
`script/setup`, then `script/test`, and you are working. You do not have to learn that this one
uses `cargo xtask` and that one uses `make` and the next uses `npm`.

## The commands

| script | what it does |
|---|---|
| `script/bootstrap` | Install every dependency: the pinned Rust toolchain (via rustup, from `rust-toolchain.toml`) and QEMU. Idempotent: it checks first and installs only what is missing. |
| `script/setup` | First run after a clone: `bootstrap`, then build. |
| `script/update` | After pulling new code: `bootstrap` (the pinned toolchain can change), then rebuild. |
| `script/decisions` | Index `design/decisions/`; `--check` enforces the numbering, the status vocabulary, that each decision's Status line agrees with its index row, and that every `§N` cited anywhere in the tree resolves. Gated in `script/lint`. |
| `script/test` | Host-logic crates, then the kernel under QEMU on **both** ISAs. The gate. |
| `script/verify` | The machine-checked proofs (Kani) over the pure-logic crates. Not in `bootstrap`: Kani pulls its own toolchain and a CBMC backend, so it is installed only where it is used. |
| `script/bench` | icount microbenchmarks; `--check` fails on >10% drift from `bench/baseline-aarch64.txt`, `--save` rewrites it, `--real` runs under HVF for magnitudes. |
| `script/roadmap` | Index the milestones; `--check` validates the status vocabulary and catches a block with no row, or a milestone cited in prose the table does not carry. Gated in `lint`. |
| `script/citations` | The third citation gate, and the only one that reads the target: a `§N (gloss)` or `milestone N (gloss)` must match that record's own title or quote its body, and an attributed block quote must still exist in the file it names. The other two prove a citation resolves to *some* entry; this proves it resolves to the right one. `--check` gates it in `lint`. See notes/citations.md. |
| `script/catch-up [<since>]` | What changed since you last looked: milestone status transitions, milestones newly minted, decisions landed or revised, what is waiting on Chris, what is ready to start, and the notes that carry the why. `<since>` is a date (`2026-08-01`) or any git rev; the default is seven days, which is a trip. A **derived view**, never a maintained one, for the reason `notes/session-handoff.md` demonstrates: a hand-written "current state" document rots, and this one is recomputed from the roadmap, the decisions and git every time it runs. Reports what it could not read and why, since the structures it reads are young enough that an old window genuinely cannot be answered in full. |
| `script/names` | Who named this, when, and what was refused. Reads the `Name:` block every crate, program and `script/` entry point carries in its own header, so the table is computed rather than maintained. Each block is `ratified` (Chris ruled), `recorded` (the tree argues the name and cites where, but nobody put it to him) or `unrecorded` (nothing says why). `--unratified` is the worklist, the last two states ordered by exposure: programs, then crates, then `script/`, and within a tier the unrecorded first. `--refused` lists every refused name and what holds the refusal, which is the query a proposer makes; `--unrecorded` is the narrower slice where research is still owed; `<name>` answers for one name, from the refusals as well as from the tree; `--check` fails the build on a name with no block and **never on a name that is merely unratified**, and is gated in `lint`. **The name of this script is provisional**, as is `--unratified`. See [naming.md](naming.md). |
| `script/initboot` | Boot straight into userspace init, skipping the milestone tour. |
| `script/qemu-check` | Is the QEMU on PATH the one `.qemu-version` pins, and does it carry the devices the suite needs? **Fails** on a missing device (that would gut a test silently), **warns** on a version mismatch (Homebrew cannot install an arbitrary older QEMU, and an unfollowable rule is worse than none). Called by `bootstrap` and by `ci-qemu`. |
| `script/ci-qemu` | CI only, Linux only: build the pinned QEMU into a cacheable prefix, because Ubuntu 24.04's 8.2 has no `riscv-iommu-pci` and apt cannot go newer. |
| `script/drift [nightly-YYYY-MM-DD]` | Does a toolchain still build us? Bare-metal build plus the host-logic tests. With no argument it checks the pin, which makes it a fast health check; given a nightly it checks that one, which is what the daily `toolchain drift` workflow does with the newest. |
| `script/toolchain-bump [YYYY-MM-DD]` | Raise the pinned nightly, with evidence: install, rebuild the std farm from scratch, run every gate. Restores the old pin if anything fails, because a half-applied toolchain bump is worse than none. Run it when the daily `toolchain drift` workflow goes red. |
| `script/test` | Run the suite: the host-logic crates in milliseconds, then the kernel under QEMU. The fast inner loop; assumes `setup` has run. `--arch aarch64\|riscv64` runs one ISA leg instead of both (the default is still both, so the parity gate cannot be weakened by forgetting it); `--cpu <model>` picks the emulated CPU (notes/cpu-models.md); `--hvf` runs the aarch64 kernel leg on the physical Apple Silicon core instead of under TCG (aarch64 only, `-cpu host` mandatory, and it skips the host-logic crates because no accelerator exists on that path; notes/hvf-leg.md). |
| `script/cpu-matrix` | Run the riscv64 suite against every QEMU CPU model in the matrix (`rv64`, `sifive-u54`, `rva22s64`, `rva23s64`, `thead-c906`), because the default `rv64` is QEMU's maximalist model and the board is an RV64GC U74. A CI gate. Preflights that `-cpu` is enforced rather than merely advertised, then runs every model without stopping at the first failure. See notes/cpu-models.md. |
| `script/ci-build` | What CI runs: `bootstrap` (a CI runner starts bare), then the tests. |
| `script/server` | Boot the OS in QEMU (the milestone tour, then the shell). An OS is the thing you *start*, so it is `server`. |
| `script/console` | Boot straight to the interactive shell at EL0. For this project the console is literally a shell running as an unprivileged process. |
| `script/shell-check` | `console`'s gating twin: boot `--features shell` on both ISAs, type eleven lines at the prompt, and check what came back (the pipe and both redirection operators, `wc gate.txt` against `wc < gate.txt`, and the wall clock). The only thing in the tree that runs a **real** init (`system_initializer` on riscv64, `hello`'s `init_boot` role on aarch64, both of which are `crates/system_initializer` since milestone 96); every other shell test has the kernel play init. Not in `script/test`, because it builds a second kernel and boots it twice. `--arch aarch64\|riscv64` for one leg. |
| `script/fmt` | Format the tree with the pinned rustfmt; `--check` reports instead of writing (the CI gate). |
| `script/gates` | Run every gate a pull request must pass, cheapest first: `script/fmt --check`, `script/lint`, `script/test`, then (milestone 81) `script/test --hvf`, the aarch64 suite again on the physical Apple Silicon core. One command instead of three, because on 2026-08-03 a change was pushed having run two of them and the third failed in CI. The HVF leg lives here rather than in a workflow because GitHub's hosted macOS arm64 runners are VMs without nested virtualization, so HVF does not exist there; when the host cannot supply it the leg **skips loudly**, naming the reason and saying that nothing in the run touched a physical core, so a Linux transcript cannot be read as silicon coverage. It costs about 16 s (measured; notes/hvf-leg.md). Deliberately NOT the whole CI surface: Kani, the CPU matrix, fuzzing, coverage, the bench tripwire and the supply-chain audit stay in CI, since a wrapper that took an hour is one nobody would run. Never writes; use `script/fmt` to format. |
| `script/lint` | Run clippy across the workspace with warnings denied (a CI gate), on both ISAs and in each boot-mode feature build, plus the non-clippy checks that share its job: broken intra-doc links, conflict markers, the roadmap status vocabulary, relative markdown links plus the notes/README.md index, DECISIONS numbering, that every `script/` has an entry here, that no file carries a module-wide `#![allow(dead_code)]` (DECISIONS §38), and the naming conventions a machine can check (no `-d` names, none of the rejected Unix vocabulary, one spelling for contract crates, a recognised branch prefix; notes/naming.md). Milestone 68 added three more: **dependency direction** (nothing under `crates/` may depend on a binary, which would still build while leaving the host tests and Kani), **unused dependencies** via `cargo-machete` (DECISIONS §46), and **spelling** via `typos`. Milestone 94 added one more: a **`TODO`/`FIXME` marker in code names the milestone that owns it** (`TODO(milestone N):`, and the block has to exist), because a marker with no home is identified work resting where nobody will look for it. Markdown is exempt, since prose explaining the convention has to spell the shape it forbids, and a note may quote a marker that was resolved milestones ago. Milestone 113 added a fourteenth clippy configuration: the **proof harnesses**, compiled with `--cfg kani` against the shim in `scripts/kani-lint-shim/`, because `cfg(kani)` is set by the model checker and by nothing else and so those modules had never been linted at all (26 warnings on the first run; notes/unsafe-obligations.md). Lint SELECTION is not here: it lives in `Cargo.toml`'s `[workspace.lints]`, with `clippy.toml` and `_typos.toml` holding the two allowlists. See DECISIONS §61 for why three candidate lints were measured and dropped. |
| `script/coverage` | Coverage for the host-logic crates, gated on an 80%-per-file line floor (a CI gate). Installs cargo-llvm-cov on first run. |
| `script/vendor-verify` | Prove each `vendor/*.pin` tree is the published tarball (sha256) plus exactly its divergence patch, byte for byte. `--write-patch` regenerates the patch after a deliberate change. Needs network on a cold cache. |
| `script/supply-chain` | The milestone-42 gate (a CI gate): cargo-deny (advisories, licences, bans, duplicates, sources) over each workspace against `deny.toml`, then `vendor-verify`. Needs network; installs the cargo-deny pinned in `.cargo-deny-version` if the installed one differs, because 0.19 and 0.20 spell `--config` differently and default it to different directories. |
| `script/fuzz` | Coverage-guided fuzzing (cargo-fuzz/libFuzzer) over the parsers that read bytes we did not write: `dtb_walk`, `elf_parse`, `gpt_table`, `crickerfs_roundtrip` (a CI gate). `--time N` sets the per-target budget (default 60s, `0` runs until stopped), `--list` explains each target, and a bare target name runs one. Installs the cargo-fuzz pinned in `.cargo-fuzz-version` on absence or mismatch. See notes/fuzzing.md. |
| `script/undefined-behavior-check` | The host-logic tests again, under Miri's interpreter: aliasing, pointer provenance, uninitialized reads, leaks, the rules no other gate checks. Weekly in CI plus on demand; not in `test` or `gates`, because the interpreter is minutes where the host tests are milliseconds. The exhaustive suites sample themselves under `cfg(miri)`, so "Miri-clean" means the sampled paths. Extra args go to `cargo miri test` (`script/undefined-behavior-check -p gpt`). See notes/undefined-behavior.md. |
| `script/interleaving-check` | The hand-rolled atomic protocols under loom, which searches **every** thread interleaving and every reordering the C11 model permits (milestone 80). The one gate that can falsify CLAUDE.md's fourth rule: Kani's harnesses are single-threaded, Miri runs one interleaving, and QEMU's TCG explores almost none of the orderings aarch64 and riscv64 allow. Covers `crates/steal_request` (the work-steal handshake) and `crates/clock_proto` (the clock page's seqlock, where it found a real torn read on its first run). `loom` is a `[target.'cfg(loom)'.dependencies]` entry, so no ordinary build resolves or compiles it. Under a second warm; extra args go to `cargo test`. Not in `test` or `gates`; see the note for why. See notes/interleaving.md. |
| `script/stack-frame-check` | What one kernel function's stack frame costs, from `-Z emit-stack-sizes`, gated at a third of the smallest kernel stack (5461 bytes of a 16 KiB thread stack). The complement of milestone 84's watermark: that says how deep the suite *went* and cannot say which function is expensive, which is the question an overflow poses. Written after `sched::reap_region_objects` carried a 6816-byte frame, of which 4096 was one `[u64; MAX_ENDPOINTS]` scratch array, against 4712 bytes of measured headroom; it compiled without a warning and was found only because a milestone's CI faulted one run in five. Gates both ISAs (§19). `--arch` narrows, `--report` prints the deepest 40 and gates nothing. Needs no emulator, so it works from a machine with no QEMU. A CI job, not in `gates`: it builds the kernel test binary twice, which is more than `gates` promises. See notes/stack-high-water.md. |
| `script/mutation` | Mutation testing (cargo-mutants) over the host crates: would any test notice if this line were wrong? A report, not a gate; the weekly `mutation testing` workflow runs it four-way sharded and publishes the per-crate table against `.cargo/mutants-baseline.txt`. `--shard k/n` splits the run, `-p CRATE` narrows it, `--report` summarizes finished output, `--save-baseline` rewrites the baseline. Exclusions (with reasons) in `.cargo/mutants.toml`; installs the cargo-mutants pinned in `.cargo-mutants-version` on absence or mismatch. See notes/mutation-testing.md. |
`fmt`, `lint`, `coverage`, `supply-chain`, `fuzz`, `miri`, and `mutants` are not part of the canonical
set; they exist so the CI format, clippy, coverage, supply-chain, fuzz, miri, and weekly mutation
jobs are one-liners. `coverage` measures only the pure-logic host crates(`abi`, `capability`, `crickerfs`, `dtb`, `elf`, `frames`, `paging`, `pci`, ...): the kernel and user
crates run under QEMU, out of reach of host instrumentation, which is the same reason DECISIONS.md
§7 keeps the testable logic in host crates in the first place. It installs its own tool rather than
leaning on `bootstrap`, so the CI test job (which runs `bootstrap`) never compiles a coverage tool
it does not use.

## They are thin wrappers, on purpose

The scripts do almost nothing themselves. `script/test` is `cargo xtask test`; `script/server`
is `cargo xtask run`; `script/console` is `cargo xtask shell`. **`cargo xtask` is still the
engine** and still the place the real build logic lives (and it exposes more than the scripts do:
`gdb`, `objdump`, `image`). The scripts add a normalized interface on top, and nothing was
duplicated to get it. If you prefer typing `cargo xtask …`, it all still works.

## Two things that are deliberately the way they are

**`script/` (singular) vs `scripts/` (plural).** The normalized entry points are in `script/`,
GitHub's convention. The older `scripts/` (plural) holds `qemu-runner-aarch64.sh` and `qemu-bounded.sh`,
which are internal plumbing that cargo and the scripts call, not things you run by hand. Two
directories an `s` apart is a little awkward, but each follows its own convention, and keeping the
runner where cargo already expects it (`.cargo/config.toml` points at `scripts/qemu-runner-aarch64.sh`)
was cheaper than moving it.

**`bootstrap` installs system packages.** Running `script/bootstrap` will `brew install qemu` on
macOS or `apt-get install` on Linux if QEMU is missing. That is the pattern's intent: a fresh
clone should be one command from working, but it is also why `script/test` does *not* call
`bootstrap` every time: re-checking a package manager on every inner-loop test run is a poor
trade. `setup`/`update` do the heavy dependency work; `test` stays fast; `ci-build` provisions
because CI has nothing to start with.

## CI leverages them

`.github/workflows/ci.yml` runs seven jobs whose actual work is a script: the test job runs
`script/ci-build`, the format job runs `script/fmt --check`, the clippy job runs `script/lint`, the verify
job runs `script/verify`, the bench job runs `script/bench --check` on both ISAs, the coverage job
runs `script/coverage`, and the supply-chain job runs `script/supply-chain`. So CI executes the same
commands a developer does, and one place (these files) defines what "test", "lint", "verify", and
"supply chain" mean.
