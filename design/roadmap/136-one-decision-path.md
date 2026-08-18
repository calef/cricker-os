# 136. One decision path, and a gate that keeps it that way

**Status: BUILT.** Raised and closed 2026-08-18, out of milestone 135's own `BUGS` section, which
named this as the one of its three recorded limitations worth a milestone: *"a later lane could
reintroduce a second decision path in `untyped.rs` and no check would notice."* The gate is two
halves of `script/lint` plus two `compile_fail` doctests, and **it was verified against seven
hand-made regressions**, one of which it originally missed. That miss is the most useful thing this
milestone produced and is recorded below rather than quietly fixed.

## In brief

Milestone 135's value is one sentence: **the thing loom searches is the thing the kernel runs.** It
lifted the region table and every decision over it out of `kernel/src/untyped.rs` into
`crates/regions`, so `script/interleaving-check` searches the real claim protocol rather than a
paraphrase of it. That sentence is worth exactly as much as it stays true, and when 135 landed
nothing checked it.

135 proposed the mechanism as *milestone 113's shim shape applied to loom crates*. **It does not
transfer, because that work is already done and was done by 135 itself.** 113 built
`scripts/kani-lint-shim/` because `cfg(kani)` code is written against intrinsics a plain rustc
cannot resolve, so making the proof harnesses visible to clippy needed a fake `kani` crate. Loom has
no such problem: it is an ordinary crates.io dependency behind `[target.'cfg(loom)'.dependencies]`,
so the whole of 113's benefit costs one flag, and `script/interleaving-check` already carries it:

    RUSTFLAGS="--cfg loom -D warnings" cargo test --release ...

That script's own comment cites 113 and says why the shim is unnecessary. So the blind spot 135
pointed at is closed, and the property 135 actually named is a **different** one that no lint
against harness code could ever have reached: linting a loom model says nothing about whether the
kernel still calls it.

So this milestone gates the real property, in three pieces:

- **The claim protocol's public surface is pinned**, name and receiver, in `script/lint`. The kernel
  is outside `crates/regions`, `Region` and its fields are private, and the surface is therefore the
  entire set of pieces the kernel could build a second decision out of. The receiver is pinned
  because `claim_for_destroy` taking `&mut self` *is* the mechanism: a lane respelling it `&self`
  over an interior-mutable table would delete the whole argument while adding and removing nothing a
  name-set check could see.
- **Every place the kernel releases region pages must have taken its warrant first.** Two functions
  in `kernel/src/untyped.rs` may reach a free loop, and each is pinned with the call that entitles
  it: `create` with `insert_root` (its rollback frees a run no name was ever minted for), `destroy`
  with `claim_for_destroy` (the only object in the system that proves nobody else is freeing the
  same run).
- **Two `compile_fail` doctests on `DestroyClaim`**, carrying explicit error codes, asserting that
  the claim cannot be forged from outside the crate (`E0451`) and cannot be duplicated (`E0599`).
  These catch what a set cannot see: a `#[derive(Clone)]` is one word and a `pub` on a field is four
  characters, and neither changes the list of public items.

## The regression the first draft missed, which is the whole reason to write this down

The first version of the lint pinned only the **set** of functions that free region pages. It was
green against this, pasted over `untyped::destroy`:

```rust
pub fn destroy(region: u64) {
    if REGIONS.lock().has_children(region) { return; }
    let Some((base, size)) = REGIONS.lock().bounds(region) else { return };
    crate::revoke::revoke_region(base, size);
    for i in 0..(size / FRAME_SIZE) {
        memory::free(Frame::from_addr(base + i * FRAME_SIZE));
    }
}
```

That is pull request #316's double free, rebuilt: read under one hold, release, revoke, free, and
never remove the slot, so two callers both pass the read and both reach the loop. It **compiles**,
and it needed no edit to `crates/regions` at all, because `has_children` and `bounds` are public
and are enough. So the surface pin could not have caught it either: nothing about the surface
changed.

Two things follow, and both are now in the gate. The set of freeing functions is the wrong unit,
because `destroy` was already in it; the right unit is whether a free site took its warrant. And a
public `&self` observer on the table is a live hazard rather than a theoretical one, which is why
the pin comments name the three that exist and ask a fourth to argue for itself.

**The gate was then verified against seven regressions and catches all seven**, each naming what is
wrong rather than that something is:

| Regression | Caught by | Message |
|---|---|---|
| the rebuilt gap above | warrant pin | `destroy() frees region pages without calling claim_for_destroy() first` |
| a third free site (`trim`) | site pin | `trim() releases region pages and is not a recorded free site` |
| `pub fn may_destroy(&self, ..) -> bool` added to the table | surface pin | `RegionTable::may_destroy (&self) is public and is not pinned` |
| `claim_for_destroy` respelled `&self` | surface pin | reported as both an unpinned `&self` and a missing `&mut self` |
| `has_children` made private | surface pin | `is pinned and is no longer public` |
| `#[derive(Clone)]` on `DestroyClaim` | doctest | the duplication snippet compiles, so the `compile_fail` fails |
| `DestroyClaim`'s fields made `pub` | doctest | the forging snippet compiles, so the `compile_fail` fails |

The doctests carry error codes (`compile_fail,E0451`) rather than a bare `compile_fail`, because a
bare one passes when the snippet fails to compile for **any** reason, including a typo, which is how
a compile-fail test rots into an assertion nobody has watched fail. That the codes are enforced was
itself checked: changing `E0451` to `E0308` fails with *"Some expected error codes were not found"*.

## Why it matters

**An ungated lift decays into a paraphrase, and the decay is invisible.** Every other outcome of
milestone 135 is protected by something: the arithmetic by Kani, the interleavings by loom, the
lock rank by `script/lint`. The claim that the kernel still *calls* the modelled code was protected
by nobody having got around to changing it. That is rung zero of the ladder, and the tenet is
explicit that "somebody will notice" belongs on no list.

**This is the failure mode that costs the most and shows the least.** A model checker that searches
code the kernel no longer runs reports success, forever, on a question nobody is asking. The tree
would keep 1,364 green executions and a `notes/interleaving.md` row while the double free came back,
and the first evidence would be a corrupted page on a customer's backup target.

**A gate nobody has watched fail is the thing this tree keeps deleting** (milestone 62 removed two
such assertions this week). The seven-regression table above is the price of not adding an eighth,
and the one that got through the first draft is the argument for paying it.

## Prior art

The shape is this repository's own, and the two closest neighbours are worth naming because the gate
is deliberately built from their parts rather than from a new idea. `script/lint`'s *"the SMB server
cannot compute a proof"* check pins a dependency's section in a manifest, in both directions, with a
message that says what to do instead; this pins an API surface the same way. Milestone 113's Kani
shim is the ancestor 135 pointed at, and the finding here is that its problem does not exist for
loom.

Outside the tree, pinning a public surface to catch semantic drift is `cargo-semver-checks` and
Rust's own `rustdoc`-JSON API snapshots; the reason not to reach for either is that both answer
"did the API break compatibility", where the question here is "did the API grow a way to observe a
decision without taking it", which no general tool can know.

## Scope note

No syscall surface, no wire format, no new dependency, and no `DECISIONS.md` section: nothing here
decides anything global. It adds one check to `script/lint`, two doctests, and this block. The
kernel and the crate are byte-identical apart from the doc comment carrying the doctests.

## BUGS

- **The free-site pin is scoped to `kernel/src/untyped.rs`, and region pages can be freed from
  elsewhere.** `memory::free` has eleven other call sites in the kernel, all of them legitimately
  freeing page tables, DMA buffers or test fixtures, so a tree-wide pin would be noise rather than a
  gate. A lane that frees region pages from `sched.rs` is not caught. The narrower claim the gate
  actually makes is "the module that owns region memory releases it in two warranted places".
- **The warrant is line order inside a function, not dataflow.** `destroy` passes because
  `claim_for_destroy(` appears on a line above `memory::free(`. A body that called
  `claim_for_destroy` and discarded the result, then freed pages it had read some other way, would
  pass. That is contrived rather than impossible, and closing it means an AST, which `script/lint`
  deliberately does not have.
- **`create`'s warrant is much weaker than `destroy`'s.** `insert_root` entitles the rollback only
  because the rollback runs when `insert_root` returned `None`; the gate checks that the call is
  present, not that the free is on the failure branch. A rewritten `create` that called
  `insert_root` and then freed a live region's pages would pass.
- **The surface pin covers `crates/regions/src/table.rs`, not the crate.** The arithmetic in
  `lib.rs` (`split_new_watermark`, `destroy_outcome`) is public, pure and Kani-proved, so it is not
  a hazard today; a new public item added to `lib.rs` that exposed table state would not be seen.
- **Nothing checks that a newly pinned item is searched by loom.** The failure message asks for it
  in words, which is rung four. A lane can add a public method, pin it, and never model it.
- **The other four loom crates have the same exposure and are not gated.** `steal_request`,
  `clock_proto`, `wake_handshake` and `canary_gate` were each lifted so loom could search them, and
  nothing checks that their callers still call them either. This milestone gated the one whose
  protocol has a known real bug behind it; whether the same shape is worth repeating four more
  times, or whether it wants one general mechanism, is not decided here.
- **A count of pinned items is not a claim about correctness.** Seventeen public items being pinned
  says the surface has not moved, not that the surface is right.
