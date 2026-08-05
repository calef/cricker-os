# The RISC-V TLB shootdown, and the flush that made ASIDs pointless

*(Milestone 58. Why `sfence.vma` needs a distributed protocol where `tlbi` needs an instruction,
what had to exist before the context-switch flush could go, and the two things this turned up that
nobody had written down.)*

## The gap, stated plainly

Until this milestone every RISC-V context switch did this:

```
csrw satp, a0
sfence.vma
```

The second instruction throws away **the whole TLB on that hart**, kernel translations included, and
it ran on every switch. Meanwhile the kernel was carefully allocating an ASID per address space,
packing it into `satp[59:44]`, and getting nothing for it: the tag cannot keep two spaces' entries
apart if there are no entries left to keep apart.

aarch64 has not done that since milestone 15. `set_ttbr0` writes the register and flushes nothing,
and the whole reuse contract lives at one point, `flush_asid` at address-space teardown. So this was
a parity gap rather than a design question, and DECISIONS §19 makes parity a gate.

## Why it was not a one-line deletion

**The flush was load-bearing for correctness, not merely slow.** Three separate things had to be
true first, and only one of them was.

### 1. User mappings must be ASID-tagged

They already were, and by omission rather than by design. Sv39's `G` bit marks a mapping global,
matching under any ASID; `paging::Sv39` sets it from `Flags::is_global()`, which the kernel
constructors set and the user constructors do not. So a user PTE was already non-global and its TLB
entries already carried whatever `satp.ASID` was live during the walk. Nothing depended on that
before, which is exactly why it was worth checking rather than assuming.

The kernel's half stays global on purpose, for the same reason it does on aarch64: every address
space maps it identically, and tagging it would cost one TLB entry per process for one translation.

### 2. `satp.ASID` must actually be wide enough

**RISC-V permits zero implemented ASID bits.** The field is WARL, and hardwiring it to zero is the
cheap option for a small core. aarch64 *mandates* eight. `crates/asid` hands out 255 numbers on the
stated assumption that even the smallest hardware ASID space is 256, which is true of one ISA and not
of the other.

On a core with no ASID bits, all 160 possible address spaces would carry tag 0 in hardware and their
entries would alias. The failure mode is one process reading another's memory, with no fault to
announce it.

So the removal is gated on a measurement, not on the specification. `mmu::probe_asid_bits` writes
ones into the field of the live `satp`, reads back which stuck, and counts them;
`asid_tagging_is_trusted()` compares that against what `asid::ASIDS` needs. Too few bits and
`write_satp` keeps the sweep. **Correct and slow beats fast and silently wrong**, and a panic would
refuse to boot a machine that works.

The threshold is derived (`(asid::ASIDS - 1).ilog2() + 1`) rather than written as `8`, so raising the
allocator moves the gate with it instead of leaving a constant behind that used to be right.

### 3. An ASID must be swept from *every* hart before it is reused

This is the milestone, and it is the whole of the difference between the two ISAs.

**`sfence.vma` is a local instruction.** It invalidates and orders for the hart that executes it and
says nothing about any other. aarch64's `tlbi aside1is` broadcasts across the inner-shareable domain
in hardware. So the contract `crates/asid` states in one line, *flush, then the number may tag
someone else*, is one instruction there and a distributed protocol here.

The protocol is SBI's RFENCE extension. `mmu::flush_asid` runs `sfence.vma x0, asid` locally, then
calls `sbi_remote_sfence_vma_asid` for every other online hart. The firmware sends each an IPI and
each runs the same instruction.

**The acknowledgement is the return of the `ecall`**, and this is the part worth being explicit
about, because "the protocol has an acknowledgement" is a claim and not a hope. OpenSBI queues the
request, sends the IPIs, and spins in `sbi_tlb_sync` until every target has drained it, so no target
holds an entry wearing the tag by the time the call returns. Linux depends on the same guarantee (its
`flush_tlb_mm` does no waiting of its own), which is the reason to believe it rather than a reading
of ours.

**The IPI arrives as an M-mode software interrupt**, so a hart with S-mode interrupts masked still
services it. That is not a footnote: without it, any kernel code that disables interrupts and spins
would deadlock whoever was flushing, and this kernel disables interrupts routinely.

## The `size` argument is a trap

SBI defines a remote fence as covering everything in two different ways, and for
`remote_sfence_vma_asid` they do different things:

| `start`, `size` | what OpenSBI runs on each target |
|---|---|
| `0`, `usize::MAX` | `sfence.vma x0, asid`: every address, **that ASID** |
| `0`, `0` | `sfence.vma`: **the entire TLB**, every ASID |

Both are correct. The second silently undoes this milestone on every hart but the caller's, which is
the worst kind of wrong: nothing fails, the numbers just quietly stop improving. The kernel passes
`start = 0, size = usize::MAX`, and `SBI_RFENCE_ALL` is named in `arch/riscv64/mod.rs` so the choice
is legible at the call site.

## How it is proved

Two tests, and the second is the one that would have caught the bug.

`asid_tagging_keeps_address_spaces_apart_without_flushes` (kernel::user::tests) was **aarch64-only
until now**, and the comment explaining why was right: on RISC-V it would have read the correct byte
because the switch had just flushed everything, not because the tagging works. A test that cannot
fail for its stated reason is worse than no test. It runs on both ISAs now, which is what makes this
one suite rather than two claims.

`an_asid_flush_reaches_the_other_cores` is new, portable, and **fails without the shootdown**. That
was checked rather than assumed, by making `flush_asid` local again and running the suite:

```
test kernel::user::tests::an_asid_flush_reaches_the_other_cores ...
[PANIC] assertion `left == right` failed: STALE TLB ON ANOTHER CORE: core 0 still translates
0x400000 to the frame this space stopped using, after a flush of its ASID.
```

The shape:

1. a probe thread on **another** core installs an address space, reads a user VA (which is what pulls
   the translation into *that* core's TLB, tagged with that space's ASID), and leaves it installed
2. this core moves the VA onto a different frame **with no per-address invalidation at all**
3. this core calls `flush_asid`
4. the probe reads the same VA again, on the same core, with the same space still installed

Two of those choices are load-bearing. **The space is never re-installed between the reads**, because
a `satp` write is a second event a core or an emulator may treat as a flush, and then step 4 would be
right for a reason that has nothing to do with the shootdown. And the mapping is **changed rather
than recycled**: tearing the space down and handing its ASID to a new one is the scenario that
matters in production, but the allocator would likely hand the new space the dead one's frames, and
reading the right byte off the right frame by accident is the same failure of proof.

It also covers aarch64's `tlbi aside1is` broadcast, which nothing tested before.

## EXAMPLES

Run just the two witnesses, on both ISAs:

```
script/test --arch riscv64 2>&1 | grep -E 'asid_tagging|an_asid_flush'
script/test --arch aarch64 2>&1 | grep -E 'asid_tagging|an_asid_flush'
```

Prove the shootdown test can fail. Make the remote half unreachable in
`kernel/src/arch/riscv64/mmu.rs`:

```rust
    let others = crate::smp::online_harts_mask() & !(1usize << crate::cpu::id());
    if others != 0 && false {          // <- the experiment
        super::sbi_remote_sfence_vma_asid(others, asid);
    }
```

then `script/test --arch riscv64`, and expect the `STALE TLB ON ANOTHER CORE` panic above. Put it
back afterwards; it is a two-character edit in both directions, which is why it is written here
rather than left behind a feature flag nobody would run.

See what the machine implements, without running the suite. The ASID width is on the `isa` line of
every boot:

```
script/console --arch riscv64
```

## The numbers, and the honest part

`script/bench --riscv`, deterministic icount ticks, against the base commit:

| benchmark | before | after | delta |
|---|---|---|---|
| ctx_switch | 471,827 | 477,635 | **+1.2%** |
| ipc_rtt_el0 | 1,738,256 | 1,766,199 | **+1.6%** |
| yield_switch | 179,097 | 179,217 | +0.07% |

**The win does not appear, and it cannot appear on this instrument.** icount counts guest
instructions retired. A TLB flush costs one instruction; its real cost is the misses that follow it,
and QEMU's TCG refills its softmmu TLB with host-side work that retires no guest instructions at all.
So the emulator charges us for the instructions we added and credits us nothing for the flush we
removed, which is exactly backwards from what the hardware does.

What we added, per switch: an atomic load and a branch for the gate, and a `csrr satp` plus a compare
for the "already installed?" early return `switch_user_root` gained (aarch64 has had it since
milestone 15; it fires on every switch between two kernel threads, which is most of them on an idle
machine). The baseline is re-saved to match, because the change is intended and understood.

**This is a tie recorded plainly rather than a win overclaimed.** The measurement that would settle it
needs hardware with a real TLB: `--real` runs under Hypervisor.framework, which executes the host's
own ISA and so has no RISC-V leg, and the VisionFive 2 has not arrived. Milestone 24's board work is
where this number gets taken.

## Two things this turned up that nobody had written down

**S-mode may not read a user page.** The ported witness test faulted on RISC-V for a reason with
nothing to do with flushing: `sstatus.SUM` gates whether S-mode may load or store through a page
marked `U`, and this kernel never sets it. EL1 reading an EL0 page is simply allowed, because we
never set `PSTATE.PAN`. So the two ISAs disagree about the default, and a test that reads through a
user VA (the only way to observe a TLB from software) needs a permission on one and nothing on the
other. `mmu::permit_kernel_access_to_user_pages` is that permission, `#[cfg(test)]` on both, because
no syscall in this ABI dereferences a user pointer and leaving `SUM` clear in a shipping build means
a kernel bug that strays into the low half faults instead of succeeding quietly.

**aarch64's `flush_asid` was missing its leading `dsb ishst`.** A `tlbi` that publishes a page-table
change must have the change visible to the other cores' table walkers first. At the teardown site the
function was written for, nothing is published (the tables are about to be freed), so its absence
never bit; the new test, which uses `flush_asid` to announce a mapping change, is a caller that would
have found otherwise. RISC-V gets that ordering for free, because `sfence.vma` is defined to order
the executing hart's own page-table writes.

## BUGS

- **A firmware that implemented RFENCE asynchronously would break this silently**, and S-mode has no
  way to detect it. The SBI spec's wording is "instructs the remote harts to execute", which OpenSBI
  reads as synchronous and another implementation might not.
  `isa::the_firmware_implements_what_the_kernel_calls` checks the extension is present; nothing
  checks it is synchronous, because nothing can. The failure would be a stale translation on another
  hart, arbitrarily far from the cause.
- **The hart mask is a bitmap relative to base 0**, so the shootdown reaches harts 0..63 on rv64. Fine
  for `cpu::MAX_CPUS`, wrong for a bigger machine, and the same limitation
  `smp::bring_up_secondaries` documents for logical-id-equals-hart-id.
- **QEMU cannot exercise the case the probe measures for the reason a real core would.** QEMU's
  softmmu TLB is not tagged by ASID; it flushes wholesale whenever `satp.ASID` changes. The test
  works around that by never re-installing the space between its two reads, so the entry it depends
  on genuinely survives, but a hardware TLB and QEMU's are not the same object and only the board
  will prove the tagging behaves as the architecture says.
- **`AddressSpace::drop` frees the region before it sweeps the ASID.** Between those two lines
  another hart could still hold entries wearing the tag, pointing at frames now on the free list.
  Nothing can *use* them (a hart must install that ASID to match, and no live space carries it until
  after the sweep, which is when the number is freed), so the order is sound. It is stated here
  because it reads alarmingly and the reason it is fine is not local to either line.
- **`share_kernel_half` copies the kernel's top-level entries once, at space creation.** A kernel
  mapping that later needs a *new* top-level entry would be invisible to every space created before
  it. Pre-existing, untouched by this milestone, and not currently reachable (the direct map covers
  all of RAM from `mmu::init`), but it is the neighbouring hazard a reader of this file should know
  about.
