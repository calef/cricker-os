# 30. The DMA boundary is proved for descriptors, and the proof says where it stops (milestone 35)

**Status: DECIDED.**

**The decision.** DMA confinement was the one isolation boundary in the system carried by tests rather
than proof, and it is the boundary that makes "you need not trust the driver" true. It is now
machine-checked, and **the milestone's deliverable is as much the boundary statement as the proof**:
the record must not let a reader conclude the whole DMA surface is proved when one path is proved and
another is mitigated by hardware we will not always have.

**Why now, and why it is not merely tidiness.** Milestone 16a's board, the VisionFive 2, has no IOMMU.
§20's hardware confinement demoted the software validator to defence in depth; on first silicon there
is no hardware underneath it, so it becomes the *sole* DMA confinement. A tested-but-unproved validator
is exactly the wrong thing to put in that position, and the ordering follows: prove it before or with
16a, not after.

**What is proved.** Three things, all `#[cfg(kani)]`, all in `script/verify`:

1. **The validator** (`crates/dma_validator`, seven harnesses). No descriptor the kernel copies into the
   shadow ring the device reads is ever out-of-region or indirect, for every descriptor bit pattern and
   every region: both directions (flags fully symbolic, so RX device-writes are covered), indirect
   descriptors, chains including cycles, ring-index wraparound through `u16`, overflowing address
   arithmetic, multi-queue block isolation, the oversized-batch bound, and the
   mutated-after-validation (TOCTOU) case the shadow ring exists to close. Termination is part of the
   property, not an assumption: the loop bounds are set one above what the code can need, so Kani's
   unwinding assertion fails if any input could spin the walk.
2. **The `Untyped::SPLIT` mint site** (`capability::split_never_widens_rights`). See the amendment below for
   what the property actually is, because §16's `GRANT` change makes the naive phrasing wrong.
3. **The IOMMU domain's page set** (`paging::domain`, six harnesses). The domain maps every whole page
   of the grant and no byte outside it, proved in both directions and format-independently, so one
   proof covers SMMUv3 (VMSAv8-64) and the RISC-V IOMMU (Sv39). **This reverses the milestone's own
   first answer**, which declined the property as the build-and-translate BMC wall. That was the right
   diagnosis of the wrong target: the wall is a symbolic IOVA walking a *built* table, and the page set
   is loopless arithmetic needing no tables. Factoring it out (`grant_pages`, `grant_page`) and having
   the builder call it took the property from "tested" to "proved" in a quarter of a second of solver
   time. The correction is recorded rather than smoothed over, because the lesson is a standing one:
   `notes/verification.md`'s rule "prefer refactoring the logic to shrinking the proof" applies to
   *declining* a proof too.

**The amendment §16 forces on the SPLIT property.** `Untyped::SPLIT` grants the child `GRANT` so a
budget is delegable, so "SPLIT never changes rights" is false and "SPLIT never widens rights" needs
saying precisely. The property proved is: **the child's rights are exactly the parent's**, `SPLIT`
being an *inheriting* mint (`Cap::mint_child`) with no rights argument at all. That is strictly
stronger than "no wider" and it is the shape that makes the delegable-budget behaviour correct rather
than an exception: a root untyped is minted once with `READ|WRITE|GRANT` (`untyped_root_cap`), `SPLIT`
inherits whatever the parent holds, and `CAP_INSERT` narrows on the way into a child. So rights along a
budget tree are monotonically non-increasing from the root, `GRANT` reaches a child only because the
root had it, and a spend-only untyped provably cannot split itself a `GRANT`-bearing child. The
delegability is a property of the *root's* mint, not a widening at `SPLIT`. Because `SPLIT` takes no
rights argument, there is no input by which a caller could ask for more.

**The residual gap, stated as the deliverable it is.** Milestone 29 (§29) found that a virtio-gpu's
backing addresses ride in a `RESOURCE_ATTACH_BACKING` **command payload**, not in a descriptor. The
validator **structurally cannot see them**: they are not in its input, so no amount of proving it
harder reaches them, and teaching the transport to parse device commands would put device knowledge in
the layer §18 keeps neutral and start a per-device arms race. So:

- an address reaching a device through a **descriptor** is *provably* confined to the driver's grant;
- an address reaching a device inside a **command payload** is confined by the **IOMMU alone**. Item 3
  above is the one useful thing this milestone could prove about that path, and it is a narrowing rather
  than a closing: such an address is stopped by having no translation in the device's domain, so "the
  domain maps exactly the grant" is precisely the property the barrier rests on, and it is now proved for
  every grant. That the hardware then faults an out-of-grant address stays an attacker test
  (`the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant`, both ISAs, asserting on the
  hardware's own fault queue). The transport still cannot see these addresses, and the enforcement is
  still the hardware's;
- on a board with **no IOMMU, nothing confines the payload path.** Not the validator, not the hardware.

That last point is where 16a's reasoning inverts, and it is the thing this section exists to prevent
being discovered later. The argument "prove the validator, because on the VisionFive 2 it is all there
is" works only for the path the validator covers. On that board a display driver is either **trusted**
with all of physical memory, or the transport grows a virtio-gpu-aware check and pays the §18 cost
knowingly. Whoever sequences 16a chooses; §29 already recorded that it is not milestone 29's call, and
milestone 35 does not get to make it silently either. The same gap is open under HVF, where PCIe DMA
runs unconfined by standing default.

**Bounds, because a proof whose bounds hide the interesting case reads as stronger than a test.** The
queue size the harnesses fix is 8, which is the kernel's own `QSIZE` and not a proof convenience:
`setup_queue` refuses a larger ring, so no unproved configuration exists. To keep that true rather than
merely currently-true, the ring layout constants now **live in `crates/dma_validator` and the kernel
aliases them**, because a proof about a copy of the layout proves nothing about the layout that runs.
Every attacker-controlled value (region base and size, descriptor `addr`/`len`/`flags`/`next`, both
ring indices) is unbounded. notes/verification.md carries the full table with each bound's
justification, and the one place the composition is an argument over four harnesses rather than a fifth
harness is named there too.
