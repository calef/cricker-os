//! Page tables, portable across formats.
//!
//! Two hardware page-table formats live under this crate: the aarch64 descriptor format
//! ([`Aarch64`], four levels) and RISC-V Sv39 ([`Sv39`], three levels). They share everything that
//! matters and differ only in the bits: the *walk* (descend the levels, allocate tables, write a
//! leaf) is written once in [`Mapper`], and each format supplies the handful of encode/decode
//! operations the walk needs through the [`PageFormat`] trait. See notes/riscv-port.md (leak #2) for
//! why this seam exists and DECISIONS.md §17.
//!
//! Both formats use 512-entry, 4 KiB tables and 4 KiB pages, because both were designed around the
//! same arithmetic: 9 index bits selects one of 512 eight-byte entries, so **a table is exactly one
//! page** and the frame allocator can supply page tables and nothing else.
//!
//! # Why this is a separate crate
//!
//! It is pure logic: addresses in, descriptors out. The host tests build real page tables in real
//! memory (using host allocations as pretend physical frames, which works because the pointer
//! arithmetic is identical) and walk them back. Milliseconds, no emulator. DECISIONS.md §7.
//!
//! # Examples
//!
//! Real aarch64 page tables, in real memory, on the host. The trick is in the section above: a
//! `Box<PageTable>` is 4 KiB-aligned because the type says so, so its address serves as a
//! "physical" frame and `phys_to_ptr` is the identity cast. **The pointer arithmetic is bit-for-bit
//! what the kernel does.**
//!
//! ```
//! use std::cell::RefCell;
//!
//! use paging::{Aarch64, Flags, Half, MapError, Mapper, PageTable};
//!
//! fn phys_to_ptr(pa: u64) -> *mut PageTable {
//!     pa as *mut PageTable
//! }
//!
//! // The pretend frame allocator, with a receipt so nothing leaks.
//! let tables: RefCell<Vec<*mut PageTable>> = RefCell::new(Vec::new());
//! let mut fresh = || {
//!     let p = Box::into_raw(Box::new(PageTable::new()));
//!     tables.borrow_mut().push(p);
//!     p as u64
//! };
//! let root = fresh();
//!
//! // SAFETY: `root` is a fresh, zeroed, 4 KiB-aligned table, and `phys_to_ptr` is the identity,
//! // which is correct because these "physical" addresses ARE host addresses.
//! let mut m: Mapper<_, fn(u64) -> *mut PageTable, Aarch64> = unsafe {
//!     Mapper::new(
//!         root,
//!         Half::Low,
//!         || Some(fresh()),
//!         phys_to_ptr as fn(u64) -> *mut PageTable,
//!     )
//! };
//!
//! // A user text page. Note what is not available: there is no writable-and-executable
//! // constructor to pass here at all.
//! m.map(0x40_0000, 0x4000_0000, Flags::user_code()).unwrap();
//! let (pa, flags) = m.translate(0x40_0000).unwrap();
//! assert_eq!(pa, 0x4000_0000);
//! assert!(flags.is_user_executable() && !flags.is_writable());
//!
//! // **Break-before-make is forced, not documented.** Overwriting a live mapping would go valid to
//! // valid (which the hardware may reject) and leak the old frame, so it is refused.
//! assert_eq!(
//!     m.map(0x40_0000, 0x5000_0000, Flags::user_data()),
//!     Err(MapError::AlreadyMapped),
//! );
//!
//! // A kernel address in the user tables is a mapping the CPU would never consult, because the top
//! // bits pick the table set before any index is extracted. An error rather than a silent no-op.
//! assert_eq!(
//!     m.map(0xffff_0000_0000_0000, 0x4000_0000, Flags::kernel_data()),
//!     Err(MapError::WrongHalf),
//! );
//!
//! // The other half of break-before-make: `unmap` hands back a `TlbFlush` that must be discharged.
//! let (freed, flush) = m.unmap(0x40_0000).unwrap();
//! assert_eq!(freed, 0x4000_0000);
//! flush.flush(|va| assert_eq!(va, 0x40_0000)); // the kernel's `tlbi vaae1is` goes here
//! assert!(m.translate(0x40_0000).is_none());
//!
//! // Now the address is free again, which is what makes the refusal above a sequencing rule rather
//! // than a prohibition.
//! m.map(0x40_0000, 0x5000_0000, Flags::user_data()).unwrap();
//!
//! drop(m); // the mapper borrows the tables; it goes first
//! for p in tables.borrow_mut().drain(..) {
//!     // SAFETY: each `p` came from `Box::into_raw` above and is registered exactly once.
//!     unsafe { drop(Box::from_raw(p)) };
//! }
//! ```
//!
//! W^X is worth stating as a claim about what is **absent**, over every constructor there is:
//!
//! ```
//! use paging::Flags;
//!
//! for f in [
//!     Flags::kernel_code(),
//!     Flags::kernel_rodata(),
//!     Flags::kernel_data(),
//!     Flags::device(),
//!     Flags::user_code(),
//!     Flags::user_rodata(),
//!     Flags::user_data(),
//!     Flags::user_device(),
//! ] {
//!     assert!(!(f.is_writable() && (f.is_kernel_executable() || f.is_user_executable())));
//!     // And the privilege split: nothing user-reachable is kernel-executable, so a wild kernel
//!     // jump into a user page faults instead of running user-chosen instructions in EL1.
//!     if f.is_user_accessible() {
//!         assert!(!f.is_kernel_executable());
//!     }
//! }
//! ```
//!
//! The user-VA gate is a conjunction, and the aligned high-half address is the case that tells `&&`
//! from `||`: an or-gate here would admit a kernel address to a user `MAP` request.
//!
//! ```
//! use paging::{Aarch64, PAGE_SIZE, is_user_page_va};
//!
//! assert!(is_user_page_va::<Aarch64>(0x40_0000));
//! assert!(!is_user_page_va::<Aarch64>(0x40_0001)); // not page-aligned
//! assert!(!is_user_page_va::<Aarch64>(0xffff_0000_0000_0000)); // aligned, and the kernel's half
//! assert_eq!(PAGE_SIZE, 4096);
//! ```
//!
//! Name: ratified 2026-08-01 (calef, the naming tenet in CLAUDE.md). Named in the group of standard
//! terms that are already right and must not be touched, because a name a reader knows from outside
//! this project costs nothing to learn and renaming it would destroy the recognition the tenet
//! exists to buy.

#![cfg_attr(not(test), no_std)]

use core::marker::PhantomData;

pub mod aarch64;
pub mod domain;
pub mod sv39;

pub use aarch64::Aarch64;
pub use domain::{DmaRegion, build_identity_domain};
pub use sv39::Sv39;

/// 4 KiB, the page size every format here maps in.
pub const PAGE_SIZE: u64 = 4096;

/// Entries per table. 4096 bytes / 8 bytes. The same for every format we support.
pub const ENTRIES: usize = 512;

/// A page table: one 4 KiB frame of entries.
///
/// `repr(C, align(4096))` matters. Every format takes the entry's high bits as the next table's
/// address and assumes the low 12 are zero, so a table must be page-aligned. This alignment is also
/// what lets the host tests allocate real, correctly-aligned tables.
#[repr(C, align(4096))]
#[derive(Clone)]
pub struct PageTable {
    /// The raw entries, in the format's own encoding. Decode with the `PageFormat` in use rather
    /// than reading these directly.
    pub entries: [u64; ENTRIES],
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PageTable {
    /// A table of all-zero entries: every slot not-present.
    pub const fn new() -> Self {
        Self {
            entries: [0; ENTRIES],
        }
    }
}

// --- Portable access flags ---
//
// `Flags` used to *be* the aarch64 descriptor's permission bits. It is now a format-neutral set of
// capabilities: what access this mapping grants, said in terms every format can encode. Each
// `PageFormat` translates these to and from its own descriptor bits (see `leaf_entry`/`leaf_flags`).
// The constructor and predicate API is unchanged, so consumers that ask for `Flags::user_code()` or
// test `is_writable()` did not move.

const CAP_WRITE: u64 = 1 << 0;
const CAP_USER: u64 = 1 << 1;
const CAP_USER_EXEC: u64 = 1 << 2;
const CAP_KERNEL_EXEC: u64 = 1 << 3;
const CAP_GLOBAL: u64 = 1 << 4;
const CAP_DEVICE: u64 = 1 << 5;

/// What access a mapping grants: readable always, plus write / execute / user / global / device as
/// set. **Format-neutral.** There is deliberately no constructor that is both writable and
/// executable (W^X): a page that is both is how a buffer overflow becomes code execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flags(u64);

impl Flags {
    /// Kernel code: readable and executable by the kernel, never writable, never user.
    pub const fn kernel_code() -> Self {
        Flags(CAP_KERNEL_EXEC | CAP_GLOBAL)
    }

    /// Kernel constants: readable by the kernel, never writable, never executable by anyone.
    pub const fn kernel_rodata() -> Self {
        Flags(CAP_GLOBAL)
    }

    /// Kernel data, stacks, heap: read/write by the kernel, never executable by anyone.
    pub const fn kernel_data() -> Self {
        Flags(CAP_WRITE | CAP_GLOBAL)
    }

    /// MMIO for the kernel. Device-typed, read/write, and never executable.
    pub const fn device() -> Self {
        Flags(CAP_WRITE | CAP_GLOBAL | CAP_DEVICE)
    }

    /// User code (milestone 7): executable by user, never by the kernel, never writable.
    ///
    /// Not executable by the kernel is not paranoia. Without it, a bug that jumps the kernel into a
    /// user page would execute *user-controlled instructions in supervisor mode*. That is a total
    /// compromise, and it is one bit (aarch64 PXN, RISC-V's U bit denying S-mode execute).
    pub const fn user_code() -> Self {
        Flags(CAP_USER | CAP_USER_EXEC)
    }

    /// User constants (milestone 7): readable by user, and **nothing else**.
    ///
    /// An ELF's `.rodata` segment is `PF_R` alone. Without this, the loader's only non-executable
    /// choice is [`user_data`](Self::user_data), which is **writable**, so we would silently grant
    /// the program more authority than its own file asked for.
    pub const fn user_rodata() -> Self {
        Flags(CAP_USER)
    }

    /// User data (milestone 7): read/write by user, never executable.
    pub const fn user_data() -> Self {
        Flags(CAP_USER | CAP_WRITE)
    }

    /// **User device memory (milestone 8): a driver's MMIO, at user level.**
    ///
    /// This is the flag that lets a driver leave the kernel. A userspace console server holds a
    /// mapping of the UART's registers with *these* bits, and its user-mode stores go straight to
    /// the hardware. Device-typed (so the CPU does not cache or reorder register writes), user
    /// read/write, and never executable.
    pub const fn user_device() -> Self {
        Flags(CAP_USER | CAP_WRITE | CAP_DEVICE)
    }

    /// The raw capability word. Opaque outside this crate; the formats use it to encode.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Whether this mapping is writable.
    pub const fn is_writable(self) -> bool {
        self.0 & CAP_WRITE != 0
    }

    /// Whether this mapping is executable in kernel (supervisor) mode.
    pub const fn is_kernel_executable(self) -> bool {
        self.0 & CAP_KERNEL_EXEC != 0
    }

    /// Whether this mapping is executable in user mode.
    pub const fn is_user_executable(self) -> bool {
        self.0 & CAP_USER_EXEC != 0
    }

    /// Whether user mode may access this mapping at all.
    pub const fn is_user_accessible(self) -> bool {
        self.0 & CAP_USER != 0
    }

    /// Device-typed memory: the CPU must not cache or reorder accesses to it.
    pub const fn is_device(self) -> bool {
        self.0 & CAP_DEVICE != 0
    }

    /// Global mappings match TLB lookups under every address space; non-global ones only under
    /// their own. User mappings must be non-global (so a context switch flushes nothing); kernel
    /// mappings must be global (the high half is identical for everyone).
    pub const fn is_global(self) -> bool {
        self.0 & CAP_GLOBAL != 0
    }

    /// Build `Flags` from a raw capability word. Crate-internal: the formats' `leaf_flags` decoders
    /// reassemble a `Flags` this way.
    pub(crate) const fn from_caps(caps: u64) -> Self {
        Flags(caps)
    }
}

/// Which half of the address space a set of tables serves: the low half (user) or the high half
/// (kernel). The bit boundary between them is per-format ([`PageFormat::SPLIT_SHIFT`]); this enum is
/// just the marker, and the format supplies the geometry via [`PageFormat::in_half`] and
/// [`PageFormat::half_base`].
///
/// # The thing that is easy to get wrong
///
/// **The top bits of a virtual address are not translated.** They are not part of any index; they
/// select *which set of tables to use* (aarch64 `TTBR0`/`TTBR1`; on RISC-V a single `satp` per
/// address space, with the sign-extended top bit distinguishing the halves). The index is extracted
/// from the lower bits **identically** for both halves, so a low address and its high-half twin are
/// the same entry within a table and differ only in which tables the hardware consults. A
/// higher-half kernel works because it is a *separate set of tables*, not because high addresses
/// index differently. A test discovered this the hard way, which is what host tests are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Half {
    /// The low half: user space.
    Low,
    /// The high half: the kernel.
    High,
}

/// **The user-VA gate: may a user process ask for a mapping at `va` at all?**
///
/// True exactly when `va` is page-aligned and in the low half for the given format. The syscall
/// layer runs this before spending anything on a user `MAP` request, for two reasons:
///
/// - **Isolation.** An admitted address is in the low half, so the request can only ever walk the
///   process's own tables. No address that passes this gate lands in the kernel's half. (The halves
///   are disjoint; that is proved per format.)
/// - **Budget.** A rejected address is rejected *before* a page is retyped for it.
///
/// Generic over the format so the split lands at the right bit for the running architecture.
pub fn is_user_page_va<F: PageFormat>(va: u64) -> bool {
    F::in_half(Half::Low, va) && va.is_multiple_of(PAGE_SIZE)
}

/// **The seam between the shared walk and a hardware page-table format.**
///
/// [`Mapper`] descends the levels, allocates intermediate tables, and writes a leaf, using nothing
/// architecture-specific except what a format supplies here: how many levels, where the halves
/// split, and the four bit operations (is-present, extract-address, encode-a-table-pointer,
/// encode-and-decode-a-leaf). Both hardware formats we support are 512-entry / 4 KiB / 4 KiB, so
/// [`ENTRIES`] and [`PAGE_SIZE`] are shared and not part of the trait.
pub trait PageFormat {
    /// Translation levels. aarch64: 4. Sv39: 3.
    const LEVELS: usize;

    /// The bit position that splits the low (user) half from the high (kernel) half. aarch64: 48
    /// (bits 63:48 select the half). Sv39: 38 (bits 63:38, the sign-extended top VA bit and above).
    const SPLIT_SHIFT: u32;

    /// Is this entry present (valid)?
    fn is_present(entry: u64) -> bool;

    /// The physical address a table-pointer or leaf entry refers to.
    fn entry_pa(entry: u64) -> u64;

    /// Encode an intermediate entry pointing at `pa` (a next-level table). No permissions: a leaf
    /// is the single source of truth for access rights, so intermediate entries carry none.
    fn table_entry(pa: u64) -> u64;

    /// Encode a leaf entry mapping physical `pa` with `flags`.
    fn leaf_entry(pa: u64, flags: Flags) -> u64;

    /// Decode a leaf entry's permission/attribute bits back into portable [`Flags`].
    fn leaf_flags(entry: u64) -> Flags;

    /// Which 9-bit slice of `va` selects an entry at `level` (0 = the top level). The top level
    /// covers the highest translated bits; each lower level shifts down by 9. Derived from
    /// [`LEVELS`](Self::LEVELS) so it is right for both a 4- and a 3-level walk.
    fn index(va: u64, level: usize) -> usize {
        let shift = 9 * (Self::LEVELS - 1 - level) as u32 + 12;
        ((va >> shift) & 0x1ff) as usize
    }

    /// Does `va` lie in `half` for this format? The top bits (above [`SPLIT_SHIFT`](Self::SPLIT_SHIFT))
    /// must be all-zero for the low half or all-one for the high half; anything between is
    /// non-canonical and faults.
    fn in_half(half: Half, va: u64) -> bool {
        let top = va >> Self::SPLIT_SHIFT;
        match half {
            Half::Low => top == 0,
            Half::High => top == (u64::MAX >> Self::SPLIT_SHIFT),
        }
    }

    /// The base virtual address of `half` (0 for the low half; the sign-extended high base for the
    /// high half). Exists to make a reader's mental model explicit; the walk does not use it.
    fn half_base(half: Half) -> u64 {
        match half {
            Half::Low => 0,
            Half::High => (u64::MAX >> Self::SPLIT_SHIFT) << Self::SPLIT_SHIFT,
        }
    }
}

/// **Proof that a page table changed and the TLB may now be lying.**
///
/// # Why this type exists at all
///
/// The CPU caches translations in a TLB. Change a mapping without invalidating it and **the CPU
/// keeps using the old translation**. Memory reads back as the *previous* owner's data. That is a
/// security hole, and close to undebuggable: the page tables *in memory are correct*; it is the
/// CPU's private cache of them that is stale, and you cannot look at it.
///
/// So `unmap` doesn't just do the work and trust you to remember. It hands you an obligation.
/// `#[must_use]` means dropping it on the floor is a compiler warning, and the only ways to
/// discharge it are [`flush`](Self::flush) (do the invalidation) or
/// [`assume_no_stale_entry`](Self::assume_no_stale_entry), which is `unsafe` and makes you say why.
///
/// # Break-before-make
///
/// The other half of the discipline, and the reason [`MapError::AlreadyMapped`] exists rather than
/// `map` silently overwriting. Changing a **valid** entry directly into a *different* **valid** one
/// is architecturally unsafe (a TLB conflict): you must go valid → invalid → invalidate → valid.
/// Refusing to overwrite an existing mapping is what forces that sequence. **The API cannot be used
/// incorrectly**, rather than merely documenting the rule and hoping.
///
/// # What does NOT need one
///
/// Mapping a page that was **invalid** before: the hardware may not cache an entry that would fault,
/// so there is no stale entry to invalidate. That is why `map` returns `()` and not this.
#[must_use = "a page table changed: the TLB MUST be invalidated, or the CPU keeps using the old \
              translation and memory reads back as the previous owner's data"]
#[derive(Debug, PartialEq, Eq)]
pub struct TlbFlush {
    va: u64,
}

impl TlbFlush {
    /// Discharge the obligation by actually invalidating. The crate emits no instructions, so the
    /// caller supplies the architecture's invalidate (aarch64 `tlbi vaae1is`, RISC-V `sfence.vma`).
    pub fn flush(self, invalidate: impl FnOnce(u64)) {
        let va = self.va;
        core::mem::forget(self); // discharged: do not run the Drop below
        invalidate(va);
    }

    /// Discharge the obligation **without** invalidating.
    ///
    /// # Safety
    /// Only sound when the TLB provably cannot hold an entry for this address: e.g. these tables are
    /// not installed anywhere yet, so the hardware has never walked them. If you are wrong, the
    /// failure is a stale translation, and the page tables will look perfectly correct while you
    /// debug it.
    pub unsafe fn assume_no_stale_entry(self) {
        core::mem::forget(self);
    }

    /// The virtual address this obligation covers.
    pub fn address(&self) -> u64 {
        self.va
    }
}

/// **You cannot drop this on the floor.** `#[must_use]` alone warns on `mapper.unmap(va);` as a
/// statement but says nothing about `let (pa, _) = mapper.unmap(va)?;`, which is exactly the shape
/// the mistake takes. Rust has no linear types, so the only way to make "you must consume this"
/// enforceable is to make *not* consuming it fail loudly. A panic is the right failure: the
/// alternative is a stale TLB entry, memory that reads back as its previous owner's data, in a
/// kernel whose page tables are provably correct in memory.
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

/// Why [`Mapper::map`] or [`Mapper::unmap`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    /// Ran out of frames while building intermediate tables.
    OutOfFrames,
    /// The virtual or physical address is not 4 KiB aligned.
    Misaligned,
    /// Something is already mapped here.
    ///
    /// **Load-bearing, not defensive.** Refusing to overwrite is what forces break-before-make: to
    /// change a mapping you must `unmap` (which hands you a [`TlbFlush`]) and then `map`. Silently
    /// overwriting would go valid → valid (which the hardware may reject) *and* leak the old frame.
    AlreadyMapped,
    /// This address belongs to the *other* half, or is non-canonical. Mapping a kernel address into
    /// the user tables would silently do nothing: the hardware would never consult this table for
    /// that address. Catching it here turns a mystery into an error.
    WrongHalf,
    /// Nothing is mapped at this address, so there is nothing to unmap.
    NotMapped,
    /// **A region that is not a region**: its `base + size` wraps `u64`, so it has no end. Only
    /// [`domain::grant_pages`] raises this, and only for an input no caller can construct today; it
    /// exists because refusing such a region is what stops the page enumeration from wrapping into
    /// addresses *outside* the grant. See [`domain::grant_pages`] for the whole argument.
    BadRegion,
}

/// Builds page tables, for any [`PageFormat`].
///
/// # Safety contract
///
/// The mapper dereferences physical addresses directly. That is sound in exactly two situations,
/// both of which apply to us:
///
/// - **the MMU is off**, so an address *is* a physical address (how we build the first table), or
/// - **physical memory is identity- or direct-mapped**, so a physical address can be turned into a
///   usable pointer by a known transform (how we edit tables afterwards).
///
/// `phys_to_ptr` is that transform. The host tests pass the identity function, and it works because
/// a host allocation's address is as good a "physical address" as any: the pointer arithmetic is
/// identical.
pub struct Mapper<A, P, F>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
    F: PageFormat,
{
    root: u64,
    half: Half,
    alloc_frame: A,
    phys_to_ptr: P,
    _fmt: PhantomData<F>,
}

impl<A, P, F> Mapper<A, P, F>
where
    A: FnMut() -> Option<u64>,
    P: Fn(u64) -> *mut PageTable,
    F: PageFormat,
{
    /// # Safety
    /// `root` must be a zeroed, page-aligned frame, and `phys_to_ptr` must satisfy the contract in
    /// the type's docs.
    pub unsafe fn new(root: u64, half: Half, alloc_frame: A, phys_to_ptr: P) -> Self {
        Self {
            root,
            half,
            alloc_frame,
            phys_to_ptr,
            _fmt: PhantomData,
        }
    }

    /// The physical address of the top-level table.
    pub fn root(&self) -> u64 {
        self.root
    }

    /// Which half of the address space this mapper builds into.
    pub fn half(&self) -> Half {
        self.half
    }

    /// Map one 4 KiB page. Walks the levels, creating tables as needed, and writes a leaf at the
    /// bottom.
    pub fn map(&mut self, va: u64, pa: u64, flags: Flags) -> Result<(), MapError> {
        // The hardware selects the table set from the top bits before it touches an index, so
        // mapping a high address into the low tables would build a mapping the CPU never consults.
        if !F::in_half(self.half, va) {
            return Err(MapError::WrongHalf);
        }

        if !va.is_multiple_of(PAGE_SIZE) || !pa.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }

        let mut table_pa = self.root;

        // Descend through every level but the last, creating intermediate tables as we go.
        for level in 0..F::LEVELS - 1 {
            let i = F::index(va, level);

            // SAFETY: `table_pa` is a page-aligned table, per the type's contract.
            let entry = unsafe { &mut (*(self.phys_to_ptr)(table_pa)).entries[i] };

            if !F::is_present(*entry) {
                let new = (self.alloc_frame)().ok_or(MapError::OutOfFrames)?;

                // SAFETY: a fresh frame from the allocator. Zero it before it becomes reachable by
                // the hardware, or the walk reads whatever garbage was in RAM and follows it.
                unsafe {
                    (*(self.phys_to_ptr)(new)).entries = [0; ENTRIES];
                }

                *entry = F::table_entry(new);
            }

            table_pa = F::entry_pa(*entry);
        }

        // The leaf.
        let i = F::index(va, F::LEVELS - 1);
        // SAFETY: as above.
        let entry = unsafe { &mut (*(self.phys_to_ptr)(table_pa)).entries[i] };

        if F::is_present(*entry) {
            return Err(MapError::AlreadyMapped);
        }

        *entry = F::leaf_entry(pa, flags);

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
    /// The frame is returned rather than freed, because the mapper does not own it: the caller took
    /// it from the frame allocator and must give it back. This clears only the leaf and leaves the
    /// intermediate tables standing, on purpose: break-before-make unmaps and immediately remaps the
    /// same address, so freeing the tables here would only reallocate them a line later. Address-
    /// space teardown does not use `unmap` at all (see notes/teardown.md); it frees the recorded
    /// frame set wholesale.
    ///
    /// Returns a [`TlbFlush`] you cannot ignore.
    pub fn unmap(&mut self, va: u64) -> Result<(u64, TlbFlush), MapError> {
        if !F::in_half(self.half, va) {
            return Err(MapError::WrongHalf);
        }
        if !va.is_multiple_of(PAGE_SIZE) {
            return Err(MapError::Misaligned);
        }

        let mut table_pa = self.root;

        for level in 0..F::LEVELS - 1 {
            let i = F::index(va, level);
            // SAFETY: per the type's contract.
            let entry = unsafe { (*(self.phys_to_ptr)(table_pa)).entries[i] };
            if !F::is_present(entry) {
                return Err(MapError::NotMapped);
            }
            table_pa = F::entry_pa(entry);
        }

        let i = F::index(va, F::LEVELS - 1);
        // SAFETY: per the type's contract.
        let entry = unsafe { &mut (*(self.phys_to_ptr)(table_pa)).entries[i] };

        if !F::is_present(*entry) {
            return Err(MapError::NotMapped);
        }

        let pa = F::entry_pa(*entry);

        // Break: the entry becomes invalid *before* anything else happens.
        *entry = 0;

        Ok((pa, TlbFlush { va }))
    }

    /// Walk the tables and report what a virtual address actually maps to. This is what the hardware
    /// does on every access, in silicon, and it is worth having in software: it is the only way to
    /// *check* that the tables say what you think they say.
    pub fn translate(&self, va: u64) -> Option<(u64, Flags)> {
        if !F::in_half(self.half, va) {
            return None;
        }

        let mut table_pa = self.root;

        for level in 0..F::LEVELS - 1 {
            let i = F::index(va, level);
            // SAFETY: per the type's contract.
            let entry = unsafe { (*(self.phys_to_ptr)(table_pa)).entries[i] };

            if !F::is_present(entry) {
                return None;
            }
            table_pa = F::entry_pa(entry);
        }

        let i = F::index(va, F::LEVELS - 1);
        // SAFETY: per the type's contract.
        let entry = unsafe { (*(self.phys_to_ptr)(table_pa)).entries[i] };

        if !F::is_present(entry) {
            return None;
        }

        let offset = va % PAGE_SIZE;
        Some((F::entry_pa(entry) + offset, F::leaf_flags(entry)))
    }
}

#[cfg(test)]
mod flag_tests {
    use super::*;

    const ALL: [Flags; 8] = [
        Flags::kernel_code(),
        Flags::kernel_rodata(),
        Flags::kernel_data(),
        Flags::device(),
        Flags::user_code(),
        Flags::user_rodata(),
        Flags::user_data(),
        Flags::user_device(),
    ];

    /// **W^X, as a property of the type rather than of our discipline.** No constructor returns a
    /// page that is both writable and executable, over every constructor there is.
    #[test]
    fn nothing_is_both_writable_and_executable() {
        for f in ALL {
            assert!(
                !(f.is_writable() && (f.is_kernel_executable() || f.is_user_executable())),
                "{f:?} is both writable and executable",
            );
        }
    }

    /// **The execute split.** Anything user-reachable is not kernel-executable (a wild kernel jump
    /// into a user page faults instead of running user-chosen instructions in supervisor mode), and
    /// anything kernel-only is not user-executable.
    #[test]
    fn no_page_is_executable_across_the_privilege_split() {
        for f in ALL {
            if f.is_user_accessible() {
                assert!(
                    !f.is_kernel_executable(),
                    "{f:?} user-reachable yet kernel-exec"
                );
            } else {
                assert!(!f.is_user_executable(), "{f:?} kernel-only yet user-exec");
            }
        }
    }

    #[test]
    fn user_rodata_is_readable_and_nothing_else() {
        let f = Flags::user_rodata();
        assert!(f.is_user_accessible());
        assert!(!f.is_writable());
        assert!(!f.is_user_executable());
        assert!(!f.is_kernel_executable());
    }

    #[test]
    fn user_device_is_device_typed_user_accessible_and_never_executable() {
        let f = Flags::user_device();
        assert!(f.is_user_accessible());
        assert!(f.is_writable());
        assert!(!f.is_user_executable() && !f.is_kernel_executable());
        assert!(f.is_device(), "a driver's MMIO must be device-typed");
    }

    /// **The ASID/global tagging split** (milestone 15): everything user-reachable is non-global
    /// (its TLB entries are tagged, so a context switch flushes nothing), everything kernel-only is
    /// global.
    #[test]
    fn user_mappings_are_tagged_and_kernel_mappings_are_global() {
        for f in ALL {
            assert_eq!(
                f.is_global(),
                !f.is_user_accessible(),
                "{f:?} is on the wrong side of the ASID split",
            );
        }
    }

    /// **`bits()` returns the word, not a constant.** The formats encode through the predicates,
    /// so nothing else in this crate reads the raw word back, and a `bits()` that returned 0 or 1
    /// would survive every other test here. Expected values hand-assembled from the CAP_* bit
    /// positions above: write 0, user 1, user-exec 2, kernel-exec 3, global 4, device 5.
    #[test]
    fn bits_returns_the_exact_capability_word() {
        assert_eq!(Flags::kernel_code().bits(), 0b01_1000);
        assert_eq!(Flags::kernel_rodata().bits(), 0b01_0000);
        assert_eq!(Flags::kernel_data().bits(), 0b01_0001);
        assert_eq!(Flags::device().bits(), 0b11_0001);
        assert_eq!(Flags::user_code().bits(), 0b00_0110);
        assert_eq!(Flags::user_rodata().bits(), 0b00_0010);
        assert_eq!(Flags::user_data().bits(), 0b00_0011);
        assert_eq!(Flags::user_device().bits(), 0b10_0011);
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;

    /// **The gate is a conjunction.** The aligned high-half address is the case that tells `&&`
    /// from `||`: it passes the alignment test, so an or-gate would admit a kernel address to a
    /// user MAP request. One aligned low address per format keeps the gate from being shorted to
    /// `false`, one unaligned low address keeps it from `true`.
    #[test]
    fn the_user_va_gate_admits_only_aligned_low_half_addresses() {
        assert!(is_user_page_va::<Aarch64>(0x1000));
        assert!(is_user_page_va::<Sv39>(0x1000));
        assert!(!is_user_page_va::<Aarch64>(0x1001));
        assert!(!is_user_page_va::<Sv39>(0x1001));
        // Page-aligned, but in the kernel half: all-ones above each format's split bit.
        assert!(!is_user_page_va::<Aarch64>(0xffff_0000_0000_1000));
        assert!(!is_user_page_va::<Sv39>(0xffff_ffc0_0000_1000));
    }

    /// **The half bases, pinned.** `half_base` exists for the reader's model and the walk never
    /// consults it, so only an exact value notices its shifts going the wrong way. The high base
    /// is all-ones above the split: bit 48 for aarch64 (where TTBR1 takes over), bit 38 for Sv39
    /// (the sign-extended top VA bit).
    #[test]
    fn half_bases_are_zero_and_the_sign_extended_top() {
        assert_eq!(Aarch64::half_base(Half::Low), 0);
        assert_eq!(Sv39::half_base(Half::Low), 0);
        assert_eq!(Aarch64::half_base(Half::High), 0xffff_0000_0000_0000);
        assert_eq!(Sv39::half_base(Half::High), 0xffff_ffc0_0000_0000);
    }

    /// **`root()` reports the frame the mapper was built on.** That value is what the kernel
    /// writes into TTBR0/satp, and nothing in this crate reads it back, so a constant here would
    /// install the wrong table in silicon while every walk test still passed.
    #[test]
    fn the_mapper_reports_the_root_it_was_built_on() {
        // SAFETY: nothing here walks the tables; new() stores its arguments and root() reads one
        // back, so the inert closures are never called.
        let m = unsafe {
            Mapper::<_, _, Aarch64>::new(0x8_2000, Half::Low, || None, |_| core::ptr::null_mut())
        };
        assert_eq!(m.root(), 0x8_2000);
    }
}
