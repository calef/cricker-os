# 58. RISC-V TLB shootdown, and the flush that makes ASIDs pointless

**Status: BUILT** 2026-08-05 (pull request #124, merge `8fdf2677`). The status read
`IN-PROGRESS since 2026-08-04, a developer holds it on milestone/58-riscv-tlb-shootdown` for twelve
days after that merge, and the branch it named had been deleted by the merge that finished it; found
2026-08-17 by the status-accuracy sweep, which found all six `IN-PROGRESS` rows naming branches that
do not exist. §76's defect class again: the index row and this file agreed with each other and both
disagreed with the tree, which is the one case `script/roadmap --check` cannot see.

**What landed, against the four items the deleted gate line sequenced.** The per-ASID flush is
`kernel/src/arch/riscv64/mmu.rs:638`, `sfence.vma zero, {asid}` for the local half. The IPI
shootdown with its acknowledgement is `super::sbi_remote_sfence_vma_asid`
(`kernel/src/arch/riscv64/mod.rs:292`), the ack being the return of the `ecall` into OpenSBI's
`sbi_tlb_sync`. The removal is gated on the probe: `write_satp` (`kernel/src/arch/riscv64/mmu.rs:80`)
now issues `sfence.vma` only when `!asid_tagging_is_trusted()`. And `bench/baseline-riscv64.txt` was
re-saved. Two witnesses, both wired without an arch `cfg` so both run on both ISAs:
`an_asid_flush_reaches_the_other_cores` (`kernel/src/user/tests.rs:611`), which was also the first
coverage aarch64's `tlbi aside1is` broadcast ever had, and
`asid_tagging_keeps_address_spaces_apart_without_flushes` (`kernel/src/user/tests.rs:523`), which was
aarch64-only and is now the milestone-15 witness meaning something on RISC-V. See
notes/riscv-tlb-shootdown.md.

**The honest result is a small regression on icount, not a win**, recorded in
notes/benchmarks.md:834: `ctx_switch` +1.2%, `ipc_rtt_el0` +1.6%. QEMU charges the added gate and
credits nothing for the removed flush, because its softmmu TLB is not ASID-tagged, so the number this
milestone was supposed to produce is not measurable on the emulator at all. The real one is milestone
24's, on the board.

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

**Sequencing, as planned and as it went.** The probe was done 2026-07-31, then the per-ASID flush,
then the IPI shootdown with its acknowledgement, then removing the flush behind the probe's gate, then
re-baselining `ctx_switch`. All four landed in that order in pull request #124. **Effort was
deliberately not estimated** because the shootdown was the unknown; the note above records what it
cost.
