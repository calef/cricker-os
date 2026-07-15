# Running under virtualization on Apple Silicon

cricker-os is aarch64. So is the Mac it is developed on (an M-series chip). That coincidence,
noted in CLAUDE.md from the start, means running the kernel under **hardware virtualization** on the
Mac is a flag, not a port: Apple's Hypervisor.framework (HVF) can put the kernel on the real core
at guest EL1, using the virtualization the chip already has, instead of QEMU translating every
instruction (TCG).

## How to run it

```
cargo xtask run --hvf          # or:  CRICKER_ACCEL=hvf cargo xtask run
```

The runner swaps two things and nothing else:

- `-machine virt,accel=hvf,gic-version=2` instead of plain `virt`.
- `-cpu host` instead of `-cpu cortex-a72`. **Mandatory**: HVF runs the physical core, so you
  cannot ask for an emulated a72. You get the actual Apple core, which is a much later ARM
  revision.

Everything else is identical. QEMU still provides the `virt` machine, so the PL011, the GICv2, and
virtio-mmio all keep working; only the CPU execution moves from software to hardware. Boot goes
from about a second to instant, and **the whole stack runs**: both userspace drivers, the
filesystem read, and the shell spawning processes, all on the M3.

## Tests stay on TCG, on purpose

`cargo xtask test` forces TCG even if `CRICKER_ACCEL=hvf` is set, and that is not a limitation to
work around. The test harness exits and reports pass/fail through **semihosting**, and semihosting
does not survive the move to real hardware (see below). TCG is also the right home for tests
regardless: deterministic, and identical on any host.

## Two things HVF taught us the first time we booted

This is exactly the "which of our assumptions were secretly QEMU-shaped" exercise that
DECISIONS.md and notes/portability.md anticipate for a new target. HVF brought it forward, because
running the real core surfaces CPU-level assumptions the way a new board would surface
device-level ones.

### 1. The physical timer belongs to the hypervisor (fixed)

The very first HVF boot trapped, at `msr CNTP_CVAL_EL0, x1`, with an "Unknown reason" exception
(ESR EC 0x00). The kernel used the **physical** timer (`CNTP_*`, INTID 30). That works on QEMU's
software CPU and would work on bare metal, but **under a hypervisor the physical timer is EL2's**,
and a guest at EL1 that touches `CNTP_CVAL_EL0` traps.

The fix is what every guest OS does: use the **virtual** timer (`CNTV_*`, INTID 27), which is
available at EL1 both on bare metal and under a hypervisor. So the change is strictly more
portable, not an HVF special case. It keeps working under TCG, works under HVF, and will work on a
real board. `kernel/src/arch/aarch64/timer.rs` now reads `CNTVCT_EL0`, arms `CNTV_CVAL_EL0`, and
listens on INTID 27.

This is the kind of correction the project records rather than hides: we had a QEMU-shaped
assumption (the physical timer is ours), it was invisible until we ran real hardware
virtualization, and the machine overruled us.

### 2. Semihosting is emulation, not hardware (so tests stay on TCG)

Under HVF, the test build trapped again, at `hlt #0xf000` — the **semihosting** instruction. QEMU
implements semihosting in its TCG translator: it recognizes the instruction while translating and
handles the call itself. Under HVF the guest runs natively, so `hlt #0xf000` executes on the real
core and traps to the *guest's own* EL1 handler; QEMU never sees it. Semihosting is a property of
the emulator, not of the machine.

The harness's `SYS_EXIT` (semihosting op 0x18) is the specific call that traps, which is why a test
run under HVF hangs (it can never tell QEMU to exit) and then recurses into a stack overflow. So
tests run under TCG, where semihosting works, and HVF is for running and experimenting.

If we ever wanted a "real" shutdown that works under both, the answer is **PSCI** (`SYSTEM_OFF`),
which the `virt` machine implements and which is a genuine power-off rather than a debugger hook.
That is the honest replacement for the semihosting exit, and a good milestone-11-era cleanup.

## Why this matters beyond the Mac

HVF is a lower-stakes rehearsal for the Raspberry Pi port. Both are the same exercise: take the
kernel off QEMU's software CPU and find what it assumed. HVF finds the *CPU* assumptions (the timer)
while QEMU still holds the *devices*; the Pi will find the *device* assumptions next. Getting the
virtual timer right now means one less surprise then.
