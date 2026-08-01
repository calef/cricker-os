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
| `script/decisions` | Index DECISIONS.md; `--check` enforces unique section numbers and that every `§N` cited anywhere in the tree resolves. Gated in `script/lint`. |
| `script/test` | Host-logic crates, then the kernel under QEMU on **both** ISAs. The gate. |
| `script/verify` | The machine-checked proofs (Kani) over the pure-logic crates. Not in `bootstrap`: Kani pulls its own toolchain and a CBMC backend, so it is installed only where it is used. |
| `script/bench` | icount microbenchmarks; `--check` fails on >10% drift from `bench/baseline.txt`, `--save` rewrites it, `--real` runs under HVF for magnitudes. |
| `script/roadmap` | Index the milestones; `--check` validates the status vocabulary and catches a block with no row, or a milestone cited in prose the table does not carry. Gated in `lint`. |
| `script/initboot` | Boot straight into userspace init, skipping the milestone tour. |
| `script/qemu-check` | Is the QEMU on PATH the one `.qemu-version` pins, and does it carry the devices the suite needs? **Fails** on a missing device (that would gut a test silently), **warns** on a version mismatch (Homebrew cannot install an arbitrary older QEMU, and an unfollowable rule is worse than none). Called by `bootstrap` and by `ci-qemu`. |
| `script/ci-qemu` | CI only, Linux only: build the pinned QEMU into a cacheable prefix, because Ubuntu 24.04's 8.2 has no `riscv-iommu-pci` and apt cannot go newer. |
| `script/drift [nightly-YYYY-MM-DD]` | Does a toolchain still build us? Bare-metal build plus the host-logic tests. With no argument it checks the pin, which makes it a fast health check; given a nightly it checks that one, which is what the daily `toolchain drift` workflow does with the newest. |
| `script/toolchain-bump [YYYY-MM-DD]` | Raise the pinned nightly, with evidence: install, rebuild the std farm from scratch, run every gate. Restores the old pin if anything fails, because a half-applied toolchain bump is worse than none. Run it when the daily `toolchain drift` workflow goes red. |
| `script/test` | Run the suite: the host-logic crates in milliseconds, then the kernel under QEMU. The fast inner loop; assumes `setup` has run. |
| `script/cibuild` | What CI runs: `bootstrap` (a CI runner starts bare), then the tests. |
| `script/server` | Boot the OS in QEMU (the milestone tour, then the shell). An OS is the thing you *start*, so it is `server`. |
| `script/console` | Boot straight to the interactive shell at EL0. For this project the console is literally a shell running as an unprivileged process. |
| `script/fmt` | Format the tree with the pinned rustfmt; `--check` reports instead of writing (the CI gate). |
| `script/lint` | Run clippy across the workspace with warnings denied (a CI gate), on both ISAs and in each boot-mode feature build, plus the non-clippy checks that share its job: broken intra-doc links, conflict markers, the roadmap status vocabulary, relative markdown links plus the notes/README.md index, DECISIONS numbering, that every `script/` has an entry here, that no file carries a module-wide `#![allow(dead_code)]` (DECISIONS §38), and the naming conventions a machine can check (no `-d` names, none of the rejected Unix vocabulary, one spelling for contract crates, a recognised branch prefix; notes/naming.md). |
| `script/coverage` | Coverage for the host-logic crates, gated on an 80%-per-file line floor (a CI gate). Installs cargo-llvm-cov on first run. |
| `script/vendor-verify` | Prove each `vendor/*.pin` tree is the published tarball (sha256) plus exactly its divergence patch, byte for byte. `--write-patch` regenerates the patch after a deliberate change. Needs network on a cold cache. |
| `script/supply-chain` | The milestone-42 gate (a CI gate): cargo-deny (advisories, licences, bans, duplicates, sources) over each workspace against `deny.toml`, then `vendor-verify`. Needs network; installs the cargo-deny pinned in `.cargo-deny-version` if the installed one differs, because 0.19 and 0.20 spell `--config` differently and default it to different directories. |

`fmt`, `lint`, `coverage`, and `supply-chain` are not part of the canonical set; they exist so the
CI format, clippy, coverage, and supply-chain jobs are one-liners. `coverage` measures only the pure-logic host crates
(`abi`, `caps`, `crickerfs`, `dtb`, `elf`, `frames`, `paging`, `pci`, ...): the kernel and user
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
GitHub's convention. The older `scripts/` (plural) holds `qemu-runner.sh` and `qemu-bounded.sh`,
which are internal plumbing that cargo and the scripts call, not things you run by hand. Two
directories an `s` apart is a little awkward, but each follows its own convention, and keeping the
runner where cargo already expects it (`.cargo/config.toml` points at `scripts/qemu-runner.sh`)
was cheaper than moving it.

**`bootstrap` installs system packages.** Running `script/bootstrap` will `brew install qemu` on
macOS or `apt-get install` on Linux if QEMU is missing. That is the pattern's intent: a fresh
clone should be one command from working, but it is also why `script/test` does *not* call
`bootstrap` every time: re-checking a package manager on every inner-loop test run is a poor
trade. `setup`/`update` do the heavy dependency work; `test` stays fast; `cibuild` provisions
because CI has nothing to start with.

## CI leverages them

`.github/workflows/ci.yml` runs seven jobs whose actual work is a script: the test job runs
`script/cibuild`, the format job runs `script/fmt --check`, the clippy job runs `script/lint`, the verify
job runs `script/verify`, the bench job runs `script/bench --check` on both ISAs, the coverage job
runs `script/coverage`, and the supply-chain job runs `script/supply-chain`. So CI executes the same
commands a developer does, and one place (these files) defines what "test", "lint", "verify", and
"supply chain" mean.
