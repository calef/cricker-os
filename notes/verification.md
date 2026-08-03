# Machine-checked proofs (Kani)

The companion to the verification thesis (DECISIONS §14). That decision says *why* we verify; this
note is *how*, and the record of the experiment that green-lit it.

## Tests sample; proofs quantify

The `capability` tests check the cases we thought to write: READ cannot become WRITE, an empty slot is
`NoSuchSlot`, a derived cap names the same object. Good tests, but they say nothing about the inputs
we did not enumerate. A proof harness asks a different question. `kani::any()` is an unconstrained
value, so:

```rust
#[kani::proof]
fn derive_never_widens_rights() {
    let src_rights = Rights(kani::any());   // ALL 2^32 patterns at once
    let requested  = Rights(kani::any());
    let mut cs: CSpace<u8> = CSpace::new(2);
    cs.put(0, Cap { object: 0u8, rights: src_rights }).unwrap();
    if cs.derive(0, 1, requested).is_ok() {
        assert!(cs.get(1).unwrap().rights.is_subset_of(src_rights));
        assert!(requested.is_subset_of(src_rights));
    }
}
```

proves "no reachable state widens rights," not "the states we tried did not." Kani compiles the
function to a logical formula and hands it to a SAT solver; `SUCCESS` means there is no assignment of
the symbolic inputs that trips an assertion or panics.

## How it actually works

The surprising part is that Kani checks "every input" without running every input. It does not loop
over 2^64 values. It reasons about them symbolically.

1. **Symbolic input.** `kani::any()` is not a random value. It is a placeholder standing for *all*
   values at once, an unknown the tool carries as algebra.
2. **The program becomes a formula.** Kani traces the harness over that unknown, turning each
   operation and branch into a logical constraint. In `index`, the `(va >> shift) & 0x1ff` becomes an
   expression in the *bits* of `va`, not a number, and the `assert!` becomes a claim about that
   expression.
3. **A solver hunts for a counterexample.** The claim, negated, goes to a SAT/SMT solver whose one
   job is to answer "is there any assignment of these bits that makes this false?"
   - **UNSATISFIABLE** = no such assignment exists = the property holds for every input. The proof.
   - **SATISFIABLE** = here is an exact input that breaks it. A counterexample, printed for you.

That is why `paging` verified in ~12 milliseconds: it is not 2^64 executions, it is one algebra
problem about the bits.

## What "bounded" means, and the one honest limit

A solver reasons completely about *fixed-size* things: a 64-bit integer, a four-level walk, a
two-slot table. What it cannot swallow whole is an *unbounded* loop or an arbitrarily large
structure, which would build an infinite formula. So Kani **bounds**: it unrolls loops to a limit and
gives structures concrete sizes.

The `paging` and `capability` harnesses have no unbounded loops (the four levels are literally four), so
their proofs are *complete*, not "up to a bound." But the moment a harness reasons over `map_range`
for a symbolic `count`, or the `Mapper` building tables, you either bound it (prove it for count <= N)
or reach for a heavier technique (induction, a tool like Verus). "Bounded model checking is automatic
but only reasons up to the bound" is the whole trade.

## What a green check does and does not mean

A proof is only as good as three things, and each is worth being blunt about:

1. **It proves what you *asserted*, not what you *meant*.** A wrong assertion verifies happily and
   means nothing. The harness is the specification, so it must be read as carefully as the code it
   checks. This is the main failure mode, not solver bugs.
2. **It covers only what the model captures.** Kani models Rust's semantics. It does not model the
   hardware, and `unsafe` that breaks Rust's assumptions is outside it. That is exactly why we verify
   the pure-logic crates (`capability`, `paging`'s arithmetic) and not the `arch/` assembly: the model is
   faithful where there is no hardware and no `unsafe`. It is also why §14 promises a *small verified
   TCB with an unverified layer beneath it*, not a proof of the whole machine. **Concurrency is the
   sharpest edge of this limit**: every queue and endpoint proof here is single-threaded, and the
   wake-before-switch-out race (notes/intrusive-queues.md) lived precisely in the SMP interleaving
   those proofs cannot see. Green harnesses and a real race coexisted; the flaky test found it.
3. **The tool is trusted.** Kani, its CBMC backend, and the SAT solver could have bugs. They are
   small and widely used, and the solver emits a checkable certificate, but it is a trust assumption.
   seL4 minimizes even its proof checker; we do not, and that is a stated limit.

## What is proved today

Eight harnesses in `crates/capability/src/lib.rs`, under `#[cfg(kani)]`:

| Harness | Property |
|---|---|
| `subset_is_reflexive` | every capability is a subset of itself |
| `subset_is_transitive` | rights cannot be laundered through a derivation chain (why a *flat* subset check suffices, with no tree walk) |
| `from_bits_cannot_forge_a_right` | an attacker-controlled syscall register cannot conjure an undefined right |
| `subset_matches_allows` | the two phrasings of the order agree, so a bug in one shows against the other |
| `derive_never_widens_rights` | the central theorem, on the real `CSpace::derive` |
| `split_never_widens_rights` | authority never widens at the *other* mint site: `Cap::mint_child` (the inheriting mint `Untyped::SPLIT` now uses) hands a child no more than the parent held (milestone 35, below) |
| `a_deleted_capability_stays_deleted` | for every table state, once `delete` succeeds the slot answers `NoSuchSlot` to both `get` and a second `delete` (the consume-on-use mechanism behind the one-shot Reply) |
| `delete_touches_only_its_slot` | deleting any slot leaves every other slot exactly as it was (consuming one caller's Reply cannot orphan another's) |

The last two run over a *symbolic* table (every slot independently empty or holding a capability
with symbolic object and rights), so "no state exists in which a consumed slot works again" is
quantified over table states, not sampled.

Two in `crates/regions/src/lib.rs`, the untyped-region accounting behind object revocation
(DECISIONS §16), where the scary property is **no double-free**:

| Harness | Property |
|---|---|
| `split_stays_within_budget_and_progresses` | a successful carve advances the watermark by exactly `want` without overflow and never past the parent's budget, and strictly progresses, so consecutive carves are disjoint runs within the parent |
| `destroy_never_frees_a_child_to_the_allocator` | a pinned or parent region refuses; a **root** frees to the allocator; a **child** *never* does, its pages return to the parent. So a page reaches the allocator only through the one root that owns it, exactly once |

The second is the no-double-free crux, and the kernel (`untyped::split`, `untyped::destroy`) *calls*
`regions::split_new_watermark` and `regions::destroy_outcome` rather than keeping a parallel copy, so
the proved arithmetic is the arithmetic that runs. It is a Phase-2-style extraction: the pure page
accounting is here (address-agnostic, in page units), the byte arithmetic and the I/O (freeing frames,
un-bumping the parent) stay in the kernel around it.

Eight in `crates/paging/src/lib.rs`, the address arithmetic under the four-level walk and the MMU
isolation invariants (the last three, closing milestone 18's MMU step):

| Harness | Property |
|---|---|
| `index_is_always_in_bounds` | every extracted table index is < 512, so the walk never reads past a table (memory safety) |
| `the_indices_and_offset_tile_the_address` | the four 9-bit indices and the 12-bit offset reassemble the low 48 bits exactly, no bit lost or shared (the `39 - 9*level` shift math is correct) |
| `the_offset_does_not_change_the_walk` | changing only the page offset leaves all four indices fixed: a whole 4 KiB page shares one leaf (page granularity) |
| `distinct_pages_take_distinct_paths` | two page-aligned addresses with the same four indices are the same page (the arithmetic core of isolation) |
| `the_two_halves_are_disjoint` | no address is in both `TTBR0` (low) and `TTBR1` (high) |
| `the_user_va_gate_admits_only_the_aligned_low_half` | `is_user_page_va` equals the bit test the syscall layer used to hand-roll, and admits no address in the kernel's half |
| `the_leaf_descriptor_keeps_address_and_permissions_apart` | the L3 descriptor `map` writes decomposes back into exactly the address and exactly the flags, for every representable physical page and every `Flags` constructor: no permission bit can redirect the address, no address bit can grant a permission |
| `the_low_half_mapper_rejects_the_high_half_untouched` | for every address outside the low half (every kernel address included), `map`/`unmap`/`translate` on a `TTBR0` mapper reject before touching any memory (the harness gives the mapper a null root and a panicking frame source, so a touch is a proof failure) |

The user-VA gate is a Phase-2-style extraction in miniature: `untyped::MAP` and `frame::MAP` both
hand-rolled `va & 0xfff != 0 || (va >> 48) != 0`; both now call `paging::is_user_page_va`, so the
gate the kernel runs is the gate that is proved. The descriptor harness leans on one assumption
worth recording: `pa` is taken as representable (bits 47:12), which is the architecture's own
descriptor format and true of every `pa` the kernel maps (frame allocator and untyped regions are
bounded by RAM, far below 2^48). `Mapper::map` masks a wider `pa` silently; nothing can hand it
one today, and if that ever changes the mask is where to add the check.

Deliberately not proved: the `Mapper` round-trip (map a page, translate it back). This was
considered and declined, not skipped. Kani only pays off on *symbolic* inputs, and here both ends are
dead: a concrete-address round-trip is a unit test Kani happens to execute (no gain over the tests
already present), and a symbolic-address round-trip reasons over a built four-level page table, the
"BMC over real memory" case that walls the same way the ELF parser did. And the invariants of the
walk that actually matter, index-in-bounds, distinct pages take distinct paths, the lossless address
split, are *already* proved in the `paging` arithmetic harnesses above. So the round-trip would burn
the solver to re-cover proved ground or hit the wall. It stays covered by the host and kernel tests.

Five in `crates/frames/src/lib.rs`, the physical frame allocator:

| Harness | Property |
|---|---|
| `two_allocations_are_distinct` | over any bitmap, two back-to-back `alloc`s never return the same frame (the property isolation rests on: one physical page is never handed to two owners) |
| `an_allocated_frame_is_aligned_and_in_range` | an allocated frame is frame-aligned and within `[base, base + total*FRAME_SIZE)` |
| `index_of_inverts_frame_addressing` | frame address and bitmap index are inverses, so naming is unambiguous |
| `containing_rounds_down_within_a_frame` | `Frame::containing` returns an aligned frame that holds the address |
| `bitmap_bytes_covers_every_frame` | the bitmap is always sized to hold one bit per frame (no out-of-bounds in `get`/`set`) |

The allocator harnesses build a small allocator over a *symbolic* bitmap directly (the `#[cfg(kani)]`
module is inside the crate, so it can reach the private fields), rather than through `new`, which
fills the bitmap all-used. The scan loops are bounded by pinning `total = 8`, so `unwind(9)` suffices.

Four in `crates/dtb/src/lib.rs`, the device-tree parser's leaf readers (the whole-parse token loop
is the same BMC wall as ELF, so the leaves are what get proved):

| Harness | Property |
|---|---|
| `be32_is_total` / `be64_is_total` | the big-endian readers never panic for any offset, even `usize::MAX` |
| `be32_reads_big_endian_when_in_bounds` | an in-bounds read is exactly `bytes[at..at+4]`, MSB first |
| `align4_rounds_up_to_a_multiple_of_four` | the padding helper rounds up correctly for any realistic length |

`be32`/`be64` were *hardened* to reach totality: their `at + 4` / `at + 8` is now a checked add, so a
near-`usize::MAX` offset from a corrupt blob returns `Truncated` instead of panicking. The 12
integration tests against a real QEMU device tree are unchanged, so the hardening is faithful. This
is the elf lesson reused: prove (and here, harden) the loopless leaves; the walk stays on the tests.

Six in `crates/ipc/src/lib.rs`, the synchronous-rendezvous state machine (the decision core of
`sched.rs`'s `Endpoint`, extracted as pure logic; **restated over the intrusive queues** at
milestone 14 phase A.3, so the rewire did not demote proved code back to argued code: the same
six properties, now over real `intrusive::Fifo`s with TCB-shaped nodes, composing with the
`Fifo`'s own FIFO proof below):

| Harness | Property |
|---|---|
| `send_preserves_the_invariant` / `recv_...` / `signal_...` | every operation preserves "at most one wait queue is ever non-empty," the invariant the whole IPC design rests on |
| `send_rendezvous_iff_a_receiver_waited` | a send rendezvouses exactly when a receiver was waiting, else blocks (no dropped message, no spurious block) |
| `recv_drains_a_pending_signal_first` | a receive takes a pending async signal before a blocked sender, so a signal is never lost |
| `a_collected_sender_is_forgotten` | once a receive collects a blocked sender, the endpoint holds no name for it in either queue and no later receive can produce it again (the endpoint half of the one-shot Reply) |

These are inductive-step proofs: assume a valid state, apply one operation, check the invariant holds.
A non-empty queue is modeled with a single waiter (the decision and the invariant depend only on
whether a queue is empty, never its length), which keeps the `VecDeque` reasoning tractable.

**Phase 2 is done.** `kernel/src/sched.rs`'s six IPC functions no longer hand-roll the rendezvous
branch six times; they call `ipc::Endpoint<Tid>` (the same generic type, so the queues are the
kernel's real endpoint state, not a model kept in sync) for the *decision*, and spend their own code
only on the bookkeeping the queues cannot express: mailboxes, waking a thread onto a run queue, the
one-shot Reply that leaves a caller blocked. The full QEMU suite (102 tests, including the Call/Reply,
frame-delegation, and revocation tests) passes unchanged, so the rewire is faithful: the kernel's IPC
path *is* the proved logic now, not a parallel copy of it. This is the first place a proof reaches all
the way into the running kernel rather than staying in a host crate.

**Phase 3, the one-shot Reply, needed no rewire at all.** "One reply, to this caller, exactly once"
(DECISIONS §12) decomposes into three legs, and it is worth recording which kind of evidence each
one rests on:

1. **The endpoint forgets a collected caller**: `a_collected_sender_is_forgotten` in `crates/ipc`.
   A `CALL`er queues as a sender and blocks; the server's receive pops it destructively, so from
   that moment the kernel-minted Reply capability is the *only* name for the blocked caller
   anywhere in the system. (The caller is never in the receiver queue: `ipc_call` does not `recv`,
   and a blocked thread cannot run to enqueue itself again.)
2. **Consume-on-use is final**: `a_deleted_capability_stays_deleted` and
   `delete_touches_only_its_slot` in `crates/capability`. The syscall layer deletes the Reply capability
   the instant it is invoked; the proofs say no table state exists in which the consumed slot can
   be invoked again, and consuming one caller's Reply cannot disturb another's.
3. **The capability cannot be duplicated or delegated**: structural, not a harness. There is no
   syscall that copies a capability within a cspace (`CSpace::derive` is kernel-internal), and the
   only cap-moving syscall, `SEND_CAP`, requires `GRANT`, which `reply_cap` deliberately never
   mints. This leg lives in the shape of the syscall surface (§4: narrow and explicit), so it is
   an inspection argument, backed end-to-end by the QEMU test in which the call server invokes its
   Reply twice and the kernel refuses the second (`user/src/hello.rs`, `call_server`).

No rewire because `capability::CSpace` and `ipc::Endpoint` already *are* the kernel's cspace and endpoint
state; the proofs landed on code the kernel was running all along.

Three in `crates/slots/src/lib.rs`, the generational thread table (milestone 14 phase A; see
notes/generational-names.md):

| Harness | Property |
|---|---|
| `a_removed_name_never_resolves_again` | once removed, a name fails `get`/`get_mut`/`remove` forever, even after its slot is reused (the stale-Tid safety that capability payloads will lean on) |
| `live_names_are_distinct_and_resolve_to_their_own_entry` | the `(generation, slot)` packing cannot alias two live entries |
| `a_name_the_table_never_minted_resolves_to_nothing` | for any u64, resolution succeeds only on exactly a name the table issued |

One in `crates/intrusive/src/lib.rs`, the scheduler's queue structure (milestone 14 phase A.2;
see notes/intrusive-queues.md):

| Harness | Property |
|---|---|
| `any_push_pop_interleaving_is_fifo_and_lossless` | the real `Fifo`, driven by a six-step *symbolic* operation sequence over three nodes, agrees with a trivially-correct model at every step: FIFO order, no node lost or invented, lengths agree, and no stale link is dereferenced |

One harness rather than several because the operation-sequence shape subsumes the single-step
properties: a push-preserves-X proof is the sequence of length one.

Three in `crates/asid/src/lib.rs`, the TLB tag allocator (milestone 15; see notes/asids.md,
including which half of the ASID contract stays on a hardware witness test rather than a proof):

| Harness | Property |
|---|---|
| `the_kernel_asid_is_never_allocated` | no reachable state hands a user space ASID 0, the kernel's tag |
| `two_live_asids_are_distinct` | live allocations never alias, from any symbolic state |
| `free_releases_exactly_its_own_asid` | free clears its own bit and no other |

Four in `crates/elf/src/lib.rs`:

| Harness | Property |
|---|---|
| `check_segment_bounds_never_panics` | the per-segment bounds/overflow arithmetic never panics, for any file length and any hostile field values |
| `a_passing_check_yields_an_in_bounds_range` | if the check passes, `p_offset <= end <= file_len`, so the segment's data slice is in bounds (what the whole-parse totality proof was really reaching for) |
| `a_passing_check_has_no_address_overflow` | if the check passes, `vaddr + memsz` did not wrap, so `validate`'s later unchecked add cannot panic |
| `page_range_is_panic_free_and_ordered` | for any `vaddr`/`memsz`, the saturating page arithmetic neither panics nor returns an inverted range (a `pub` helper that must be safe on its own) |

Two in `crates/crickerfs/src/lib.rs`, the initrd parser the kernel runs on boot input
(the archive format the initrd and disk images use; kept over tar by the reuse record in
notes/prior-art.md, and proved because the kernel-side parse is TCB code):

| Harness | Property |
|---|---|
| `the_validation_implies_reads_slice_is_in_bounds` | for every entry value and image length, parse's acceptance check makes `read`'s slice arithmetic safe: no panic, bytes inside the image |
| `a_short_image_is_refused_not_indexed` | any image under one block is `Truncated` before a byte past the length check is touched |

Whole-parse totality hit the same wall as ELF and dtb below (a one-block symbolic image put
CBMC past 20 CPU-minutes), and was decomposed the same way; the module comment records what is
deliberately unproved and why it is sound anyway.

Four in `crates/pci/src/lib.rs`, the config-space decode the kernel runs on **device** input
(a hostile or broken PCI function can answer the closures with anything):

| Harness | Property |
|---|---|
| `ecam_offset_stays_inside_the_window` | any BDF's config page lies inside the 256-bus ECAM window, so the kernel's volatile accessors cannot escape a correctly-sized mapping |
| `intx_irq_is_total_and_bounded` | the swizzle is total (the pin-0 underflow that panicked debug builds is gone, hardened with saturating arithmetic) and lands within `base..=base+3` |
| `read_bars_is_total_for_any_device` | the BAR size probe never panics on garbage device answers (`!mask + 1` cannot overflow: the type bits are masked first) |
| `the_capability_walk_terminates_on_any_device` | a capability list forming ANY graph, cycles included, is walked at most 64 hops; the bounded-walk discipline proved rather than argued |

Seven in `crates/dma_validator/src/lib.rs`, the DMA-confinement validator (milestone 35). This is the
last isolation boundary in the system that was attacker-tested but never proved. It confines a
userspace virtio driver's DMA: on every `NOTIFY` the kernel walks the driver's descriptors, refuses
any whose buffer escapes the driver's granted region (or is indirect), and copies the validated ones
into a kernel-private **shadow ring** the device reads, so the driver cannot touch what the device
acts on. The logic was lifted out of `kernel/src/virtio.rs::validate_and_shadow` (which now calls it)
so it could be proved, the same Phase-2 move `regions` and `ipc` made; the kernel's QEMU attacker
suite (the DMA-escape and indirect-escape end-to-end tests, on both ISAs) is unchanged and green, so
the extraction is faithful.

| Harness | Property |
|---|---|
| `in_region_is_sound` | the confinement predicate is sound and total: for every base/size/addr/len, if `in_region` accepts then `base <= addr` and `addr + len <= base + size` with no overflow (direction-agnostic, so it underwrites both TX device-reads and RX device-writes) |
| `an_accepted_descriptor_is_confined` | for every descriptor bit pattern (flags fully symbolic, so the device-writable RX bit is covered) and every region, an accepted descriptor is not indirect and its whole buffer is in-region |
| `validate_and_shadow_confines_every_chain` | **the main theorem**: over a fully symbolic driver descriptor table and region, no descriptor the walk copies into the shadow is ever out-of-region or indirect, so the device only ever reads confined descriptors. Symbolic-index-bounded, so it also proves the walk never reads or writes past a ring |
| `an_oversized_batch_is_refused` | a batch claiming more than `qsize` new entries is refused before a single descriptor is read or written (the DoS bound on the outer loop; the memory closures panic if called) |
| `a_descriptor_mutated_after_validation_cannot_reach_the_device` | the shadow ring closes the time-of-check/time-of-use race: after a validated copy, the driver aiming its own descriptor at any address cannot change what the device reads from the shadow |
| `the_outer_walk_stays_inside_the_rings_and_terminates` | the loop that feeds the chain walk: for **every** `(from_idx, to_idx)` pair, wraparound included, every ring access lands inside its own ring and the loop terminates |
| `distinct_queues_occupy_disjoint_blocks` | multi-queue isolation (milestone 30): for any two distinct in-range queues, one queue's whole ring area ends before the other's block begins, and a queue's descriptor table ends before its own available ring begins |

The main theorem proves the confinement core, `shadow_one_head`, which the write closure instruments
so it asserts "in-region and not indirect" the instant each descriptor lands in the shadow. It is
proved for **one** newly-published head, not the whole ring, and that is a decomposition, not a
sample: the invariant is checked on *every* shadow write, and one head's chain already writes up to
`qsize` fully symbolic descriptors, so the per-write property is quantified over arbitrary descriptor
content and position; the outer loop only repeats that validated processing for each further head
(its bound the separate `an_oversized_batch_is_refused`), reaching no new descriptor state. Batching
the whole ring pushed the SAT formula to `qsize * qsize` symbolic reads and out of a practical
`script/verify` budget (three minutes) for no added coverage; the single-head form verifies in ~20s.

### The bounds, and why each one is adequate

Bounded model checking means somebody chose the bounds, and a proof whose bounds hide the interesting
case reads as stronger than a test while being worth less. So, each bound in the DMA harnesses, stated
with its justification:

| Bound | Value | Why it is adequate |
|---|---|---|
| queue size (`QS`) | 8 | **It is the system's own bound, not a proof convenience.** `dma_validator::LAYOUT_QSIZE` is the kernel's `QSIZE`, `setup_queue` refuses `num > QSIZE`, and the kernel now *aliases* the crate's constant rather than keeping a copy. So the proof is over the shipping configuration, and no larger ring can exist to be unproved. |
| chain length | ≤ `qsize` = 8 | The walk is `for _ in 0..qsize`, and a chain cannot usefully be longer: there are only 8 descriptors, so any longer walk is revisiting one. A **cycle** is therefore covered rather than excluded: `next` is fully symbolic, so `0 → 1 → 0 → …` is among the proved inputs, and the loop bound is what makes it terminate instead of hanging. |
| loop unrolling | `unwind(10)` / `unwind(11)` | One more than each loop can need, so Kani's *unwinding assertion* is part of the proof: if any input could drive a loop longer, verification fails. That turns the bound from an assumption into the **termination proof**. Checked by falsification: delete `validate_and_shadow`'s batch-size guard and the unwinding assertion fails at iteration 11. |
| batch size | ≤ `qsize` | Proved as a property (`an_oversized_batch_is_refused`), not assumed: a claim of more than `qsize` new entries is refused before a single read. |
| queue count | `MAX_QUEUES` = 2 | Compile-time asserted in the kernel (`MAX_QUEUES * RING_BLOCK <= FRAME_SIZE`, one shadow frame per device) and enforced at runtime (`setup_queue`/`notify` refuse `queue >= MAX_QUEUES`). |
| region base/size, descriptor `addr`/`len`/`flags`/`next`, ring indices | **unbounded** | Fully symbolic `u64`/`u16`. Every attacker-controlled value is unconstrained, which is the point: the bounds above are all structural (how many slots a ring has), never a restriction on what an attacker may write into one. |

The one place the composition is an argument rather than a single harness, said plainly: "the whole
batch is confined" follows from the per-head theorem, the per-write invariant, the outer-loop bound,
and the ring-bounds harness *taken together*. Each leg is proved; joining them is a reading of four
harnesses, not a fifth harness. That is the same shape as the one-shot Reply's three legs below, and
it is recorded for the same reason.

#### Non-vacuity: `kani::cover!`, and why a bound needs it

A bound raises a question an assertion cannot answer. If the harness's assumptions turned out to be
jointly unsatisfiable, or a bound quietly excluded the interesting shape, every assertion would pass
and the harness would prove **nothing** while reporting `SUCCESSFUL`. This note has always named that
as the main failure mode ("it proves what you asserted, not what you meant"), and until milestone 35
the only defence was reading the harness carefully.

`kani::cover!(condition)` inverts the question: it **fails when the condition is unreachable**. So it
turns "this harness really does exercise the case I claim" from a reading into a result. Milestone 35
introduces it, with four cover properties where the risk was real:

- `the_outer_walk_stays_inside_the_rings_and_terminates` covers that a **wrapped** batch (`to < from`)
  is reachable, that a wrapped batch is *walked to completion* rather than merely refused, that some
  batch is accepted (so the harness is not vacuously refusing everything), and that some batch is
  refused (so the guards are reachable). Without those, "wraparound is covered because `from` and `to`
  are unconstrained" would be an inference about the code rather than a checked fact.
- the two domain harnesses that stack assumptions cover that a **multi-page** grant satisfies them, so
  neither is quantifying over an empty set and neither has `i` pinned to zero.

The cheap general rule this suggests: **any harness with more than a couple of `kani::assume`s, or any
harness whose interesting case is a corner of an unconstrained input, should carry a `cover!` for that
case.** It costs no solver time worth measuring and it is the only thing that catches a vacuous proof.

### The IOMMU domain: proved where it can be, tested where it cannot

Milestone 35's third item was to confirm the IOMMU domain builder
(`paging::domain::build_identity_domain`, milestone 16b) has a *maps-exactly-the-grant* proof, the
hardware sibling of the validator property (the device's DMA domain maps precisely the granted frames
and nothing else). The first pass at the milestone **declined** it: the property is a `Mapper`
build-and-translate round trip (a symbolic IOVA walking a *built* four-level table), which is the
BMC-over-real-memory wall this note already declined for the ELF parser and for `Mapper` itself.

That was the right diagnosis of the wrong target, and the correction is on the record because it is
this note's own rule being applied: **prefer refactoring the logic to shrinking the proof.** The
domain is "an identity map over exactly the granted pages," and the *page set* is loopless arithmetic
that needs no tables at all. Factored out (`grant_pages`, `grant_page`) it proves in a quarter of a
second, and the builder now calls it instead of `map_range`'s unchecked `va + i * PAGE_SIZE`, so the
proved page set is the page set that runs.

Six in `crates/paging/src/domain.rs`:

| Harness | Property |
|---|---|
| `an_enumerated_page_lies_inside_the_grant` | **soundness, the security direction**: every page the domain maps is page-aligned and lies wholly inside a granted region, so no ungranted byte becomes device-reachable (and since IOVA == PA, no ungranted physical memory is translatable) |
| `every_whole_page_of_the_grant_is_enumerated` | **completeness, the functional direction**: every whole page of a grant is mapped, proved *constructively* (the witness index is `(iova - base) / PAGE_SIZE`, so it says which iteration maps it) |
| `the_enumeration_is_injective` | no page is enumerated twice, so a legal grant cannot fail its own build against `AlreadyMapped` |
| `the_grant_enumeration_is_total` | neither entry point panics or overflows, for any base, size, or index |
| `a_page_index_below_the_count_always_resolves` | the builder's defensive `ok_or` is dead code, provably |
| `a_grant_the_domain_cannot_express_is_refused` | the two inputs that could produce an over-map (an unaligned base, an end that wraps `u64`) are refused, so the builder maps nothing rather than something rounded |

Completeness is there because without it the property is half a property: **a domain that mapped
nothing at all would satisfy soundness perfectly** and confine the device by starving it, surfacing
as a mysterious device fault rather than a refusal. Proving both directions is what makes
"*exactly* the grant" mean what it says.

One proof covers **both** IOMMUs, which is the right shape for a §19 parity gate: the page set does
not depend on the page-table format, so the same harnesses underwrite the SMMUv3 (VMSAv8-64) domain
on aarch64 and the RISC-V IOMMU (Sv39) domain on riscv. No second harness, no parity gap.

**The residual, named rather than implied.** These prove the page set the builder *asks* the mapper
for. They do not prove "`Mapper::map` writes exactly one leaf for the page it is told and touches
nothing else". That is the build-and-translate round trip, and it stays on the wall. It is
underwritten by the proved walk arithmetic (`distinct_pages_take_distinct_paths`, so an ungranted page
cannot alias a granted leaf; `index_is_always_in_bounds`; the leaf codec keeping address and
permissions apart, proved for *both* formats in `aarch64.rs` and `sv39.rs`; the two-halves-disjoint
gate) plus `domain.rs`'s build-and-translate tests on both formats (`aarch64_domain_confines_a_region`,
`sv39_domain_confines_a_region`, `two_disjoint_regions_map_and_the_gap_does_not`) and milestone 16b's
end-to-end attacker test in which the hardware faults an escaping DMA. So: the page set is proved,
the mapper writing it faithfully is tested and composed. That is the honest line, and it is a better
line than the first pass drew.

**Every one of these properties was falsified before it was believed.** Round the page count up and
soundness fails; round it down and completeness fails while soundness correctly still holds (an
under-map is safe); drop the wrap refusal and soundness fails. One falsification corrected a claim in
the code: soundness rests on `grant_pages` **flooring**, not on `grant_page`'s partial-page guard,
which cannot fire for any index the builder passes. The comment there now says so, because a reader
hardening the wrong line would have thought the guard was the load-bearing one.

### What the DMA proof does NOT establish: addresses that never enter a descriptor

This is the part to read before repeating "DMA confinement is proved," because said without it that
sentence is wrong in a way that matters.

**The proof is about descriptor chains.** `validate_and_shadow` sees the descriptors a driver
publishes in a virtqueue, and the harnesses above quantify over every one of them. That is the whole
address surface for a disk and for a NIC: every byte those devices touch is named by a descriptor the
kernel validated and copied into a shadow the driver cannot reach.

**It is not the whole address surface for a GPU.** Milestone 29 found this and DECISIONS §29 records
it: virtio-gpu's *backing* addresses ride inside a `RESOURCE_ATTACH_BACKING` **command payload**, not
in a descriptor. The kernel bounds the descriptor carrying that command, so the payload is in-region
bytes; the addresses *inside* it are bytes the transport does not parse. The validator therefore
**structurally cannot see them**, and no amount of proving it harder changes that: the addresses are
not in its input. Teaching it to parse them would push virtio-gpu knowledge into the layer DECISIONS
§18 keeps device-neutral, and would start a per-device arms race with the next device class that
carries addresses in a payload.

So the two paths have genuinely different evidence, and conflating them is the error to avoid:

| Path | What confines it | Strength of the evidence |
|---|---|---|
| Addresses in **descriptors** (disk, NIC, and the GPU's own command ring) | the shadow-ring validator, plus the IOMMU where present | **machine-checked for every input** (`crates/dma_validator`), plus end-to-end attacker tests on both ISAs and both transports |
| Addresses in a **command payload** (virtio-gpu backings) | the IOMMU, and *only* the IOMMU | **the barrier's allow-list is proved exact; the hardware honouring it is attacker-tested.** `an_enumerated_page_lies_inside_the_grant` and `every_whole_page_of_the_grant_is_enumerated` prove the domain maps exactly the granted pages, which is the property that makes an out-of-grant payload address untranslatable; `the_iommu_refuses_the_gpu_a_framebuffer_outside_the_drivers_grant` then points a backing at a frame left out of the domain and asserts the IOMMU's fault queue recorded a fault there, on both ISAs |

The middle column of that second row is the one useful thing this milestone could prove about the payload
path, and it is worth naming rather than leaving as a side effect of item 3. A payload-borne address is
stopped by having **no translation in the device's domain**, so "the domain maps exactly the grant" is
exactly the property that barrier rests on. Proving it moved the payload path from "tested end to end" to
"the allow-list is proved exact, the hardware honouring it is tested end to end". A narrowing, not a
closing: the transport still cannot see these addresses, and the enforcement is still the hardware's.

And the consequence that made milestone 35 load-bearing in the first place cuts the other way here.
The reason to prove the validator now, rather than later, is that **milestone 16a's board has no
IOMMU** (the VisionFive 2; notes/target-hardware.md), so on first silicon the validator stops being
defence in depth and becomes the sole DMA confinement. That argument works for the descriptor path
precisely because the validator covers it. For the payload path it inverts: on a board with no IOMMU,
**nothing covers it.** Not the validator (the addresses are not in its input), not the hardware (there
is none). A display driver on the VisionFive 2 is therefore either *trusted* with all of physical
memory, or the transport grows a virtio-gpu-aware check and pays the §18 cost knowingly. That is a
decision for whoever sequences 16a; what this note owes them is that it is a decision and not an
oversight. The same holds under HVF, where PCIe DMA runs unconfined by standing default.

Stated for a skeptic in one sentence, which is how it should be stated: *every address that reaches a
device through a virtqueue descriptor is provably confined to the driver's grant; addresses that reach
a device inside a command payload are confined by the IOMMU alone, that confinement is tested rather
than proved, and on a board without an IOMMU it does not exist.*

## The calendar, and where BMC's cost actually is

Eleven in `crates/calendar/src/lib.rs`, milestone 51's civil-date arithmetic (see notes/calendar.md
for the crate itself). It looks like the ideal target: closed-form integer arithmetic, no loops, no
tables, no allocation, and a round-trip property that is a single equation. Half of that turned out
to be true, and the half that did not is the useful part of this entry.

| Harness | Property |
|---|---|
| `the_calendar_algorithms_are_mutual_inverses` | **the central theorem**: for every one of the 3,652,425 days in the supported range, `civil_from_days` then `days_from_civil` returns the day it started from. The leap-year rules, the month lengths, the century exception and its exception all live inside those two functions, and a bijection cannot have them wrong in a way that cancels |
| `a_day_number_always_decodes_to_a_real_date` | every day number decodes to a date that *exists*: month 1..=12, day within that month's length in that year, and the validating constructor accepts the result. This is what stops the bijection from being vacuous (a decoder that consistently invented February 30 would still be one) |
| `every_real_date_survives_its_own_day_number` | the other direction, over fully symbolic year/month/day/hour/minute/second: every field combination the constructor accepts round-trips through its day number, and its timestamp lands inside the supported range |
| `later_days_are_later_dates` | monotonicity: for any two days, the earlier decodes to a date that sorts earlier under the derived `Ord`. What makes `Format::Date` sortable text, and what rules out a boundary (month end, leap day, century) stepping the calendar backwards for one day |
| `day_of_year_is_bounded_and_366_means_leap` | day of year is 1..=366, and 366 **iff** 31 December of a leap year |
| `weekdays_advance_one_day_at_a_time` | consecutive days differ, the ISO number steps 1..=7 *in order*, and seven days on is the same weekday: the cycle, not merely the change |
| `unix_to_civil_and_back_is_the_identity` | the seconds round trip, over a four-year window straddling the epoch (below) |
| `every_format_is_ascii` | all five formats, for every representable date at every legal offset: within the fixed buffer, never truncated, every byte printable ASCII |
| `the_unix_format_fits_any_i64` | the decimal writer fits any `i64`, `i64::MIN` included, in 20 bytes |
| `parse_is_total_on_hostile_bytes` | the RFC 3339 parser never panics on **arbitrary bytes** at any length up to 26: no UTF-8 assumption, no ASCII assumption. A `date -s` argument and an NTP-adjacent exchange are both text this program did not write |
| `rfc3339_output_parses_back_to_itself` | everything the crate prints, it reads back, for every representable `DateTime`: same instant *and* same offset (the equality is on the whole value, so an offset silently normalised away fails it) |

### The finding: the arithmetic is cheap and the `&str` boundary is not

Two costs surprised us, in opposite directions from the guess.

**A 64-bit division by 86,400 is the expensive part of a calendar.** The Hinnant algorithms round-trip
over the entire ten-thousand-year range in under a minute. Add one `div_euclid(86_400)` over a
symbolic 64-bit timestamp and the *same* property does not finish in twenty minutes, or with kissat
instead of CaDiCaL. That is not about calendars: a bit-blasted 64-bit divider with an unconstrained
dividend is close to the worst thing you can hand a CDCL solver, and the divisions inside
`civil_from_days` are cheap only because the era arithmetic keeps their operands small. Measured, in
the same harness at different widths: one day of timestamps 3s, four years 64s, the full range
neither in twenty minutes nor with a different solver.

So the harnesses are **factored along that seam** rather than bounded uniformly. The calendar half
(days to a date, and everything derived from a day number) is proved over the full range, unbounded
below the type. The seconds half, which is the only place the expensive division appears, is proved
over 1968-01-01 through 1971-12-31. That window is chosen, not convenient: the risk it carries is
"did someone write truncating division where Euclidean was needed", which is a property of the *code*
and shows up only on a negative timestamp (in Rust `-1 / 86400` is `0`, so the naive version reports
1970-01-01 for every instant in the last day of 1969). The window straddles the epoch, contains
negative seconds, zero and positive seconds, a leap day and three year boundaries, and `kani::cover!`
checks that the first two are actually reachable inside it rather than argued to be. The composition
is stated in the crate: `from_unix` is `civil_from_days(secs.div_euclid(86_400))` plus a time of day,
and `to_unix` is `days_from_civil(...) * 86_400` plus the same time of day, so the two halves join.

**Iterating a slice whose length is symbolic costs more than the parser it wraps.** Three harnesses
originally went through `&str`, and each ran past ten minutes *in symbolic execution*, before the
solver saw anything. The cause was not the calendar or the grammar: it was `core::str::from_utf8`
and `for &b in slice` over a variable-length slice, which makes CBMC branch on the length at every
step. Two changes fixed it and both improved the code rather than weakening the proof:

- **`Formatted`'s ASCII check became an index loop** over the fixed 32-byte buffer with an `i < len`
  guard, and the "therefore `from_utf8` succeeds" step became one line of composition (ASCII is a
  subset of UTF-8) instead of a proof through the standard library's validator. Ten minutes and
  counting became 130 seconds, for a property that now covers every representable date at every
  offset rather than one day.
- **The parser grew a byte-level entry point**, `parse_rfc3339_bytes`, with the `&str` version as its
  wrapper. RFC 3339 *is* ASCII, so bytes are what the grammar is defined on, and a caller holding a
  network buffer no longer has to validate UTF-8 for a function that rejects every non-ASCII byte
  anyway. Totality went from ten minutes-plus to 17 seconds, **and got stronger**: it now quantifies
  over arbitrary bytes including sequences that are not UTF-8 at all, which is exactly the input a
  network client will hand it. The print-then-parse round trip went from not finishing to 38 seconds
  over the full range.

The general lesson, which is the same one the ELF parser taught in a different costume: when BMC
stalls, look at what the harness *touches* rather than at what it is trying to prove. Here the
property was fine and the plumbing was expensive.

A third cut came free from restating a theorem rather than weakening it. Monotonicity was written
over an arbitrary pair of days and cost 228s, because a second symbolic day number buys a second copy
of the whole decode. Written over **adjacent** days it costs 38-50s and is the same theorem: the
order is transitive, so `d(n) < d(n+1)` for every `n` gives `d(a) < d(b)` for every `a < b`. The
general property, phrased as its induction step.

The crate verifies in **about seven minutes** on an M-series laptop, which is the largest single
entry in `script/verify` and is worth knowing before adding to it. The two costs that dominate are
`every_format_is_ascii` (150-170s, five formats over every representable date at every offset) and
`unix_to_civil_and_back_is_the_identity` (81-84s, the one harness that pays for the 86,400 division).
Timings vary by 10-20% with host load; those are from two full runs.

### Falsified before believed

Per the rule below, the two properties that carry the crate were broken on purpose and the harnesses
caught both:

| Break | Harness | Result |
|---|---|---|
| `is_leap_year` reduced to `year % 4 == 0`, dropping both century clauses | `every_real_date_survives_its_own_day_number` | FAILED in 8s. The constructor accepts 1900-02-29, the day number decodes back to 1900-03-01, and the round trip is not the identity |
| `div_euclid`/`rem_euclid` replaced with `/` and `%` in `from_unix` | `unix_to_civil_and_back_is_the_identity` | FAILED in 32s, on the hour-bound assertion: a negative remainder makes the hour negative before the cast. This is the bug the window straddling the epoch exists to catch, and it is caught |

## Where BMC hit a wall: the ELF parser

The goal for `elf` was the big one: prove `Elf::parse` *total*, that no byte string, however hostile,
makes it panic. A parser over attacker-controlled input is the textbook case for it, and a panic
there is a crafted binary halting the kernel. It did not work, and the reason is worth keeping.

Two things put it past bounded model checking:

1. **A loop Kani bounds too loosely.** `parse` has an `O(n^2)` overlap check over up to
   `MAX_PHNUM = 64` program headers. The real bound is far tighter (the header table must fit in the
   file, which at any small input size allows one or two headers), but that bound is *nonlinear*
   (`phoff + phnum * phentsize <= len`). Kani uses the *linear* `phnum <= 64` cap it can see for the
   unwinding assertion, so it insists on unrolling 64 deep, and `unwind(65)` did not return in 7+
   minutes.
2. **Symbolic slice offsets.** `phoff` and each segment's `p_offset` come out of the file, so the
   reads land at *symbolic positions* in a symbolic array. That is expensive for the solver's memory
   model, and it did not return even after pinning the header count to a single segment to kill the
   loop.

So *whole-parse* totality is deferred. But the first path forward turned out to recover most of what
it was for, so it is worth following the story to its end rather than stopping at the wall:

- **Factor the leaf arithmetic into a pure function, and prove that.** Done. The per-segment bounds
  and overflow checks are now `check_segment_bounds`, a loopless function over a header's raw fields
  and the file length, and the three harnesses above prove it never panics, that a passing check
  yields an in-bounds range (`p_offset <= end <= file_len`, which is what makes `segment_at`'s slice
  safe), and that a passing check rules out the `vaddr + memsz` overflow. That is the actual panic
  surface, proved for every input, without ever touching the loop. The refactor left the tests
  unchanged, so it is faithful.
- **A loop-invariant tool (Verus)**, if the *loop itself* (the `O(n^2)` overlap check) ever needs
  proving rather than just the arithmetic inside it. Not needed yet.
- **Shrink `MAX_PHNUM`.** Changing product code to suit the prover; still the last resort.

The lesson, kept: BMC blunted against the loop and the symbolic slice base, and the fix was not a
bigger hammer but a smaller target. Decomposing the risky arithmetic out of the loop moved it from
"the solver never returns" to "verified in under a second." What remains unproved is narrow and
named: that the *number* of segments and their mutual overlap are handled without panic across all
64 possible headers, which the by-example tests still cover.

## The glob matcher, and the two things that made it tractable

Six in `crates/glob/src/lib.rs`, milestone 47's pattern matcher (see [glob.md](glob.md) for the crate
itself, which carries the harness table). The target is a loop over two byte strings where the
pattern is untrusted, so the property that matters is the one BMC is best at: **totality**, no panic
and no hang on any input.

It also produced two findings worth having next to the calendar's, because both are about the same
thing: **the cost of a proof is the shape of the code, not the size of the claim.**

- **Two loops became one, and the claim did not move.** The first version found a bracket
  expression's closing `]` in one loop and tested membership in another, nested inside the match loop
  Kani was already unrolling. One harness reached 3.5 GB and twelve minutes before it was killed.
  Scanning the class once, deciding membership as it goes, removed a whole loop from the unrolling
  and is less work at runtime too. DECISIONS §46 rule 1 in one edit.
- **An unwind bound too high is as expensive as a claim too big.** These harnesses were first written
  with `#[kani::unwind(60)]`, picked from a loose algebraic bound. The measured worst case over the
  same domain is **10**. A host test now enumerates that domain and pins the number, so the unwind
  bounds are derived from a measurement rather than from arithmetic on a worst case that cannot
  happen. Every outer iteration charges at least one step, which is what makes the measured step
  count a sound upper bound for the iteration count.

Kani also falsified a *harness* here rather than the code: with a fully symbolic class body, `[!y]`
is already a negated class, so "`[xy]` and `[!xy]` are complements" is false when `x` is `!`. It came
back in 42 seconds with the counterexample. Worth recording because the reflex on a red harness is to
suspect the code.

**The cost, stated rather than discovered.** About ten minutes of solver time for the six, which puts
`glob` second to `calendar` in this suite. Two thirds of it is the two harnesses that quantify over a
symbolic-length pattern **and** a symbolic-length name at once, which is the calendar's finding again
from a different direction: the expensive thing is not the property, it is the second symbolic
length. Cutting one harness's name bound from three bytes to two took it from 279s to 199s without
weakening it, because that harness's rule is a predicate on the name's first byte.

## Running it

```
script/verify
```

Self-installs Kani on first run (its own nightly toolchain and a CBMC backend, a minute of
download), then runs `cargo kani` over every crate carrying harnesses: **107 harnesses across 19
crates** as of 2026-08-03. (This line said 67 for a while after it was 69, then "a few minutes" for
a month after that stopped being true; both counts and the timing are now measured, not remembered.)
**Measured 2026-08-03 on the Apple Silicon dev machine: 21m40s wall for the full suite**, warm
install, exit clean; CI's ubuntu-arm runner takes ~42 minutes for the same work. Not in
`script/bootstrap`, because the kernel build does not need it; same self-install pattern as
`script/coverage`. A new proof crate goes in that script's list, and a new harness in an existing
crate is picked up with no change.

`script/verify --affected-since <base>` answers a different question without proving anything: can
the diff since `<base>` reach a proof at all? The proofs are a function of the harness crates and
their transitive dependencies, so the script asks `cargo metadata` for that closure and classifies
every changed file; only a change confined to documentation, to workflow files other than
verify.yml, or to crates outside the closure (the kernel and the user programs, which no proof
compiles) reports `not-needed`. Anything it cannot attribute, the workspace manifests and lockfile
included, runs the proofs by default. `.github/workflows/verify.yml` reads the last line, which is
how a kernel-only pull request stops paying the 42 minutes.

## The rules that keep proofs cheap and honest

- **Proofs live behind `#[cfg(kani)]`.** An ordinary `cargo build`/`cargo test` never compiles them,
  and the crate needs no dependency on `kani` (its intrinsics are injected only under `cargo kani`).
- **Verify pure logic first.** The §7 host crates (`capability`, `paging`, `elf`, `frames`, the ASID
  allocator when it lands) are the frontier: small, allocation-light, already host-compiled. Bounded
  model checking is happiest there.
- **Spread inward from the capability core**, the order §14 sets: `capability`, then IPC (rendezvous,
  one-shot reply), then the MMU isolation invariants. **All three steps are done** (milestone 18);
  each proved a property the security story previously rested on by argument. The frontier now
  moves with milestone 14: proving properties *of the kernel* at scale wants a kernel that does
  not allocate.
- **A harness that needs a huge bound is a design smell.** If a property needs Kani to explore an
  unbounded loop or a giant structure, that is often the code telling you the logic is not as local
  as it should be. Prefer refactoring the logic to shrinking the proof. **This applies to *declining* a
  proof too**, which milestone 35 learned the hard way: the IOMMU domain property was written off as the
  build-and-translate wall, and the wall was real but it was not where the property lived. Before
  recording a proof as impossible, check whether a smaller target carries it.
- **Falsify a property before believing it.** Break the code the harness guards and confirm the harness
  fails. Every milestone 35 property was falsified this way, and one falsification corrected a claim in
  the code (the load-bearing guard was not the one the comment pointed at). A harness that cannot be
  made to fail is not evidence.
- **Guard against vacuity with `kani::cover!`.** Assumptions and bounds can silently empty a harness's
  input set, and a vacuous harness reports `SUCCESSFUL`. A `cover!` fails when a state is unreachable,
  so it is the one check that catches this. See the non-vacuity section above.
