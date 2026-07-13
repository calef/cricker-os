# aarch64 page tables

The data structure the MMU walks. See [mmu.md](mmu.md) for *why*; this is *how*.

## The shape

Four levels, 4 KiB pages, 48-bit virtual addresses.

```
 47      39 38      30 29      21 20      12 11         0
┌──────────┬──────────┬──────────┬──────────┬────────────┐
│ L0 index │ L1 index │ L2 index │ L3 index │   offset   │
│  9 bits  │  9 bits  │  9 bits  │  9 bits  │  12 bits   │
└──────────┴──────────┴──────────┴──────────┴────────────┘
```

Each table is exactly one 4 KiB page holding 512 eight-byte descriptors, and 9 bits of index
selects one of 512.

**That is not a coincidence.** The page size, the descriptor size, and the index width are
chosen so that a table is *exactly one page*. Which means the frame allocator can supply page
tables and nothing else is needed. The whole structure is self-hosting on 4 KiB frames.

## The thing a failing test taught us

**Bits 63:48 are not translated. They choose which *table* to use.**

| Top 16 bits | Register | Who |
|---|---|---|
| all **zero** | `TTBR0_EL1` | userspace (low half) |
| all **one** | `TTBR1_EL1` | the kernel (high half) |

The 48-bit index is extracted from bits 47:12 **identically for both**. Which means, within a
single table, these are *the same entry*:

```
0xffff_0000_4008_0000
0x0000_0000_4008_0000
```

So a **higher-half kernel works because `TTBR1` is a separate set of tables**, not because
high addresses index somewhere different. That is not what you would guess, and a host test
found it: an assertion that the physical address "would not be mapped" failed, because it
was the same descriptor.

Anything in between (top bits neither all-zero nor all-one) is a **non-canonical address**
and faults before any table is consulted. There is no memory there and there never can be.

`Half::Low` / `Half::High` in the `paging` crate encode this, and `map()` refuses an address
from the wrong half. Without that check, mapping a kernel address into the userspace tables
builds a mapping the CPU will *never consult*, and you chase the ghost for hours.

## The descriptor, and its traps

### The same two bits mean different things at different levels

Bits [1:0]:

| Value | At L0-L2 | At L3 |
|---|---|---|
| `0b11` | **table** pointer | **page** |
| `0b01` | **block** (1 GiB at L1, 2 MiB at L2) | reserved |
| `0b00` / `0b10` | invalid | invalid |

**A descriptor is not self-describing.** There is no "I am a page" bit; the *level* says it.
Which is why you cannot walk a page table without tracking what level you're at.

### AF, bit 10. Forget it and nothing works.

If the **Access Flag** is clear, the *first* access to that page raises an "Access Flag fault"
instead of succeeding.

The bit exists so an OS can do page-replacement policy: hardware sets it on first touch, and
the kernel periodically clears it to see which pages are actually being used. We are not doing
page replacement, so we set it eagerly on every mapping and the hardware never bothers us.

Leaving it clear produces a fault that looks *nothing* like "you forgot a bit," which is why
it is the single most common aarch64 paging bug there is. Every `Flags` constructor sets it,
and there is a test that will fail if a new one forgets.

### AP, bits [7:6]. The encoding is not intuitive.

| AP | EL1 (kernel) | EL0 (user) |
|----|--------------|------------|
| `00` | read/write | **no access** |
| `01` | read/write | read/write |
| `10` | read-only | no access |
| `11` | read-only | read-only |

Read it as: **bit 7 means read-only, bit 6 means userspace may touch it.**

### PXN and UXN, bits 53 and 54

*Privileged* eXecute Never and *Unprivileged* eXecute Never. Two separate bits, and the
distinction matters enormously:

**`PXN` on user pages is not paranoia.** Without it, a kernel bug that jumps into a user page
executes **user-controlled instructions at EL1**. Total compromise. The defence is one bit.

### AttrIndx, bits [4:2]: memory *type* is indirect

The descriptor doesn't say "this is device memory." It says **"look up slot N"**, and
`MAIR_EL1` says what slot N means. Eight slots, three bits per page.

We use:

| Slot | Meaning |
|---|---|
| 0 | **Device-nGnRnE.** No gathering, no reordering, no early write acknowledgement. |
| 1 | **Normal, write-back cacheable.** |

**MMIO must be device-typed.** Map the UART as *normal* memory and the CPU may cache it,
reorder writes to it, merge two writes into one, and **speculatively read it**. Every one of
those is catastrophic for a device, because reading a FIFO register *has a side effect*. The
byte is consumed.

If the descriptor's `AttrIndx` and `MAIR_EL1`'s contents ever disagree, the UART gets mapped
as cacheable normal memory and the machine behaves like it is haunted.

## W^X, enforced by construction

`Flags` has named constructors and no bag-of-bools builder. There is deliberately **no
`Flags::writable_and_executable()`**.

A page that is both writable and executable is how a buffer overflow becomes code execution.
The test `nothing_is_both_writable_and_executable` iterates every constructor, so adding a bad
one is a build failure rather than a security hole.

| Constructor | EL1 | EL0 |
|---|---|---|
| `kernel_code()` | read + execute | nothing |
| `kernel_rodata()` | read | nothing |
| `kernel_data()` | read + write | nothing |
| `device()` | read + write, device-typed | nothing |
| `user_code()` | read (**PXN**: no execute) | read + execute |
| `user_data()` | read + write | read + write |

## Blocks: the optimization we haven't taken yet

A **block** descriptor at L1 or L2 short-circuits the walk and maps a big contiguous region
directly: 1 GiB at L1, 2 MiB at L2.

Mapping 128 MiB of RAM with 4 KiB pages costs **32768 descriptors and 64 tables**. With 2 MiB
blocks it costs **64 descriptors and one table**. The kernel's direct map will want them.

We aren't using them yet, because correctness first and the tests are easier with one code
path. The constant is defined and the reason is written down, which is the honest state.

## Why this is a host-testable crate

The page table format is pure logic: addresses in, descriptors out. So it lives in
`crates/paging` and its tests run on the host in milliseconds (DECISIONS.md §7).

**The trick that makes it work:** a `Box<PageTable>` is 4 KiB-aligned (the type declares it)
and has a real address. So the tests hand those addresses to the mapper as pretend "physical"
frames, and `phys_to_ptr` is the identity cast. **The pointer arithmetic is bit-for-bit what
the kernel does.** We are testing the real code path, not a model of it.

That's what caught the `Half` discovery above, at zero cost, before a single instruction ran
on the machine.

---

*Add to this file as new paging details come up.*
