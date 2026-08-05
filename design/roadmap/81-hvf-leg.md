# 81. An HVF leg: the test suite on the physical core

**Status: BUILT** 2026-08-04 (PR #95). Raised 2026-08-03, same survey as 79.

The infrastructure exists and is used one-sidedly: `CRICKER_ACCEL=hvf` runs the kernel on the
physical Apple Silicon core, `script/bench --real` and `script/server --hvf` use it, and `script/test`
has never run there as a habit. That leaves the suite's only execution environments emulated ones,
while a machine with real caches, real reordering, and a real GIC sits under the emulator the whole
time. Until the board arrives this is the only real silicon this project can test on at all.

Per-PR in hosted CI is not an option: GitHub's macOS arm64 runners are themselves virtual machines
and do not expose nested virtualization, so HVF is unavailable there. A self-hosted runner on the
dev machine would close that gap and is deliberately not part of this milestone; it couples CI to a
laptop that sleeps.

**The delivery vehicle is `script/gates`** (Chris, 2026-08-03), not a new script and not a habit a
human has to remember. `script/gates` already exists for exactly this reason: it is the one command
a person or an agent runs before pushing, and it was created because "three commands is two too many
to remember at the moment you are about to push". An HVF leg that lived anywhere else would be a
fourth command, which is the failure gates was built to end. So gates grows a final leg: when the
host has HVF (macOS on Apple Silicon with a capable QEMU), run the aarch64 suite again under it;
when it does not, **skip loudly**, one line saying what was skipped and why, so a CI transcript can
never be misread as having had silicon coverage. Every lane on this machine then carries the leg for
free, and the integrator's merge run does too, through the same wrapper.

The work: wire `--hvf` through `script/test`/`cargo xtask test` (today it is an env var the runner
script reads), run the full aarch64 suite under it, and fix or honestly record what differs. Timer
behaviour will differ, because under HVF guest time is host time and no icount instrument exists;
part of the milestone is learning which tests that perturbs, which feeds milestone 78's per-assertion
work rather than competing with it. Then the gates leg, and notes/scripts.md.

## Scope note

HVF covers aarch64 only, with `-cpu host` mandatory, so it is one machine and no model variation;
the cpu matrix keeps its job. riscv64 has no equivalent until the board lands, so the leg's honest
name is "the aarch64 suite on real silicon", not "the suite on real silicon".

Gates' charter is checks that run locally in minutes, so the leg must be timed before it is added:
native execution should beat TCG comfortably, but that is a number to measure, not assume. If it
proves slow, the fallback is a `--full` flag rather than silent omission, and the skip-loudly line
says which mode ran.
