# 13. Capability revocation and untyped reclamation (frames)

**Status: DECIDED.**

Decided and built 2026-07-22 (milestone 13). The direction was parked in "Open design ideas" and
notes/capability-lifecycle.md; the concrete mechanism is designed here, because it is a
capability-model change gated on a memory-safety precondition.

## What it closes

A granted capability could not be retracted and a spent page could not be reclaimed. That was safe
only by a structural accident: retyped frames are spend-only and never reused, so a peer that still
mapped a shared frame after the granter left was mapping valid, non-reused memory. `untyped::destroy`
carried a tripwire spelling this out: wiring up any reclamation before revocation exists turns those
dangling mappings into a use-after-free.

## The scope: frame revocation, not the full tree

seL4 keeps a capability-derivation tree and revokes a *subtree* (revoke Bob's copy while keeping
Alice's and my own). We build the **unmap side only** and revoke *all* derivatives of a page, which
is exactly what reclamation wants and is the memory-safety-critical half. The full tree buys subtree
granularity, which nothing on the roadmap needs, so by §4 it waits for a driver. This is a considered
terminal design, not a way-station: if subtree revoke is ever required, the machinery here
(unmap-from-any-address-space, the revoke-before-reclaim discipline, `untyped::destroy`) is reused
unchanged, and only the index (an object-to-holders list) is rebuilt as a tree.
design/roadmap/13-capability-revocation.md has the argument.

## The mechanism

A **mapping database, lite** (revoke.rs): every mapping of an untyped-derived page, `(phys, root,
va)`, recorded at `Untyped::MAP` and `Frame::MAP`, and forgotten when an address space is torn down
(so a stale root is never walked after its tables are freed and reused). To revoke a page:

1. Delete every `Frame` capability to it from every cspace, so no `Frame::MAP` started afterward can
   re-establish a mapping.
2. Unmap it from every address space that held it, with the broadcast TLB flush we already use, so
   SMP and the no-ASID case are covered.

Two entry points: **`Frame::REVOKE`** (a method needing `GRANT`, the un-share trigger; it does not
reclaim, since the untyped is spend-only) and **`untyped::destroy`** (revoke every mapped page in the
region, then return the pages to the allocator, the reclaim trigger). Reclamation is now safe because
"no live mapping survives" replaces "spend-only, never reused".

## Tests

`revoke_unmaps_a_shared_page_from_every_address_space` (a page mapped in two address spaces is
unmapped from both), `destroy_unmaps_a_region_before_reclaiming_it` (the tripwire's use-after-free
made impossible: the mapping is gone before the frame returns to the allocator), and, at EL0,
`a_process_revokes_a_frame_and_loses_the_capability`.

## Deferred, deliberately

- The full capability-derivation tree and subtree-granularity revoke (above).
- Revocation of non-memory objects (endpoints, IRQs): no unmapping, just cspace removal, and less
  urgent since they are not the memory-safety seam.
- Reclaim-on-process-death: an additive step now that explicit `revoke` and `destroy` are safe.
- Returning a single revoked frame to a reusable pool: the untyped is still a spend-only bump
  allocator, so `Frame::REVOKE` un-shares but does not reclaim; `untyped::destroy` reclaims a region.

## The one honest race

A `Frame::MAP` in flight on another core, between a revoke's cap-delete and its unmap, can slip a
mapping past the revoke. seL4 closes this with a mapping-database lock held across the whole
operation; that lock is the deferred full-database machinery. Named, not hidden.
