# A security audit

A four-part adversarial review of the whole kernel, done after milestone 11. Four independent
reviewers read the code cold, one per dimension, and every finding was verified against the source
before it was believed. This note is the record: the threat model, what held up, what did not, what
was fixed, and what is deferred on purpose.

## Threat model

A malicious or buggy userspace ELF running at EL0 that wants to (a) escape confinement and reach
kernel or another process's memory, (b) forge or widen a capability it was not granted, (c) corrupt
the kernel with a crafted binary, filesystem, or message, or (d) exhaust kernel resources to take
the machine down. No remote attacker, no crypto, single core, runs under QEMU or HVF.

## What held up

The core boundaries are sound, and the review confirmed each with reasoning rather than assertion:

- **The capability boundary.** No method is invocable without its rights (SEND needs WRITE, RECV
  needs READ, `Untyped MAP` needs WRITE, `Irq` needs READ). Userspace only ever names a capability
  *slot*, which `CSpace::get` bounds-checks; the object index inside (endpoint id, untyped region,
  intid) always comes from a kernel-minted capability, never a user register. Rights cannot widen,
  and delegation is not even exposed to userspace yet. No process can reach another's CSpace,
  mailbox, or endpoints.
- **The MMU.** Every `user_*` page-flag has correct AP/PXN/UXN bits; W^X holds as a *type
  invariant* (there is no writable-and-executable constructor, proved over all of them by a test).
  EL0 cannot name, read, write, or execute any kernel high-half address. `map_physical`
  (arbitrary-physical mapping) is not reachable from EL0; the only user mapping path is
  `Untyped MAP`, which is confined to the process's own budget and own low half.
- **TLB discipline.** Stack VAs and address spaces are flushed on reuse, so a new owner cannot read
  a dead owner's data.
- **The scheduler and locks.** No third instance of the two bugs milestone 9 fixed (interrupt
  restored under the lock; no idle thread). `HELD_RANK` is `NONE` at every `switch_to`; IPC
  blocking races, IRQ re-entrancy, and the untyped-MAP TOCTOU all check out on single core.
- **The loaders and parsers.** ELF file-bounds and header-table math are `checked_*`; `.bss` is
  genuinely zeroed; the "load over the kernel" attack is refused by construction (`Half::Low`); the
  device-tree parser reads every field through bounds-checked accessors and cannot be made to loop
  or read out of bounds.

## What was wrong, and is now fixed

**1. A crafted ELF could panic the kernel (`crates/elf`).** The parser checked `p_offset +
p_filesz` and `memsz >= filesz` but never `p_vaddr + p_memsz`. A segment with `memsz` near
`u64::MAX` passed validation, then overflowed the entry-in-segment check and `page_range`. With
overflow-checks on (the shipping dev profile) that is a **panic**, i.e. a hostile initrd halting
the kernel, which is precisely the denial of service the ELF crate exists to prevent. Fixed with a
`checked_add` on `vaddr + memsz` (new `Error::AddressOverflow`), saturating arithmetic in the
`pub` `page_range`, and two tests (`u64::MAX` memsz, and 65-header count). Also capped the program
header count (`MAX_PHNUM = 64`), because 65535 headers turned the O(n²) overlap check into a
multi-second stall.

**2. A spawn flood could panic the kernel (`shell_service`).** The process service did
`.expect(...)` on `sched::spawn`, so once memory was tight, out-of-memory became a kernel panic.
Now it degrades: a failed spawn returns a sentinel result to the shell ("could not spawn a
process") and the service keeps serving. Per-process spawn *quotas* are the real fix and are
deferred with untyped kernel objects; not turning OOM into a panic is the cheap, honest hardening.

**3. A failed `Untyped MAP` silently burned the process's own budget.** The syscall retyped a page
before attempting the map, so a bad `va` (misaligned, or high-half) spent a page for nothing. Now
those cheap cases are rejected before any page is retyped. (An already-mapped `va` still costs one
page, which is process-local and bounded by the untyped, so it is left as-is.)

**4. Stale documentation described defences the code no longer has.** Comments still pointed at
`user_slice` and the confused-deputy `AT S1E0R` reader, which milestone 8 deleted when the console
left the kernel; a future reader could have believed a pointer path was guarded when there is no
pointer path at all. Corrected, and the historical `abi::console` methods are marked as such.

## What is deferred, on purpose, and named honestly

- **DMA has no IOMMU, so a *hostile* driver owns all of physical memory.** This is the single most
  severe item in the system and it is inherent to the platform, not a coding defect. The virtio
  device is a second bus master doing DMA against raw physical addresses with no MMU in front of
  it, and the driver writes those addresses. Fault isolation (milestone 9) is real; malice
  isolation is not, absent an IOMMU or a kernel that validates every descriptor. `notes/virtio.md`
  now states this plainly rather than implying the isolation covers malice.
- **No per-process resource limits.** Untyped bounds a process's *pages*, but `Thread` structs,
  16 KiB kernel stacks, endpoints, and capability tables still come from the kernel heap with no
  quota. A process that drives spawns, or blocks workers forever on an endpoint it never drains,
  accumulates kernel memory without bound (it now fails gracefully rather than panicking, but the
  memory is still spent). Quotas are the untyped-kernel-objects continuation of milestone 11.
- **No IPC timeouts and no revocation.** A thread blocked on an endpoint that never gets a peer is
  never reclaimed. This is the accumulation primitive above, and it is the seL4 depth (capability
  derivation tree, revocation) deliberately parked.

## The shape of the result

The MMU-and-capability confinement, the part the whole architecture is built to get right, held up
under adversarial reading with no exploitable hole. The real defects were two panics on untrusted
input, both now fixed and tested, and the honest limitations are the ones a single-core learning
kernel without an IOMMU or resource quotas is expected to have. Naming them is the point; a kernel
that pretends to isolate a hostile DMA driver is worse than one that says it cannot.
