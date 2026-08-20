# 103. What a `Frame` names

**Status: SUPERSEDED BY 102.** Kept because the survey outlives the decision.

**The fork this was researching was answered while it was being researched.** calef chose option 1
on 2026-08-20 (§102, a `Frame` names a run), which is the same answer L4 reached and which this
survey found Barrelfish had already built. So nothing here is waiting on anybody.

**It is kept, and renumbered from a provisional 101 to 103**, because the four recalled claims it
checked are the reason a reader should trust §102 rather than merely obey it: two of the four
framings a maintainer offered from memory turned out to be wrong, and finding that out is what
separates a decision from a guess. The pricing in section 3 is also the closest thing this tree has
to a cost model for the syscall surface, and §102 does not restate it.

**This section was salvaged from a lane the account's spend limit killed mid-run**, with 214 lines
uncommitted in its worktree. What follows is the lane's own text, unedited.

The number is **provisional**: this lane cut its branch from `main` at
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

The four options are set out in full in section 6 below with their measured prices. In summary:

| | What changes | Verdict on the evidence |
|---|---|---|
| **A. A `Frame` names a run** `(pa, pages)` | `Object::Frame`, `frame::MAP`, `frame::REVOKE`, the revocation database's key | Live. The most general answer and the one that most changes what a capability means. |
| **B. Grow `CSPACE_SLOTS`** | one constant | **Dominated, and by arithmetic rather than taste.** See section 5. A TCB is written into one 4 KiB page and a slot is 24 bytes, so **at most 170 slots fit a page even if a `Thread` held nothing else**; 475 frames needs 480. This is not "a one-number change paid in TCB size", which is what `kernel/src/cap.rs` currently says. |
| **C. `aspace::MAP_INTO` at spawn** | nothing in the model | Live, and the only option that changes no syscall meaning. Costs 475 map calls, and the client ends up holding **no capability for its own pixels**, which is what milestone 108 spent itself closing on this exact path. |
| **D. A large `Frame`: one 2 MiB page** | `PageFormat` gains block descriptors; `Object::Frame` gains an order | Live, and cheaper than this lane expected. `800x608x4 = 1,945,600` fits one 2 MiB page with 37 pages to spare, and `SURFACE_VA` is already 2 MiB-aligned. |

**A and D are the same decision at different granularity**, which is the useful way to see them: both
make one capability name more than one page, and they differ in whether the size is arbitrary
(`pages: u64`) or a power-of-two order the hardware already understands. That is exactly the split
between Barrelfish's frame capability and seL4's frame sizes, and the survey in section 3 is mostly
about what each side of it costs.

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

Four systems were offered from memory when this fork was raised, and marked as recalled. This lane
opened the sources. **One survived intact, one with a qualification, and two of the four framings are
wrong**: the flexpage's verb, and Mach's paternity of the VMO. Five more systems were read because a
recalled list is not a survey, and one of them has already built option A. Everything quoted below
was retrieved from the source named beside it.

### L4 flexpages: the geometry is right and "names" is wrong

The claim was that an fpage is *"a power-of-two-sized, power-of-two-aligned region named as one unit
in map and grant operations, the canonical prior art for exactly this problem."*

**The geometry is exactly right.** L4 X.2 (the *L4 eXperimental Kernel Reference Manual*, Rev 7,
`l4ka.org/l4ka/l4-x2-r7.pdf`, its section 4.1):

> Fpages (Flexpages) are regions of the virtual address space. [...] An fpage of size 2^s has a
> 2^s-aligned base address b, i.e., b ≡ 0 (mod 2^s), where s≥10 for all architectures.

and, which is the part that reads most like what we want, until its second half:

> Mapped fpages are considered inseparable objects. That is, if an fpage is mapped, the mapper can
> not later partially unmap the mapped page; the whole fpage must be unmapped in a single operation.
> The mappee can, however, separate the fpage and map fpages (objects) of smaller size. Partially
> unmapping an fpage might or might not work on some systems. The kernel will give no indication as
> to whether such an operation succeeded or not.

**That last sentence is L4 conceding exactly the invariant section 5 is worried about**, and conceding
it as undefined behaviour rather than as a rule. A receiver may split a run into smaller runs, and
what a partial unmap then does is unspecified and unreported. Whatever this tree decides, it should
not decide that.

**But an fpage is not a capability, and classic L4 has none.** An fpage is a **two-word descriptor
placed in message registers** and interpreted by the map/grant operation; the authority behind it is
the mapper's *own existing mapping*, not a slot in a table. From the same manual, under MapItem:

> An fpage (see page 40) or IO fpage that should be mapped is sent to the mappee as part of a
> message. [...] The fpage is specified by a two-word descriptor:

> The effective access rights for the newly mapped page are calculated by bitwise AND-ing the access
> rights specified in the snd fpage and the access rights that the mapper itself has on that fpage.

The lane's check on this: **the string "capabilit" does not occur anywhere in the L4 X.2 reference
manual.** X.2 has no capability space at all; the address space *is* the protection domain. The V2
manual (Au and Heiser, UNSW-CSE-TR-9801, `cgi.cse.unsw.edu.au/~reports/papers/9801.pdf`) says the
same thing in its own words: *"Fpages are specified by the mapper and received by the mappee as part
of an IPC message."* The fpage survives into L4Ka::Pistachio and OKL4 2.1.1 in that same role
(OKL4's `fpage.h`: *"Flexpages are size-aligned memory objects and can cover multiple hardware
pages"*).

**So the fpage is prior art for a map operation over a run, not for a capability that names one.**
That distinction is the entire fork: option C is an operation over a run and options A and D are
capabilities that name one, and citing fpages as support for A or D would have been citing the wrong
half.

**The L4 family's actual answer arrives with its capabilities, and it is an indirection object.**
Fiasco.OC and L4Re did add a capability table (`l4re.org/doc/l4re_concepts_naming.html`:
*"Capabilities are stored in per-task capability tables (the object space)"*), and in that system
memory is named by a **dataspace capability**, with the fpage demoted to the transfer descriptor:

> A dataspace is an abstraction for any thing that is available via usual memory access
> instructions. (`l4re.org/doc/classL4Re_1_1Dataspace.html`)

One dataspace capability, arbitrary size, attached to a task's region map; `map()` then *"will
attempt to map the largest possible flexpage that covers the given local address."* **That is option
A's shape, arrived at by a system that started from option C's and found it insufficient once it had
capabilities to name things with.** It is the single most relevant piece of prior art on this fork
and it was not the one offered.

One aside worth recording because it is a lever nobody listed. L4Re's fpage grew a type field
(`L4_FPAGE_MEMORY`, `L4_FPAGE_IO`, `L4_FPAGE_OBJ`), and an `L4_FPAGE_OBJ` fpage describes a
power-of-two-aligned range of **capability slots**. If the sixteen-slot cspace ever becomes the
binding limit for a reason that is not this one, that is the prior art for addressing slots in runs.

### seL4 frame sizes: correct, with one caveat about what the manual actually says

The claim was that seL4 keeps one capability per frame, but frames come in sizes, so a 2 MiB
framebuffer is one frame capability. **Correct**, verified against the *seL4 Reference Manual*
version 16.0.0, 22 July 2026 (`sel4.systems/Info/Docs/seL4-manual-latest.pdf`).

AArch64, the manual's section 7.1.2.2: `seL4_PageBits` 4 KiB at mapping level 3, `seL4_LargePageBits` 2 MiB at level 2,
`seL4_HugePageBits` 1 GiB at level 1. The manual's own gloss on that column is the block-descriptor
confirmation: the mapping level *"refers to the level of the paging structure at which this page must
be mapped."* The object types are `seL4_ARM_SmallPageObject`, `seL4_ARM_LargePageObject`
(`libsel4/arch_include/arm/sel4/arch/objecttype.h`) and `seL4_ARM_HugePageObject`
(`libsel4/sel4_arch_include/aarch64/sel4/sel4_arch/objecttype.h`).

**And the size is chosen by the object type, not by a size argument**, which is a shape decision
worth copying or refusing deliberately. The manual's section 2.4.2:

> For all other object types, the size is fixed, and the size_bits argument to
> seL4_Untyped_Retype() is ignored.

Only CNode, SchedContext and Untyped are variable-sized. So seL4 spells a large frame as an
**order**, and there is nothing between 4 KiB and 2 MiB.

**The riscv64 parity is exact**, which matters for rule 5: the manual's section 7.1.2.6 gives 4 KiB at level 2, 2 MiB at
level 1, 1 GiB at level 0, as `seL4_RISCV_4K_Page`, `seL4_RISCV_Mega_Page`, `seL4_RISCV_Giga_Page`.
(The naming is inconsistent between the two architectures, `*PageObject` against `*_Page`. If we take
the shape, we should not take that.)

**Two corrections to what was assumed alongside the claim.** The manual states the **virtual**
address alignment explicitly (section 7.1.2: *"The virtual address for a Page mapping must be aligned to the
size of the Page"*, and `seL4_ARM_Page_Map` returns `seL4_AlignmentError` for it), and this lane
found **no sentence stating a physical alignment requirement**; that follows from retype allocating
size-aligned within an untyped rather than from a rule anyone wrote down, so the manual should not be
cited for it. And the AArch64 page-size table is **not** qualified by hypervisor configuration: the
guess that `arm_hyp` or a 40-bit PA changes the frame sizes is not supported by this revision.

Note also what seL4 does *not* do, because section 5's overlap argument depends on it: it has no
`Frame::SPLIT`. A large frame is retyped from untyped, the untyped is spent, and the same memory
cannot also be a small frame. That is what keeps its frame capabilities equal-or-disjoint, and it is
the invariant we would be putting at risk.

### Fuchsia VMOs: correct, and the qualification is where the size lives

The claim was that one handle names an arbitrarily-sized memory object and page granularity is not
in the capability model. **Correct.** From Zircon's own syscall definitions
(`zircon/vdso/vmo.fidl`, read at `fuchsia.googlesource.com`):

> `zx_vmo_create()` creates a new, zero-filled, [virtual memory object] (VMO), which represents a
> container of zero to *size* bytes of memory managed by the operating system. [...] The size of the
> VMO will be rounded up to the next system page size boundary [...] One handle is returned on
> success, representing an object with the requested size.

**The qualification, and it is the one that matters here: the size is not in the capability either.**
The handle is opaque; the extent is kernel state, reached by `zx_vmo_get_size()`. That is the
**object-identity** shape, and it has a consequence this fork should weigh: **delegating a subrange
costs a second kernel object.** `zx_vmo_create_child` with `ZX_VMO_CHILD_SLICE` takes an arbitrary
page-aligned `(offset, size)` and mints a new handle with a parent/child relationship the kernel
tracks. Attenuating a *mapping* is free (`zx_vmar_map` takes `(vmo_offset, len)`); attenuating the
*object* is not.

And the framebuffer case is spelled out, privileged: `zx_vmo_create_physical(resource, paddr, size,
out)` turns a `(paddr, size)` run into one handle and requires an MMIO resource, while
`zx_vmo_create_contiguous` requires a Bus Transaction Initiator handle. **Contiguity is a driver
privilege there, not a property of ordinary memory**, which is the same division this tree already
makes between `Frame` and `DeviceFrame`.

### Mach memory objects: partly, and the paternity is folklore

Two halves, and both need correcting.

**A Mach memory object is a port right, so it is a capability, but it carries no size.** The extent
is a parameter of the mapping call, not a property of the object. GNU Mach's reference manual, §5.5
(`gnu.org/software/hurd/gnumach-doc/Mapping-Memory-Objects.html`):

> *memory_object* is the port that represents the memory object: used by user tasks in `vm_map`
> [...] Within a memory object, *offset* specifies an offset in bytes.

with `size` a separate argument of `vm_map`. And a memory object is **backing store served by a
userspace pager**, not a run of physical pages: *"A memory manager is a server task that responds to
specific messages from the kernel in order to handle memory management functions for the kernel."*
So it answers "how do I name backing store" rather than "how do I name 469 physical pages", and it is
*further* from this fork than a VMO is. The thing in Mach that resembles a VMO is the kernel-internal
`vm_object`, which is not a port and therefore not a capability at all; conflating the two is the
specific error available here.

**And the ancestry claim is not documented.** Fuchsia's own record of where Zircon came from
(`docs/concepts/kernel/zx_and_lk.md`) says:

> Zircon was born as a branch of LK and even now many inner constructs are based on LK while the
> layers above are new.

Neither the VMO page nor the ancestry page mentions Mach, and this lane found no Fuchsia primary
source claiming it. **Do not write that VMOs descend from Mach memory objects.** The defensible
sentence is that they occupy the same design slot, convergently.

### What the four did not include, and one of them is this fork's exact shape

Five more systems were read because a maintainer's list is not a survey. Three change the picture.

#### Barrelfish: `(base, bytes)` in the capability, and the honest price

**This is option A, built, documented to the bit layout, and shipped.** Barrelfish Technical Note
013, *Capability Management*, rev 3.0, §3.2.8 (`barrelfish.org/publications/TN-013-CapabilityManagement.pdf`):

> A frame capability refers to a page-aligned region of physical memory with a size that is a
> multiple of 4096 bytes. A frame capability may be mapped into a domain's virtual address space
> (by copying it to a VNode).

```
datatype frame cap "Frame capability" {
   base   64 "Physical base address of mappable region";
   bytes  64 "Size of the region";
};
```

**And it makes a split this tree should copy or refuse deliberately.** §3.2.4:

> A RAM capability refers to a naturally-aligned power-of-two-sized region of kernel-accessible
> memory.

So the **allocation** type keeps the power-of-two, naturally-aligned discipline that makes an
allocator and a derivation tree tractable, and only the **mappable** type relaxes to
page-aligned-multiple-of-4096. That is precisely the seam this tree has between `untyped` (which is
where the no-double-free proof lives) and `Frame` (which is what gets mapped). It says option A can
be taken **without touching `crates/regions`' arithmetic at all**, which is the same conclusion
D-narrow reached by a different route.

**The price Barrelfish pays, and it is the one to price here.** Arbitrary overlapping ranges need a
mapping database to make revoke work, and TN-013 records that its insert *"is logarithmic time in the
size of the mapping database"*. This tree's `revoke` is a flat table keyed on an exact physical
address. Option A does not merely change a key; it changes a lookup from equality to containment,
and containment over overlapping ranges is a different data structure.

Worth knowing for a different reason: Barrelfish's L1 CNodes are resizable to 2^24 slots. **It did
not solve this by growing the cspace**; it grew the cspace for unrelated reasons and solved this by
putting the run in the capability.

#### KeyKOS: the same sixteen slots, answered with a tree

The coincidence is worth the citation on its own. From the KeyKOS architecture notes
(`cap-lore.com/Agorics/Library/KeyKos/Architecture/Pages.Nodes.html`):

> A page holds 4096 bytes of data and a node has sixteen slots.

and from the *KeyKOS Nanokernel Architecture* paper (§4):

> A segment is a collection of pages or other segments. [...] Nodes are the glue that holds segments
> together. KeyKOS implements segments as a tree of nodes with pages as the leaves of the tree.

**A system with this tree's exact slot count met this exact problem and answered it with a tree
rather than by widening the slot.** A segment's size is a power of sixteen, and sub-segments are
page-granular, so attenuation is finer than the size quantum. EROS narrowed the same idea by putting
the **height of the tree in the capability itself** (*"Node capabilities encode the height of the
tree that they name"*, SOSP'99 §3.1), which is the first design on this list to put extent metadata
in the capability word.

This is a fifth option, and this lane is not proposing it: a tree of capabilities is a much larger
change than any of the four, and this tree has no node object. It is here because it is the answer a
sixteen-slot system actually gave.

#### Coyotos: the documented failure mode of the power-of-two family

Coyotos replaced EROS's nodes with GPTs (also sixteen slots) and made the size explicit as `l2v`:
*"Each slot of the GPT names a subspace of size 2^l2v bytes."* **And it names, precisely, what a
power-of-two answer costs at delegation time.** From the Coyotos Microkernel Specification v0.6+,
§4.3:

> the invoker may wish to transmit a 2^11 page (2^23 byte) subspace, but the subspace may currently
> be dominated by a GPT having l2v=21. That is: there is no single slot in the GPT that directly
> holds a capability of the desired span. [...] When such a send is attempted, the invoker will
> receive a `SplitFault` exception.

with the whole scheme conditioned on the region being *"naturally aligned"*. **Read against this
fork: 475 pages can never be one Coyotos capability, and it could never be one L4 fpage either.** The
power-of-two family (fpages, KeyKOS segments, EROS heights, Coyotos `l2v`) buys a compact encoding
and pays in expressiveness, and option D is a member of that family. That is not an argument against
D, because D rounds *up* into a 2 MiB page and wastes 37 pages rather than failing; it is the reason
D's waste is structural rather than incidental.

*Provenance caveat: `coyotos.org` returns HTTP 522, so this was read from a third-party cache
(`hydra-www.ietfng.org/capbib/cache/shapiro:coyotosspec.html`) and not diffed against the canonical
copy.*

#### Genode, and one encoding note

**Genode's dataspace is L4Re's answer generalised** (Genode Foundations 24.05, §3.4.1):

> A dataspace is an RPC object that resides in core and represents a contiguous physical
> address-space region with an arbitrary size.

One capability, arbitrary size, **physically contiguous by definition**, with the size retrieved by
invoking `size()` rather than read out of the capability. Partial delegation is at the region map:
a holder *"has the option to attach a mere window of the dataspace"*. Same object-identity shape as
Fuchsia, arrived at independently.

And one encoding note, from CHERI Concentrate (IEEE ToC 2019), because it is the only system here
that measured what an explicit run costs. CHERI's 256-bit format stored base, length and address as
three independent 64-bit fields and was **abandoned for size**; its predecessor's power-of-two
segments were rejected because *"this power-of-two alignment restriction prevents precise enforcement
of irregular object sizes"* and padding *"results in severe memory fragmentation"*. The compromise
the paper credits (Low-fat) stores a run as base-block, top-block and a block-size exponent, which
read with the page as the block is: **store a page count, not a byte length.** If option A wins and
capability width ever matters, that is the shape to reach for.

### What the survey settles

**Two shapes exist and they are not a spectrum.**

- **Object identity** (Fuchsia VMO, Genode dataspace, L4Re dataspace, Mach memory object, and this
  tree's own `Untyped`): the capability is an opaque name, the extent is kernel state. Delegating a
  subrange costs a new kernel object.
- **The run in the capability** (Barrelfish frame cap): `(base, bytes)` with no rounding. Delegating
  a subrange is a retype at an offset. Pays for it in a mapping database with containment lookups.
- **The power-of-two family** (L4 fpages, KeyKOS, EROS, Coyotos, seL4's frame sizes) is a third
  thing rather than a middle: a region that is not naturally aligned and power-of-two sized is not
  expressible as one capability at all.

Option A is the second shape or the first; option D is the third. **The tree already has a worked
example of the first** (`Untyped`), which is the cheapest fact in this whole section and the one that
should probably decide the encoding once the shape is chosen.

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

**And the obvious workaround does not work, which is worth writing down before somebody proposes
it.** The driver is not what is scarce. It maps the surface only to read pixels back
(`user/src/display.rs`, `surface_pixel`); the device is handed a *physical* address it already has
in `a1`, so a driver that gave up its readback could hold **one** DMA frame instead of ten. That
moves the ceiling to the painting client, whose cspace starts its surface at slot 3
(`CLIENT_SLOT_SURFACE`) and so holds at most **twelve** frames: 49,152 bytes, a 128x96 surface, and
**a 16x6 text grid.** Rearranging slots buys two rows. **The binding limit is sixteen slots against
a run of hundreds, and no allocation of them among the parties changes the order of magnitude.**

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

**The price the prior art names and this lane would otherwise have missed**: `revoke` currently looks
a mapping up by **equality** on a physical address, and runs turn that into **containment**, over
ranges that may overlap. Barrelfish, which built exactly this capability, pays for it with a mapping
database whose insert is *"logarithmic time in the size of the mapping database"*. That is the real
cost of option A, and it is not in the syscall.

**And the prior art also names the mitigation.** Barrelfish keeps its *allocation* type
(RAM) naturally-aligned and power-of-two and relaxes only its *mappable* type (Frame) to a
page-multiple run. This tree has the same seam: `untyped` is where the no-double-free proof lives and
`Frame` is what gets mapped. Option A can therefore leave `crates/regions` alone, which is the same
place D-narrow lands from the other direction.

**What it forecloses.** Nothing structurally; it is the most general answer. It is also the one that
cannot be walked back, because "a Frame is a page" is a sentence in a dozen files and in the head of
anyone who has read them.

**What it buys beyond this milestone.** Any future driver with a large DMA region, which is every
driver worth having: a NIC's ring buffers, an NVMe queue pair, a second display.

### B. Grow `CSPACE_SLOTS`

**Dominated by arithmetic** (section 5). Include it only as the small, orthogonal change it actually is.

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
seL4's answer, L4's, KeyKOS's, EROS's and Coyotos's. That family's cost is documented rather than
speculative: Coyotos raises a `SplitFault` when a region is not naturally aligned and power-of-two
sized, because such a region **is not expressible as one capability at all**. D is the gentler member
of the family, because it rounds up and wastes rather than failing, but the 37 wasted pages are
structural and not an artefact of this surface's dimensions.

It does not foreclose A: a run of large frames is still a run.

---

## 7. How reversible is it, and who has already acted on it

**A, C and D all touch things two programs agree on**, which is the *move fast on what can be undone*
tenet's first irreversible category:

- **A and D change `frame::MAP`'s meaning**, and `Frame::MAP` is called by seven migrated programs
  (`disk_surveyor`, the roster probe, `disk_partitioner`, `mkfs`, the virtio-gpu driver, `painter`,
  `display_terminal`). A change here is not a change to one call site; it is a change to what those
  programs mean.
- **The revocation key is a wire fact**, because §13's guarantee is asserted in prose, in tests and
  in `notes/frames.md`, and rests on a frame being a page. Weakening it by admitting overlapping
  frames is the kind of fact that leaves the machine.
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

- the `size_of::<Thread>() <= FRAME_SIZE` assertion and the corrected `CSPACE_SLOTS` comment (section 5),
  which are worth doing whichever option wins;
- whether a large frame, if D wins, is spelled as an **order** (seL4's shape: 4 KiB / 2 MiB / 1 GiB
  and nothing between) or as a **page count** (A's shape at a coarser grain). They are different
  claims about what memory is.
