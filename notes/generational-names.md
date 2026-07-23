# Generational names

*(Milestone 14 phase A. The mechanism behind `crates/slots`, the scheduler's thread table since
the `BTreeMap` was retired. See design/kernel-objects-from-untyped.md, decision D2.)*

## The problem it solves

A kernel constantly hands out names for things that die: thread ids, file descriptors,
capability slots. Every such name can outlive what it named. A Unix pid you stashed last week
may belong to a different process today; that is a real bug class (pid reuse races), and Unix
mitigates it with ceremony (pidfds) rather than by construction.

We had the same exposure in miniature: a `Tid` inside a `Reply` capability, or parked in a wait
queue, can in principle outlive its thread. The old thread table was safe against that only in
the weak sense: a dead Tid's map lookup failed, because ids came from a global counter and were
never reused. Never-reused names are safe but unbounded, and the map holding them was unbounded
too, which milestone 14 forbids.

## The mechanism

A name is `(generation, slot)` packed in one u64: slot in the low 32 bits, generation in the
high 32.

- **Lookup** indexes the slot and compares the generation. O(1), two loads.
- **Remove** bumps the slot's generation. Every outstanding name for the old occupant now fails
  the compare, *forever*, including after the slot is reused: the new occupant's names carry the
  new generation.

So slots are reused (the table is a fixed array, milestone 14's requirement) while names behave
as if they never were (the safety the counter used to buy). A stale name is not a dangling
reference and not somebody else's thread; it is `None`.

Game engines rediscovered this shape independently (they call it a slotmap, and entity ids in
ECS engines are exactly generational indices), for the same reason kernels need it: many
short-lived objects, and stored references that must fail safely rather than dangle.

## What it buys the capability model

The path to capability-only thread naming (design doc, D2) ends with capabilities carrying
direct thread references. seL4 makes that safe with the CDT: destroying a TCB revokes every
capability naming it. We deferred the CDT. Generational names are the other way to be safe:
a `Reply(Tid)` whose caller is gone resolves to nothing, checked at use, no revocation sweep
required. The table is therefore the first step of the capability-only path, not a detour;
the intrusive-list work (D1) removes the lookups one structure at a time, and what remains of
the table at the end is exactly this validity check.

## The details that matter

- **The boot thread is tid 0 by construction**: a fresh table's first insert mints slot 0,
  generation 0, which packs to 0. What used to be a hardcoded key is now a property.
- **`u64::MAX` can never be minted** (slot would have to be 2^32-1 with 128 slots), so
  `cpu::NO_TID` keeps working as the "no thread" sentinel, and it is also the `UNNAMED`
  placeholder a `Thread` carries between construction and insertion.
- **Generations are 32-bit and wrap.** A single slot reused 2^32 times could resurrect an
  ancient name. Recorded honestly in the crate doc; not a bound anything real approaches.
- **Insert is O(N)** (scan for a free slot); lookup, the hot-path operation, is O(1).

## What is proved (milestone 18 style, before anything leans on it)

Three Kani harnesses in `crates/slots` (`script/verify`):

| Harness | Property |
|---|---|
| `a_removed_name_never_resolves_again` | after remove, the name fails `get`/`get_mut`/`remove` even once the slot is reused, and the reuser's name differs |
| `live_names_are_distinct_and_resolve_to_their_own_entry` | the packing cannot alias two live entries |
| `a_name_the_table_never_minted_resolves_to_nothing` | for any u64, resolution succeeds only on exactly the name the table issued: names cannot be forged |
