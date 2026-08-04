# 24. A second aarch64 *board*: Virtualization.framework (optional)

**Status: OPTIONAL.** Modernized 2026-08-03 to the current block standard, with the two design
forks named that the original one-liner left implicit; the substance is unchanged.

**Gate: DECISION.** Fork two, the host-side runner: a Swift runner in this tree or a dependency on
an existing CLI such as vfkit, and the block says whoever picks this up brings the choice to Chris
first. Sequencing is a separate question and not a blocker, since the default EFI path follows
milestone 88's stage 1 while the Linux-boot-protocol path needs neither.

**In brief.** Boot under Apple's Virtualization.framework, not QEMU's `virt`: a different machine
of the *same* ISA, with no PL011, its own device discovery, and its own boot handoff.

**Why it matters.** Proves the `arch/` **board** boundary on a second machine without buying one
(cheaper than 16's silicon, distinct from 20's second ISA): if rule 1 has been kept, this is a new
directory, not a diff across the tree. It also lets cricker-os run under the same VMM that runs
macOS and Linux guests on the dev machine. Optional; a portability exercise, **not** a
benchmarking prerequisite (guest-internal microbenchmarks are VMM-independent, and the HVF leg
already runs the physical core under QEMU's board).

**Fork one, the boot loader, and it defines the milestone's size.** VZ offers two:

- **`VZLinuxBootLoader`** boots any image carrying the ARM64 Linux boot header (a 64-byte magic
  prefix) and hands the guest a device tree with its address in `x0`, per the Linux boot
  protocol. Small delta: add the header, reuse the existing DTB front door (milestone 60), write
  the virtio-console driver, done. No UEFI, no ACPI. (The old note that QEMU does not pass a DTB
  pointer in `x0` stays true for QEMU's ELF path; under this protocol the pointer is real.)
- **`VZEFIBootLoader`** is generic UEFI, sharing milestone 88's boot stub. Bigger alone, but if
  88 proceeds the stub exists anyway and this milestone collapses to "same stub, different VMM."

Sequencing follows from the fork: **default is the EFI path, after 88's stage 1**, one boot stub
serving two VMMs and every cloud provider. The Linux-protocol path is the recorded fast-track,
worth taking early only if the virtio-console driver is wanted sooner, which 88 may decide (see
below).

**The genuinely new artifact either way: a virtio-console driver.** VZ has no PL011; the console
is virtio. This is the tree's first console that is not a memory-mapped UART, and it may
double-serve: whether OCI's serial console presents a 16550 or virtio-console is one of milestone
88's recorded unknowns, so this driver is potentially on 88's path, not just this one's. If 88's
stage 2 finds virtio-console, build the driver there and this milestone inherits it.

**Fork two, the host-side runner, named so it does not ambush a lane.** VZ requires a host binary
with the `com.apple.security.virtualization` entitlement. Either the tree grows a small Swift
runner (a new language in the repository, a decision to make deliberately, not in passing) or it
takes a dependency on an existing CLI such as vfkit (§46: a dependency is a decision, and this
one is a whole subsystem we would never write, which is the vendoring side of that rule's line).
Undecided; whoever picks this milestone up brings the choice to Chris first.

## Scope note

Nothing here regresses the QEMU board: `virt` stays the primary development machine, and the
board boundary this milestone exists to prove means the VZ directory must not leak into it (rule
1, applied at board rather than ISA granularity). Interrupt controller and memory layout under VZ
are facts to read from what VZ actually presents (it declares a GICv3 and its layout in the DTB
it hands over), not assumed from QEMU's `virt`.
