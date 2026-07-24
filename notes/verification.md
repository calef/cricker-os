# Machine-checked proofs (Kani)

The companion to the verification thesis (DECISIONS §14). That decision says *why* we verify; this
note is *how*, and the record of the experiment that green-lit it.

## Tests sample; proofs quantify

The `caps` tests check the cases we thought to write: READ cannot become WRITE, an empty slot is
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

The `paging` and `caps` harnesses have no unbounded loops (the four levels are literally four), so
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
   the pure-logic crates (`caps`, `paging`'s arithmetic) and not the `arch/` assembly: the model is
   faithful where there is no hardware and no `unsafe`. It is also why §14 promises a *small verified
   TCB with an unverified layer beneath it*, not a proof of the whole machine. **Concurrency is the
   sharpest edge of this limit**: every queue and endpoint proof here is single-threaded, and the
   wake-before-switch-out race (notes/intrusive-queues.md) lived precisely in the SMP interleaving
   those proofs cannot see. Green harnesses and a real race coexisted; the flaky test found it.
3. **The tool is trusted.** Kani, its CBMC backend, and the SAT solver could have bugs. They are
   small and widely used, and the solver emits a checkable certificate, but it is a trust assumption.
   seL4 minimizes even its proof checker; we do not, and that is a stated limit.

## What is proved today

Seven harnesses in `crates/caps/src/lib.rs`, under `#[cfg(kani)]`:

| Harness | Property |
|---|---|
| `subset_is_reflexive` | every capability is a subset of itself |
| `subset_is_transitive` | rights cannot be laundered through a derivation chain (why a *flat* subset check suffices, with no tree walk) |
| `from_bits_cannot_forge_a_right` | an attacker-controlled syscall register cannot conjure an undefined right |
| `subset_matches_allows` | the two phrasings of the order agree, so a bug in one shows against the other |
| `derive_never_widens_rights` | the central theorem, on the real `CSpace::derive` |
| `a_deleted_capability_stays_deleted` | for every table state, once `delete` succeeds the slot answers `NoSuchSlot` to both `get` and a second `delete` (the consume-on-use mechanism behind the one-shot Reply) |
| `delete_touches_only_its_slot` | deleting any slot leaves every other slot exactly as it was (consuming one caller's Reply cannot orphan another's) |

The last two run over a *symbolic* table (every slot independently empty or holding a capability
with symbolic object and rights), so "no state exists in which a consumed slot works again" is
quantified over table states, not sampled.

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
milestone 14 phase A.3, so the rewire did not demote proved code back to argued code — the same
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

1. **The endpoint forgets a collected caller** — `a_collected_sender_is_forgotten` in `crates/ipc`.
   A `CALL`er queues as a sender and blocks; the server's receive pops it destructively, so from
   that moment the kernel-minted Reply capability is the *only* name for the blocked caller
   anywhere in the system. (The caller is never in the receiver queue: `ipc_call` does not `recv`,
   and a blocked thread cannot run to enqueue itself again.)
2. **Consume-on-use is final** — `a_deleted_capability_stays_deleted` and
   `delete_touches_only_its_slot` in `crates/caps`. The syscall layer deletes the Reply capability
   the instant it is invoked; the proofs say no table state exists in which the consumed slot can
   be invoked again, and consuming one caller's Reply cannot disturb another's.
3. **The capability cannot be duplicated or delegated** — structural, not a harness. There is no
   syscall that copies a capability within a cspace (`CSpace::derive` is kernel-internal), and the
   only cap-moving syscall, `SEND_CAP`, requires `GRANT`, which `reply_cap` deliberately never
   mints. This leg lives in the shape of the syscall surface (§4: narrow and explicit), so it is
   an inspection argument, backed end-to-end by the QEMU test in which the call server invokes its
   Reply twice and the kernel refuses the second (`user/src/hello.rs`, `call_server`).

No rewire because `caps::CSpace` and `ipc::Endpoint` already *are* the kernel's cspace and endpoint
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

## Running it

```
script/verify
```

Self-installs Kani on first run (its own nightly toolchain and a CBMC backend, a minute of
download), then runs `cargo kani -p caps`. Not in `script/bootstrap`, because the kernel build does
not need it; same self-install pattern as `script/coverage`.

## The rules that keep proofs cheap and honest

- **Proofs live behind `#[cfg(kani)]`.** An ordinary `cargo build`/`cargo test` never compiles them,
  and the crate needs no dependency on `kani` (its intrinsics are injected only under `cargo kani`).
- **Verify pure logic first.** The §7 host crates (`caps`, `paging`, `elf`, `frames`, the ASID
  allocator when it lands) are the frontier: small, allocation-light, already host-compiled. Bounded
  model checking is happiest there.
- **Spread inward from the capability core**, the order §14 sets: `caps`, then IPC (rendezvous,
  one-shot reply), then the MMU isolation invariants. **All three steps are done** (milestone 18);
  each proved a property the security story previously rested on by argument. The frontier now
  moves with milestone 14: proving properties *of the kernel* at scale wants a kernel that does
  not allocate.
- **A harness that needs a huge bound is a design smell.** If a property needs Kani to explore an
  unbounded loop or a giant structure, that is often the code telling you the logic is not as local
  as it should be. Prefer refactoring the logic to shrinking the proof.
