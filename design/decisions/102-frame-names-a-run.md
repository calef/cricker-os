# 102. A Frame names a run of pages

**Status: DECIDED.** Raised 2026-08-19 by milestone 29's lane, which found that the terminal font
(§100's gohufont-14) cannot ship without a bigger scanout, and a bigger scanout is 469 capabilities
in a sixteen-slot cspace. Decided 2026-08-20 by calef: option 1, a `Frame` names a run.

**What is blocked: milestone 29's terminal font.** The scanout grows to 800x608 (475 frames) and
gohufont-14 ships on it. This decision is the prerequisite.

## What is being decided

Whether `Object::Frame` carries a page count, so a single `Frame` capability names a contiguous run
of physical pages rather than exactly one. `frame::MAP` maps the whole run; `frame::REVOKE` unmaps
it. Existing callers pass `count = 1` and get the same behavior.

## Why

A DMA region is a run of pages in every dimension the hardware cares about: contiguous in physics,
contiguous in the virtual address space, covered as a single range by the IOMMU domain the kernel
programmed for it. Representing it as N separate capabilities is an artifact of `Object::Frame`'s
current definition, not a design choice with a reason.

This project's own principle is that a name is a claim (§39). A `Frame` that names one page is
making the wrong claim about a run of pages. The capability should name what the hardware names.

The constraint that made it acute: `CSPACE_SLOTS` is 16, and the virtio-gpu driver's nine-page DMA
region already fills 9 of them. An 800x608 scanout is 475 frames, and the build-time guard
(`DRIVER_SLOT_DMA + DMA_FRAMES <= abi::fault::FAULT_EP_SLOT`) fires. Growing `CSPACE_SLOTS` works
but doesn't fix the underlying mismatch; it buys room for the wrong representation.

## What changes

### The object

```rust
pub enum Object {
    // ...
    /// A contiguous run of physical pages. `count` is at least 1.
    /// A single-page frame is `Frame(phys, 1)`, and every existing caller passes 1.
    Frame(phys: u64, count: u64),
    // ...
}
```

### The kernel constructor

```rust
pub fn frame_cap(phys: u64, count: u64, rights: Rights) -> Cap {
    Cap { object: Object::Frame(phys, count), rights }
}
```

Existing callers pass `count: 1`. `grant_run` passes `count` and grants one capability instead of
N, which is the whole point: 475 capabilities, 475 syscalls, and 475 mapping records collapse into
one of each.

### `frame::MAP`

`invoke(cap, MAP, va, writable, untyped_slot)` maps the run starting at `va`. The page tables come
from the untyped, same as today. The mapping is recorded for revocation, one record per page (the
revocation table is per-page, not per-capability, and that doesn't change).

The signature is additive: the `count` lives on the capability, not in the syscall arguments, so
`MAP`'s argument shape doesn't change. A caller that holds a `Frame(phys, 475)` calls `MAP` once
and gets 475 pages mapped contiguously.

### `frame::REVOKE`

`invoke(cap, REVOKE, _, _, _)` unmaps the run from every address space and deletes every capability
to it. Same semantics, applied to the whole run. The revocation table walk is one entry per page,
same as today.

### What does NOT change

- **`frame::MAP`'s argument shape.** `va`, `writable`, `untyped_slot` are the same. The `count` is
  on the capability, not in the syscall.
- **The revocation table.** It is per-page (`revoke::record_mapping(phys, root, va)`), not per-capability.
  A `REVOKE` on a run walks the same table N times, once per page in the run. No format change.
- **`aspace::MAP_INTO`.** The spawner-side mapping path is unchanged. It takes a `Frame` capability
  and maps it; a run-capable `Frame` maps the whole run in one call.
- **Rights.** `READ`, `WRITE`, `GRANT`, `ENUMERATE` apply to the whole run. A narrowed capability
  (READ only) maps the whole run read-only. There is no per-page narrowing within a run; if that is
  ever needed, it is a separate fork.
- **`Object::DeviceFrame`.** Device MMIO pages stay single-page. A device register page is one
  page; there is no run to name.

## Surface cost

| Area | Cost |
|------|------|
| `Object::Frame` | Gains a `count: u64` field. The enum variant grows by 8 bytes. |
| `frame_cap` | Gains a `count` parameter. Existing callers pass `1`. |
| `frame::MAP` dispatch | Loops over `count`, mapping each page and recording each mapping. |
| `frame::REVOKE` dispatch | Loops over `count`, revoking each page. |
| `grant_run` | Collapses to one `grant_at` call with one capability. |
| ABI | **No new syscall, no new method, no new constant.** `frame::MAP` and `frame::REVOKE` keep their opcodes; the `count` rides on the capability. |
| Kani proofs | The `capability` crate's invariant on `Object` carries through; the `Frame` variant's equality is `(phys, count)` rather than `phys`. |

## What this retires

- **The `CSPACE_SLOTS` pressure for DMA regions.** A driver that needs N pages of DMA holds one
  capability, not N. The sixteen-slot cspace has room.
- **The `grant_run` loop.** It becomes one call. The function may be inlined or removed.
- **notes/frames.md's recorded fork.** The BUGS entry ("A `Frame` names one page, and a DMA region
  is a run of them") is resolved by this decision. The BUGS section is updated to record that a
  `Frame` names a run as of §102, and the fork is closed.

## What this does NOT decide

- **Per-page rights within a run.** A `Frame(phys, 475)` is all-read or all-write based on the
  capability's rights. Narrowing to "pages 0-400 read-only, 401-474 read-write" is a separate fork
  and is not taken. If a future consumer needs it, it can hold two capabilities: `Frame(phys, 401)`
  and `Frame(phys + 401 * 4096, 74)`.
- **`CSPACE_SLOTS`.** This decision does not grow it. 16 slots is sufficient when DMA regions are
  one capability. If a future consumer needs more slots for other reasons, that is a separate
  decision.
- **`Object::DeviceFrame`.** Device MMIO pages stay single-page. A device register page is one
  page; there is no run to name.
- **The `aspace::MAP_INTO` spawn path.** It still works and is still the right shape for
  spawn-time mappings where the client should hold no frame capability. This decision makes it
  better (one `MAP_INTO` call instead of N) but doesn't change its semantics.

## Why not the alternatives

### Grow `CSPACE_SLOTS` (option 2)

One number, paid in TCB size: 24 bytes a slot, so 512 slots is 12 KiB of cspace per thread against
today's 384 bytes, times `MAX_THREADS` = 128. Also moves `abi::fault::FAULT_EP_SLOT` (`CSPACE_SLOTS
- 1`), which every supervised program agrees on.

Buys the pixels and none of the elegance. The 475 `MAP` calls and 475 mapping records stay. It's
brute force that doesn't fix the underlying mismatch: the capability model names one page when the
hardware names a run.

### `aspace::MAP_INTO` at spawn (option 3)

The spawner holds the client's `Aspace`, maps each frame into it, and deletes its own cap between
iterations. One slot serves the whole run; the client holds no frame capability at all. No model
change, no new method.

Works for the display driver, which has no use for delegating its own surface. But it removes the
client's ability to manage its own memory, which is a step backward for the capability model, and
it's still 475 kernel-side map operations. This is a valid short-term move but not the right
long-term answer.

### seL4's approach

seL4 retypes N frames and you hold N capabilities. Its cspaces are radix trees rather than
fixed-size arrays, so N capabilities is cheap. This kernel uses fixed-size sixteen-slot cspaces for
a reason (TCB size, static analysis, Kani-reachable bounds). The representation should match the
container: a fixed-size cspace wants fewer, fatter capabilities, not more of them.

## Sequencing

1. **This decision** (§102): `Object::Frame` gains a `count`, `frame::MAP` and `frame::REVOKE`
   operate on the run.
2. **Milestone 29's lane**: grows the scanout to 800x608, puts gohufont-14 on it, and ships the
   terminal font. The `grant_run` loop collapses to one call; the cspace fits.
3. **Retrofit**: other DMA-region holders (disk driver, network driver) take `Frame(phys, count)`
   instead of N separate capabilities. This is mechanical and can land in the same lane or a
   follow-up.
