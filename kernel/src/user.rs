//! Userspace. EL0. The actual operating system boundary.
//!
//! Everything before this was a Rust program that boots. From here on, the machine runs code
//! that **we did not compile and do not trust**, and the kernel's job stops being "do things"
//! and starts being "decide what is allowed."
//!
//! # Entering EL0 is returning from an exception that never happened
//!
//! There is no "drop to EL0" instruction. There is only `eret`, which restores whatever
//! `SPSR_EL1` says and jumps to `ELR_EL1`, and the exception level to return to is *in*
//! `SPSR_EL1`. So we do not need a new way down. We need a **fake way back**: fabricate a
//! [`TrapFrame`] with `SPSR = EL0t`, point `sp` at it, and fall into the `exception_restore`
//! that milestone 2 already wrote.
//!
//! This is the second time the project has pulled exactly this trick. `Thread::spawn` fakes a
//! `switch_to` frame so that the `ret` which *resumes* a thread also *starts* one
//! (notes/threads.md). Both times the "start" path turned out to be the "resume" path with a
//! forged frame, and no new code at all.
//!
//! # What milestone 4 already paid for
//!
//! The kernel lives entirely in `TTBR1`, at `0xffff_...`. Userspace lives in `TTBR0`, at
//! `0x0000_...`. **The hardware picks the table register from bits 63:48 of the address**, so:
//!
//! - The kernel is mapped in every address space, for free. Nobody had to copy anything.
//! - A syscall **does not switch page tables**. There is nothing to flush and nothing to remap.
//! - Installing a process is one `msr ttbr0_el1`.
//!
//! None of that was written for milestone 7. It fell out of a higher-half decision made three
//! milestones ago, and `Flags::user_code()` / `Flags::user_data()` have been sitting in the
//! `paging` crate, unused, waiting for today.
//!
//! # What is deliberately NOT here
//!
//! **A syscall ABI.** The user program below executes `svc #0` and asks for nothing. There is
//! no syscall number, no argument convention, no return value. DECISIONS §10 chose
//! capabilities, and the syscall surface gets designed against a capability table at 7d, in one
//! piece, on purpose. Not accreted here because it was convenient.

use elf::Elf;
use frames::{FRAME_SIZE, Frame};
use paging::{Flags, Half, MapError, Mapper};

use crate::arch::exceptions::{TrapFrame, enter_user};
use crate::arch::mmu::{self, phys_to_ptr};
use crate::arch::sync_icache;
use crate::memory;

/// Where a user program's stack goes. One page, and `sp` starts at the top of it: stacks grow down.
///
/// There is no matching `USER_CODE_VA` any more: it existed for `exec`, the one-page raw
/// machine-code loader the hand-assembled programs needed, and every program the kernel runs now
/// names its own load address in its ELF header.
pub const USER_STACK_VA: u64 = 0x0000_0000_0050_0000;
pub const USER_STACK_TOP: u64 = USER_STACK_VA + FRAME_SIZE;

/// A user address space: an L0 table for `TTBR0`, and every frame that hangs off it.
///
/// The `frames` vec holds **both** the pages we mapped and the intermediate page tables the
/// mapper allocated to reach them, because the allocator we hand the `Mapper` records
/// everything it hands out. That is the fix for the leak milestone 6 found the hard way
/// (`unmap_page` frees a leaf and leaves its L1/L2/L3 standing), applied *before* it bites:
/// an address space dies all at once, so we do not need `unmap` at all. We free the frames and
/// throw the whole table away.
pub struct AddressSpace {
    root: Frame,
    /// **This address space's TLB tag, for life** (milestone 15; crates/asid). Every user
    /// mapping is `nG`, so its TLB entries carry this number, and a context switch flushes
    /// nothing: the other spaces' entries just stop matching. Freed at drop, after
    /// `flush_asid` has made every entry so tagged vanish, which is what makes the number
    /// reusable.
    asid: u16,
    /// **The untyped region every page of this address space comes from, and who frees it**
    /// (milestone 14 phase B.4): the root table, the intermediate tables, and every owned leaf
    /// are retyped out of one region. The region *is* the record of what this address space
    /// owns, which is why there is no frame list: teardown is `untyped::destroy`, one call,
    /// made safe by §13 revocation.
    ///
    /// It carries the owner rather than just the name because **two different things build an
    /// address space and only one of them owns its memory**. See [`Backing`].
    backing: Backing,
}

/// **Who returns the region an [`AddressSpace`] spends, and the reason this is a type rather
/// than a comment.**
///
/// A space is built two ways, and they differ in exactly this. [`AddressSpace::new`] carves its
/// own region out of the frame allocator, so nobody else has a name for it and its `Drop` is the
/// only thing that can ever free it. [`user_aspace_create`] is handed a region that the caller
/// already holds an `Untyped` capability to (the `RETYPE_OBJ(ASPACE)` engine, milestone 19b), and
/// that caller reclaims it with `Untyped::DESTROY`. **A lent region has two names for one run of
/// memory, and only one of them may free it.**
///
/// Until 2026-08-18 both cases stored a bare `u64` and `Drop` called `untyped::destroy`
/// unconditionally, on the theory that a lent region is still pinned (`retype_object_page` pins,
/// `sched::reclaim_region` unpins) so the borrower's `destroy` is refused. That reasoning holds
/// only while the pin is still set, and `reclaim_region` clears it **before** the reaper's
/// deferred drop can land: `sched::finish_switch` hoists a dead thread's space out from under
/// `SCHED`, releases the lock, and only then drops it. Two `untyped::destroy` calls for one
/// region then overlap, both pass the refusal check, and both free every page of the run. That is
/// the intermittent `double free of frame 0x82a3e000` in
/// `force_kill_tests::destroy_reclaims_a_region_whose_resident_is_blocked_in_recv`
/// (notes/object-revocation.md BUGS, one sighting in 45 runs on riscv64).
///
/// Making it a two-variant enum with the name inside is rung one of AGENTS.md's ladder: a space
/// cannot be constructed without saying who frees its region, so the borrower's `Drop` cannot
/// free memory it does not own even if the pin is gone.
#[derive(Clone, Copy)]
enum Backing {
    /// The space carved this region itself and holds the only name for it. `Drop` frees it.
    Owned(u64),
    /// The region was handed in and belongs to whoever holds its `Untyped` capability. `Drop`
    /// **must not** free it; the memory comes back at that owner's `sched::reclaim_region`.
    Lent(u64),
}

impl Backing {
    /// The region to retype from. Spending a lent region is correct and is the whole point of
    /// `RETYPE_OBJ(ASPACE)`: the space runs on the caller's budget. Only *freeing* is restricted.
    fn region(self) -> u64 {
        match self {
            Backing::Owned(region) | Backing::Lent(region) => region,
        }
    }
}

/// Page-table-and-slack overhead an address space needs beyond its content pages: the L0 root,
/// an L1 and L2, a handful of L3s (one per 2 MiB window touched, `Spawn` maps included), and
/// margin. Sixteen pages = 64 KiB, generous for every process this kernel builds.
const AS_OVERHEAD: u64 = 16;

impl AddressSpace {
    /// Carve this address space's budget: `content_pages` of expected leaves plus the
    /// page-table overhead. Everything the address space ever owns comes out of this region,
    /// and running out is a clean `OutOfFrames` at map time, spending nobody's memory but its
    /// own. The region's pages are retyped zeroed, so the root needs no separate scrub.
    pub fn new(content_pages: u64) -> Option<Self> {
        let region = crate::untyped::create(content_pages + AS_OVERHEAD)?;
        let root = crate::untyped::retype_page(region)?;

        // Share the kernel into this root. On RISC-V a process runs on a single `satp` that must map
        // both the process (low half) and the kernel (high half), so the root gets copies of the
        // kernel root's high-half entries. On aarch64 the kernel lives in a separate TTBR1 and this
        // is a no-op. See arch::mmu::share_kernel_half and DECISIONS §17.
        mmu::share_kernel_half(root);

        // Into the revocation registry (phase C): this is how a later revoke finds our mapping
        // log, whose pages this same region will pay for. Full registry = no address space.
        if !crate::revoke::register_space(root, region) {
            crate::untyped::destroy(region);
            return None;
        }

        // A TLB tag of our own (milestone 15). Cannot exhaust: the allocator holds 255 and the
        // registry above admitted us, bounding live spaces at 160. The `?` is honesty, not a path.
        let Some(asid) = ASIDS.lock().alloc() else {
            crate::revoke::forget_root(root);
            crate::untyped::destroy(region);
            return None;
        };

        Some(AddressSpace {
            root: Frame::from_addr(root),
            asid,
            backing: Backing::Owned(region),
        })
    }

    /// Map one fresh, zeroed page at `va`, and hand back a **kernel** view of it.
    ///
    /// The returned slice is at `pa | KERNEL_VA_BASE` (the direct map), because the kernel
    /// cannot address `va` itself: `va` is a *low* address and means something entirely
    /// different from EL1's point of view. Two names for one frame, which is what the direct
    /// map is for.
    pub fn map_new(&mut self, va: u64, flags: Flags) -> Result<&'static mut [u8], MapError> {
        // Out of the address space's own region: the watermark is the ownership record, so
        // there is nothing to push anywhere. `retype_page` hands the page back zeroed, which is
        // what keeps `.bss` free for the loader.
        let frame =
            crate::untyped::retype_page(self.backing.region()).ok_or(MapError::OutOfFrames)?;
        self.map_at(va, frame, flags)?;

        // SAFETY: the frame is ours (retyped from our region), and the direct map is valid for
        // it. 'static is a lie we tell for convenience and then keep: the frame outlives every
        // use of this slice, because the region is freed only at `Drop`.
        let page = unsafe {
            core::slice::from_raw_parts_mut(
                mmu::phys_to_virt(frame) as *mut u8,
                FRAME_SIZE as usize,
            )
        };
        Ok(page)
    }

    /// Map an **existing** physical page into this address space, at `va`, with `flags`.
    ///
    /// The frame is **not** recorded for freeing, because we do not own it: it is either a
    /// device's MMIO (the PL011, for a console server) or a page **shared** with another address
    /// space (a message buffer). Freeing MMIO is meaningless, and freeing a shared page when one
    /// of its two holders dies would hand live memory to the allocator. So `Drop` leaves it
    /// alone. The intermediate page tables reaching it *are* recorded, exactly as in `map_new`,
    /// because those genuinely belong to this address space.
    ///
    /// This one function is what lets a driver leave the kernel: it is how the UART's registers
    /// get into a userspace server's address space, and how a shared buffer gets into both a
    /// client's and a server's.
    pub fn map_physical(&mut self, va: u64, phys: u64, flags: Flags) -> Result<(), MapError> {
        self.map_at(va, phys, flags)
    }

    /// Map `phys` at `va`. Intermediate tables come from this address space's own region, so
    /// they are covered by the one teardown call; the target page is whoever's it was.
    fn map_at(&mut self, va: u64, phys: u64, flags: Flags) -> Result<(), MapError> {
        let root = self.root.addr();
        let region = self.backing.region();

        // SAFETY: `root` is a zeroed L0 table. Half::Low, so the mapper refuses a high address:
        // mapping the kernel's half into TTBR0 would build a translation the hardware never
        // consults, and we would chase the ghost for hours.
        let mut mapper = unsafe {
            Mapper::<_, _, crate::arch::mmu::Format>::new(
                root,
                Half::Low,
                || crate::untyped::retype_page(region),
                phys_to_ptr,
            )
        };

        mapper.map(va, phys, flags)
    }

    /// The physical address of the L0 table: what page-table walks (translate, unmap,
    /// revocation) use. Not what goes in `TTBR0_EL1` any more; that is [`ttbr0`](Self::ttbr0),
    /// which carries the ASID too.
    #[cfg_attr(not(test), allow(dead_code))] // the walkers that use it live in the tests
    pub fn root(&self) -> u64 {
        self.root.addr()
    }

    /// The composed `TTBR0_EL1` value: root plus this space's ASID, ready to install.
    pub fn ttbr0(&self) -> u64 {
        mmu::ttbr0_value(self.root.addr(), self.asid)
    }
}

/// The machine's ASID allocator (milestone 15; the crate carries the proofs). Taken alone, at
/// address-space creation and teardown, holding nothing else that matters; a leaf-adjacent rank.
static ASIDS: crate::sync::IrqSafeMutex<asid::Allocator> =
    crate::sync::IrqSafeMutex::new(crate::sync::rank::ASIDS, asid::Allocator::new());

/// The most user-built address spaces alive at once (milestone 19b). They are immortal until
/// 19c wires process death, so this bounds creations for now; the revocation registry's
/// `MAX_SPACES` (160) leaves room for all of them beside the exec-built spaces.
const MAX_USER_SPACES: usize = 32;

/// **The user-aspace registry** (milestone 19b): the kernel-side records behind
/// `Object::Aspace` capabilities, named generationally like everything since milestone 14. The
/// `AddressSpace` in the slot is the same type exec builds, so every mechanism that works on a
/// process's space (region-paid tables, revocation logs, ASID tagging) works on a user-built
/// one identically. Entries are never removed in 19b; their `Drop` (which would destroy a
/// region the creator still holds a capability to) stays dormant until 19c designs teardown.
static USER_SPACES: crate::sync::IrqSafeMutex<slots::Table<AddressSpace, MAX_USER_SPACES>> =
    crate::sync::IrqSafeMutex::new(crate::sync::rank::ASPACES, slots::Table::new());

/// Create an address space **in and backed by** `region` (the `RETYPE_OBJ(ASPACE)` engine): the
/// root page is retyped from it (pinning it, atomically with the carve), and the region becomes
/// the space's table-and-record budget, exactly as for an exec-built space. `None` on an
/// exhausted region, a full registry, or ASID exhaustion (unreachable; the type is honest).
pub fn user_aspace_create(region: u64) -> Option<u64> {
    let root = crate::untyped::retype_object_page(region)?;
    mmu::share_kernel_half(root); // RISC-V single-satp: the process root carries the kernel high half

    if !crate::revoke::register_space(root, region) {
        return None; // registry full; the carved page is spent, the caller's own loss (B.4 rule)
    }
    let Some(asid) = ASIDS.lock().alloc() else {
        crate::revoke::forget_root(root);
        return None;
    };

    let space = AddressSpace {
        root: Frame::from_addr(root),
        asid,
        // Lent, not owned: the caller holds the `Untyped` capability to this region and reclaims
        // it with `DESTROY`. See `Backing` for the double free that taught us to say so.
        backing: Backing::Lent(region),
    };
    let name = USER_SPACES.lock().insert_with(|_| space);
    if name.is_none() {
        // Undo the bookkeeping; the page stays spent on the caller's budget.
        crate::revoke::forget_root(root);
        ASIDS.lock().free(asid);
    }
    name
}

/// Map `phys` into the user-built space `name` at `va` (the `MAP_INTO` engine). Tables and the
/// §13 record come from the space's own backing region; an unrecordable mapping is unmapped and
/// refused, exactly as at the `frame::MAP` syscall, because a mapping revocation cannot see is
/// the §13 use-after-free.
pub fn user_aspace_map(name: u64, va: u64, phys: u64, flags: Flags) -> Result<(), MapError> {
    let mut spaces = USER_SPACES.lock();
    let space = spaces.get_mut(name).ok_or(MapError::NotMapped)?;

    space.map_physical(va, phys, flags)?;
    if !crate::revoke::record_mapping(phys, space.root(), va) {
        mmu::unmap_user_at(space.root(), va);
        return Err(MapError::OutOfFrames);
    }
    // A code page a loader just filled via data writes (milestone 19d): the instruction fetcher
    // has its own cache and has never heard of those bytes. On aarch64 the I-cache is not
    // coherent with the D-cache, so make it so now, via the frame's direct-map VA (any VA that
    // maps the physical page works; caches are PIPT to the point of unification). Without this,
    // the child fetches whatever was in the frame before the loader wrote its program.
    if flags.is_user_executable() {
        sync_icache(mmu::phys_to_virt(phys), FRAME_SIZE as usize);
    }
    Ok(())
}

/// The root table of a user-built space, so tests can ask the walker what the space really
/// maps. Test support only: nothing in the kernel navigates a user-built space by root.
#[cfg(test)]
pub fn user_aspace_root(name: u64) -> Option<u64> {
    USER_SPACES.lock().get(name).map(|s| s.root())
}

/// **Take a user-built address space out of the registry** (milestone 19c.3): `Tcb::CONFIGURE`
/// moves it into the TCB, so it stops being a standalone object and starts dying with the
/// thread. `None` if the name does not resolve. This is what retires 19b's "immortal until 19c"
/// note: a bound space is reaped, an unbound one still leaks until teardown wiring, which is the
/// half-built audit's job.
pub fn take_user_aspace(name: u64) -> Option<AddressSpace> {
    USER_SPACES.lock().remove(name)
}

/// **Tear down every user address space whose root page lies in `[base, end)`** (object revocation,
/// the address-space case): each removed `AddressSpace` drops here, and its `Drop` forgets its
/// revocation records and frees its ASID (its region's memory comes back at the enclosing
/// `reclaim_region`, which unpins after this). This retires the "an unbound one still leaks" note on
/// `take_user_aspace`: a space created but never bound into a TCB is reclaimed with its region.
///
/// Bound spaces are **not** here: `CONFIGURE` moved them out of this registry into a TCB, so they
/// die with the thread (`Thread`'s drop), not through this sweep. Takes only the aspace-registry
/// lock, no `SCHED`, so `sched::reclaim_region` runs it as a step separate from the thread reap.
pub fn reap_aspaces_in_region(base: u64, end: u64) {
    // Find-then-remove one at a time, never dropping an `AddressSpace` while holding the registry
    // lock: its `Drop` takes the revocation, region, and ASID locks, and must not do so under ours.
    loop {
        let victim = {
            let spaces = USER_SPACES.lock();
            spaces.iter().find_map(|(name, space)| {
                let root = space.root.addr();
                (base <= root && root < end).then_some(name)
            })
        };
        let Some(name) = victim else { break };
        // `remove` returns the space; the registry lock is released at the `;`, then the space drops.
        let space = USER_SPACES.lock().remove(name);
        drop(space);
    }
}

/// Put a space back into the registry (milestone 19c.3): the unwind path if `CONFIGURE` took a
/// space and then could not bind it. It gets a fresh name; the caller's stale aspace cap will no
/// longer resolve, which is correct (the operation failed, but the space is not lost).
pub fn readopt_user_aspace(space: AddressSpace) -> Option<u64> {
    USER_SPACES.lock().insert_with(|_| space)
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Drop this address space's entries from the revocation database (§13) before its page
        // tables are freed and reused: a stale (root, va) would send a later revoke to walk tables
        // that now belong to someone else.
        crate::revoke::forget_root(self.root.addr());

        // If we are the live address space, stop being it BEFORE the frames go back on the free
        // list. Otherwise the TTBR0 the CPU is walking points at memory the allocator has
        // already handed to somebody else, and the next low-half access reads whatever they put
        // there. This is about the *walker*, not the TLB: since milestone 58 neither ISA flushes
        // anything on a root switch, and the cached translations are dealt with by the `flush_asid`
        // at the bottom of this function, which is the half that has to reach the other cores.
        if mmu::current_user_root() == self.root.addr() {
            mmu::deactivate_user();
        }

        // One call: revoke anything delegated out of this region (nothing can be: the region
        // has no capability, so userspace could never retype from it), then return the whole
        // run, root and tables and leaves alike, to the allocator. This is the
        // "reclaim-on-process-death" wiring §13 deferred; the frame list it replaced is gone.
        //
        // **Only for a region we own.** A lent one (`user_aspace_create`) belongs to whoever
        // holds its `Untyped` capability, and freeing it here is a double free of the whole run
        // the moment `sched::reclaim_region` has already unpinned it. `Backing` carries the
        // whole argument.
        if let Backing::Owned(region) = self.backing {
            crate::untyped::destroy(region);
        }

        // The ASID contract (crates/asid): invalidate every TLB entry wearing our tag, THEN
        // hand the number back. In the other order, the next owner of this ASID could hit our
        // stale translations, which is exactly the bug tagging exists to prevent.
        mmu::flush_asid(self.asid);
        ASIDS.lock().free(self.asid);
    }
}

/// Why a binary was refused.
///
/// **A bad user program must not be a kernel panic.** Every one of these is a thing a file can
/// simply *say*, and the answer is to decline and kill the thread, not to take the machine down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// The file is not an aarch64 static ELF we are willing to run. See `elf::Error`.
    NotLoadable(elf::Error),

    /// It asked to be loaded somewhere it may not go.
    ///
    /// **Including a KERNEL address.** An ELF gets to name its own load address, so this is
    /// exactly the thing a hostile binary tries: ask to be mapped over the kernel. It is
    /// refused by construction rather than by a check, because the `Mapper` is built with
    /// `Half::Low` and a high address is not a thing it can express (`MapError::WrongHalf`).
    Unmappable(MapError),
}

/// Parse an ELF, build an address space, and put it in memory. Do **not** run it.
///
/// Split out from [`run`] on purpose: this is the part that can fail, so it is the part a test can
/// call without dying (`run` diverges into the new process, or into `exit`).
pub fn load(image: &[u8]) -> Result<(AddressSpace, u64), LoadError> {
    let elf = Elf::parse(image).map_err(LoadError::NotLoadable)?;

    // The budget, counted from the file before anything is carved: every segment's pages, plus
    // one for the stack. (AS_OVERHEAD covers the tables.) A binary that lies about its size
    // simply exhausts its own region and fails to map, spending nobody else's memory.
    let content: u64 = elf
        .segments()
        .map(|seg| {
            let (start, end) = seg.page_range(FRAME_SIZE);
            (end - start) / FRAME_SIZE
        })
        .sum::<u64>()
        + 1;

    let mut space =
        AddressSpace::new(content).ok_or(LoadError::Unmappable(MapError::OutOfFrames))?;

    map_segments(&mut space, &elf)?;

    space
        .map_new(USER_STACK_VA, Flags::user_data())
        .map_err(LoadError::Unmappable)?;

    Ok((space, elf.entry()))
}

/// Lay an ELF's loadable segments into `space`, honouring their permissions exactly (milestone
/// 19d factored this out of `load` so `spawn_init` shares it; init's userspace loader mirrors it).
/// A read-only segment gets `user_rodata`, not `user_data`: a loader that widens permissions is
/// a loader you cannot reason about. `.bss` is free because `map_new` zeroes every page.
fn map_segments(space: &mut AddressSpace, elf: &Elf) -> Result<(), LoadError> {
    for seg in elf.segments() {
        let flags = if seg.is_executable() {
            Flags::user_code()
        } else if seg.is_writable() {
            Flags::user_data()
        } else {
            Flags::user_rodata()
        };

        let (start, end) = seg.page_range(FRAME_SIZE);
        let mut va = start;
        while va < end {
            let page = space.map_new(va, flags).map_err(LoadError::Unmappable)?;

            // Which of the file's bytes land in this page? An intersection, because `p_vaddr`
            // need not be page-aligned.
            let file_lo = seg.vaddr;
            let file_hi = seg.vaddr + seg.data.len() as u64;
            let lo = va.max(file_lo);
            let hi = (va + FRAME_SIZE).min(file_hi);
            if lo < hi {
                let dst = (lo - va) as usize;
                let src = (lo - file_lo) as usize;
                let n = (hi - lo) as usize;
                page[dst..dst + n].copy_from_slice(&seg.data[src..src + n]);
            }

            if seg.is_executable() {
                sync_icache(page.as_ptr() as u64, FRAME_SIZE as usize);
            }
            va += FRAME_SIZE;
        }
    }
    Ok(())
}

/// The program QEMU loaded into RAM for us, found via the device tree.
///
/// **The same road Linux's initramfs travels.** Nothing about this binary is known to the kernel
/// at build time: QEMU put a file somewhere in RAM and wrote the address into
/// `/chosen/linux,initrd-start`, and `memory::init` read it there and told the frame allocator
/// to keep its hands off. That reservation was written at milestone 3, for this.
#[cfg_attr(feature = "bench", allow(dead_code))] // the bench boot runs no user programs
pub fn initrd() -> Option<&'static [u8]> {
    let (start, size) = memory::initrd_region()?;

    // SAFETY: the region came from the device tree, it is inside RAM, the frame allocator has
    // been told it is forbidden, and the direct map names it. Nothing else will ever write here.
    Some(unsafe {
        core::slice::from_raw_parts(mmu::phys_to_virt(start) as *const u8, size as usize)
    })
}

/// The bytes of the program named `name` inside the initrd archive (milestone 19f). The initrd is a
/// nifefs image carrying init plus the programs init loads. The milestone tour and the
/// kernel-side service demos still run a role of the one `hello` binary, so they ask for `"init"`;
/// `spawn_init` and `boot_via_init` instead take the whole archive, because init parses the rest
/// itself. Returns `None` if there is no initrd, it will not parse, or it holds no such program.
// Used by the milestone tour, the kernel-wired virtio/console/shell demos, and the tests that load
// a user program; dead only in the bench boot, which runs no user programs.
#[cfg_attr(feature = "bench", allow(dead_code))]
pub fn program(name: &str) -> Option<&'static [u8]> {
    nifefs::Fs::parse(initrd()?).ok()?.read(name)
}

/// A physical page to map into a new process's address space, at a chosen VA.
///
/// The frame is **not** owned by the process (it is shared, or it is device MMIO), so it is not
/// freed when the process dies. See [`AddressSpace::map_physical`].
#[derive(Clone, Copy)]
pub struct Mapping {
    pub va: u64,
    pub phys: u64,
    pub flags: Flags,
}

/// **Everything a new process is handed at birth.** Its world, made explicit.
///
/// A capability system has no ambient environment: no inherited file descriptors, no `PATH`, no
/// uid. So a process gets *exactly* what is in this struct and nothing else. The whole of what it
/// can do is a function of `arg0`, `grants`, and `maps`, and reading a `Spawn` literal tells you
/// the complete authority of the thing you are about to start.
pub struct Spawn<'a> {
    /// Lands in `x0` at `_start`. A tiny channel for "which role are you" that needs no
    /// capability, the way a real kernel hands a new process its argc.
    pub arg0: u64,
    /// Lands in `x1`. A second scalar the process needs before it can name anything: the virtio
    /// driver's DMA region physical address, which it must write into device descriptors and
    /// cannot discover, because a process only knows virtual addresses.
    pub arg1: u64,
    /// Lands in `x2`. The virtio driver's device registers sit at a sub-page offset (slots are
    /// 0x200 apart, pages are 0x1000), so we map the containing page and tell the driver where in
    /// it the slot begins.
    pub arg2: u64,
    /// Capabilities, granted into slots 0, 1, 2, ... in order.
    pub grants: &'a [crate::cap::Cap],
    /// Extra pages: a shared buffer, a device's registers. Mapped after the ELF's own segments.
    pub maps: &'a [Mapping],
}

/// Where the kernel maps the initrd read-only into init's address space (milestone 19d): init
/// reads the ELF to parse it here. High enough not to collide with init's own segments (`0x40_0000`)
/// or its stack (`0x50_0000`).
#[cfg_attr(not(test), allow(dead_code))] // becomes the boot path at 19d.2; test-driven until then
pub const INITRD_VA: u64 = 0x2000_0000;

/// **Spawn the init task** (milestone 19d): load `image` as an ordinary user process, but also
/// map the whole initrd read-only at [`INITRD_VA`] so init can parse it, and hand init a building
/// budget (an untyped, slot 0) plus `report` (slot 1, `WRITE|GRANT` so init can endow a child).
/// init enters with `x0` = `role` and `x1` = the initrd length. This is the one program the kernel
/// still loads; init loads the rest (design/init-and-granular-spawn.md).
/// The interrupt the kernel routes to init for the IRQ-delegation test (19d.2b).
///
/// aarch64: SGI 3, distinct from the scheduler's RESCHED (0) and the older endpoint SGIs (1, 2).
/// RISC-V has no software-generated interrupt a test can raise on itself at all (the SBI IPI
/// arrives down the *software*-interrupt arm, never touching `irq_route`), so it names the console
/// UART's own line, which is the one interrupt this ISA can assert by hand. That makes it the same
/// number as [`UART_RX_INTID`] there, deliberately; [`spawn_init`] binds the route once and grants
/// two capabilities naming it. See `sched::tests`' `DELIVERY_IRQ`, which reached the same conclusion.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "aarch64")]
pub const INIT_TEST_SGI: u32 = 3;
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "riscv64")]
pub const INIT_TEST_SGI: u32 = 10;

/// The console UART's receive interrupt on QEMU `virt`. init routes and delegates it so the input
/// driver it builds (19d.2c) can wait on keystrokes. aarch64's PL011 is SPI 1 = INTID 33; RISC-V's
/// NS16550 is PLIC source 10.
///
/// **The documented fallback, not the answer.** The boot paths ask the machine first
/// ([`uart_irq_and_source`]): on the JH7110, UART0 interrupts on PLIC line 32, and a kernel that
/// armed this constant there enabled an unrelated source, proven on silicon when a key press at
/// boot 13's completed tour reached nothing (notes/visionfive2.md, BUGS). This number is what a
/// tree that does not say falls back to, which on QEMU is also the right answer.
#[cfg(target_arch = "aarch64")]
pub const UART_RX_INTID: u32 = 33;
#[cfg(target_arch = "riscv64")]
pub const UART_RX_INTID: u32 = 10;

/// The console UART's interrupt line and which source decided it: the device tree's answer when
/// it gave one (`memory::uart_irq`), else [`UART_RX_INTID`], QEMU `virt`'s constant. The source
/// string exists to be printed: a bench transcript that names the number's origin is diagnosable,
/// and the one that did not already cost a boot (notes/visionfive2.md).
pub fn uart_irq_and_source() -> (u32, &'static str) {
    match crate::memory::uart_irq() {
        Some(n) => (n, "device tree"),
        None => (UART_RX_INTID, "QEMU-virt fallback; the tree did not say"),
    }
}

/// The console UART's registers, physically. aarch64 `virt` puts a PL011 at `0x0900_0000`; RISC-V
/// `virt` puts an NS16550 at `0x1000_0000`. init holds a device capability for it and delegates it
/// to the console and input drivers it builds. Matches `console::UART_PHYS`.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "aarch64")]
pub const UART_PHYS: u64 = 0x0900_0000;
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "riscv64")]
pub const UART_PHYS: u64 = 0x1000_0000;

/// The archive entry holding the milestone 7-19 **role catalogue**: the one binary the kernel
/// re-enters at a chosen role to play a client, a server, or init itself.
///
/// It is `hello` in both cases; only the name it is packed under differs. aarch64 packs it as
/// `init`, because there it *is* the boot program. RISC-V's `init` is the portable `builder` demo,
/// so hello goes in under its own name. Reading the wrong one gets a program with no such roles.
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "aarch64")]
pub const INIT_ROLES_ENTRY: &str = "init";
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "riscv64")]
pub const INIT_ROLES_ENTRY: &str = "hello";

/// Init's stack, in pages (19d.2c): init loads whole ELFs with deep call chains, so its stack is
/// larger than an ordinary process's one page. 8 pages (32 KiB) is generous.
#[cfg_attr(not(test), allow(dead_code))]
const INIT_STACK_PAGES: u64 = 8;

/// The role at which `hello` **is** the boot path: it builds the console, the line discipline, the
/// input driver and the shell, then stays alive as the spawn service. It is named at module level
/// because [`spawn_init`] has to know which role gets a filesystem, and "the one that runs the
/// prompt" is the answer.
#[cfg_attr(not(test), allow(dead_code))]
pub const INIT_BOOT_ROLE: u64 = 27;

/// Returns a [`holding::Holding`] over init's thread and its building budget, so a test that is
/// finished with this init can hand **2048 frames** back. That number is not incidental: six
/// `spawn_init` tests reserve 8 MiB each and the measured aarch64 boot spent 12289 frames on them,
/// **42% of everything the suite never returned** (notes/frames.md). The boot path ignores the
/// holding, correctly: the boot's init is the system.
// The aarch64 test module is the only caller: 19d.2 shipped, and the shape that actually became
// the boot path is `init_boot` below. RISC-V boots the same system through `riscv_shell_boot`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn spawn_init(image: &'static [u8], role: u64, report: crate::sched::EpId) -> holding::Holding {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd to hand init");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);

    // Route the test interrupt (19d.2b) BEFORE spawning init: the test raises the SGI as soon as
    // this returns, and an interrupt that fires before it is routed is dropped ("unexpected
    // interrupt"), not queued. Setting up the route here means the fire is counted on the routed
    // endpoint even though the init-built child is not yet waiting; the child's WAIT drains it.
    crate::sched::bind_irq(INIT_TEST_SGI, crate::sched::create_endpoint());
    crate::arch::irq::enable(INIT_TEST_SGI);
    // And the UART receive interrupt (19d.2c): the input driver init builds waits on it. Route and
    // enable it here, so init can delegate the Irq cap to that driver. The number is the machine's
    // (uart_irq_and_source; on the JH7110 the QEMU constant armed the wrong PLIC source, see its
    // doc), and the line names the source so a transcript is diagnosable. On QEMU RISC-V the
    // discovered line and INIT_TEST_SGI are the SAME source (see INIT_TEST_SGI), and binding it
    // twice would leave the first endpoint routed to nothing while the test waits on it, so bind
    // once and grant twice.
    let (uart_rx_intid, uart_irq_source) = uart_irq_and_source();
    crate::println!("  uart irq  : {uart_rx_intid} ({uart_irq_source})");
    if uart_rx_intid != INIT_TEST_SGI {
        crate::sched::bind_irq(uart_rx_intid, crate::sched::create_endpoint());
        crate::arch::irq::enable(uart_rx_intid);
    }

    // The initrd is a nifefs archive (milestone 19f), not a bare ELF: it carries init plus the
    // programs init will load. The kernel reads only the one entry it must, "init". This is the same
    // "honest residue" as before (something has to load the first program), now naming that program
    // through a fixed archive index instead of assuming it sits at offset 0. Every other program is
    // init's to parse. See notes/init-and-loading.md.
    //
    // Read and MEASURE it here, on the boot path, before anything is spawned (milestone 22 phase
    // B.1): the check has to be the thing that decides whether a thread is created at all, not
    // something the new thread does to itself. `trust::require` halts on a mismatch, so past this
    // line the bytes are the ones this kernel image was built against.
    let boot_fs = match nifefs::Fs::parse(image) {
        Ok(fs) => fs,
        Err(e) => {
            crate::println!("  boot archive is not a nifefs image: {e:?}");
            crate::arch::halt();
        }
    };
    let Some(init_bytes) = boot_fs.read(INIT_ROLES_ENTRY) else {
        crate::println!("  boot archive has no '{INIT_ROLES_ENTRY}' program");
        crate::arch::halt();
    };
    crate::trust::require(INIT_ROLES_ENTRY, init_bytes);
    // And the table init measures *its* loads against (milestone 104). The whole archive is about to
    // be mapped into init, so this is the same decision one link down: what the kernel hands over
    // has to be what this kernel image was built against, or the refusals init makes with it are
    // worth nothing.
    crate::trust::require_program_measurements(&boot_fs);

    // **The filesystem, for the boot role only** (milestone 50). Bring up the block server and the
    // FS server here, before init exists, and hand init the service endpoint and the page its
    // clients map; init narrows both into the shell, which is what makes `>` and `<` reachable from
    // a real prompt. `None` is the ordinary case for a run with no RedoxFS disk attached, and the
    // whole chain from here to the prompt treats it as "this boot has no filesystem" rather than as
    // a failure. The other roles are milestone 19d's tests, which wire their own worlds.
    let fs = if role == INIT_BOOT_ROLE {
        program("fs_server").and_then(|fs_server| {
            fs_service::root_directory(fs_service::blk_server_image(), fs_server)
        })
    } else {
        None
    };

    // **The wall clock, for the boot role only** (milestone 51's wiring). Started here, before init
    // exists, for the same reason the filesystem is: init is the one that hands it on, and a service
    // spawned after its client would be a race. See [`boot_clock_page`] for why the grant does not
    // depend on whether the machine turned out to have an RTC. The other roles are milestone 19d's
    // tests, whose slot numbering must not move.
    let clock_page = if role == INIT_BOOT_ROLE {
        Some(boot_clock_page())
    } else {
        None
    };

    // **init's building budget is carved here, not inside the thread**, so the caller has a name for
    // it and can reclaim it. A large untyped init retypes the child's aspace, frames and TCB from,
    // sized for a full copy of the initrd program plus its tables and init's scratch. Carving it out
    // here changes nothing about what init gets; it changes who can name it afterwards, which is the
    // whole difference between 8 MiB spent and 8 MiB lent. See notes/frames.md.
    let build_region = crate::untyped::create(2048).expect("no building budget for init");

    let tid = crate::sched::spawn(move || {
        let elf = match Elf::parse(init_bytes) {
            Ok(e) => e,
            Err(e) => {
                crate::println!("  init image is not loadable: {e:?}");
                crate::sched::exit();
            }
        };
        // A region big enough for init's own segments, the initrd's page tables, and slack.
        let content: u64 = elf
            .segments()
            .map(|seg| {
                let (start, end) = seg.page_range(FRAME_SIZE);
                (end - start) / FRAME_SIZE
            })
            .sum::<u64>()
            + 1
            + initrd_pages / 512
            + INIT_STACK_PAGES
            + 8;
        let mut space = AddressSpace::new(content).expect("no memory for init");
        map_segments(&mut space, &elf).expect("could not lay out init");
        // A multi-page stack: init loads whole ELFs with deep call chains (the loader loop,
        // copy_from_slice, the elf parser), so one page overflows. Map INIT_STACK_PAGES down from
        // USER_STACK_TOP; the entry sp is unchanged (USER_STACK_TOP).
        for k in 0..INIT_STACK_PAGES {
            space
                .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
                .expect("could not map init's stack");
        }

        // Map the initrd, one page at a time, read-only. These are reserved RAM pages the frame
        // allocator does not own, so this maps rather than allocates.
        for i in 0..initrd_pages {
            space
                .map_physical(
                    INITRD_VA + i * FRAME_SIZE,
                    initrd_start + i * FRAME_SIZE,
                    Flags::user_rodata(),
                )
                .expect("could not map the initrd into init");
        }

        crate::sched::adopt_address_space(space);
        // The delegable root budget: init narrows and hands budgets to the children it builds, so
        // the root carries GRANT (milestone 31). Rights only narrow downward from here.
        crate::sched::grant(crate::cap::untyped_root_cap(build_region)).expect("grant untyped");
        crate::sched::grant(crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ))
        .expect("grant report");
        // A device capability for the UART (slot 2), so init can build a driver and hand it the
        // registers (19d.2). WRITE (device access) | GRANT (init delegates it to the driver).
        crate::sched::grant(crate::cap::device_frame_cap(
            UART_PHYS,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ))
        .expect("grant uart device");
        // An interrupt capability (slot 3): the third delegatable device authority, so init can
        // build an interrupt-driven driver (19d.2b). The route was set up above, before the spawn;
        // this only grants init the Irq cap (a per-thread act). READ (WAIT/ACK) | GRANT (delegate).
        crate::sched::grant(crate::cap::irq_cap_rights(
            INIT_TEST_SGI,
            crate::cap::Rights::READ.union(crate::cap::Rights::GRANT),
        ))
        .expect("grant test irq");
        // The UART receive interrupt (slot 4), for the input driver init builds (19d.2c). The
        // same discovered number the route above was bound with, or the cap would name a source
        // no endpoint serves.
        crate::sched::grant(crate::cap::irq_cap_rights(
            uart_rx_intid,
            crate::cap::Rights::READ.union(crate::cap::Rights::GRANT),
        ))
        .expect("grant uart rx irq");
        // The clock page (slot 5), read-only, before the filesystem pair so its slot number does not
        // depend on whether a disk was attached. `READ` is the whole of DECISIONS §43's split at
        // this boundary: init can hand a child a reader and has nothing that could set the time.
        // `GRANT` so it can hand one on at all.
        if let Some(phys) = clock_page {
            crate::sched::grant(crate::cap::frame_cap(
                phys,
                crate::cap::Rights::READ.union(crate::cap::Rights::GRANT),
            ))
            .expect("grant the clock page");
        }
        // The file service (slot 6) and the page its clients share with it (slot 7), when this boot
        // has a filesystem. GRANT on both, because init's job with them is to delegate: it narrows
        // the endpoint into the shell and maps the frame into its address space. `a2` carries the
        // rights the endpoint holds, which is also how init is told there is one at all.
        let fs_rights = match fs {
            Some((file_ep, file_shared)) => {
                crate::sched::grant(crate::cap::endpoint_cap(
                    file_ep,
                    crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
                ))
                .expect("grant the file service");
                crate::sched::grant(crate::cap::frame_cap(
                    file_shared,
                    crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
                ))
                .expect("grant the shared file page");
                fs_proto::dir::ALL
            }
            None => 0,
        };

        enter_frame(elf.entry(), USER_STACK_TOP, role, initrd_len, fs_rights)
    })
    .expect("could not spawn init");

    let mut held = holding::Holding::new();
    held.add_thread(tid);
    held.add_region(build_region);
    held
}

/// **The init boot path** (milestone 19d.2c): spawn init at the boot role and return. init
/// brings up the console out of its own budget and announces the system through it, so the
/// system's first output comes from a userspace driver init built, not from the kernel. The
/// report endpoint is unused on this path (init prints via the console it builds, not back to the
/// kernel); it is created only to satisfy `spawn_init`'s shape.
// The aarch64 interactive boot hands off here for every non-bench build (the tour, `--features
// shell`, and `--features initboot`), since milestone 28 retired the kernel-wired `shell_service`.
#[cfg(not(any(test, feature = "bench")))]
pub fn boot_via_init(image: &'static [u8]) {
    let report = crate::sched::create_endpoint();
    // The holding is dropped on purpose: on this path init **is** the system, and there is nobody
    // to hand its memory back to.
    let _ = spawn_init(image, INIT_BOOT_ROLE, report);
}

/// **The SMB serve boot** (milestone 54, `--features smb_serve`, `cargo xtask smb-serve`): wire
/// the net server and the SMB adapter in serve-forever mode, print the mount instructions, and
/// leave the machine to them. A demo boot in the spirit of `--features shell`, and deliberately
/// minimal: no init, no console, no shell, because a file server's whole interface is its port.
///
/// Reached only on aarch64 (the riscv boot parks in its own tour first), which is scope rather
/// than accident: the point of this boot is a Mac mounting the guest, and the SMB *protocol*
/// path is gated on both ISAs by the test suite. See notes/smb.md.
#[cfg(feature = "smb_serve")]
pub fn smb_serve_boot() {
    use crate::println;
    let Some(net_stack) = program("net_stack") else {
        println!("smb-serve: no net_stack program in the initrd");
        return;
    };
    let Some(smb_server) = program("smb_server") else {
        println!("smb-serve: no smb_server program in the initrd");
        return;
    };
    // The share: the real filesystem when a RedoxFS disk is attached (`cargo xtask smb-serve`
    // builds and attaches one), the baked-in fixture otherwise. Wired before the adapter exists,
    // like every service-before-client here.
    // Read-write, because the point of this boot is a real Mac exercising the whole adapter and
    // the write path is half of it. That is a loud choice rather than a default: sessions are
    // guest, guest means everyone (`smb_proto`'s BUGS), so anything reachable on the forwarded
    // port may change the image. The banner below says so where the person about to mount it
    // will read it.
    let fs = program("fs_server")
        .and_then(|fs_server| fs_service::root_directory(fs_service::blk_server_image(), fs_server))
        .map(|(ep, shared)| (ep, shared, virtio_service::SMB_SHARE_FS_READ_WRITE));
    if fs.is_none() {
        println!("smb-serve: no RedoxFS disk; serving the baked-in fixture share");
    }
    let Some(mdns_responder) = program("mdns_responder") else {
        println!("smb-serve: no mdns_responder program in the initrd");
        return;
    };
    let Some((ip, smb, mdns)) =
        virtio_service::start_smb_serve(net_stack, smb_server, mdns_responder, fs)
    else {
        println!(
            "smb-serve: no virtio-net device to serve on (the runner attaches one when NIFE_NET is set)"
        );
        return;
    };
    println!();
    println!(
        "smb-serve: DHCP lease {}.{}.{}.{}",
        (ip >> 24) & 0xff,
        (ip >> 16) & 0xff,
        (ip >> 8) & 0xff,
        ip & 0xff,
    );
    let word = crate::sched::ipc_recv(smb)[0];
    if word == 1 {
        println!("smb-serve: the SMB adapter is listening on guest port 445.");
        println!("smb-serve: on the Mac (assuming xtask's default forward, 127.0.0.1:10445):");
        println!(
            "smb-serve:   Finder > Go > Connect to Server: smb://127.0.0.1:10445/share  (as Guest)"
        );
        println!(
            "smb-serve:   or: mkdir /tmp/nife-share && mount_smbfs -N //GUEST@127.0.0.1:10445/share /tmp/nife-share"
        );
        println!(
            "smb-serve: the share is READ-WRITE and every session is admitted as guest, so \
             anything that can reach the forwarded port can change the image. See notes/smb.md, \
             including its BUGS. The files are the RedoxFS image's (motd, scratch, doc/...) \
             unless the fixture fallback was announced above, which is read-only."
        );
        println!(
            "smb-serve: it answers Apple's AAPL create context claiming the Time Machine volume \
             capability (milestone 55). Whether macOS accepts that is untested; nothing here has \
             met a Mac's Time Machine UI, and durability stops at the block server (notes/smb.md)."
        );
    } else {
        println!("smb-serve: the adapter failed to bind its port (code {word:#x})");
    }
    // The discovery half (milestone 55). A separate process holding a separate authority: this one
    // has the UDP port and no share, the adapter has the share and no port a browser can find.
    let word = crate::sched::ipc_recv(mdns)[0];
    if word == 1 {
        println!(
            "smb-serve: the mDNS responder is advertising _smb._tcp, _adisk._tcp and \
             _device-info._tcp on 5353."
        );
        println!(
            "smb-serve:   on a Mac on the SAME SEGMENT: dns-sd -B _adisk._tcp   (QEMU's user-mode \
             networking does not carry multicast, so this needs real hardware; see notes/mdns.md)"
        );
        println!("smb-serve:   what it advertises is user/mdns_responder.conf, not compiled-in.");
    } else {
        println!("smb-serve: the mDNS responder failed to start (code {word:#x})");
    }
}

/// Load the initrd program and become it, handed the world described by `spawn`. Never returns.
pub fn run(image: &[u8], spawn: Spawn) -> ! {
    let (mut space, entry) = match load(image) {
        Ok(v) => v,
        Err(e) => {
            crate::println!();
            crate::println!("  refused to load a user program: {e:?}");
            crate::println!("  the kernel is fine.");
            crate::sched::exit();
        }
    };

    // The extra pages go in BEFORE we hand the address space off: a shared message buffer, or a
    // device's MMIO for a driver. This is the line that puts a UART into a userspace process.
    for m in spawn.maps {
        space
            .map_physical(m.va, m.phys, m.flags)
            .expect("could not map a Spawn page into the new address space");
    }

    crate::sched::adopt_address_space(space);

    // HAND IT ITS WORLD. Granted in order, so slot 0 is `grants[0]`, and reading the caller's
    // `Spawn` literal tells you the entire authority of the process. There is no path it can
    // say, no uid it can be. A capability system's "environment" is not a variable, it is this.
    for &cap in spawn.grants {
        crate::sched::grant(cap).expect("no free capability slot");
    }

    enter_at(entry, spawn.arg0, spawn.arg1, spawn.arg2)
}

/// Drop to EL0 at `entry`, on a fresh stack, with `arg0` in `x0`. Never returns.
///
/// `arg0` reaches `_start` as its first argument (AAPCS64 puts it in `x0`). It is how the kernel
/// tells one binary which of several roles to play, the way a real kernel hands a new process
/// its argc/argv. See the console server, which is the same ELF as its client with a different
/// `arg0`.
/// Drop the **current** thread to EL0 at `entry` on `user_sp`, no arguments (milestone 19c.3).
/// The entry path for a thread started through the TCB object surface, which runs on the freshly
/// scheduled thread rather than the one that called `START`. The address space is already
/// installed (the context switch that scheduled us in used our `space` field). This is `enter_at`
/// with a caller-chosen stack and zero args; `enter_at` is now the exec wrapper over it.
pub fn enter_at_on_current(entry: u64, user_sp: u64, arg0: u64, arg1: u64, arg2: u64) -> ! {
    enter_frame(entry, user_sp, arg0, arg1, arg2)
}

fn enter_at(entry: u64, arg0: u64, arg1: u64, arg2: u64) -> ! {
    enter_frame(entry, USER_STACK_TOP, arg0, arg1, arg2)
}

fn enter_frame(entry: u64, user_sp: u64, arg0: u64, arg1: u64, arg2: u64) -> ! {
    // THE TRAPFRAME IS NOT AN ORDINARY LOCAL, and this cost us an afternoon.
    //
    // It must sit at the TOP OF THIS THREAD'S KERNEL STACK, because that is where the hardware
    // will look for it. `enter_userspace` does `mov sp, x0`, and `exception_restore` leaves
    // SP_EL1 = x0 + 272 across the `eret`. So when the user traps back in, `SAVE_CONTEXT`
    // subtracts 272 and rebuilds the frame **at exactly this address**. It had better be
    // writable, and it had better be a stack.
    //
    // The first version wrote `enter_userspace(&TrapFrame { .. })`, and every field of that
    // struct is a compile-time constant, so Rust CONST-PROMOTED IT INTO .rodata. The kernel
    // set SP_EL1 to read-only memory, and the user's first `svc` faulted trying to write its
    // own trap frame there. See notes/userspace.md: the kernel then walked `sp` DOWNWARD
    // through .rodata and the whole of .text, 272 bytes and one fault at a time, until it fell
    // out of the bottom of the image into writable RAM and could finally tell us.
    let top = crate::sched::current_kernel_stack_top()
        .expect("a user thread needs a kernel stack of its own to be trapped onto");

    // **The frame goes at the very top of the kernel stack, on both ISAs, and it must be ABOVE the
    // live `sp`.** Everything below `sp` belongs to somebody else: a callee's frame, and on a trap
    // the 288/272 bytes the vector subtracts from `sp` to build its own frame. An object parked
    // there is not stored, it is lent.
    //
    // RISC-V used to compute this from the live `sp` instead (`(current_sp().min(top) - size) & !15`),
    // because its TCB entry path is shallow and a frame at the top would have overlapped this
    // function's own stack. That traded a deterministic overlap for an intermittent one, and
    // milestone 71 caught it: `current_sp()` is a real call at opt-level 0, so it returned
    // `sp - 16`, which put the frame at `sp - 304` while `trap.s` builds an S-mode trap frame at
    // `sp - 288`. The two differ by exactly 16 bytes, so the user frame's `x[2]` (the user `sp`)
    // sat precisely on the trap frame's `x[0]` slot, which `trap.s` writes as a literal zero. Any
    // timer interrupt taken between building the frame and consuming it therefore rewrote the whole
    // frame: user `sp` read 0 every time, `sepc` read whatever `t5` held, and `sstatus` read the
    // trap's `scause` (whose UXL bits are 0, an illegal U-mode XLEN). When `t5` happened to be 0 the
    // `sepc == 0` guard in `enter_user` fired; when it did not, the thread `sret`ed to a garbage PC,
    // died on its first instruction, and never answered whoever was waiting on it, which is a
    // lost-wakeup hang with no guard message. See notes/riscv-port.md.
    //
    // The shallow-path problem the old code was avoiding is real, and the fix for it is a
    // reservation rather than a moving target: `user_entry_trampoline` (both ISAs) drops `sp` by a
    // frame's worth before the first Rust frame exists, so this region is off-limits to the entry
    // path by construction. See arch/*/context.s.
    //
    // `thread_trampoline` deliberately does NOT reserve, and the asymmetry is the point. Only the
    // TCB path can be shallow; the exec path reaches here through `run` and the ELF loader, so its
    // frames are always far below the stack top, and reserving would spend a frame's worth on every
    // kernel thread to insure against a depth that cannot happen. If that ever stops being true, the
    // assertion below is what says so.
    let slot = top - size_of::<TrapFrame>() as u64;
    let frame = slot as *mut TrapFrame;

    // And prove it, rather than trusting the reasoning above. This is one check, once per
    // exec, against a bug whose symptom is a nested fault storm that eats the kernel image.
    assert!(
        mmu::translate(frame as u64).is_some_and(|(_, f)| f.is_writable()),
        "the user's TrapFrame at {frame:p} is not in writable memory",
    );

    // **The invariant the milestone 71 fault violated**, checked rather than reasoned about. A slot
    // at or below the live `sp` is one a callee or a trap will build over, and the old RISC-V
    // placement failed this on the very first user entry. Necessary rather than sufficient: this
    // function's own frame sits *above* `sp` and is not covered, which is what the trampoline
    // reservation handles. Cheap enough to keep: one comparison per exec.
    assert!(
        slot >= crate::arch::current_sp(),
        "the user's TrapFrame at {frame:p} is below the live sp: a callee frame or a trap frame \
         will be built over it",
    );

    // SAFETY: `frame` is 16-byte-aligned writable kernel stack (a KernelStack top is page
    // aligned and TrapFrame is a multiple of 16), the user code and stack are mapped, and the
    // user address space is installed. `arch` owns the register layout: we ask for a user-entry
    // frame and hand it back to `arch` to make the jump (notes/riscv-port.md, leak #3).
    unsafe {
        frame.write(TrapFrame::for_user_entry(
            entry,
            user_sp,
            [arg0, arg1, arg2],
        ));
        enter_user(frame)
    }
}

// --- the test programs the kernel loads by name ---
//
// **There used to be five hand-assembled programs here**, three aarch64 and two RISC-V, written as
// `global_asm!` machine code in `.rodata` and copied into a user page by a one-page loader. They
// were honest milestone-7a scaffolding: there was no ELF loader and no filesystem to load from, so
// the "binary" rode inside the kernel image.
//
// They are gone (milestone 19's user-test port). The behaviours are ordinary (yield twice, read a
// forbidden address, spin forever), the toolchain builds them for both targets, and the initrd
// already delivers thirty other programs, so the scaffolding had outlived its reason twice over:
// once when 7c shipped the ELF loader, and again when the second ISA turned "one hand-written
// program" into "two hand-written programs, forever". Keeping them would have meant hand-assembling
// every one of them a second time to run the same tests on RISC-V.
//
// What replaced each:
//
//   - aarch64 `hello`, riscv `USER_HELLO` (yield, yield)  -> `outlaw`, role `OUTLAW_ROUND_TRIP`
//   - aarch64 `outlaw`  (read a kernel address)           -> `outlaw`, role `OUTLAW_READ_KERNEL`
//   - aarch64 `spin`    (loop, no syscall, no stack)      -> the `spinner` binary (DECISIONS §24)
//   - riscv `USER_REPORTER` (invoke a cap, SEND a word)   -> `riscv_worker_demo`, which builds a
//     process from the same parts and runs a real ELF through them
//
// This also removed `exec`, the one-page raw-machine-code loader they needed. Every program the
// kernel runs now arrives as an ELF.

/// The `outlaw` program's roles (user/src/outlaw.rs), passed in the first argument register.
///
/// `ROUND_TRIP` yields twice and exits: two syscalls from user mode, where the second can only
/// happen if the return from the first genuinely put the thread back at EL0/U-mode.
// The tests use it on both ISAs; of the two boot tours only RISC-V's has a syscall-count step.
#[cfg_attr(not(test), allow(dead_code))]
pub const OUTLAW_ROUND_TRIP: u64 = 0;

/// `READ_KERNEL` reads the address handed to it in the second argument register, which is what makes
/// the program portable: the kernel's own memory lives at a different virtual address on each ISA,
/// and the caller knows which. See `tests::a_user_program_cannot_read_a_kernel_address`.
// The tour uses it, and the shell/initboot/bench boots skip the tour.
#[cfg_attr(not(test), allow(dead_code))]
pub const OUTLAW_READ_KERNEL: u64 = 1;

/// **Load and run a real compiled ELF at U-mode on RISC-V** (milestone 20, the user-ELF step).
///
/// This takes the bytes of the `worker` program (a Rust binary compiled to a riscv64 ELF, delivered
/// as the initrd)
/// and runs them through the kernel's *real* ELF loader. [`load`] parses the file, builds an address
/// space with each `PT_LOAD` segment mapped W^X at the VA it names, and maps a stack; nothing here is
/// riscv-specific except that the loader was just taught to accept `EM_RISCV`. The worker is granted
/// WRITE on one endpoint as its slot 0, started with the input `n` in its second argument register
/// (`a1`), squares it, and SENDs the answer home.
///
/// Receiving `n*n` proves the whole ELF path works on RISC-V: parse, segment mapping with correct
/// permissions, the entry point, argument passing across the `START` boundary, and the endpoint
/// SEND, all from a program the kernel did not hand-write. `load` is arch-neutral; this is the same
/// code aarch64 runs, now on the RISC-V address space and trap path.
#[cfg(target_arch = "riscv64")]
pub fn riscv_worker_demo(worker: &[u8], n: u64) -> Result<u64, LoadError> {
    // The kernel's real loader: parse, build the address space, map the W^X segments and a stack.
    let (space, entry) = load(worker)?;
    // `load` returns an owned AddressSpace; the TCB path binds one by registry name, so register it.
    let aspace_name = readopt_user_aspace(space).expect("register the loaded aspace");

    // The worker's one authority: WRITE on a report endpoint, which it will hold as slot 0.
    let result = crate::sched::create_endpoint();
    let result_cap = crate::cap::endpoint_cap(result, crate::cap::Rights::WRITE);

    // Build the thread from parts: a TCB, the cap in slot 0, configure at the ELF's entry, start.
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    let slot = crate::sched::tcb_insert_cap(tid, result_cap, None).expect("cap insert");
    assert_eq!(slot, 0, "the worker's report cap must land in slot 0");
    crate::sched::configure_tcb(tid, entry, USER_STACK_TOP, aspace_name).expect("configure");
    // The worker reads its input from a1 (the second argument); a0 and a2 are unused.
    crate::sched::start_tcb(tid, [0, n, 0]).expect("start");

    Ok(crate::sched::ipc_recv(result)[0])
}

/// **The richer initrd: userspace init builds the system** (milestone 20). The RISC-V counterpart of
/// [`spawn_init`], trimmed to the portable core (no GIC, no PL011 device cap, no IRQ delegation: this
/// proves the composition model, not the aarch64 interactive system).
///
/// The initrd is a nifefs archive holding `init` (the portable `builder` program) plus `worker`.
/// The kernel loads only `init`, maps the whole archive read-only into its address space, and grants
/// it exactly two capabilities: a large untyped budget (slot 0) and a report endpoint with
/// WRITE|GRANT (slot 1). From those, `init` reads `worker` out of the archive by name, builds it as a
/// child entirely from its own budget (a userspace ELF loader), hands the child a WRITE view of the
/// report endpoint as its slot 0, and starts it with an input. The child squares the input and SENDs
/// the answer straight to the report endpoint, which this function is waiting on. The kernel never
/// parsed or mapped the worker: init did. That is the whole point (DECISIONS §17, and the aarch64
/// init lineage in notes/init-and-loading.md), now on RISC-V.
#[cfg(target_arch = "riscv64")]
pub fn riscv_initrd_demo(archive: &'static [u8]) -> Result<u64, LoadError> {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);

    // Read only the one entry the kernel must: "init" (the builder). The rest is init's to parse.
    let fs = nifefs::Fs::parse(archive).expect("initrd is not a nifefs archive");
    let init_bytes = fs.read("init").expect("archive has no 'init' program");
    // Measured boot (milestone 22 phase B.1): the boot program is checked against the digest compiled
    // into this kernel image before its address space exists, and a mismatch halts. Same check, same
    // trust root, same place in the sequence as aarch64's `spawn_init`; the parity gate (§19) asks
    // for exactly that.
    crate::trust::require("init", init_bytes);
    // The measurement table too (milestone 104). `builder` does not read it (it loads exactly one
    // worker and is milestone 20's demo, not the interactive system), but the whole archive is
    // mapped into it either way and the parity gate (§19) asks for the same check in the same place
    // on both boards, not for the same check on the boot path that happens to use it.
    crate::trust::require_program_measurements(&fs);
    let elf = Elf::parse(init_bytes).map_err(LoadError::NotLoadable)?;

    // init's address space: its own segments, a deep stack (it runs an ELF loader loop), and the
    // whole archive mapped read-only so it can parse the programs it loads.
    let content: u64 = elf
        .segments()
        .map(|seg| {
            let (s, e) = seg.page_range(FRAME_SIZE);
            (e - s) / FRAME_SIZE
        })
        .sum::<u64>()
        + 1
        + initrd_pages / 512
        + INIT_STACK_PAGES
        + 8;
    let mut space =
        AddressSpace::new(content).ok_or(LoadError::Unmappable(MapError::OutOfFrames))?;
    map_segments(&mut space, &elf)?;
    for k in 0..INIT_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .map_err(LoadError::Unmappable)?;
    }
    for i in 0..initrd_pages {
        space
            .map_physical(
                INITRD_VA + i * FRAME_SIZE,
                initrd_start + i * FRAME_SIZE,
                Flags::user_rodata(),
            )
            .map_err(LoadError::Unmappable)?;
    }

    // Register the space, then build init's TCB with its two capabilities: budget (slot 0), report
    // endpoint WRITE|GRANT (slot 1, so init may delegate a narrowed view to the child it builds).
    let aspace_name = readopt_user_aspace(space).expect("register init aspace");
    let report = crate::sched::create_endpoint();
    let build_region = crate::untyped::create(2048).expect("no building budget for init");

    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    // The delegable root budget (milestone 31): init hands narrowed budgets to its children, so the
    // root carries GRANT; rights only narrow downward from here.
    let s0 = crate::sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(build_region), None)
        .expect("insert budget");
    assert_eq!(s0, 0, "init's budget must land in slot 0");
    let s1 = crate::sched::tcb_insert_cap(
        tid,
        crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        ),
        None,
    )
    .expect("insert report");
    assert_eq!(s1, 1, "init's report endpoint must land in slot 1");
    crate::sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace_name).expect("configure");
    // init reads the archive length from its second argument (a1), as the worker reads its input.
    crate::sched::start_tcb(tid, [0, initrd_len, 0]).expect("start");

    // Bench diagnostics (2026-08-14, first-silicon session): the tour hung inside this demo on the
    // VisionFive 2 with nothing on the wire, because the recv below blocks silently. Narrate the
    // stages and, while the boot thread is parked in recv, have a watcher print the thread table a
    // few times so the serial log says what init and its child are DOING during the silence. Cheap,
    // honest, and QEMU boots print a few extra lines; remove or keep at merge, integrator's call.
    crate::println!("    init : measured, built, started; waiting for the child's word");
    crate::sched::spawn(|| {
        for i in 1..=5u32 {
            crate::arch::timer::spin_for(crate::arch::timer::frequency() * 2);
            // A finished tour needs no witness: boot 13 reached the final banner and the five
            // dumps that followed showed only a quiescent machine waiting for input, five times.
            // A stalled tour still gets all five.
            if crate::sched::boot_stage() >= 10 {
                break;
            }
            // svc is the machine-wide ecall count; tx is console::_print's byte count. Together
            // with the dump header's tour stage they are boot 11's discriminators (fifth bench
            // stop): a stage past the demo with tx grown past these dumps' own output proves the
            // tour's missing serial lines were emitted and lost downstream, where boots 7
            // through 9 left "did the tour advance" to inference over a frozen svc count.
            crate::println!(
                "    diag : +{}s, svc={}, tx={}",
                i * 2,
                {
                    use core::sync::atomic::Ordering;
                    crate::arch::exceptions::SVC_COUNT.load(Ordering::Relaxed)
                },
                crate::console::tx_bytes()
            );
            crate::sched::dump_threads();
        }
    });

    // The corruption canary, armed for exactly the window the bench stops happened in: parked
    // here waiting for the child. Every byte that changes in the thread table or the endpoint
    // registry prints with its address and before/after, so boot 11 can tell a legal delta (the
    // worker's TCB appearing, the UART demo's endpoints) from a stray write. Disarmed on return.
    crate::sched::canary_arm_registries();
    // The word the child SENDs home (init built the pipe; the child sent through it).
    let word = crate::sched::ipc_recv(report)[0];
    crate::sched::canary_disarm();
    Ok(word)
}

/// **Start the interrupt-driven UART driver as an unprivileged userspace process** (milestone 20).
///
/// The device-interrupt story's real form: a driver that owns the UART's interrupt by *capability*,
/// not by privilege. The kernel loads `driver` from the archive, builds its address space, maps the
/// NS16550's registers into it device-typed (so the driver reads the byte itself; the kernel is not
/// in the data path), and grants it exactly two capabilities: an `Irq` capability for the UART
/// interrupt (slot 0) and a report endpoint (slot 1). It routes the interrupt to the endpoint the
/// `Irq` cap waits on, starts the driver, and arms the source (PLIC), the receive interrupt (UART),
/// and supervisor external interrupts (`sie.SEIE`).
///
/// Returns the report endpoint. This does **not** block: the caller spawns a receiver so the boot
/// tour continues, and the driver's `WAIT`/read/report/`ACK` loop runs whenever a byte arrives. The
/// `ACK` is the point of the whole exercise: it crosses the `arch::irq` seam (the PLIC on RISC-V, the
/// GIC on aarch64) to re-arm the source, from an unprivileged process holding only a capability.
#[cfg(target_arch = "riscv64")]
pub fn riscv_uart_driver_demo(
    archive: &'static [u8],
    uart_irq: u32,
) -> Result<crate::sched::EpId, LoadError> {
    const DRIVER_UART_VA: u64 = 0x0070_0000; // must match user/src/driver.rs UART_VA
    const UART_PHYS: u64 = 0x1000_0000; // the NS16550 on QEMU virt

    let fs = nifefs::Fs::parse(archive).expect("initrd is not a nifefs archive");
    let driver_bytes = fs.read("driver").expect("archive has no 'driver' program");
    let elf = Elf::parse(driver_bytes).map_err(LoadError::NotLoadable)?;

    // The driver's address space: its segments, a stack, and the UART's registers device-typed.
    let content: u64 = elf
        .segments()
        .map(|seg| {
            let (s, e) = seg.page_range(FRAME_SIZE);
            (e - s) / FRAME_SIZE
        })
        .sum::<u64>()
        + 1
        + INIT_STACK_PAGES
        + 8;
    let mut space =
        AddressSpace::new(content).ok_or(LoadError::Unmappable(MapError::OutOfFrames))?;
    map_segments(&mut space, &elf)?;
    for k in 0..INIT_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .map_err(LoadError::Unmappable)?;
    }
    // The UART registers, device-typed and user-accessible: the driver reads RBR/LSR directly.
    space
        .map_physical(DRIVER_UART_VA, UART_PHYS, Flags::user_device())
        .map_err(LoadError::Unmappable)?;

    let aspace_name = readopt_user_aspace(space).expect("register driver aspace");

    // Route the UART interrupt to an endpoint; the Irq cap's WAIT blocks on it. The report endpoint
    // is where the driver SENDs each byte, and where the caller's receiver waits.
    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(uart_irq, irq_ep);
    let report = crate::sched::create_endpoint();

    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    // slot 0: the Irq capability (READ permits WAIT/ACK). slot 1: the report endpoint (WRITE).
    let s0 = crate::sched::tcb_insert_cap(
        tid,
        crate::cap::irq_cap_rights(uart_irq, crate::cap::Rights::READ),
        None,
    )
    .expect("insert irq cap");
    assert_eq!(s0, 0, "the Irq cap must land in slot 0");
    let s1 = crate::sched::tcb_insert_cap(
        tid,
        crate::cap::endpoint_cap(report, crate::cap::Rights::WRITE),
        None,
    )
    .expect("insert report");
    assert_eq!(s1, 1, "the report endpoint must land in slot 1");
    crate::sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace_name).expect("configure");
    crate::sched::start_tcb(tid, [0, 0, 0]).expect("start");

    // Arm the whole chain, now that the driver is running and routed: the source at the PLIC, the
    // receive interrupt at the UART, and supervisor external interrupts in `sie`.
    crate::drivers::plic::enable(uart_irq, crate::arch::irq::boot_s_context());
    crate::console::rx_enable();
    crate::arch::exceptions::enable_external();

    Ok(report)
}

/// **Boot the interactive shell system on RISC-V** (parity D). The riscv counterpart of aarch64's
/// `spawn_init` + `init_boot`: load `system_initializer` (the portable system builder) as the boot process, map
/// the whole initrd into it, and grant it a large untyped budget (slot 0), the NS16550's registers as
/// a device cap (slot 1), the UART receive interrupt as an `Irq` cap (slot 2), the wall clock page
/// read-only (slot 3, milestone 51's wiring), and the file service plus its shared page (slots 4 and
/// 5) when a RedoxFS disk is attached. From those, `system_initializer` builds the console server,
/// the input driver, and the shell out of its own budget and wires them together; the kernel touches
/// none of it. Unlike the other demos this
/// does not block: `system_initializer` and its children run on the scheduler while the boot thread parks.
#[cfg(target_arch = "riscv64")]
#[cfg_attr(not(feature = "shell"), allow(dead_code))] // the `shell` boot mode is the only caller
pub fn riscv_shell_boot(archive: &'static [u8], uart_irq: u32) -> Result<(), LoadError> {
    use crate::cap::Rights;
    const UART_PHYS: u64 = 0x1000_0000; // the NS16550 on QEMU virt

    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);

    let fs = nifefs::Fs::parse(archive).expect("initrd is not a nifefs archive");
    let init_bytes = fs
        .read("system_initializer")
        .expect("archive has no 'system_initializer' program");
    // Measured boot (milestone 22 phase B.1): `system_initializer` is riscv's boot program, so it is in the
    // trust root under its own name and checked here, before its address space is built.
    crate::trust::require("system_initializer", init_bytes);
    // And the table it measures the six boot components and every spawnable program against
    // (milestone 104). This is the boot path that genuinely uses it: `crates/system_initializer` is
    // the same code aarch64's init runs, so the two boards extend the chain by the same lines.
    crate::trust::require_program_measurements(&fs);
    let elf = Elf::parse(init_bytes).map_err(LoadError::NotLoadable)?;

    // system_initializer's address space: its segments, a deep stack (it runs an ELF loader that builds three
    // children), and the whole archive mapped read-only so it can load them by name.
    let content: u64 = elf
        .segments()
        .map(|seg| {
            let (s, e) = seg.page_range(FRAME_SIZE);
            (e - s) / FRAME_SIZE
        })
        .sum::<u64>()
        + 1
        + initrd_pages / 512
        + INIT_STACK_PAGES
        + 8;
    let mut space =
        AddressSpace::new(content).ok_or(LoadError::Unmappable(MapError::OutOfFrames))?;
    map_segments(&mut space, &elf)?;
    for k in 0..INIT_STACK_PAGES {
        space
            .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
            .map_err(LoadError::Unmappable)?;
    }
    for i in 0..initrd_pages {
        space
            .map_physical(
                INITRD_VA + i * FRAME_SIZE,
                initrd_start + i * FRAME_SIZE,
                Flags::user_rodata(),
            )
            .map_err(LoadError::Unmappable)?;
    }
    let aspace_name = readopt_user_aspace(space).expect("register system_initializer aspace");

    // Route the UART receive interrupt to an endpoint; the input driver's Irq cap will WAIT on it.
    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(uart_irq, irq_ep);
    let build_region =
        crate::untyped::create(2048).expect("no building budget for system_initializer");

    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    // slot 0: the delegable root budget (milestone 31), GRANT included so system_initializer can split off a
    // budget for the shell and hand it on; rights only narrow downward. slot 1: the NS16550
    // registers, WRITE|GRANT so system_initializer maps them into the console and input drivers. slot 2: the
    // UART Irq, READ|GRANT so it can delegate it to input.
    let s0 = crate::sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(build_region), None)
        .expect("insert budget");
    assert_eq!(s0, 0);
    let s1 = crate::sched::tcb_insert_cap(
        tid,
        crate::cap::device_frame_cap(UART_PHYS, Rights::WRITE.union(Rights::GRANT)),
        None,
    )
    .expect("insert uart device");
    assert_eq!(s1, 1);
    let s2 = crate::sched::tcb_insert_cap(
        tid,
        crate::cap::irq_cap_rights(uart_irq, Rights::READ.union(Rights::GRANT)),
        None,
    )
    .expect("insert uart irq");
    assert_eq!(s2, 2);
    // The clock page (slot 3), read-only, ahead of the filesystem pair so its number is the same on
    // every boot. `READ` is DECISIONS §43's split at this boundary: init can endow a reader and holds
    // nothing that could set the time. See [`boot_clock_page`].
    let s3 = crate::sched::tcb_insert_cap(
        tid,
        crate::cap::frame_cap(boot_clock_page(), Rights::READ.union(Rights::GRANT)),
        None,
    )
    .expect("insert the clock page");
    assert_eq!(s3, 3);
    // The file service (slot 4) and the page its clients share with it (slot 5), when this boot has
    // a filesystem (milestone 50). GRANT on both, because init's job with them is to delegate: it
    // narrows the endpoint into the shell and maps the frame into its address space. `a2` carries
    // the rights the endpoint holds, which is also how init is told there is one at all. `None` is
    // the ordinary case for a run with no RedoxFS disk attached.
    let fs_rights = match program("fs_server")
        .and_then(|fs_server| fs_service::root_directory(fs_service::blk_server_image(), fs_server))
    {
        Some((file_ep, file_shared)) => {
            let s4 = crate::sched::tcb_insert_cap(
                tid,
                crate::cap::endpoint_cap(file_ep, Rights::WRITE.union(Rights::GRANT)),
                None,
            )
            .expect("insert the file service");
            assert_eq!(s4, 4);
            let s5 = crate::sched::tcb_insert_cap(
                tid,
                crate::cap::frame_cap(file_shared, Rights::WRITE.union(Rights::GRANT)),
                None,
            )
            .expect("insert the shared file page");
            assert_eq!(s5, 5);
            fs_proto::dir::ALL
        }
        None => 0,
    };
    crate::sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace_name).expect("configure");
    crate::sched::start_tcb(tid, [0, initrd_len, fs_rights]).expect("start"); // a1 = archive length

    // Arm the interrupt chain so the input driver's keystrokes flow: the source at the PLIC and
    // supervisor external interrupts in `sie`. The input driver arms the NS16550's own RX interrupt
    // (its IER) when it starts, and re-arms the PLIC source through its Irq cap's ACK.
    crate::drivers::plic::enable(uart_irq, crate::arch::irq::boot_s_context());
    crate::arch::exceptions::enable_external();
    Ok(())
}

/// Bringing the console driver up in userspace, and wiring a client to it.
///
/// **This is the milestone-8 payload.** It creates the shared machinery (two endpoints and a
/// shared page), spawns the console *server* as a user process that owns the UART, and returns
/// what a client needs to reach it. The server binary and the client binary are the *same ELF*,
/// told apart by the argument in `x0`.
// The milestone tour is the only consumer, so this is dead in exactly the configurations that
// have no tour: a test build, and the three alternate boot modes. The allow sits on the module
// because the module is one wiring, not a bag of independent items.
#[cfg_attr(
    any(test, feature = "shell", feature = "bench", feature = "initboot"),
    allow(dead_code)
)]
pub mod console_service;

/// Bringing the virtio block driver up in userspace.
///
/// **Milestone 9's headline.** The kernel enumerates the bus (kernel/src/virtio.rs) to find the
/// block device, then hands a userspace driver everything it needs and nothing it does not: the
/// device's registers, a DMA page, an interrupt, and an endpoint to report what it read. The
/// kernel does not touch the device.
#[cfg_attr(not(test), allow(dead_code))] // the tour spawns it; the tests drive it
pub mod virtio_service;

/// **A service's memory, remembered so a finished test can hand it back.** The bookkeeping half of
/// DECISIONS §16 object revocation, applied to the services the test boot builds: without it a boot
/// that runs many service-shaped tests runs out of frames, and does so in whichever innocent test
/// happens to allocate next. See the module note and notes/frames.md.
#[cfg_attr(not(test), allow(dead_code))] // the tests are the callers; the tour never tears down
pub mod holding;

/// **The RedoxFS filesystem service** (milestone 32 phase 2): three confined processes and the
/// endpoints and shared pages that wire them, spawned by the test that proves the stack end to end.
///
/// ```text
///   disk ──virtio──► block server ──blk IPC──► FS server ──file IPC──► client ──► report to kernel
/// ```
///
/// The kernel builds the wiring and hands each process exactly its world (a `Spawn` literal each);
/// it never sees a filesystem operation, an opcode, or a byte of file data. The FS server owns
/// RedoxFS and its own heap; the block server owns the DMA confinement; the client holds only a
/// directory capability. This is the same shape as `virtio_service` and the console, one level up.
///
/// The service drives the **second** mmio block disk (the RedoxFS image); the first is the nifefs
/// disk the phase-1 driver tests use. `None` if there is no such disk attached to this run.
#[cfg_attr(not(test), allow(dead_code))] // spawned only by the phase-2 test
pub mod fs_service;

/// **The block-device roster and the disk surveyor** (milestone 57, notes/block-devices.md).
///
/// Two authorities that every other operating system hands out as one: a **read-only mapping**
/// listing what block devices exist, and a **block-service endpoint** for exactly one of them. A
/// program with the first can see the machine's disks and open none of them; a program with the
/// second was handed one disk and has no way to name a second.
///
/// The kernel's part is small and stops early: scan the buses, write the page, confine one device
/// under a block server, spawn. It never reads a partition table. Every byte of GPT judgement is in
/// `crates/gpt`, whose tests run on the host against tables `sgdisk` and macOS `diskutil` wrote.
///
/// Arch-neutral, like the clock and entropy wirings: one portable binary over one host-tested
/// contract, so **both ISAs run literally the same test** (DECISIONS §19).
#[cfg_attr(not(test), allow(dead_code))] // the tests are its only caller today
pub mod disk_service;

/// **Reading a real disk's partition table, and the difference between listing and holding**
/// (milestone 57).
///
/// The first test is the half of the milestone that is not optional: which blocks of a device are a
/// filesystem is written in the partition table and nowhere else, so this reads one off a virtio-blk
/// device. The table was written by `sgdisk`, in C++, by people who never heard of this project;
/// that provenance is what makes the parse worth asserting.
///
/// The second is the negative control the first would be weaker without. The roster is a read-only
/// mapping, so a program that knows exactly where it is still cannot add a device to it or turn an
/// entry into a handle. `lsblk` plus `parted` cannot make that claim.
#[cfg(test)]
mod disk_tests;

/// **Play an application printing to a display terminal**: put `text` in its output page and
/// `OP_WRITE` it.
///
/// Shared by both of the terminal's wirings (the whole scanout, and a compositor window) because the
/// terminal contract does not know which one it is in: an `OP_WRITE` is an `OP_WRITE`. Returns when
/// the reply arrives, which the contract says means the bytes are on the console's side, so a test
/// needs no polling and no sleep between writes.
#[cfg_attr(not(test), allow(dead_code))] // the milestone-29 tests are the callers
fn term_print(out: u64, ep: crate::sched::EpId, text: &[u8]) {
    assert!(
        text.len() <= FRAME_SIZE as usize,
        "an OP_WRITE past its output page",
    );
    let base = mmu::phys_to_virt(out);
    for (i, &b) in text.iter().enumerate() {
        // SAFETY: inside the output frame this kernel allocated and shares with the terminal.
        unsafe { core::ptr::write_volatile((base + i as u64) as *mut u8, b) };
    }
    // The bytes must be visible to the terminal before the request that names them.
    //
    // PAIR: no acquire fence, and none is needed. The terminal is blocked in `recv_cap` and the
    // `ipc_call` below is what wakes it, so the kernel's release of the `SCHED` lock and the
    // terminal's acquire of it are the pair (`spin::Mutex` locks `Acquire` and unlocks `Release`).
    // Redundant, kept: it is one `dmb` on a path that prints a line, and the contract does not
    // forbid a terminal that polls its page instead of blocking. See notes/memory-ordering.md.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let w0 = line_editor::proto::req(line_editor::proto::OP_WRITE, text.len() as u64);
    let r = crate::sched::ipc_call(ep, [w0, 0]);
    assert_eq!(
        r[0],
        text.len() as u64,
        "the terminal consumed {} of {} bytes",
        r[0],
        text.len(),
    );
}

/// **The display service** (milestone 29, the display ladder's rung one): a confined virtio-gpu
/// driver and a client that draws, wired by the kernel and then left alone.
///
/// ```text
///   virtio-gpu ──virtio (PCIe, behind the IOMMU)──► display driver ──display IPC──► painter
///        │                                              │                             │
///        └──── DMA: the whole region ───────────────────►│                             │
///                                    the surface (pages 1..) ─────── shared ──────────┘
/// ```
///
/// The kernel's part is the same as every other service here: build the wiring, hand each process a
/// `Spawn` literal, and know nothing about what they do. It never sees a virtio-gpu command, a
/// pixel, or a rectangle. What is new is the **size** of the DMA region, and that is the whole
/// memory story: a framebuffer does not fit in the single page the disk and NIC drivers get, so the
/// region is `1 + gfx_proto::SURFACE_FRAMES` **contiguous** frames, page 0 for the rings and the
/// control buffers and the rest for the surface. Registering the whole run as the driver's DMA region
/// is what keeps the framebuffer inside the grant: the shadow-ring validator bounds every descriptor
/// to it, and `iommu::confine` maps exactly it, so the device can reach the pixels and nothing else.
/// The block server already took two pages this way (milestone 32); this is the same move, wider.
///
/// The client maps only the surface frames. It never sees page 0, so it cannot touch a descriptor
/// ring, and it holds no `Virtio` capability, no interrupt, and no physical address. See
/// notes/framebuffer-contract.md.
#[cfg_attr(not(test), allow(dead_code))] // spawned only by the milestone-29 test
pub mod display_service;

/// **The compositor: one screen, several mutually distrusting clients** (milestone 33, the display
/// ladder's rung two).
///
/// ```text
///   display (or a kernel stand-in) ──gfx FLUSH(damage)──► compositor ◄──one doorbell──── window clients
///                                            the scanout, shared ──┘  │                 (a surface each)
///                                                                     └─► one input endpoint per focusable
/// ```
///
/// The kernel's part is what it always is: allocate the frames, mint the endpoints, hand each process
/// a `Spawn` literal, and know nothing about what they do. It never sees a pixel, a window, or a
/// damage rectangle. What is worth reading here is the **shape of the grants**, because the isolation
/// this rung exists to prove is a property of exactly that shape:
///
/// - every client's control page and surface are its own frames, mapped **at the same virtual
///   addresses** in every client. Two clients' surfaces are the same address in different address
///   spaces, so "my neighbour's surface" is not somewhere a client can reach by guessing;
/// - the clients' frames are allocated as **one contiguous run**, deliberately, so that the page just
///   past a client's grant really is its neighbour's memory. That makes the attack in
///   `a_client_holds_no_capability_for_its_neighbours_pixels_or_the_screen` a fair one: the attacker is
///   handed the exact address, the bytes it wants are physically adjacent, and the mapping is the only
///   thing in its way;
/// - the screen and the window list are mapped **read-only** and **only** into a client granted them.
///   That mapping is the screenshot capability and the enumeration capability; there is no verb for
///   either, and a client without the mapping has nothing to ask and nowhere to look.
#[cfg_attr(not(test), allow(dead_code))] // spawned only by the milestone-33 tests
pub mod compositor_service;

/// **The keyboard service** (milestone 29's input): a confined userspace virtio-input driver that
/// turns key events into the bytes a terminal understands, and publishes them where the compositor
/// reads them.
///
/// ```text
///   virtio-input ──virtio (PCIe, IOMMU)──► kbd ──the input ring──► whoever maps it
///                                           └──doorbell COMMIT──► "look at the surfaces"
/// ```
///
/// The grant shape is the whole security argument and it is worth reading beside
/// `compositor_service`: the driver gets the device, its interrupt, its DMA page, the doorbell, and
/// **the ring page**. It does not get any client's endpoint, so it cannot choose who receives what
/// it types; that is focus, and focus is the compositor's decision expressed as which of the input
/// capabilities *it* holds it uses (DECISIONS §33). And the ring is what makes typing possible at
/// all: the doorbell every client holds is content-free, so a client that rang it forever could not
/// produce a single character.
///
/// In the test below the **kernel** plays the compositor, which is the same substitution three of the
/// four rung-two tests make: it holds the doorbell and the ring, so the bytes a real keyboard
/// produced are a value it can read and compare rather than a picture it has to infer.
#[cfg_attr(not(test), allow(dead_code))] // spawned only by the milestone-29 test
pub mod keyboard_service;

/// **The clock service** (milestone 51 lane A, DECISIONS §43): the RTC's registers, the wall
/// clock's offset, and the propose endpoint, in one confined userspace process.
///
/// The kernel's whole part in wall-clock time is here and it is small: find the RTC in the device
/// tree (by `compatible`, `memory::rtc_region`), allocate one frame for the clock page, and hand
/// the service the registers, the page read/write, and an endpoint. It does not read the clock, does
/// not know what time it is, and has no notion of an offset. Everything after the spawn is
/// userspace agreeing with userspace over `clock_proto`.
///
/// Arch-neutral, like the display and compositor wiring: the component is one portable binary
/// carrying both RTC drivers, and the *machine* says which one it has, so **both ISAs run literally
/// the same test** (DECISIONS §19).
// The interactive boot calls `start` on both ISAs since milestone 51's wiring lane; the rest of the
// module (the propose helper, the kernel-side page reader) is still the tests' alone.
#[cfg_attr(not(test), allow(dead_code))]
pub mod clock_service;

/// **The clock page the interactive boot hands init**, and the one place both ISAs agree on what a
/// machine with no clock looks like (milestone 51's wiring; `spawn_init`, `riscv_shell_boot`).
///
/// The grant is **unconditional**, and that is the design rather than an oversight. A zeroed page
/// reads as `clock_proto::state::UNKNOWN` (`a_zeroed_page_reads_as_unknown`), so a boot with no
/// `clock` program in its initrd hands init a page that honestly says "the machine has no clock it
/// believes" instead of no page at all. That keeps the slot numbering the same on every boot, which
/// matters more than it sounds: init's cspace is read positionally, and a capability whose *slot*
/// depends on what the machine turned out to have is a wiring nobody can check by reading.
///
/// It is also the DECISIONS §43 split, delivered: init gets `READ` on a frame. Nothing on this path
/// can hand a child the writable mapping that would let it set the time, because init never had one.
fn boot_clock_page() -> u64 {
    match program("clock") {
        Some(image) => {
            let wiring = clock_service::start(image);
            // The service publishes the RTC reading and *then* announces, with a blocking send. It
            // does not need to be drained for the page to be right, but an undrained announcement
            // parks the service inside it forever, so it would never serve a proposal. One thread
            // whose whole life is that receive costs nothing and leaves the propose endpoint live.
            let report = wiring.report;
            let _ = crate::sched::spawn(move || {
                crate::sched::ipc_recv(report);
                crate::sched::exit();
            });
            wiring.page_phys
        }
        // No `clock` program packed: allocate the page anyway and leave it zeroed. This is the
        // honest unknown clock, and it is the same state `date`'s test allocates deliberately.
        None => {
            let phys = crate::memory::alloc()
                .expect("no frame for the clock page")
                .addr();
            // SAFETY: freshly allocated, reachable through the direct map, owned by nobody yet.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            };
            phys
        }
    }
}

/// **Wall-clock time** (milestone 51 lane A, DECISIONS §43).
///
/// Arch-neutral on purpose: one portable binary carrying both RTC drivers, one host-tested
/// contract, and the machine's own device tree choosing between them, so **both ISAs run literally
/// these tests** rather than two copies that can drift (DECISIONS §19, parity is a gate).
#[cfg(test)]
mod clock_tests;

/// **`date`** (milestone 51; DECISIONS §43, notes/date.md).
///
/// The command that makes the wall clock visible to a person, and the first thing in the tree that
/// exercises `crates/calendar` against a clock the machine actually read. Arch-neutral like the
/// service it reads from: one portable binary over one host-tested contract, so **both ISAs run
/// literally these tests** (DECISIONS §19).
///
/// What these prove that nothing else does: the printed text, parsed back, names the same instant
/// the kernel computes independently from the page; and **an unknown clock produces a sentence
/// rather than 1970 or a panic**, which DECISIONS §43 listed as proven by construction only. It is
/// proven in the guest now, on a board whose RTC works, because the page is the thing under test
/// and a frame nobody has published to is an honest unknown clock.
#[cfg(test)]
mod date_tests;

/// **The entropy service** (milestone 56, DECISIONS §44): a virtio-rng device, its DMA page, its
/// interrupt, and the request endpoint clients hold, in one confined userspace process.
///
/// The kernel's whole part in randomness is here and it is smaller than the clock's: find an RNG on
/// whichever bus the caller named, confine it to one DMA page, and hand the service the transport,
/// the interrupt, and two endpoints. **The kernel never reads the device and holds no entropy of
/// its own.** Everything after the spawn is userspace agreeing with userspace over
/// `entropy_proto`.
///
/// The authority split is the point, and it is one sentence: the service holds the device; a client
/// holds an endpoint that means *"you may obtain randomness"*. Those are different powers, and only
/// the second one is safe to hand around. A client cannot program the queue, cannot map the page
/// the device writes into, and cannot ask for anything the service did not ask on its behalf.
///
/// Arch-neutral: one portable binary, both transports, both ISAs (DECISIONS §19).
#[cfg_attr(not(test), allow(dead_code))] // the tests and std_service are its callers
pub mod entropy_service;

/// **Randomness that an adversary cannot predict** (milestone 56, DECISIONS §44).
///
/// Not arch-gated and not transport-gated: the same binary, the same contract, the same assertions,
/// over virtio-mmio and over PCIe on both ISAs, because a random source that works on one bus is
/// not a random source (§18, §19).
///
/// What these prove that nothing else would: that bytes from a *device* reach a userspace client
/// through a capability that names no device, that consecutive draws are not the same bytes (a
/// stuck source, a re-served buffer, or a driver reading a stale ring all present as repeats), and
/// that the count in a reply is honoured so a caller cannot be handed zeros it mistakes for entropy.
#[cfg(test)]
mod entropy_tests;

/// **The credential service, its provisioner, and its clients** (milestone 56, the credential half;
/// notes/credentials.md).
///
/// The kernel's part is the wiring, and here more than anywhere the wiring *is* the argument. Four
/// processes, and the difference between three of them is one field of a `Spawn` literal:
///
/// | process | slot 0 | what that means |
/// |---|---|---|
/// | `credentialer` | the provision endpoint (READ) **and** the verify endpoint (READ, slot 1) | holds the store |
/// | `credentialer_test_client` provisioner | the **provision** endpoint (WRITE) | may write the store, until the seal |
/// | `credentialer_test_client` client | the **verify** endpoint (WRITE) | may ask a question about the store |
/// | `credentialer_test_client` attacker | the **verify** endpoint (WRITE) | the identical endowment, used otherwise |
///
/// The kernel never sees a secret, holds no store, and computes no hash. It creates two endpoints,
/// two frames, and a budget, and hands each process a different subset. Everything after the spawn
/// is userspace agreeing with userspace over `cred_proto`.
///
/// **Two frames and not one**, which is the detail worth stating: the provisioner writes plaintext
/// secrets into its page, so a client sharing that frame would read them. The two pages are
/// separate physical frames and neither process is ever given the other's.
///
/// Arch-neutral: one portable binary each, both ISAs (DECISIONS §19). Argon2id is arithmetic on
/// `u64`s and neither the service nor its clients contain a line of assembly.
#[cfg_attr(not(test), allow(dead_code))] // the milestone-56 credential tests are its callers
pub mod credential_service;

/// **A secret you can check and cannot read** (milestone 56, the credential half).
///
/// Not arch-gated: the same binaries, the same contract, the same assertions on aarch64 and
/// riscv64, because a credential store that authenticates on one instruction set is not a
/// credential store (§19).
///
/// What these prove that nothing else would:
///
/// - that a **userspace** client with one endpoint and no store gets a correct yes/no over a real
///   Argon2id verification, with the salt drawn from a real virtio-rng;
/// - that the **identical endowment**, used by a program that wants to write the store instead of
///   reading it, cannot;
/// - that the frame a client shares with the service holds **nothing** after the answer, which is
///   the strongest form of "the reply carried no data" that a test can check.
#[cfg(test)]
mod credential_tests;

/// **The NTP client, and the test server that answers it** (milestone 51; DECISIONS §43, §44).
///
/// The kernel's part is the wiring, and the wiring *is* the argument. An NTP client here gets five
/// slots: a report endpoint, the socket contract's endpoint, an untyped budget, the clock service's
/// **propose** endpoint, and the entropy service's endpoint. What it does not get is a mapping of
/// the clock page, in either direction, which is the whole difference between this and a Unix
/// `ntpd` running as root.
///
/// The test server is a role of the same binary holding `READ` on the endpoint the client holds
/// `WRITE` on. Substituting the peer at a capability boundary is how a capability system tests a
/// client: the client's code does not change and cannot tell. See user/src/ntp.rs for what that
/// proves and what it leaves to milestone 30's socket-contract tests.
///
/// Arch-neutral: one portable binary, both ISAs (DECISIONS §19).
#[cfg_attr(not(test), allow(dead_code))] // the tests are its callers
pub mod ntp_service;

/// **An NTP client may propose a time and may not set one** (milestone 51).
///
/// The milestone's demonstrable claim, and the one Unix cannot make: `ntpd` runs as root and may set
/// the clock to anything. Here the network-facing component holds an endpoint the clock service is
/// free to refuse, and holds no mapping of the page the offset lives in. These tests take that
/// apart: the happy path lands as a **proposal**, a reply that fails validation moves nothing, a
/// proposal outside the policy's bounds is refused **by the service**, and a write aimed straight at
/// the clock page kills the process.
///
/// Not arch-gated: one portable binary, the same assertions on aarch64 and riscv64 (DECISIONS §19).
#[cfg(test)]
mod ntp_tests;

/// Milestone 11: hand a process an untyped budget and let it spend it.
#[cfg_attr(not(test), allow(dead_code))] // the tour and the userspace tests
pub mod untyped_service;

/// **The untyped-backed userspace heap** (milestone 27): spawn the `allocator_exerciser` workload, the
/// first program that links `extern crate alloc`, with an untyped budget (slot 0) and a report
/// endpoint (slot 1). The program wires `user_rt::heap` as its global allocator, churns
/// `Vec`/`String`/`BTreeMap` with frees in arbitrary order, asserts every intermediate result
/// itself (a wrong value faults), and reports a magic word plus how many bytes of heap it
/// committed. Portable: the same test runs the riscv64 ELF on riscv and the aarch64 ELF on
/// aarch64, out of each arch's own initrd.
#[cfg(test)]
pub mod alloc_service;

#[cfg(test)]
mod heap_tests;

/// **The compositor: one screen, several mutually distrusting clients** (milestone 33, rung two of
/// the display ladder).
///
/// Arch-neutral, like rung one and for the same reasons: two portable binaries in both archives, one
/// host-tested contract crate, and an isolation property that is the kernel's own (mappings and
/// capabilities), so **both ISAs run literally these tests**.
///
/// Three of the four tests do not need a GPU at all, and take a **kernel stand-in for the display**
/// instead. That is not a shortcut, it is two things at once: it keeps four device bring-ups down to
/// one, and it makes the flush rectangles *observable*, which is how "a one-window redraw does not
/// cost a whole screen" becomes an assertion instead of a claim. It is also the swappable-component
/// story falling out for free: the compositor cannot tell whether the endpoint it flushes to is a
/// virtio-gpu driver or the kernel.
///
/// **These tests run before `display_tests`** (`compositor_tests` sorts first), which matters for the
/// host-side scanout check: the composed screen goes up first and rung one's pattern last, and
/// `cargo xtask` looks for both in that order. See notes/compositor.md.
#[cfg(test)]
mod compositor_tests;

/// **The display: virtio-gpu, a confined driver, and a client that draws** (milestone 29, rung one
/// of the display ladder).
///
/// Arch-neutral on purpose, unlike most of the device tests here: the driver and the client are
/// portable binaries in both archives, the transport is the same PCIe seam on both boards, and the
/// contract is one host-tested crate, so **both ISAs run literally this test** rather than two
/// copies of it that can drift (DECISIONS §19: parity is a gate).
#[cfg(test)]
mod display_tests;

/// **Rust `std` on the native ABI** (milestone 27): spawn the `std_exerciser` demo, an ordinary Rust
/// program (no `no_std`, no attributes) built for the `*-unknown-nife` custom target with std's
/// PAL implemented directly over the capability ABI. It gets the same two grants as `allocator_exerciser`,
/// an untyped budget (slot 0, which the std `GlobalAlloc` draws the heap from) and an endpoint
/// (slot 1, which `println!` SENDs to). Its stdout is a fixed, deterministic transcript the test
/// reassembles from the endpoint and checks byte for byte. Portable: the aarch64 ELF runs on
/// aarch64 and the riscv64 ELF on riscv, out of each arch's own initrd.
///
/// **Since milestone 51 it is also granted a wall clock**: a clock service is started first, and
/// the program gets that service's page as a `Frame` capability with `READ` in slot 5 plus a
/// read-only mapping of it. That is the whole of a std program's wall-clock authority, and it is
/// what turns `SystemTime::now()` from "1970 plus uptime" into a real answer (DECISIONS §43).
#[cfg(test)]
pub mod std_service;

#[cfg(test)]
mod std_tests;

/// **Capability delegation: authority moves between processes at runtime.**
///
/// Every other capability in nife is minted by the kernel and handed to a process at spawn.
/// That made the kernel a central authority-granting oracle, which is the ambient-authority shape
/// §10 argued against, just relocated. A capability system's defining move is that a process can
/// pass authority it holds to another process, narrowing it on the way, and only if it was trusted
/// to (`GRANT`). This wires the smallest scenario that exercises all three: a *granter* delegates a
/// resource capability to a *receiver* over a channel, narrowed to `WRITE` (no `GRANT`); the
/// receiver uses it and then cannot pass it on. See user/src/hello.rs `granter()/receiver()`.
/// **Frame capabilities: shared memory a process holds, maps, and delegates.**
///
/// The payoff of delegation applied to memory. A *producer* retypes a page out of its own untyped
/// into a `Frame` capability, maps it, writes into it, and delegates a READ-only view to a
/// *consumer*, which maps the same physical page and reads what the producer wrote. The kernel
/// copies nothing and pre-arranges nothing: the two processes compose the sharing themselves, and
/// the read-only narrowing means the consumer can look but not write. See user/src/hello.rs
/// `frame_producer()/frame_consumer()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
#[cfg(test)]
pub mod frame_service;

// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
#[cfg(test)]
pub mod delegation_service;

/// **Milestone 19a: a process mints an endpoint from its own memory, at EL0.** The maker holds
/// an untyped budget and a channel; the peer holds the channel and a report line. Everything
/// else, the endpoint itself included, is created at runtime by the maker out of its own pages
/// and delegated. See user/src/hello.rs `ep_maker()/ep_user()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
#[cfg(test)]
pub mod retype_ep_service;

/// **Milestone 19b: a process builds an address space, at EL0.** One role: an untyped budget
/// and a report line; everything else it constructs. See user/src/hello.rs `aspace_builder()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
#[cfg(test)]
pub mod aspace_service;

/// **Milestone 12: Call/Reply, at EL0.** One request endpoint, a server that answers a caller it was
/// never wired to, and the one-shot reply capability proven across the boundary. See
/// user/src/hello.rs `call_server()/call_client()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
#[cfg(test)]
pub mod call_service;

/// **Milestone 13: revoke a frame, at EL0.** One process with an untyped budget retypes a frame,
/// maps it, revokes it, and reports whether the revoke deleted its own capability. See
/// user/src/hello.rs `revoke_demo()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
#[cfg(test)]
pub mod revoke_service;

/// **The in-kernel userspace suite, on both instruction sets** (milestone 19's user-test port).
///
/// It was aarch64-only for most of this project's life, and the module comment used to say the
/// reason was the tests: "every test drives a hand-written aarch64 program through `exec` and reads
/// aarch64 fault registers". That was true, and it was the wrong thing to fix. The tests were fine;
/// their *scaffolding* was aarch64. Three things moved and the tests came along unchanged:
///
/// 1. The hand-assembled programs became real ELFs the toolchain builds for both targets (the
///    `outlaw` binary and the `spinner` that already existed). See the note above `OUTLAW_ROUND_TRIP`.
/// 2. `ESR`/`FAR` became `arch::UserFault`, the same fact in words RISC-V can say, which is what
///    keeps "a PERMISSION fault at exactly this address" assertable rather than softened to "a fault
///    happened".
/// 3. `hello`, which carries the milestone 7-19 role catalogue, was found to build for RISC-V once
///    six syscalls it had hand-rolled in aarch64 `asm!` were routed through `user_rt`, which already
///    had portable versions of all six.
///
/// **What is still gated, and why, is written at each test rather than here**, because a blanket
/// module comment is how the old claim survived past the point of being true. Two kinds of gate
/// appear below: a property that has no RISC-V analogue at all (`el1_runs_on_sp_el1`), and a
/// property whose RISC-V twin lives in `riscv_virtio_tests` and would be duplicated rather than
/// gained. See notes/riscv-parity-scope.md.
#[cfg(test)]
mod tests;

/// **Forcible teardown: `DESTROY` tears a runaway down** (DECISIONS §16 amendment, §24's second-`^C`
/// tier). A child spinning at EL0, never yielding and never checking an endpoint, cannot be waited
/// out; its region's owner must be able to reclaim it anyway. This is the one cross-ISA test in this
/// file, because the mechanism it proves is pure portable scheduler logic: the only per-architecture
/// part is the single spin instruction (`b .` / `j .`), and the whole capability dance around it is
/// the same code both ISAs run. It is separate from the aarch64 module above precisely so it can run
/// on both, which the parity gate (DECISIONS §19) asks of every kernel capability.
#[cfg(test)]
mod force_kill_tests;

/// **An init that gives its authority away, and a supervision tree that outlives it** (milestone 22
/// phase B.2).
///
/// Cross-ISA, because every piece is portable: the whole tree is four ordinary user programs
/// (`root_supervisor`, `spawner`, `sub_server_supervisor`, `flaky`) built out of the capability verbs, and the kernel's only
/// part is the fault endpoint phase A already built.
///
/// The kernel spawns `root_supervisor` the way it spawns init: the archive mapped read-only, one untyped
/// budget, one report endpoint. `root_supervisor` then builds a construction sub-server and a supervisor, hands
/// each exactly what it needs, and **deletes its own budget**. From then on the tree runs without it:
/// the sub-server crashes, its supervisor hears about it, reaps it through the spawner, and asks for a
/// replacement, which runs and exits cleanly. init could not have done any of that, and that is what
/// these two tests prove.
#[cfg(test)]
mod authority_tests;

/// **The interactive boot's half of the same idea: a job's memory comes home** (milestone 22, the
/// increment that migrated the hand-validated boot path).
///
/// The tree above proves an init that can hand its construction authority away entirely. The
/// interactive init cannot: it stays the shell's spawn service, so it must keep *some* budget. What
/// it can do instead is keep a **bounded** one and make it renewable, which is what these two tests
/// are about. Every job the prompt spawns is built in a region split off that pool and born
/// supervised, and `job_undertaker` (one endpoint capability, no memory at all) collects the corpse
/// through `Endpoint::REAP`, which returns the region to **init's** pool under §13 region ownership.
///
/// The pair is a control and a claim, in that order: three jobs exhaust the pool when nothing
/// collects, and twelve go through the same pool when `job_undertaker` does. Neither is a timing
/// argument; the assertion in both is which budget the pages are in.
///
/// Cross-ISA, because every piece is portable: `job_undertaker` is an ordinary program in both archives
/// and the reap authorization reads two TCB fields.
#[cfg(test)]
mod job_undertaker_tests;

/// **A memory-unsafe C component, confined** (milestone 36, DECISIONS §31).
///
/// The thesis (§14) is a verified core that confines unverified workloads, and C is the most
/// unverified workload available: no bounds checks, no borrow checker, nothing between a bad index
/// and a store. So this is not a dilution of the claim, it is the sharpest available test of it. The
/// contrast is concrete rather than rhetorical: in a monolith, C filesystem or driver code with this
/// bug is a kernel memory corruption; here it is a page fault in an unprivileged process, and its
/// supervisor restarts it.
///
/// **What is under test is the seam, not the C.** `user/c/c_seam.c` is deliberately throwaway: 150
/// lines, one honest function and two one-line bugs. What the milestone de-risks is everything around
/// it, before a real foreign component (libghostty-vt, milestone 29's later rung) depends on it: a
/// bare-metal clang in the build for both ISAs, a Rust `user_rt` shell that holds every capability so
/// the C can hold none, and five libc symbols shimmed rather than a libc ported.
///
/// **The four claims, and how each is proven rather than assumed.** All four are asserted from
/// outside the faulting address space, by `c_confiner`, after the component is dead:
///
/// 1. *It faults*, rather than silently corrupting and continuing. Proven by the death message
///    existing at all, with `EVENT_FAULT` and a non-zero kernel-stamped tid.
/// 2. *The fault is the bug we planted.* The kernel's reported fault address equals the address the C
///    code computed, so the crash is not something unrelated on the way there, which would make the
///    rest of the assertions vacuous.
/// 3. *Nothing outside the grant changed.* Two witness pages, both position-derived patterns
///    checked byte by byte through the confiner's own mappings. `WITNESS_RO` is the **same physical
///    frame** the component holds read-only, so an unchanged page is not "the store landed
///    elsewhere"; the page was reachable and the store did not happen. `WITNESS_FAR` is a
///    **different frame at the same virtual address**, which is the statement that a virtual
///    address means nothing outside the address space that owns it.
/// 4. *The supervisor restarts it and the restart works.* Not "an instance ran": the replacement's
///    output is read out of the shared grant and checked against an independent Rust computation of
///    the same checksum, so a restart that produced a process which merely reported for duty fails.
///
/// The in-grant marker byte is the control for all of it. Each misbehaving C function stores inside
/// its grant first, and that store must be visible; a process whose stores never worked would satisfy
/// every witness check while proving nothing.
///
/// Both ISAs, because a fault that only manifests on one would be a finding, not a pass. The two
/// bugs take *different* fault paths on each (a permission fault on the read-only page, a translation
/// fault on the unmapped one), which is more of each architecture's fault machinery than any previous
/// test has exercised from userspace.
#[cfg(test)]
mod c_seam_tests;

/// **A running component replaced under a talking client** (milestone 23, DECISIONS §41).
///
/// The flagship the roadmap points at, and the thing to notice about it is what the kernel does not
/// contain. There is no component object, no swap syscall, no naming service, and no
/// lifecycle-aware anything: `swapper` is an unprivileged process with a budget, one device
/// capability and four endpoints, and the swap is the composition of mechanisms that already
/// existed for their own reasons. What milestone 23 needed the kernel to grow is exactly one thing:
/// `Frame::REVOKE` now answers on a `DeviceFrame`, with take-back semantics (§41).
///
/// **The claim is not that a swap completes. It is that a client does not notice.** So the shape is
/// the one milestones 29, 33 and 36 used: two witnesses in two address spaces, an attacker with
/// real authority, and a control that must fail.
///
/// 1. **The client's witness**, computed inside `chatty` from the replies it received. It holds one
///    capability to one endpoint for its whole life, calls sixty-four times in a plain loop, and
///    checks every answer against its own independent computation of the digest. It has no code
///    path for "the server went away" because there is no such event to have one for.
/// 2. **The operator's witness**, a shared page in `swapper`'s address space that each instance
///    stamps with its own version per request. Read after every writer is dead, it says that no
///    request went unserved (nothing was lost in the down window) and that the version never goes
///    backwards (**there were never two owners of the device at once**, which is the whole reason
///    step 2 revokes).
/// 3. **The control that must fail**: the outgoing instance is told to read one UART register
///    *after* the operator revoked it. It faults, and the kernel's fault message carries the
///    device's own virtual address. Before the revoke the same read succeeded, which is what makes
///    this a receipt rather than a coincidence.
/// 4. **The attacker**, `chatty` in its usurper role, endowed with exactly the honest client's
///    capabilities including a real working capability to the stable endpoint. It tries to park
///    itself in `RECV_CAP` and become the server. `NotPermitted`: its capability carries `WRITE`
///    and not `READ`, so endpoint-only naming does not mean "whoever holds the endpoint is the
///    server".
///
/// **The replacement is written in C** (`user/c/c_swappable.c`, over the seam DECISIONS §31 built),
/// and that is the strongest form of the claim: what held across the swap was the contract, not a
/// recompile of the same source.
///
/// The second test covers the latency ladder's opt-in rung, `broker`. Both ISAs, because a swap
/// that only worked on one would be a finding, not a pass.
#[cfg(test)]
mod live_swap_tests;

/// **Measured boot: the kernel refuses to enter an init it was not built for** (milestone 22 phase
/// B.1, DECISIONS §22).
///
/// Cross-ISA, because the check is portable: one hash implementation (`crates/measured_boot`), one trust
/// root generated into the kernel image by `build.rs`, called from the boot path on both
/// architectures (aarch64 `spawn_init`, riscv `riscv_initrd_demo` / `riscv_shell_boot`).
///
/// **What these two prove, and why the boot path itself cannot be tested directly.** A real refusal
/// halts the machine, so a test cannot take that branch and live. What *can* be proven, and is what
/// actually matters, is the decision: the same function the boot path consults says Ok for the bytes
/// in the initrd QEMU loaded (which proves the whole build composition end to end: userspace built,
/// archive packed, digest written, kernel compiled with it, and the digest in the running image
/// matches the archive in RAM), and says Err for bytes off by one bit. The boot path's only response
/// to Err is `arch::halt()`, which is three lines up from here in `trust::require` and is the sort of
/// thing a reader can check by looking.
#[cfg(test)]
mod measured_boot_tests;

/// **The fault endpoint: a supervisor watches a child die and reap it** (milestone 22, DECISIONS
/// §26). These are the cross-ISA tests, because the mechanism is portable: a supervised child that
/// faults (or exits) turns into a five-word message on its supervision endpoint, its corpse persists
/// until the supervisor reaps it with §16 revocation, and a fresh child runs in its place. The only
/// per-architecture parts are the two tiny code stubs (a null load that faults, and a `SEND` + exit),
/// and even those are the same shape both ISAs already use elsewhere in this file. The kernel is the
/// only sender on the fault endpoint, so the tid the supervisor reads is trustworthy without a badge.
#[cfg(test)]
mod supervision_tests;

/// **A supervisor may collect a corpse without being able to build one** (DECISIONS §32,
/// `endpoint::REAP`). Cross-ISA, because the authorization check is architecture-neutral: it reads
/// two fields of a TCB and compares two generational names, so a divergence here would mean
/// something is wrong under `arch/`, not in this feature.
///
/// **What shape these tests are, and why.** Every reap goes through the real syscall dispatcher
/// (`syscall::invoke`), from a thread whose capability space holds **endpoint capabilities and
/// nothing else**: that is what a supervisor's authority actually is, and calling `sched` directly
/// would prove the helper rather than the boundary. The *building* is done with kernel-internal
/// calls, which is deliberate: it keeps the builder's authority out of the supervisor's cspace, so
/// "structurally unable to build" is a fact about the table these tests audit rather than a promise.
///
/// The accounting proof is the one that makes §32 worth having. A test that only showed the corpse
/// gone would be satisfied by a reap that quietly handed the pages to the reaper. So the builder's
/// region is one the test still owns and can measure, and the assertion is that its watermark comes
/// back down and it can spend those pages again, while the supervisor's cspace does not grow.
#[cfg(test)]
mod reap_tests;

/// **A process listing is a capability, not a fact about the machine** (milestone 126,
/// `endpoint::SURVEY`, notes/process-view.md). Cross-ISA for the same reason `reap_tests` is: the
/// scope decision reads one field of a TCB and compares two generational names, so a divergence
/// here would mean something is wrong under `arch/` rather than in this feature.
///
/// **The shape, and why it is this shape.** Every survey goes through the real syscall dispatcher
/// (`syscall::invoke`), and the walk is driven by `ps::collect`, which is the loop `user/src/ps.rs`
/// really runs: a bug in the cursor protocol therefore cannot hide in the gap between the kernel's
/// half and the program's. The tests build real supervised children out of a real region, so the
/// domain under test is one the kernel built rather than one a helper described.
///
/// The negative control is the one that matters, and it keeps milestone 108's shape: a viewer run
/// against a domain it was not granted is **refused loudly** rather than shown an empty list, and
/// an empty domain answers rather than refusing. Both are asserted in the same test, because
/// neither claim means anything without the other.
///
/// `pgrep`'s filter is driven here too, and this is the only place in the tree that can be: the
/// selector arrives in a register, and the prompt cannot spell one (`crates/pgrep`'s `BUGS`). The
/// negative control gains a fourth answer with it, which is a selector that **matched nothing** in a
/// domain that really has members: distinct from an empty domain and from a refusal, where upstream
/// `pgrep` collapses all three into printing nothing.
#[cfg(test)]
mod survey_tests;

/// **Scheduled execution, where every entry is a grant** (milestone 129, notes/scheduled-execution.md).
///
/// One module for both ISAs, like `dir_capability_tests`: nothing in it is architecture-specific, so
/// the parity gate (DECISIONS §19) is met by literally the same test running twice.
///
/// The claim is Unix cron's inversion. A crontab line runs as a user and can do whatever that user
/// can do, and there is nothing to print and nothing to check; here an entry is a grant expression
/// checked at registration by the same `grant_plan::plan` the prompt uses, so what a scheduled child
/// will hold is printable before the first tick. The test reads that plan off the real program
/// running the real `user/timetable.conf`, then watches what fires.
///
/// The negative control is what makes it worth having: the shipped document contains entries a Unix
/// cron would simply have run (`date` wants a clock, `ps` wants a process view), and the timetable
/// holds neither, so both are refused **in writing, before anything fires** and neither ever runs.
#[cfg(test)]
mod timetable_tests;

/// **The directory capability, attacked** (milestone 47, notes/dir-capability.md).
///
/// One module for both ISAs rather than an aarch64 test with a riscv twin, which the FS tests above
/// have. Nothing here is architecture-specific: it wires three portable programs and asserts on a
/// bitmap, so the only difference between the legs is which binary carries the block-server role,
/// and that is one `cfg` in [`blk_server_image`] rather than a second copy of every assertion. The
/// parity gate (DECISIONS §19) is met by literally the same test running twice.
#[cfg(test)]
mod dir_capability_tests;

/// **The navigation builtins, and the property that two shells cannot name each other's files**
/// (milestone 47's commands; notes/shell-navigation.md).
///
/// One module for both ISAs, for [`dir_capability_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
///
/// What is wired is the **real shell binary**, in a role that reads a script instead of a keyboard,
/// holding a `fs_subtree_caretaker`'s narrowed endpoint where the interactive one holds a terminal.
/// So the builtins under test are the builtins at the prompt rather than a reimplementation of
/// them, and the thing being confined is a shell.
#[cfg(test)]
mod shell_navigation_tests;

/// **`rm` as a program, and a recursive removal bounded by the capability it was handed**
/// (milestone 47's `rm -r`; notes/rm.md).
///
/// One module for both ISAs, for [`dir_capability_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
///
/// What is wired is the **real `rm` binary** (`user/src/rm.rs`) behind a real
/// `fs_subtree_caretaker`, started the way the shell would start it: the name in a grant's two
/// argument words and the options in the spec word, in `grant_plan::rmopt`'s bit order, so the numbers
/// here come from the manifest the prompt checks against rather than from a second copy of an
/// ordering.
///
/// The thing being demonstrated is not that a loop can delete a tree. It is that **the walk stops
/// exactly where the capabilities stop**: the same command line against the same tree does the
/// whole job through one grant and cannot begin through a narrower one, and no branch in the
/// program decides which.
#[cfg(test)]
mod rm_program_tests;

/// **Globbing: the expansion you see is the grant** (milestone 47's globbing lane;
/// notes/glob-grant.md).
///
/// One module for both ISAs, for [`dir_capability_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
///
/// What is wired is the **real shell binary** (expanding one pattern two ways over a real
/// `READDIR`) and then the **real `rm` binary** behind a real `fs_nameset_caretaker`. The argument
/// the two halves make together is the one Unix cannot make: the names a command displays are
/// literally the authority it would transfer, and nothing else in the directory moves.
#[cfg(test)]
mod glob_grant_tests;

/// Parity C: the virtio-blk driver, its two attackers, and the DMA confinement, on RISC-V.
///
/// These are the riscv twins of the three disk tests in the aarch64 module above, separate
/// because that module leans on aarch64-only scaffolding (the hand-written 7a user programs and
/// the PL011-wired `hello` roles), while these need only the ELF loader and the initrd archive.
/// The driver is the SAME `virtio` module the aarch64 roles compile, packed as the dedicated
/// `blk` binary (user/src/blk.rs); the kernel-side wiring (`virtio_service`) is the same code,
/// unconditionally. What these prove that aarch64's runs do not: userspace device drivers with
/// DMA, and the kernel's DMA confinement, on the second ISA.
#[cfg(all(test, target_arch = "riscv64"))]
mod riscv_virtio_tests;

/// **The operators, end to end: `|` is two processes and an endpoint** (milestone 50,
/// notes/pipes.md).
///
/// One module for both ISAs, for [`shell_navigation_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
///
/// What is wired is the **real shell binary**, in a role that reads a script instead of a keyboard,
/// with the interactive endowment: a terminal, a spawn channel, a result channel, and a budget. The
/// kernel plays the two parties on the other ends.
///
/// - **The terminal.** The test itself serves `line_editor::proto::OP_WRITE` and collects every byte
///   the shell prints. So the assertion is made against *what a person would see*, which is the
///   strongest form this can take: a pipeline that ran but printed the wrong thing fails here.
/// - **init.** A second thread serves `grant_plan::spawnproto`, receiving the delegated sink and source
///   capabilities and building each stage with them. It is deliberately the same protocol
///   `user/src/system_initializer.rs` serves, because the shell cannot tell the difference and neither should
///   this test; what it is not is the same *code*, and that gap is named in notes/pipes.md's BUGS.
#[cfg(test)]
pub mod pipeline_service;

/// **`>`, `<` and `|` at a real prompt** (milestone 50, notes/pipes.md).
///
/// The claim under test is one sentence: **a program holds an endpoint for its output and cannot
/// tell what is on the other end.** So the assertions are all of the form "the same binary, two
/// destinations, the same bytes", never "the pipeline printed something".
#[cfg(test)]
mod pipeline_tests;

/// **`>` and `<` at a prompt that holds a filesystem** (milestone 50, notes/pipes.md).
///
/// [`pipeline_tests`]'s shell with one more capability: a directory at slot 4, narrowed by a
/// `fs_subtree_caretaker` to one subtree of the real RedoxFS image. Everything else is identical,
/// which is the point of running both. The refusal in
/// `pipeline_tests::a_redirection_a_shell_cannot_back_is_refused_rather_than_dropped` and the file
/// written here are the same binary, and the only difference between them is one cspace slot.
///
/// The assertions are all of the "same producer, two destinations, the same bytes" shape, because
/// that is the only shape that can distinguish a redirection that worked from one that wrote
/// something plausible: a `>` that dropped every second byte would still produce a file, and a `wc`
/// that agreed with it would still print three numbers.
///
/// One module for both ISAs, for [`shell_navigation_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
#[cfg(test)]
mod redirection_tests;

/// **`time <command>` at a real prompt** (milestone 86, notes/time-command.md).
///
/// [`pipeline_tests`]'s shell with at most one more capability: a read-only clock page. The claim
/// under test is that **the timed command needs no authority to be timed**, so the timing is the
/// shell's own reading and the child is spawned with exactly the endowment its command line names.
///
/// The three clock states are three cspaces rather than three branches, which is the shape
/// [`redirection_tests`] uses for the directory: a published page, a page nobody published to, and
/// no capability at all. Two of those refusals are `date`'s sentences one milestone later, and the
/// only reason they are reachable is that the wiring changed.
///
/// One module for both ISAs, for [`shell_navigation_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
#[cfg(test)]
mod time_tests;

/// **Quoting, sequencing and `$?` at a real prompt** (milestone 67, notes/swish-language.md).
///
/// The **same run** of the same script [`redirection_tests`] asserts about, whose tail milestone 67
/// added: one shell, once. A seventh scripted shell would have been a seventh live process whose
/// frames nothing reclaims, and wiring one put [`time_tests`] over the frame pool intermittently
/// (`refused to load a user program: Unmappable(OutOfFrames)`). The wiring these lines need is
/// [`redirection_tests`]'s exactly, so a second copy bought nothing but the failure.
///
/// It is still its own module, because what it claims is its own: the redirection tests are about
/// where bytes go, and these are about what a word *is* and what a status means.
///
/// The assertions are pairs, which is [`redirection_tests`]'s shape and for the same reason. `echo
/// "*.txt"` against `echo *.txt` is one line quoted and one not; `worker 3 && echo yes` against
/// `worker && echo yes` is one connector against a refused left-hand side. A single line proving
/// "it printed something" would pass on a shell that ignored quoting entirely.
///
/// One module for both ISAs, for [`shell_navigation_tests`]'s reason: nothing here is
/// architecture-specific, so the parity gate (DECISIONS §19) is met by the same test running twice.
#[cfg(test)]
mod language_tests;

/// **The sink contract, and the one behaviour it changed** (milestone 50, notes/sink-protocol.md).
///
/// Two claims, one per test, and they need each other. The first is that a program cannot tell what
/// its output slot holds; the second is that when what it held is destroyed, the program finds out.
/// Without the second, "indifferent" would mean "unable to notice anything", which is a much
/// cheaper property and the wrong one.
///
/// Both run on both ISAs (§19), because the claim is about a contract and not about an instruction
/// set.
#[cfg(test)]
mod sink_tests;

/// **No test may leak a runnable thread** (the regression proxy for the test-thread starvation that
/// made the RedoxFS mount overrun the hang watchdog under the net boot). A one-shot driver that
/// spins forever instead of exiting stays `Ready`/`Running` for the rest of the boot; enough of them
/// crammed onto core 0 (the scheduler places every spawn and wake on the current core, DECISIONS
/// "Open design ideas": the SMP placement gap) starve a later heavy test past the 60 s watchdog.
///
/// It quiesces first (yielding lets a just-finished thread be reaped by the next context switch),
/// then asserts nothing but the idle threads and this probe is still runnable. A leak fails here with
/// the offending thread in the dump, on the test that leaked's own turf, rather than as a mysterious
/// watchdog trip three tests later.
///
/// **The name is what makes this run last, not its position in the file**, and getting that wrong is
/// how the module spent many milestones never policing the one place that needed it. Tests run in
/// link order, which is alphabetical by module path, so being the last thing in the file bought
/// nothing: as `no_leaked_threads` it sorted before `tests`, and `kernel::user::tests` is precisely
/// the module whose whole subject is user threads. Measured on 2026-08-02, the probe ran 158 test
/// lines before the last test it was supposed to police.
///
/// So it is named to sort after `tests`, and the tree's own word for it (`notes/riscv-parity-scope.md`
/// calls this the leak police) is the name.
///
/// # BUGS
///
/// The ordering is still only alphabetical. A future `kernel::user` module sorting after
/// `thread_leak_police` would run after the probe and could leak unpoliced, silently, exactly as
/// `tests` did. Nothing enforces this; there is no "run me last" attribute in
/// `custom_test_frameworks`. If that happens, the symptom will again be a starvation watchdog
/// somewhere unrelated rather than a failure here.
#[cfg(test)]
mod thread_leak_police;
