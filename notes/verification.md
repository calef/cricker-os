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
   TCB with an unverified layer beneath it*, not a proof of the whole machine.
3. **The tool is trusted.** Kani, its CBMC backend, and the SAT solver could have bugs. They are
   small and widely used, and the solver emits a checkable certificate, but it is a trust assumption.
   seL4 minimizes even its proof checker; we do not, and that is a stated limit.

## What is proved today

Five harnesses in `crates/caps/src/lib.rs`, under `#[cfg(kani)]`:

| Harness | Property |
|---|---|
| `subset_is_reflexive` | every capability is a subset of itself |
| `subset_is_transitive` | rights cannot be laundered through a derivation chain (why a *flat* subset check suffices, with no tree walk) |
| `from_bits_cannot_forge_a_right` | an attacker-controlled syscall register cannot conjure an undefined right |
| `subset_matches_allows` | the two phrasings of the order agree, so a bug in one shows against the other |
| `derive_never_widens_rights` | the central theorem, on the real `CSpace::derive` |

Five in `crates/paging/src/lib.rs`, the address arithmetic under the four-level walk:

| Harness | Property |
|---|---|
| `index_is_always_in_bounds` | every extracted table index is < 512, so the walk never reads past a table (memory safety) |
| `the_indices_and_offset_tile_the_address` | the four 9-bit indices and the 12-bit offset reassemble the low 48 bits exactly, no bit lost or shared (the `39 - 9*level` shift math is correct) |
| `the_offset_does_not_change_the_walk` | changing only the page offset leaves all four indices fixed: a whole 4 KiB page shares one leaf (page granularity) |
| `distinct_pages_take_distinct_paths` | two page-aligned addresses with the same four indices are the same page (the arithmetic core of isolation) |
| `the_two_halves_are_disjoint` | no address is in both `TTBR0` (low) and `TTBR1` (high) |

Not yet proved, and the heavier next step: the `Mapper` itself, mapping a page and translating it
back, which reasons over built tables and a bounded frame pool rather than pure arithmetic. That is
where the "bounded" tradeoff above starts to bite.

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
- **Spread inward from the capability core**, the order §14 sets: `caps` now, then IPC (rendezvous,
  one-shot reply), then the MMU isolation invariants. Each step proves a property the security story
  currently rests on by argument.
- **A harness that needs a huge bound is a design smell.** If a property needs Kani to explore an
  unbounded loop or a giant structure, that is often the code telling you the logic is not as local
  as it should be. Prefer refactoring the logic to shrinking the proof.
