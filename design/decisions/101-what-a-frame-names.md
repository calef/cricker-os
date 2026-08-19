# 101. What a `Frame` names

**Status: PROPOSED.** The number is **provisional**: this lane cut its branch from `main` at
`22da81ae`, where §100 is the highest section, and the integrator mints the real one at merge.

**The fork.** `Object::Frame(pa)` names exactly one 4 KiB page and occupies one of sixteen cspace
slots. Milestone 29's lane (PR #364) was briefed to grow the display scanout to 800x600 so that
§100's gohufont-14 would give a usable text grid, and **could not build it**. The blocker is the
capability model rather than memory: the virtio-gpu driver's DMA region already holds nine slots of
the sixteen, so the ceiling is nine frames and 36,864 bytes, and an 800x600 surface needs 469. A
`const` assertion at `kernel/src/user/display_service.rs` fails the build, as designed. Every
non-square shape under the ceiling gives five text rows or fewer, so **no scanout reachable today
makes gohufont-14 a terminal.**

This section prices four options. It gives no recommendation, deliberately: three of the four change
what a syscall method means, which is the irreversible column of the *move fast on what can be
undone* tenet, and a recommendation on that fork is most of the way to a decision.

**What is blocked until this is answered.** Milestone 29's scanout, and therefore §100's font
landing anywhere a person can read it. Nothing else is waiting; the display path is the only place
in the tree where a single object is a run of hundreds of pages.

---

## 1. What else was considered, and why each lost

The four options are set out in full in §101.6 below with their measured prices. In summary:

| | What changes | Verdict on the evidence |
|---|---|---|
| **A. A `Frame` names a run** `(pa, pages)` | `Object::Frame`, `frame::MAP`, `frame::REVOKE`, the revocation database's key | Live. The most general answer and the one that most changes what a capability means. |
| **B. Grow `CSPACE_SLOTS`** | one constant | **Dominated, and by arithmetic rather than taste.** See §101.5. A TCB is written into one 4 KiB page and a slot is 24 bytes, so **at most 170 slots fit a page even if a `Thread` held nothing else**; 475 frames needs 480. This is not "a one-number change paid in TCB size", which is what `kernel/src/cap.rs` currently says. |
| **C. `aspace::MAP_INTO` at spawn** | nothing in the model | Live, and the only option that changes no syscall meaning. Costs 475 map calls, and the client ends up holding **no capability for its own pixels**, which is what milestone 108 spent itself closing on this exact path. |
| **D. A large `Frame`: one 2 MiB page** | `PageFormat` gains block descriptors; `Object::Frame` gains an order | Live, and cheaper than this lane expected. `800x608x4 = 1,945,600` fits one 2 MiB page with 37 pages to spare, and `SURFACE_VA` is already 2 MiB-aligned. |

**A and D are the same decision at different granularity**, which is the useful way to see them: both
make one capability name more than one page, and they differ in whether the size is arbitrary
(`pages: u64`) or a power-of-two order the hardware already understands. That is exactly the split
between Fuchsia's VMOs and seL4's frame sizes, and between an arbitrary run and an L4 flexpage.

---

## 2. What this tree already does in the analogous case

Three answers, and the first one settles more than it looks like it does.

**`Object::Untyped` already names a run of pages with one capability**, and has since milestone 11.
It does not carry the run in the capability word: it carries a **generational name into a kernel-side
table** (`kernel/src/untyped.rs`, `MAX_REGIONS = 256`, over `crates/regions`), and the table holds
`(base_page, pages, watermark)`. So the tree has a worked, proved, host-tested precedent for "one
capability, N pages", and its shape is **indirection, not a fatter capability**. Option A can copy it
exactly and inherit the stale-safety that comes with it: reclaiming a region bumps its slot's
generation and every outstanding capability to it stops resolving.

**The DMA path already has a `(base, size)` run type, and it is Kani-proved.**
`paging::domain::DmaRegion { base, size }` is a physical run a device may touch, with
`grant_pages`/`grant_page` as the "which whole pages does this run contain" arithmetic and six
harnesses over it. Whatever a run-shaped `Frame` decides about partial pages and wrapping, that
argument is already written down and machine-checked one module over.

**And the display driver's own DMA region is already a run in physics and nine capabilities in the
model**, which is the whole complaint: `display_service.rs` allocates it with one
`memory::alloc_contiguous(DMA_FRAMES)` and then mints nine `Frame` capabilities over consecutive
pages of it (`grant_run`). The kernel *knows* it is one run at the moment it hands it over and throws
that fact away.

---

## 3. The prior art, read rather than recalled

*Filled in by this lane against primary sources. Four claims were offered from memory when this fork
was raised; each is marked below with whether it survived.*

<!-- PRIOR-ART -->

---

## 4. Is the premise true?

Two premises were checked before anything was priced, and both hold.

**The arithmetic.** `800 x 600 x 4 = 1,920,000` is 468.75 pages, so `gfx_proto`'s
`SURFACE_BYTES.is_multiple_of(4096)` assertion fails on it; PR #364 found this and it is right.
**800x608** is the nearest shape with an 800 width (the height must be a multiple of 32), giving
`1,945,600` bytes, **475 pages exactly**, and a 100x43 grid at gohufont-14's 8x14. A 2 MiB page is
`2,097,152` bytes, so the whole surface fits inside one with 151,552 bytes (37 pages) unused.
`SURFACE_VA` is `0x60_0000`, which is 2 MiB-aligned, so no address moves.

**The ceiling.** `cap::CSPACE_SLOTS = 16`, `abi::fault::FAULT_EP_SLOT = 15`, and
`DRIVER_SLOT_DMA = 5`, so the driver's region may hold at most ten slots and `DMA_FRAMES` is
`1 + SURFACE_FRAMES`. Nine surface frames is the ceiling, 36,864 bytes, exactly as reported.

---

## 5. What each option costs, measured

### The walk is the shared cost, and it is shared with the IOMMU

Everything below that touches page size lands in `crates/paging`, and the crate has exactly three
walks: `Mapper::map`, `Mapper::unmap`, `Mapper::translate`. All three are the same fixed loop:

```rust
for level in 0..F::LEVELS - 1 {
    let i = F::index(va, level);
    let entry = /* read */;
    if !F::is_present(entry) { /* create, or fail */ }
    table_pa = F::entry_pa(entry);
}
```

**`is_present` cannot tell a table from a block, and the walk assumes table.** That is the hazard,
and it is sharper than "unsupported": if a 2 MiB block descriptor existed at level 2 today and
anything mapped a 4 KiB page beneath it, `F::entry_pa` would return the *block's* physical address
and the mapper would write page-table entries **into the framebuffer**. `translate` would return
pixels as a page-table address. So block support is not additive; every walk needs a new
discrimination step, and `PageFormat` needs a method it does not have.

The crate's own aarch64 test already names the trap, in
`a_descriptor_carries_the_table_or_page_bit_as_well_as_valid`: *"at L0-L2 it is a block descriptor,
which maps a 1 GiB or 2 MiB span from bits the walk meant as a table pointer."*

**And the same three walks build the IOMMU domains.** `paging::domain::build_identity_domain` is a
`Mapper` over the same formats, because SMMUv3 walks VMSAv8-64 and the RISC-V IOMMU walks Sv39. So a
bug in the new discrimination is a bug in device confinement as well as in process isolation. That is
the argument for why this belongs in a decision rather than in a lane's judgment.

### aarch64 and riscv64 are not symmetric, and the asymmetry is in the encoder

| | aarch64 (`Aarch64`, 4 levels) | riscv64 (`Sv39`, 3 levels) |
|---|---|---|
| 2 MiB is | a block at level 2 (`index` shift 21) | a leaf at level 1 (shift 21) |
| 1 GiB is | a block at level 1 (shift 30) | a leaf at level 0 (shift 30) |
| Encoding a block | **new code.** `leaf_entry` writes bits[1:0] = `0b11`, which at L0-L2 means *table*. A block is `0b01`, which at L3 is a *fault*. So the format needs a `block_entry`, and it is level-sensitive. | **none.** `leaf_entry` already writes a valid leaf at any level; a superpage is the same PTE with the low PPN bits zero. |
| Telling table from leaf | `entry & (1 << 1)`, and the answer depends on the level | `entry & (R\|W\|X) == 0`, level-independent |
| Alignment the hardware requires | VA and PA both 2 MiB-aligned | PPN[0] must be zero, i.e. PA 2 MiB-aligned, or the walk page-faults |

So riscv64 needs no new descriptor construction at all and aarch64 does; but **both need the same new
trait method and the same three walk changes**, because the discrimination predicate differs from
`is_present` on both. Rule 5's parity gate is satisfiable in one change; it is the aarch64 encoder
that carries the extra risk, because `0b01` at the wrong level is a translation fault rather than a
compile error.

### The Kani cost, which is smaller than the fork's framing assumed

`crates/paging` carries **18 harnesses**: six in `aarch64.rs`, six in `sv39.rs`, six in `domain.rs`
(133 in the tree as a whole, counted from the merged tree at `22da81ae`). The premise offered when
this fork was raised was that "a second page size is a second case in every one of them, and that may
be the real price rather than the mapping code." **Measured, that is not so.**

- **Twelve are size-independent and change not at all.** `index_is_always_in_bounds`,
  `the_indices_and_offset_tile_the_address`, `distinct_pages_take_distinct_paths` and
  `the_two_halves_are_disjoint` are quantified over `level` or over the whole address already, on
  both architectures. All six `domain.rs` harnesses are about `DmaRegion` arithmetic and never see a
  descriptor.
- **Two widen** (one per architecture): `the_user_va_gate_admits_only_the_aligned_low_half` pins
  `is_user_page_va` to `va & 0xfff == 0`, and a block VA gate is a different predicate.
- **Two need a twin** (one per architecture): `the_leaf_keeps_address_and_permissions_apart` must be
  proved for `block_entry` as well as `leaf_entry`.
- **And one genuinely new property is owed, per architecture, and it is the important one**: that
  table and block are **totally and exclusively** distinguishable at every level, so no descriptor is
  ever read as the other kind. Nothing in the tree proves that today because nothing needed it, and
  it is the property that stands between a block descriptor and a mapper writing PTEs into pixels.

**So: roughly +6 harnesses and 2 rewritten, against 18.** The real price is the walk rewrite, not the
proofs, and the proofs are what make the walk rewrite affordable.

### The allocator and the region table

- **`crates/frames` has no aligned allocation.** `alloc_contiguous(count)` scans for a run of `count`
  free pages with no alignment argument, and `Frame::new` panics on anything not 4 KiB aligned. A
  2 MiB frame needs a 512-page run at a 512-page-aligned physical address, so this is a new method
  plus a harness (the crate has five). Cheap, and the fragmentation headroom exists: the ledger in
  `notes/frames.md` measures the longest free run at 14,080 pages after a full test boot.
- **`crates/regions` is where option D gets expensive, and only if userspace can mint a large
  frame.** `untyped::retype_page` bumps a watermark one page at a time, and `regions` is proved for
  **no double free** on the strength of `split_new_watermark` and `destroy_outcome`'s LIFO un-bump.
  Aligning a watermark up to a 2 MiB boundary leaves a hole, and a hole is exactly what the LIFO
  return does not model. **That is the scariest place in the tree to add a case.**

  **It is avoidable, and this is the finding that most changes option D's price.** The display path's
  memory does not come from a user retype at all: `display_service.rs` calls
  `crate::memory::alloc_contiguous(DMA_FRAMES)` directly and mints the capabilities itself. So option
  D splits in two:

  - **D-narrow.** The kernel may mint a large `Frame`; `Untyped::RETYPE` still mints 4 KiB only. No
    `regions` change, no watermark alignment, no touching the no-double-free proof.
  - **D-full.** Userspace can retype a large frame from its own untyped. Adds the `regions`
    alignment problem and its proof obligation.

  D-narrow unblocks milestone 29 completely. D-full is a later decision that D-narrow does not
  foreclose.

### Revocation, and the invariant that is actually at stake

This is the part of the fork that is not about page tables, and it is the reason A and D are
irreversible rather than merely expensive.

`Object::Frame(pa)` has a property nothing states out loud: **two frames are equal or disjoint.**
`revoke::record_mapping(phys, root, va)` keys on the exact physical address and `revoke_frame(phys)`
deletes every capability naming *that* page. Give a frame a size and capabilities can **overlap**: a
2 MiB frame and a 4 KiB frame inside it are two different objects over the same memory, and
`revoke_frame` on either would miss the other.

The invariant survives if large frames are minted only where memory is spent exclusively (the
kernel's own `alloc_contiguous`, or a watermark bump that cannot hand the same page out twice), which
is how seL4 keeps it: retype consumes the untyped. **It does not survive a `Frame::SPLIT`**, the
operation somebody will ask for within a week of a large frame existing. Whichever option wins,
`Frame::SPLIT` is a separate decision and should be refused by default rather than added quietly.

### Option C's price, stated honestly

Option C changes no model and is therefore the cheapest to build, which under the *elegance and
performance beat implementation convenience* tenet is an argument that must say out loud that it is
an argument from effort. Its real costs:

- **The client holds no capability for its own pixels.** It cannot delegate the surface, cannot hand
  a peer a read-only view, and cannot have it revoked at the granularity of the object, because there
  is no object. Milestone 108 migrated this exact path away from that state and
  `notes/frames.md` records what it bought.
- **475 mappings, 475 revocation records.** The records are two pages against `AS_OVERHEAD`'s sixteen
  of slack, so the cost is real and not binding.
- **`MAP_INTO` is a user-built-space operation** (`kernel/src/user.rs`, `user_aspace_map`, over the
  `USER_SPACES` table), and it takes a `Frame` capability in a slot as its `a1`. The display path
  spawns through `sched::run` with a kernel-built `Spawn`, not through the userspace loader. So "at
  spawn" here means the kernel calling the recorded mapping engine 475 times, which keeps
  revocability but is a different code path from the one the option's name suggests. That is a real
  change of shape, not a no-op.

### Option B is dominated, and the constant's own comment is wrong

`kernel/src/cap.rs` says: *"Growing it is a one-number change here, paid in TCB size."* Measured, the
TCB size is not a slope, it is a wall.

A `Thread` is **written into one 4 KiB page** (`sched.rs`, `insert_from_page` /`insert_at`, milestone
19c.2: *"Each `Thread` lives at the start of one page"*). A cspace slot is
`Option<Cap<Object>>` = **24 bytes** (`Object` is 16, `Rights` is a `u32`, and `Option` takes the
discriminant's niche; measured, not estimated). Sixteen slots is 384 bytes.

- 475 surface frames plus the driver's control page needs **at least 480 slots = 11,520 bytes.**
- **170 slots is the absolute ceiling** if a `Thread` contained nothing but its cspace, and it
  contains a saved context, a name, queue links and a good deal more.

So option B cannot reach this workload without making a TCB multi-page, which unpicks milestone
19c.2's page-resident TCB and the retype path built on it. It remains a sensible *small* change (16
to 32, say) for the ordinary pressure of a program holding a dozen objects; it is not an answer to
this fork.

**Two things fall out of that measurement and want a home regardless of which option wins**,
recorded here rather than left in a lane report:

1. **`kernel/src/cap.rs`'s comment on `CSPACE_SLOTS` should say what the real ceiling is**, because
   as written it invites exactly the change that would silently break.
2. **Nothing asserts that a `Thread` fits in its page.** `insert_at` does `ptr.write(thread)` into a
   4 KiB page with no `const` guard, so a `CSPACE_SLOTS` raised past the fit would scribble the next
   page and fail arbitrarily far from the edit. That is rung one of the ladder available for free:
   `const _: () = assert!(size_of::<Thread>() <= FRAME_SIZE)`.

---

## 6. The four options

### A. A `Frame` names a run: `(base, pages)`

**What changes.** `Object::Frame` carries a length, or (following `Untyped`) a generational name into
a kernel table that holds it. `frame::MAP` maps the whole run and needs a page count or a per-page
loop inside the kernel. `frame::REVOKE` and `revoke::record_mapping` key on a run rather than a page.

**Cost.** The syscall surface (`frame::MAP`'s meaning changes for every existing caller), the
revocation database's key, and the overlap invariant above. No paging change at all: a run of 4 KiB
pages is 475 ordinary leaf mappings, so `crates/paging` and its 18 harnesses are untouched.

**What it forecloses.** Nothing structurally; it is the most general answer. It is also the one that
cannot be walked back, because "a Frame is a page" is a sentence in a dozen files and in the head of
anyone who has read them.

**What it buys beyond this milestone.** Any future driver with a large DMA region, which is every
driver worth having: a NIC's ring buffers, an NVMe queue pair, a second display.

### B. Grow `CSPACE_SLOTS`

**Dominated by arithmetic** (§101.5). Include it only as the small, orthogonal change it actually is.

### C. `aspace::MAP_INTO` at spawn

**What changes.** Nothing in the model. The kernel maps 475 pages into the client's space through the
recorded engine.

**Cost.** The client holds no object for its pixels; 475 records; a different spawn path for this
service.

**What it forecloses.** It re-opens on the display path the gap milestone 108 closed everywhere else,
and it makes the display the one service whose memory is not a capability. If the answer to "why does
this path look different?" is "because a Frame is one page", the model has been worked around rather
than decided.

### D. A large `Frame`: one 2 MiB page

**What changes.** `PageFormat` gains block-descriptor support (a discrimination method and, on
aarch64, an encoder); the three walks gain a block branch; `Object::Frame` gains an order or a size;
`crates/frames` gains an aligned allocation. In **D-narrow**, `Untyped::RETYPE` is untouched and
`crates/regions` is untouched.

**Cost.** The walk rewrite, which is shared with the IOMMU domain builder, plus ~6 new Kani harnesses
and 2 rewritten. Internal fragmentation: 37 pages (151,552 bytes) wasted on an 800x608 surface, and
up to 511 pages wasted on a surface that just clears a boundary.

**What it buys beyond this milestone.** A 2 MiB mapping is **one TLB entry instead of 512**, which is
a measurable win for a framebuffer that is written by a compositor every frame and read by a device.
Nothing else in the tree gets that today, and `script/bench` could measure it.

**What it forecloses.** It commits the tree to power-of-two sizes as the way memory scales, which is
seL4's answer and L4's. It does not foreclose A: a run of large frames is still a run.

---

## 7. How reversible is it, and who has already acted on it

**A, C and D all touch things two programs agree on**, which is the *move fast on what can be undone*
tenet's first irreversible category:

- **A and D change `frame::MAP`'s meaning**, and `Frame::MAP` is called by seven migrated programs
  (`disk_surveyor`, the roster probe, `disk_partitioner`, `mkfs`, the virtio-gpu driver, `painter`,
  `display_terminal`). A change here is not a change to one call site; it is a change to what those
  programs mean.
- **The revocation key is a wire fact**, because §13's guarantee ("a mapping revocation cannot see is
  the use-after-free") is a claim made in prose, in tests, and in `notes/frames.md`. Weakening it by
  admitting overlapping frames is the kind of fact that leaves the machine.
- **C changes nothing anybody has agreed on**, and is therefore the reversible option. That is its
  strongest argument and it should be weighed as one.

**Nobody outside this tree has acted on any of it.** There is no published claim about what a
`Frame` names, so the cost is the internal one above and not a retraction.

---

## What I need from you

Which of the four. The lane can build any of them; only the choice is yours, and A and D are the two
that change what a capability means.

If the answer is **D-narrow**, milestone 29 is unblocked without touching `crates/regions` or the
no-double-free proof, and `Untyped::RETYPE` keeps its current meaning. If the answer is **C**, the
display becomes the one service in the tree whose memory is not a capability, and that should be
recorded in `notes/frames.md`'s `BUGS` rather than left to be rediscovered. If the answer is **A**,
`Frame::SPLIT` needs refusing in the same breath, or the equal-or-disjoint invariant goes with it.

Two small things are yours only if you want them; they block nothing:

- the `size_of::<Thread>() <= FRAME_SIZE` assertion and the corrected `CSPACE_SLOTS` comment (§101.5),
  which are worth doing whichever option wins;
- whether a large frame, if D wins, is spelled as an **order** (seL4's shape: 4 KiB / 2 MiB / 1 GiB
  and nothing between) or as a **page count** (A's shape at a coarser grain). They are different
  claims about what memory is.
