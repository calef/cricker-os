# 18. Verify the capability core, then spread inward

**Status: BUILT.**

**In brief.** Machine-checked proofs of `capability`, then IPC, then MMU isolation

**Why it matters.** **the verification itself.** **Built:** `capability`, IPC (rendezvous + one-shot Reply), and the MMU isolation invariants are all proved

**Green-lit and started; see DECISIONS §14 and notes/verification.md.** This is the verification
thesis as an actual work item rather than an aspiration.

**Deliverable.** Machine-checked proofs (Kani) of the security-critical logic, spreading inward from
the capability core. `crates/capability` is proved already: five harnesses covering "`derive` never widens
rights," "userspace cannot forge a right," and the subset order's reflexivity and transitivity, each
for *every* input rather than sampled cases (`script/verify`). Next, in order, IPC (the rendezvous
and the one-shot reply) and the MMU isolation invariants.

**Why here.** It is the differentiator (§14), and it is cheap to start: the §7 pure-logic crates
already compile for the host, and proofs live behind `#[cfg(kani)]` so they never touch an ordinary
build. It also interlocks with 14: proving properties *of the kernel* (not just its logic crates) at
scale wants a kernel that does not allocate.

**Prior art.** seL4 (Isabelle/HOL refinement, verified C) is the mountain; we took the tractable path
(bounded model checking, Rust). Verus is the deeper Rust option to revisit if a property needs
unbounded proof.

**Status (2026-07-29), with milestone 35 done.** The proved set is now broad: 13 crates, ~60
harnesses, covering `capability`, `ipc` (rendezvous, one-shot reply, the collected-sender path), the MMU
codec on *both* formats (`paging`: VMSAv8-64 and Sv39, level-walk and leaf permission separation),
generational names (`slots`: a removed name never resolves again), frame allocation, region
split/destroy arithmetic, ELF parsing, the device-tree reader, ASID allocation, PCI decode, and now
the DMA-confinement validator (`dma_validator`) and the IOMMU domain's page set (`paging::domain`), both
milestone 35. An audit against the TCB (prompted by asking "what should we prove that we haven't")
found the boundaries proved with **one glaring exception: the DMA-confinement validator was
attacker-tested, never proved.** Milestone 35 closed it: the validator is extracted and proved for every
input, the `Untyped::SPLIT` mint site is proved to hand a child *exactly* its parent's rights, and the
IOMMU domain's *maps-exactly* property is proved too (its page set, in both directions,
format-independently, so one proof covers both IOMMUs; the build-and-translate round trip stays on the
declined BMC wall and on tests). What milestone 35 explicitly does **not** prove, and says so in three
places rather than leaving it to be inferred: addresses that reach a device inside a **command payload**
instead of a descriptor, which the validator structurally cannot see and only an IOMMU stops, so on a
board without one they are unconfined. See DECISIONS §30 and notes/verification.md.
