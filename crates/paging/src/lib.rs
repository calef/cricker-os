//! aarch64 page tables.
//!
//! Four levels, 4 KiB pages, 48-bit virtual addresses. A virtual address is chopped up
//! like this:
//!
//! ```text
//!  47      39 38      30 29      21 20      12 11         0
//! ┌──────────┬──────────┬──────────┬──────────┬────────────┐
//! │ L0 index │ L1 index │ L2 index │ L3 index │   offset   │
//! │  9 bits  │  9 bits  │  9 bits  │  9 bits  │  12 bits   │
//! └──────────┴──────────┴──────────┴──────────┴────────────┘
//! ```
//!
//! Each table is exactly one 4 KiB page holding 512 eight-byte descriptors. 9 bits of
//! index selects one of 512. That is not a coincidence: the page size, the descriptor
//! size, and the index width are chosen so that **a table is exactly one page**, which
//! means the frame allocator can supply page tables and nothing else is needed.
//!
//! # Why this is a separate crate
//!
//! It is pure logic: addresses in, descriptors out. The host tests build real page tables
//! in real memory (using host allocations as pretend physical frames, which works because
//! the pointer arithmetic is identical) and walk them back. Milliseconds, no emulator.
//! DECISIONS.md §7.

#![cfg_attr(not(test), no_std)]

pub const PAGE_SIZE: u64 = 4096;

/// Descriptors per table. 4096 bytes / 8 bytes.
pub const ENTRIES: usize = 512;

/// L0, L1, L2, L3.
pub const LEVELS: usize = 4;

/// A page table: one 4 KiB frame of descriptors.
///
/// `repr(C, align(4096))` matters. The hardware requires a table to be page-aligned (it
/// takes bits [47:12] of the descriptor as the next table's address and assumes the low 12
/// are zero), and this is what lets the host tests allocate real, correctly-aligned tables.
#[repr(C, align(4096))]
#[derive(Clone)]
pub struct PageTable {
    pub entries: [u64; ENTRIES],
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PageTable {
    pub const fn new() -> Self {
        Self {
            entries: [0; ENTRIES],
        }
    }
}

/// Which 9-bit slice of the address selects an entry at this level.
///
/// L0 uses bits 47:39, L1 uses 38:30, L2 uses 29:21, L3 uses 20:12.
pub const fn index(va: u64, level: usize) -> usize {
    let shift = 39 - 9 * level;
    ((va >> shift) & 0x1ff) as usize
}

// --- Descriptor bits ---
//
// The aarch64 descriptor format is compact and full of traps. The two that will actually
// bite you are called out below.

/// Bits [1:0] = 0b11 means "valid, and a table pointer" at L0-L2, or **"valid, and a
/// page"** at L3.
///
/// **The same two bits mean different things depending on the level.** There is no bit that
/// says "I am a page"; the level says it. A descriptor is not self-describing, which is why
/// you cannot walk a page table without knowing what level you are at.
const VALID: u64 = 1 << 0;
const TABLE_OR_PAGE: u64 = 1 << 1;

/// Bits [1:0] = 0b01: a **block** descriptor at L1 (1 GiB) or L2 (2 MiB).
///
/// A block short-circuits the walk: instead of pointing at another table, it maps a big
/// contiguous region directly. We don't use them yet, but the kernel's direct map will want
/// 2 MiB blocks eventually, because mapping 128 MiB of RAM with 4 KiB pages costs 32768
/// descriptors and 64 tables, and with 2 MiB blocks it costs 64 descriptors and one table.
#[allow(dead_code)]
const BLOCK: u64 = VALID;

/// **Bit 10, the Access Flag. Forget this and nothing works.**
///
/// If AF is clear, the *first* access to the page raises an "Access Flag fault" instead of
/// succeeding. The bit exists so an OS can implement page-replacement policy: the hardware
/// sets it on first touch, and the kernel can periodically clear it to see which pages are
/// actually being used.
///
/// We are not doing page replacement. We set it eagerly on every mapping, and the hardware
/// never bothers us. Leaving it clear produces a fault that looks nothing like "you forgot
/// a bit", which is why it is the single most common aarch64 paging bug.
const AF: u64 = 1 << 10;

/// Bits [9:8], shareability. `0b11` = inner shareable.
///
/// Tells the hardware how far coherency must extend. For normal cacheable memory on a
/// system with more than one core (which is every system we care about), inner shareable is
/// the answer. Get it wrong and caches quietly stop being coherent between cores, which is
/// a bug you will find in about six months.
const SH_INNER: u64 = 0b11 << 8;

/// Bits [4:2]: which of the eight `MAIR_EL1` slots describes this memory's *type*.
///
/// The descriptor doesn't say "this is device memory." It says "look up slot N," and
/// `MAIR_EL1` says what slot N means. One level of indirection, so the eight attribute
/// combinations you actually use fit in three bits per page.
const fn attr_index(slot: u64) -> u64 {
    (slot & 0b111) << 2
}

/// Bits [7:6], access permission. The encoding is not intuitive:
///
/// | AP | EL1 (kernel) | EL0 (user) |
/// |----|--------------|------------|
/// | 00 | read/write   | **no access** |
/// | 01 | read/write   | read/write |
/// | 10 | read-only    | no access |
/// | 11 | read-only    | read-only |
///
/// Read it as: **bit 7 means read-only, bit 6 means userspace may touch it.**
const AP_RW_EL1: u64 = 0b00 << 6;
const AP_RW_EL0: u64 = 0b01 << 6;
const AP_RO_EL1: u64 = 0b10 << 6;
const AP_RO_EL0: u64 = 0b11 << 6;

/// Bit 53: Privileged eXecute Never. The kernel may not execute this page.
const PXN: u64 = 1 << 53;

/// Bit 54: Unprivileged eXecute Never. Userspace may not execute this page.
const UXN: u64 = 1 << 54;

/// Physical address bits of a descriptor: [47:12].
const ADDR_MASK: u64 = 0x0000_ffff_ffff_f000;

/// Which `MAIR_EL1` slot describes what. Set up in the kernel; the numbers must agree.
pub mod mair {
    /// Slot 0: device memory, nGnRnE. No gathering, no reordering, no early write ack.
    ///
    /// The strictest possible ordering, and it is what MMIO needs. Mapping the UART as
    /// *normal* memory would let the CPU cache it, reorder writes to it, merge two writes
    /// into one, and speculatively read it. Every one of those is catastrophic for a
    /// device: a "read" of a FIFO register has a side effect.
    pub const DEVICE: u64 = 0;

    /// Slot 1: normal memory, write-back, read-allocate, write-allocate, cacheable.
    pub const NORMAL: u64 = 1;

    /// The value to write into `MAIR_EL1`: eight bytes, one per slot.
    ///
    /// - Slot 0 = `0x00` — Device-nGnRnE
    /// - Slot 1 = `0xff` — Normal, WB non-transient, RW-allocate, inner+outer
    pub const VALUE: u64 = (0xff << 8) | 0x00;
}

/// What a mapping is allowed to do.
///
/// Built as named constructors rather than a bag of bools, because the combinations that
/// make sense are few and the ones that don't are dangerous. There is no
/// `Flags::writable_and_executable()`, on purpose: **W^X**. A page that is both writable
/// and executable is how a buffer overflow becomes code execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u64);

impl Flags {
    /// Kernel code: readable and executable by EL1, never writable, never executable by
    /// EL0.
    pub const fn kernel_code() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RO_EL1 | UXN)
    }

    /// Kernel constants: readable by EL1, never writable, never executable by anyone.
    pub const fn kernel_rodata() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RO_EL1 | UXN | PXN)
    }

    /// Kernel data, stacks, heap: read/write by EL1, never executable by anyone.
    pub const fn kernel_data() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RW_EL1 | UXN | PXN)
    }

    /// MMIO. Device-typed, and never executable.
    ///
    /// Note: **no `SH_INNER`.** Shareability is meaningless for device memory, which is
    /// never cached, and the architecture ignores the field. Setting it would be harmless
    /// but would suggest we didn't know that.
    pub const fn device() -> Self {
        Flags(AF | attr_index(mair::DEVICE) | AP_RW_EL1 | UXN | PXN)
    }

    /// User code (milestone 7): executable by EL0, never by EL1.
    ///
    /// PXN is not paranoia. Without it, a bug that jumps the kernel into a user page would
    /// execute *user-controlled instructions at EL1*. That is a total compromise, and PXN is
    /// one bit.
    pub const fn user_code() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RO_EL0 | PXN)
    }

    /// User data (milestone 7): read/write by EL0, never executable.
    pub const fn user_data() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RW_EL0 | UXN | PXN)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub fn is_writable(self) -> bool {
        // Bit 7 clear = read/write.
        self.0 & (1 << 7) == 0
    }

    pub fn is_kernel_executable(self) -> bool {
        self.0 & PXN == 0
    }

    pub fn is_user_executable(self) -> bool {
        self.0 & UXN == 0
    }

    pub fn is_user_accessible(self) -> bool {
        self.0 & (1 << 6) != 0
    }
}

/// Which translation table base register a set of page tables belongs to.
///
/// # The thing that is easy to get wrong
///
/// **Bits 63:48 of a virtual address are not translated.** They are not part of any index.
/// They select *which table to use*:
///
/// | Top 16 bits | Register | Who |
/// |---|---|---|
/// | all **zero** | `TTBR0_EL1` | userspace (low half) |
/// | all **one**  | `TTBR1_EL1` | the kernel (high half) |
///
/// The 48-bit index is then extracted from bits 47:12 **identically** for both. Which means
/// `0xffff_0000_4008_0000` and `0x0000_0000_4008_0000` are *the same entry* within a single
/// table. They differ only in which table the hardware consults.
///
/// So a higher-half kernel works because **`TTBR1` is a separate set of tables**, not
/// because high addresses index differently. A test discovered this the hard way, which is
/// exactly what host tests are for.
///
/// Anything in between (top bits neither all-zero nor all-one) is a **non-canonical
/// address** and faults immediately. There is no memory there and never can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    /// `TTBR0_EL1`. Virtual addresses `0x0000_0000_0000_0000` ..= `0x0000_ffff_ffff_ffff`.
    Low,
    /// `TTBR1_EL1`. Virtual addresses `0xffff_0000_0000_0000` ..= `0xffff_ffff_ffff_ffff`.
    High,
}

impl Half {
    /// Does this address belong to this half?
    pub const fn contains(self, va: u64) -> bool {
        let top = va >> 48;
        match self {
            Half::Low => top == 0,
            Half::High => top == 0xffff,
        }
    }

    /// The address a `Half::High` mapping actually indexes with.
    ///
    /// (Nothing, in the end: `index()` ignores bits 63:48 anyway. This exists to make the
    /// reader's mental model explicit rather than to change any bits.)
    pub const fn base(self) -> u64 {
        match self {
            Half::Low => 0,
            Half::High => 0xffff_0000_0000_0000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// Ran out of frames while building intermediate tables.
    OutOfFrames,
    /// The virtual or physical address is not 4 KiB aligned.
    Misaligned,
    /// Something is already mapped here. Overwriting silently is how you lose a page and
    /// never find out.
    AlreadyMapped,
    /// This address belongs to the *other* half, or is non-canonical.
    ///
    /// Mapping a kernel address into the userspace tables would silently do nothing useful:
    /// the hardware would never consult this table for that address. Catching it here turns
    /// a mystery into a compile-time-shaped error.
    WrongHalf,
}

/// Builds page tables.
///
/// # Safety contract
///
/// The mapper dereferences physical addresses directly. That is sound in exactly two
/// situations, and both apply to us:
///
/// - **the MMU is off**, so an address *is* a physical address (how we build the first
///   table), or
/// - **physical memory is identity-mapped or direct-mapped**, so a physical address can be
///   turned into a usable pointer by a known transform (how we edit tables afterwards).
///
/// `phys_to_ptr` is that transform. The host tests pass the identity function, and it works
/// because a host allocation's address is as good a "physical address" as any: the pointer
/// arithmetic is identical.
pub struct Mapper<A, P>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    root: u64,
    half: Half,
    alloc_frame: A,
    phys_to_ptr: P,
}

impl<A, P> Mapper<A, P>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
{
    /// # Safety
    /// `root` must be a zeroed, page-aligned frame, and `phys_to_ptr` must satisfy the
    /// contract in the type's docs.
    pub unsafe fn new(root: u64, half: Half, alloc_frame: A, phys_to_ptr: P) -> Self {
        Self {
            root,
            half,
            alloc_frame,
            phys_to_ptr,
        }
    }

    pub fn root(&self) -> u64 {
        self.root
    }

    pub fn half(&self) -> Half {
        self.half
    }

    /// Map one 4 KiB page.
    ///
    /// Walks L0 → L1 → L2 → L3, creating tables as needed, and writes a page descriptor at
    /// the bottom.
    pub fn map(&mut self, va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
        // The hardware picks TTBR0 or TTBR1 from bits 63:48 before it touches an index. So
        // mapping a high address into the low tables would build a mapping the CPU will
        // never consult, and we'd chase the ghost for hours. See `Half`.
        if !self.half.contains(va) {
            return Err(MapError::WrongHalf);
        }

        if va % PAGE_SIZE != 0 || pa % PAGE_SIZE != 0 {
            return Err(MapError::Misaligned);
        }

        let mut table_pa = self.root;

        // Descend through L0, L1, L2, creating intermediate tables as we go.
        for level in 0..LEVELS - 1 {
            let i = index(va, level);

            // SAFETY: `table_pa` is a page-aligned table, per the type's contract.
            let entry = unsafe { &mut (*(self.phys_to_ptr)(table_pa)).entries[i] };

            if *entry & VALID == 0 {
                let new = (self.alloc_frame)().ok_or(MapError::OutOfFrames)?;

                // SAFETY: a fresh frame from the allocator. Zero it before it becomes
                // reachable by the hardware, or the walk reads whatever garbage was in RAM
                // and follows it somewhere fatal.
                unsafe {
                    (*(self.phys_to_ptr)(new)).entries = [0; ENTRIES];
                }

                // A *table* descriptor. No attributes here: permissions on an intermediate
                // table would be an ADDITIONAL restriction on everything beneath it, and
                // we want the leaf to be the single source of truth.
                *entry = (new & ADDR_MASK) | TABLE_OR_PAGE | VALID;
            }

            table_pa = *entry & ADDR_MASK;
        }

        // L3: the leaf.
        let i = index(va, LEVELS - 1);
        // SAFETY: as above.
        let entry = unsafe { &mut (*(self.phys_to_ptr)(table_pa)).entries[i] };

        if *entry & VALID != 0 {
            return Err(MapError::AlreadyMapped);
        }

        // At L3, `0b11` means PAGE, not TABLE. Same bits, different meaning. See the
        // constant's docs.
        *entry = (pa & ADDR_MASK) | flags.bits() | TABLE_OR_PAGE | VALID;

        Ok(())
    }

    /// Map `count` consecutive pages.
    pub fn map_range(
        &mut self,
        va: u64,
        pa: u64,
        count: u64,
        flags: Flags,
    ) -> Result<(), MapError> {
        for i in 0..count {
            self.map(va + i * PAGE_SIZE, pa + i * PAGE_SIZE, flags)?;
        }
        Ok(())
    }

    /// Walk the tables and report what a virtual address actually maps to.
    ///
    /// This is what the hardware does on every single memory access, in silicon, and it is
    /// worth having in software: it is the only way to *check* that the tables say what you
    /// think they say, and at milestone 4 that check is the difference between a working
    /// kernel and a machine that vanishes.
    pub fn translate(&self, va: u64) -> Option<(u64, Flags)> {
        if !self.half.contains(va) {
            return None;
        }

        let mut table_pa = self.root;

        for level in 0..LEVELS - 1 {
            let i = index(va, level);
            // SAFETY: per the type's contract.
            let entry = unsafe { (*(self.phys_to_ptr)(table_pa)).entries[i] };

            if entry & VALID == 0 {
                return None;
            }
            table_pa = entry & ADDR_MASK;
        }

        let i = index(va, LEVELS - 1);
        // SAFETY: per the type's contract.
        let entry = unsafe { (*(self.phys_to_ptr)(table_pa)).entries[i] };

        if entry & VALID == 0 {
            return None;
        }

        let offset = va % PAGE_SIZE;
        Some((
            (entry & ADDR_MASK) + offset,
            Flags(entry & !ADDR_MASK & !VALID & !TABLE_OR_PAGE),
        ))
    }
}
