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
    // `| 0x00` is spelled out on purpose: it is slot 0 (Device-nGnRnE). Writing both bytes keeps
    // the one-byte-per-slot layout legible next to the doc above; clippy sees a no-op OR.
    #[allow(clippy::identity_op)]
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

    /// User constants (milestone 7): readable by EL0, and **nothing else**.
    ///
    /// An ELF's `.rodata` segment is `PF_R` alone. Without this, the loader's only non-executable
    /// choice is [`user_data`](Self::user_data), which is **writable** — so we would silently
    /// grant the program more authority than its own file asked for. A loader that widens
    /// permissions is a loader you cannot reason about.
    pub const fn user_rodata() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RO_EL0 | UXN | PXN)
    }

    /// User data (milestone 7): read/write by EL0, never executable.
    pub const fn user_data() -> Self {
        Flags(AF | SH_INNER | attr_index(mair::NORMAL) | AP_RW_EL0 | UXN | PXN)
    }

    /// **User device memory (milestone 8): a driver's MMIO, at EL0.**
    ///
    /// This is the flag that lets a driver leave the kernel. A userspace console server holds a
    /// mapping of the PL011's registers with *these* bits, and its EL0 stores go straight to the
    /// hardware. Device-typed (so the CPU does not cache or reorder register writes, exactly as
    /// [`device`](Self::device) for the kernel's own MMIO), user read/write, and never
    /// executable.
    ///
    /// No `SH_INNER`: shareability is meaningless for device memory and the architecture ignores
    /// the field. See the note on [`device`](Self::device).
    pub const fn user_device() -> Self {
        Flags(AF | attr_index(mair::DEVICE) | AP_RW_EL0 | UXN | PXN)
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

#[cfg(test)]
mod flag_tests {
    use super::*;

    /// **W^X, as a property of the type rather than of our discipline.**
    ///
    /// There is no constructor that returns a page which is both writable and executable, and
    /// this asserts it over every constructor there is, so adding a bad one fails the build's
    /// tests rather than shipping.
    #[test]
    fn nothing_is_both_writable_and_executable() {
        for f in [
            Flags::kernel_code(),
            Flags::kernel_rodata(),
            Flags::kernel_data(),
            Flags::device(),
            Flags::user_code(),
            Flags::user_rodata(),
            Flags::user_data(),
            Flags::user_device(),
        ] {
            assert!(
                !(f.is_writable() && (f.is_kernel_executable() || f.is_user_executable())),
                "{f:?} is both writable and executable",
            );
        }
    }

    /// **The execute split, over every constructor there is.** Anything EL0 can reach is PXN
    /// (the kernel never executes user-reachable memory, so a wild kernel jump into a user page
    /// faults instead of running user-chosen instructions at EL1), and anything EL0 cannot reach
    /// is UXN (userspace never executes kernel memory, even as defense in depth behind the AP
    /// bits). Like the W^X test: adding a constructor that breaks the split fails the build's
    /// tests rather than shipping.
    #[test]
    fn no_page_is_executable_across_the_privilege_split() {
        for f in [
            Flags::kernel_code(),
            Flags::kernel_rodata(),
            Flags::kernel_data(),
            Flags::device(),
            Flags::user_code(),
            Flags::user_rodata(),
            Flags::user_data(),
            Flags::user_device(),
        ] {
            if f.is_user_accessible() {
                assert!(
                    !f.is_kernel_executable(),
                    "{f:?} is user-reachable yet executable at EL1",
                );
            } else {
                assert!(
                    !f.is_user_executable(),
                    "{f:?} is kernel-only yet executable at EL0",
                );
            }
        }
    }

    /// The three user mappings are exactly the three an ELF can ask for, and no more.
    #[test]
    fn user_rodata_is_readable_and_nothing_else() {
        let f = Flags::user_rodata();
        assert!(f.is_user_accessible(), "EL0 cannot read its own .rodata");
        assert!(!f.is_writable(), "an ELF's .rodata is writable");
        assert!(
            !f.is_user_executable(),
            "an ELF's .rodata is executable at EL0"
        );
        assert!(
            !f.is_kernel_executable(),
            "an ELF's .rodata is executable at EL1"
        );
    }

    #[test]
    fn user_device_is_device_typed_user_accessible_and_never_executable() {
        let f = Flags::user_device();
        assert!(
            f.is_user_accessible(),
            "a driver at EL0 cannot reach its own MMIO"
        );
        assert!(f.is_writable(), "a driver cannot write its MMIO");
        assert!(!f.is_user_executable() && !f.is_kernel_executable());
        // Device attr index, not normal: the CPU must not cache or reorder register writes.
        assert_eq!(
            f.bits() & (0b111 << 2),
            (mair::DEVICE << 2),
            "not device-typed"
        );
    }

    #[test]
    fn user_code_is_not_executable_by_the_kernel() {
        // PXN is not paranoia: a bug that jumped EL1 into a user page would otherwise execute
        // user-controlled instructions at EL1, which is a total compromise. One bit.
        assert!(!Flags::user_code().is_kernel_executable());
        assert!(Flags::user_code().is_user_executable());
        assert!(!Flags::user_code().is_writable());
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

/// **The user-VA gate: may EL0 ask for a mapping at `va` at all?**
///
/// True exactly when `va` is page-aligned and in the low ([`Half::Low`], `TTBR0`) half. The
/// syscall layer runs this before spending anything on a user `MAP` request, for two reasons:
///
/// - **Isolation.** An admitted address is in the low half, so the request can only ever walk the
///   process's own `TTBR0` tables. There is no address that passes this gate and lands in the
///   kernel's half. (The halves are disjoint; that is proved.)
/// - **Budget.** A rejected address is rejected *before* a page is retyped for it. The mapper
///   would refuse a bad address anyway, but by then the page was already spent from the
///   process's own untyped, a silent self-inflicted leak.
///
/// The kernel used to hand-roll this as bit tests at each syscall site; it now calls this, and
/// the harness below proves the readable definition equals those bits for every address.
pub const fn is_user_page_va(va: u64) -> bool {
    Half::Low.contains(va) && va.is_multiple_of(PAGE_SIZE)
}

/// **Proof that a page table changed and the TLB may now be lying.**
///
/// # Why this type exists at all
///
/// The CPU caches translations in a TLB. Change a mapping without invalidating it and **the
/// CPU keeps using the old translation**. Memory reads back as the *previous* owner's data.
///
/// That is a security hole, and it is close to undebuggable: the page tables *in memory are
/// correct*. It is the CPU's private cache of them that is stale, and you cannot look at it.
///
/// So `unmap` doesn't just do the work and trust you to remember. It hands you an obligation.
/// `#[must_use]` means dropping it on the floor is a compiler warning, and the only ways to
/// discharge it are [`flush`](Self::flush) (do the invalidation) or
/// [`assume_no_stale_entry`](Self::assume_no_stale_entry), which is `unsafe` and makes you say
/// why.
///
/// # Break-before-make
///
/// The other half of the discipline, and the reason [`MapError::AlreadyMapped`] exists rather
/// than `map` silently overwriting.
///
/// On aarch64, changing a **valid** descriptor directly into a *different* **valid**
/// descriptor is architecturally unsafe: it can raise a TLB conflict abort, and the hardware
/// is permitted to do essentially anything. You must go valid → invalid → invalidate → valid.
///
/// Refusing to overwrite an existing mapping is what forces that sequence: to change a
/// mapping you must `unmap` (which yields one of these) and then `map`. **The API cannot be
/// used incorrectly**, rather than merely documenting the rule and hoping.
///
/// # What does NOT need one
///
/// Mapping a page that was **invalid** before. ARMv8 does not permit the TLB to cache an entry
/// that would fault, so there can be no stale entry to invalidate. That is why `map` returns
/// `()` and not this, and why the kernel's 32768 boot-time mappings cost nothing extra.
#[must_use = "a page table changed: the TLB MUST be invalidated, or the CPU keeps using the old \
              translation and memory reads back as the previous owner's data"]
#[derive(Debug, PartialEq, Eq)]
pub struct TlbFlush {
    va: u64,
}

impl TlbFlush {
    /// Discharge the obligation by actually invalidating.
    ///
    /// The `paging` crate is pure logic and deliberately emits no instructions, so the caller
    /// supplies the architecture's invalidate (on aarch64: `tlbi vaae1is`).
    pub fn flush(self, invalidate: impl FnOnce(u64)) {
        let va = self.va;
        core::mem::forget(self); // discharged: do not run the Drop below
        invalidate(va);
    }

    /// Discharge the obligation **without** invalidating.
    ///
    /// # Safety
    ///
    /// Only sound when the TLB provably cannot hold an entry for this address: e.g. these
    /// tables are not installed in any TTBR yet, so the hardware has never walked them.
    ///
    /// If you are wrong, the failure is a stale translation, and the page tables will look
    /// perfectly correct while you debug it.
    pub unsafe fn assume_no_stale_entry(self) {
        core::mem::forget(self);
    }

    pub fn address(&self) -> u64 {
        self.va
    }
}

/// **You cannot drop this on the floor.**
///
/// `#[must_use]` alone is not enough: it warns on `mapper.unmap(va);` as a statement, but says
/// nothing about
///
/// ```ignore
/// let (pa, _) = mapper.unmap(va)?;   // obligation destructured away, silently
/// ```
///
/// which is exactly the shape the mistake takes in real code. Rust has no linear types, so the
/// only way to make "you must consume this" enforceable is to make *not* consuming it fail
/// loudly.
///
/// A panic is the right failure. The alternative is a stale TLB entry: memory that reads back
/// as its previous owner's data, in a kernel whose page tables are *provably correct in
/// memory*, because the lie lives in a CPU cache you cannot inspect. Better to die here, with
/// the address in hand.
impl Drop for TlbFlush {
    fn drop(&mut self) {
        panic!(
            "page table changed at {:#x} but the TLB was never invalidated: \
             the CPU is still using the old translation. \
             Call .flush() or (unsafely) .assume_no_stale_entry().",
            self.va
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// Ran out of frames while building intermediate tables.
    OutOfFrames,
    /// The virtual or physical address is not 4 KiB aligned.
    Misaligned,
    /// Something is already mapped here.
    ///
    /// **This error is load-bearing, not defensive.** Refusing to overwrite is what forces
    /// break-before-make: to change a mapping you must `unmap` (which hands you a
    /// [`TlbFlush`]) and then `map`. Silently overwriting would let you go valid → valid,
    /// which aarch64 does not permit, *and* leak the old physical frame, which nothing would
    /// ever notice.
    AlreadyMapped,
    /// This address belongs to the *other* half, or is non-canonical.
    ///
    /// Mapping a kernel address into the userspace tables would silently do nothing useful:
    /// the hardware would never consult this table for that address. Catching it here turns
    /// a mystery into a compile-time-shaped error.
    WrongHalf,
    /// Nothing is mapped at this address, so there is nothing to unmap.
    NotMapped,
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

        if !va.is_multiple_of(PAGE_SIZE) || !pa.is_multiple_of(PAGE_SIZE) {
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

    /// Remove a mapping, and return the physical frame it pointed at.
    ///
    /// The frame is returned rather than freed, because the mapper does not own it: the caller
    /// took it from the frame allocator and the caller must give it back. Silently dropping it
    /// would leak a page per unmap, which at process teardown is a leak per page of every
    /// process that ever exits.
    ///
    /// Returns a [`TlbFlush`] you cannot ignore. See that type: the whole point is that this
    /// is the operation you must not forget to follow up on.
    ///
    /// This clears only the L3 leaf and leaves the intermediate L1/L2/L3 tables standing, on
    /// purpose. Break-before-make (changing a mapping) unmaps and immediately remaps the same
    /// address, so freeing the tables here would only reallocate them a line later.
    ///
    /// A naive teardown-by-unmap *would* therefore leak a page table per address space. The
    /// kernel does not do that. It never tears an address space down with `unmap` at all:
    /// `user::AddressSpace` records every frame the mapper hands out (leaves and tables alike)
    /// and, on drop, frees the whole set and discards the root. That is strictly cheaper than a
    /// walk-back-up-and-reclaim would be (no tree walk, no per-leaf TLB flush), and it is why a
    /// reclaiming `unmap` was considered and deliberately not built. See notes/teardown.md.
    pub fn unmap(&mut self, va: u64) -> Result<(u64, TlbFlush), MapError> {
        if !self.half.contains(va) {
            return Err(MapError::WrongHalf);
        }
        if !va.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }

        let mut table_pa = self.root;

        for level in 0..LEVELS - 1 {
            let i = index(va, level);
            // SAFETY: per the type's contract.
            let entry = unsafe { (*(self.phys_to_ptr)(table_pa)).entries[i] };
            if entry & VALID == 0 {
                return Err(MapError::NotMapped);
            }
            table_pa = entry & ADDR_MASK;
        }

        let i = index(va, LEVELS - 1);
        // SAFETY: per the type's contract.
        let entry = unsafe { &mut (*(self.phys_to_ptr)(table_pa)).entries[i] };

        if *entry & VALID == 0 {
            return Err(MapError::NotMapped);
        }

        let pa = *entry & ADDR_MASK;

        // Break. The descriptor becomes invalid *before* anything else happens, which is the
        // "break" half of break-before-make.
        *entry = 0;

        Ok((pa, TlbFlush { va }))
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

/// Machine-checked proofs of the address arithmetic (DECISIONS §14, milestone 18).
///
/// The four-level walk is memory-safe and isolating only if the index math is right for *every*
/// virtual address. These harnesses prove exactly that, symbolically, over all 2^64 addresses at
/// once, rather than on the handful a test would pick. See notes/verification.md.
///
/// Scope: the pure *arithmetic* (`index`, `Half`). Proving the `Mapper` itself (build tables, then
/// `translate` them back) means reasoning over built memory and a bounded frame pool; that is the
/// heavier next step, noted in notes/verification.md, not done here.
#[cfg(kani)]
mod verification {
    use super::*;

    /// **The walk never indexes past a table.** For every address and every level, the extracted
    /// index is < 512, so `entries[index(va, level)]` is always in bounds. An index of 512+ would
    /// read past a table into the next page: a memory-safety bug and an isolation break.
    #[kani::proof]
    fn index_is_always_in_bounds() {
        let va: u64 = kani::any();
        let level: usize = kani::any();
        kani::assume(level < LEVELS);
        assert!(index(va, level) < ENTRIES);
    }

    /// **The four indices and the offset tile the address exactly.** The bits each level selects,
    /// plus the 12-bit page offset, reassemble the low 48 bits with nothing lost and nothing
    /// overlapping. This is what proves the `39 - 9*level` shift arithmetic correct: if two levels
    /// shared a bit, two different addresses could walk to one entry.
    #[kani::proof]
    fn the_indices_and_offset_tile_the_address() {
        let va: u64 = kani::any();
        let reconstructed = ((index(va, 0) as u64) << 39)
            | ((index(va, 1) as u64) << 30)
            | ((index(va, 2) as u64) << 21)
            | ((index(va, 3) as u64) << 12)
            | (va & (PAGE_SIZE - 1));
        assert_eq!(reconstructed, va & 0x0000_ffff_ffff_ffff);
    }

    /// **Every byte of a page walks to the same leaf.** Changing only the 12-bit offset leaves all
    /// four indices unchanged, so a whole 4 KiB page shares one leaf descriptor. That is "page
    /// granularity", stated as a property of the index math.
    #[kani::proof]
    fn the_offset_does_not_change_the_walk() {
        let va: u64 = kani::any();
        let off: u64 = kani::any();
        kani::assume(off < PAGE_SIZE);
        let base = va & !(PAGE_SIZE - 1);
        let inside = base | off;
        assert_eq!(index(base, 0), index(inside, 0));
        assert_eq!(index(base, 1), index(inside, 1));
        assert_eq!(index(base, 2), index(inside, 2));
        assert_eq!(index(base, 3), index(inside, 3));
    }

    /// **Distinct pages take distinct paths.** Two page-aligned addresses with the same four table
    /// indices are the same page. So no two different pages ever reach the same leaf slot, which is
    /// the arithmetic core of address-space isolation.
    #[kani::proof]
    fn distinct_pages_take_distinct_paths() {
        let a: u64 = kani::any::<u64>() & 0x0000_ffff_ffff_f000;
        let b: u64 = kani::any::<u64>() & 0x0000_ffff_ffff_f000;
        kani::assume(
            index(a, 0) == index(b, 0)
                && index(a, 1) == index(b, 1)
                && index(a, 2) == index(b, 2)
                && index(a, 3) == index(b, 3),
        );
        assert_eq!(a, b);
    }

    /// **The two halves are disjoint.** No address belongs to both `TTBR0` (low) and `TTBR1`
    /// (high); the top 16 bits decide, and all-zero and all-one are mutually exclusive. The
    /// kernel/user split rests on this.
    #[kani::proof]
    fn the_two_halves_are_disjoint() {
        let va: u64 = kani::any();
        assert!(!(Half::Low.contains(va) && Half::High.contains(va)));
    }

    /// **The user-VA gate admits exactly the aligned low half.** The readable definition equals
    /// the bit test the syscall layer used to hand-roll, for every address; and every admitted
    /// address is page-aligned and in the low half, never the high one. With the disjointness
    /// proof above, this is the front door of isolation: no address a user `MAP` request can get
    /// past the gate ever names the kernel's tables.
    #[kani::proof]
    fn the_user_va_gate_admits_only_the_aligned_low_half() {
        let va: u64 = kani::any();
        assert_eq!(is_user_page_va(va), va & 0xfff == 0 && va >> 48 == 0);
        if is_user_page_va(va) {
            assert!(Half::Low.contains(va) && !Half::High.contains(va));
        }
    }

    /// **A leaf descriptor keeps the address and the permissions apart.** `map` writes
    /// `(pa & ADDR_MASK) | flags | TABLE_OR_PAGE | VALID` and `translate` reads the two halves
    /// back with masks. For every representable physical page and every flag set the kernel can
    /// construct, both reads are exact: no permission bit can redirect the address, and no
    /// address bit can grant a permission. If any `Flags` constructor ever grew a bit inside
    /// `ADDR_MASK` (or the low type bits), mappings would silently point at the wrong frame or
    /// carry rights nobody asked for; this proof is what fails instead.
    ///
    /// `pa` is assumed representable (bits 47:12 only). That is the physical contract, not a
    /// dodge: descriptors have 36 address bits by architecture, and every `pa` the kernel maps
    /// comes from a frame allocator or an untyped region, both bounded by RAM far below 2^48.
    #[kani::proof]
    fn the_leaf_descriptor_keeps_address_and_permissions_apart() {
        let pa: u64 = kani::any();
        kani::assume(pa & !ADDR_MASK == 0);

        let all = [
            Flags::kernel_code(),
            Flags::kernel_rodata(),
            Flags::kernel_data(),
            Flags::device(),
            Flags::user_code(),
            Flags::user_rodata(),
            Flags::user_data(),
            Flags::user_device(),
        ];
        let i: usize = kani::any();
        kani::assume(i < all.len());
        let flags = all[i];

        // The exact write `Mapper::map` performs at L3, and the exact reads `translate` performs.
        let leaf = (pa & ADDR_MASK) | flags.bits() | TABLE_OR_PAGE | VALID;
        assert_eq!(leaf & ADDR_MASK, pa);
        assert_eq!(Flags(leaf & !ADDR_MASK & !VALID & !TABLE_OR_PAGE), flags);
    }

    /// **The user mapper refuses every address outside its half, and the reject path touches
    /// nothing.** For all 2^64 - 2^48 addresses outside the low half, every kernel address
    /// included, `map`, `unmap`, and `translate` on a `TTBR0` mapper reject before reading or
    /// writing any memory: the mapper here has a null root and a frame source that panics, so
    /// any touch on the reject path is a proof failure, not just a wrong answer.
    #[kani::proof]
    fn the_low_half_mapper_rejects_the_high_half_untouched() {
        let va: u64 = kani::any();
        kani::assume(!Half::Low.contains(va));

        // SAFETY: for the Mapper's contract to matter, some path must dereference the root or
        // call the allocator; the proof is that on these inputs none does.
        let mut m = unsafe {
            Mapper::new(
                0,
                Half::Low,
                || -> Option<u64> { panic!("allocated a frame on a rejected mapping") },
                |_| core::ptr::null_mut(),
            )
        };

        assert_eq!(m.map(va, 0, Flags::user_data()).err(), Some(MapError::WrongHalf));
        assert_eq!(m.unmap(va).err(), Some(MapError::WrongHalf));
        assert!(m.translate(va).is_none());
    }
}
