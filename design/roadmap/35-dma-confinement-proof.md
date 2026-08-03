# 35. Prove the DMA-confinement boundary (extends 18)

**Status: BUILT.**

**In brief.** Extract the shadow-ring validator (`validate_and_shadow`) out of `kernel/src/virtio.rs` into a host-testable logic crate and machine-check it: no validated descriptor chain, in either direction and including indirect descriptors and multi-queue, can reference memory outside the driver's granted DMA region. Add the `Untyped::SPLIT` "never widens rights" harness (the one fresh-mint site the caps proof doesn't reach) and confirm the IOMMU domain builder's *maps-exactly-the-grant* property is proved, not just tested.

**Why it matters.** **closes the one isolation boundary we test instead of prove.** Every other confinement seam (caps, MMU, IPC, generational names) is Kani-proved for all inputs; DMA is attacker-tested only. It is also the boundary that makes "don't trust the driver" true, so the proof belongs here, not on the confined component. **Load-bearing for 16a:** the VisionFive 2 has no IOMMU, so on first silicon this validator is the *sole* DMA confinement, not defence in depth

**The gap, stated precisely.** `validate_and_shadow` (`kernel/src/virtio.rs`) is the shadow-ring
logic that stops a malicious userspace driver from pointing a device's DMA at memory it was not
granted. It is the boundary that makes "the kernel confines the driver, so you need not trust the
driver" *true*. Every other isolation boundary in the system is Kani-proved for all inputs; this
one is covered by attacker tests that hit specific cases. It is pure bounds-checking over
descriptor structures, exactly what bounded model checking is good at. The only reason it is not
already proved is *where it lives*: the proved things are host-compilable pure-logic crates, and
the validator sits inside the kernel crate.

**Deliverable.**

1. **Extract and prove the validator.** Lift the validation logic into a `crates/`-style
   host-testable crate (the way `capability`, `ipc`, and `paging` were carved out), then prove the core
   property: no validated descriptor chain can reference memory outside the driver's granted
   region. Cover **both directions** (TX device-reads and RX device-writes-into-driver-memory,
   the milestone 30 addition), **indirect descriptors** (the escape the attacker suite already
   probes), and **multi-queue** (per-queue block isolation, also milestone 30). The kernel keeps
   calling the proved logic; the extraction must not change behaviour, held against the green
   attacker suite.
2. **The `Untyped::SPLIT` rights harness.** SPLIT mints a child budget at `untyped_cap_rights`, a
   fresh-mint site *outside* `capability::derive`, so the existing "derive never widens rights" proof
   does not reach it. It is currently pinned by one kernel test (added with milestone 31's
   rights-inheritance fix). Add the companion harness, "split never widens rights", beside the
   existing one, so the "authority never widens" story is proved at *every* mint site.
3. **Confirm the IOMMU domain property.** `paging`'s codec is proved; verify that the domain
   builder (`build_identity_domain`, milestone 16b) has a harness for the *maps-exactly-the-grant*
   property (the device domain maps precisely the granted frames and nothing else), not just a
   test. It is the sibling of the validator property, on the hardware side.

**Done (2026-07-29).** DECISIONS §30 is the decision record; notes/verification.md has the harness
tables, the bounds with their justifications, and the boundary statement; notes/dma.md leads with the
what-is-proved-and-what-is-not map.

1. **The validator** is `crates/dma_validator`, host-testable pure logic the kernel's
   `validate_and_shadow` calls; **seven** Kani harnesses prove no descriptor the walk shadows escapes
   the granted region or is indirect, covering both directions (symbolic flags include the RX
   device-writable bit), indirect descriptors, chains including cycles, **ring-index wraparound through
   `u16` and outer-loop termination**, overflowing address arithmetic, multi-queue block isolation, the
   oversized-batch bound, and the mutated-after-validation (TOCTOU) case. The QEMU attacker suite
   (DMA-escape and indirect-escape, both ISAs, both transports) is unchanged and green, so the
   extraction is faithful. The ring layout constants moved *into* the crate with the kernel aliasing
   them, because a proof about a copy of the layout proves nothing about the layout that runs.
2. **`split_never_widens_rights`** in `crates/capability` proves the `Untyped::SPLIT` mint (routed through
   `Cap::mint_child`) gives the child **exactly** the parent's rights. §16's amendment (SPLIT grants
   `GRANT` so a budget is delegable) makes the loose phrasing wrong, so the property is stated and
   proved as equality: `mint_child` takes no rights argument, delegability is a property of the *root's*
   mint, and rights down a budget tree are monotonically non-increasing.
3. **The IOMMU domain's *maps-exactly* property is proved**, reversing this milestone's own first
   answer. That answer declined it as the build-and-translate BMC wall, which was the right diagnosis of
   the wrong target: the wall is a symbolic IOVA walking a *built* table, and the *page set* the builder
   feeds the mapper is loopless arithmetic needing no tables. Factored out (`paging::domain::grant_pages`,
   `grant_page`, which `build_identity_domain` now calls instead of `map_range`'s unchecked
   `va + i * PAGE_SIZE`) and proved by six harnesses in both directions: soundness (no page outside the
   grant) and completeness (no whole page of the grant unmapped, because a domain mapping *nothing*
   would satisfy soundness perfectly and confine by starvation). Format-independent, so one proof covers
   SMMUv3 and the RISC-V IOMMU with no parity gap. The residual link, "`Mapper::map` writes exactly one
   leaf and touches nothing else", stays on the proved walk arithmetic plus `domain.rs`'s
   build-and-translate tests on both formats and 16b's hardware attacker test.

**Every new property was falsified before it was believed**, and one falsification corrected the code's
own comment: soundness rests on `grant_pages` flooring, not on `grant_page`'s partial-page guard, which
cannot fire for any index the builder passes. Proving the domain also hardened it against a region whose
`base + size` wraps `u64` (recorded as a proof obligation closed, not a reachable bug: the frame
allocator would run dry long before the multiply could wrap).

**The residual gap this milestone does NOT close, stated because a proof that reads as broader than it
is does damage.** The proof is about *descriptor chains*. Per §29, a virtio-gpu's backing addresses ride
in a `RESOURCE_ATTACH_BACKING` **command payload**, which the validator **structurally cannot see** (the
addresses are not in its input), and teaching the transport to parse device commands would breach §18.
So: descriptor-borne addresses are provably confined; payload-borne addresses are confined by the
**IOMMU alone**, whose allow-list item 3 now proves exact (a narrowing, not a closing: the hardware
honouring that allow-list stays an attacker test, and the transport still cannot see the addresses); and
**on a board with no IOMMU nothing confines them at all.**
That inverts this milestone's own load-bearing argument for the payload path: "prove the validator
because on the VisionFive 2 it is all there is" holds only where the validator can look. On that board a
display driver is either trusted with all of physical memory or the transport grows a device-aware
check. Whoever sequences 16a decides; it is a decision, not an oversight.

**Why it is load-bearing now, not later.** Milestone 16a's board, the VisionFive 2, **has no
IOMMU** (notes/target-hardware.md). We demoted the software validator to "defence in depth" when
16b landed the emulated IOMMU, but on first real silicon there is no hardware behind it: the
shadow-ring validator is the *sole* DMA confinement. So this proof should precede or accompany
16a, not trail the optional reach work. It is the §18 thesis ("spread inward from the capability
core") reaching the last unproved isolation boundary, and it is the one place the "verified core"
claim currently rests on testing.

**What stays unproved, on purpose.** The confined components themselves (`smoltcp`, RedoxFS, the
drivers) are *not* proof targets: the whole point of the capability core is that a confined
component need not be trusted. Proof effort belongs at the confinement boundary, not on the code
it confines. Likewise the userspace-only crates (`user_heap`, `grant_plan`, `line_editor`) and scheduler
placement policy stay host-tested; a bad placement is a performance bug, not a safety hole.
