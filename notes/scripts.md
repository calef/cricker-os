# The `script/` entry points

Every command you need to work on this repo lives in `script/`, one short file each, with the
same names GitHub's [Scripts to Rule Them All](https://github.com/github/scripts-to-rule-them-all)
pattern uses. The whole idea is muscle memory: clone any repo that follows the pattern, run
`script/setup`, then `script/test`, and you are working. You do not have to learn that this one
uses `cargo xtask` and that one uses `make` and the next uses `npm`.

## The commands

| script | what it does |
|---|---|
| `script/bootstrap` | Install every dependency: the pinned Rust toolchain (via rustup, from `rust-toolchain.toml`) and QEMU. Idempotent — it checks first and installs only what is missing. |
| `script/setup` | First run after a clone: `bootstrap`, then build. |
| `script/update` | After pulling new code: `bootstrap` (the pinned toolchain can change), then rebuild. |
| `script/test` | Run the suite — the host-logic crates in milliseconds, then the kernel under QEMU. The fast inner loop; assumes `setup` has run. |
| `script/cibuild` | What CI runs: `bootstrap` (a CI runner starts bare), then the tests. |
| `script/server` | Boot the OS in QEMU (the milestone tour, then the shell). An OS is the thing you *start*, so it is `server`. |
| `script/console` | Boot straight to the interactive shell at EL0. For this project the console is literally a shell running as an unprivileged process. |
| `script/fmt` | Check formatting against the pinned rustfmt (a CI gate). |
| `script/lint` | Run clippy across the workspace with warnings denied (a CI gate). |

`fmt` and `lint` are not part of the canonical set; they exist so the CI format and clippy jobs
are one-liners.

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
macOS or `apt-get install` on Linux if QEMU is missing. That is the pattern's intent — a fresh
clone should be one command from working — but it is also why `script/test` does *not* call
`bootstrap` every time: re-checking a package manager on every inner-loop test run is a poor
trade. `setup`/`update` do the heavy dependency work; `test` stays fast; `cibuild` provisions
because CI has nothing to start with.

## CI leverages them

`.github/workflows/ci.yml` runs three jobs whose actual work is a script: the test job runs
`script/cibuild`, the format job runs `script/fmt`, the clippy job runs `script/lint`. So CI
executes the same commands a developer does, and there is one place — these files — that defines
what "test" and "lint" mean.
