# 58. RISC-V TLB shootdown, and the flush that makes ASIDs pointless

**Status: NOT-STARTED.**

**In brief.** `write_satp` follows every `csrw satp` with a bare `sfence.vma`, so **every RISC-V
context switch throws away the entire TLB** while carrying an ASID it then gets no benefit from. The
fix is not deleting the instruction; it is building what has to exist first.

## This is a parity gap, not a design question

aarch64 already does it right: `set_ttbr0` writes the register and flushes nothing, and a separate
`flush_asid(asid)` is documented as "the teardown half of the ASID contract (crates/asid): after
this, and only after this, the number may tag someone else." `crates/asid` is Kani-proven and its own
header states the intent: "a context switch stops flushing anything". **RISC-V simply does not use
the machinery that is already built and already proven on the other ISA.**

## Why it is not a one-line deletion

- **`sfence.vma` does not broadcast, and that is the whole milestone.** aarch64's `TLBI` invalidates
  across every core in hardware. RISC-V's `sfence.vma` affects only the hart that runs it, so
  flushing an ASID machine-wide means an IPI to every hart, each running its own `sfence.vma`, and an
  acknowledgement before the number may be reused. **That is a distributed protocol with real races,
  and getting it wrong is silent**: stale translations mean one process reading another's memory with
  no crash to announce it.
- **The free path must flush per-ASID** (`sfence.vma x0, asid`), which today does not exist at all.
- **The `satp.ASID` width must be checked**, which is now done: `mmu::asid_bits()` probes it at boot
  and `the_hardware_has_at_least_the_asid_bits_the_allocator_assumes` fails loudly below 8. **Removing
  the flush must be gated on that number.**

## The thing to understand before touching it

**The unconditional flush is currently load-bearing for correctness, not merely slow.** `satp.ASID` is
WARL and RISC-V permits *zero* implemented bits; `crates/asid` hands out 255 numbers on the stated
assumption that even the smallest hardware ASID space is 8 bits, which is true of aarch64 (mandated)
and **not guaranteed by RISC-V**. On a core with no ASID bits, all 160 address spaces would carry
ASID 0 and their TLB entries would alias. Nothing has bitten us because the flush discards every
entry before it can. Delete the flush without the probe gating it and the failure mode is
cross-process memory disclosure.

## The trade, stated plainly

The **win** is a full TLB flush removed from every RISC-V context switch; `ctx_switch` is paying for
it now and would show the improvement. The **risk is asymmetric**, and should drive the sequencing:
the upside is a benchmark number, the downside is silent memory disclosure. So the shootdown gets
**proven, not argued**, and it is why milestone 19's test lane correctly left this alone rather than
taking it as a side effect of writing tests.

**Sequencing.** The probe is done (2026-07-31). Next is the per-ASID flush, then the IPI shootdown
with its acknowledgement, then removing the flush behind the probe's gate, then re-baselining
`ctx_switch`. **Effort: not estimated**; the shootdown is the unknown, and it is the kind of unknown
that deserves measurement before a number.
