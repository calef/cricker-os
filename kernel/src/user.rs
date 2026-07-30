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

use crate::arch::exceptions::{TrapFrame, enter_user};
use crate::arch::mmu::{self, phys_to_ptr};
use crate::arch::sync_icache;
use crate::memory;
use elf::Elf;
use frames::{FRAME_SIZE, Frame};
use paging::{Flags, Half, MapError, Mapper};

/// Where a user program's code goes. Low half, so the hardware walks `TTBR0`.
// Used by `exec` and the tour; the shell, initboot, and bench boots run neither.
#[cfg_attr(
    any(feature = "shell", feature = "bench", feature = "initboot"),
    allow(dead_code)
)]
pub const USER_CODE_VA: u64 = 0x0000_0000_0040_0000;

/// Where its stack goes. One page, and `sp` starts at the top of it: stacks grow down.
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
    /// **The untyped region every page of this address space comes from** (milestone 14 phase
    /// B.4): the root table, the intermediate tables, and every owned leaf are retyped out of
    /// one region carved at creation. The region *is* the record of what this address space
    /// owns, which is why there is no frame list: teardown is `untyped::destroy`, one call,
    /// made safe by §13 revocation. The region has no capability minted for it, so userspace
    /// can never retype or delegate from it; it is kernel bookkeeping with a budget.
    region: u64,
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
            region,
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
        let frame = crate::untyped::retype_page(self.region).ok_or(MapError::OutOfFrames)?;
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
        let region = self.region;

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
/// MAX_SPACES (160) leaves room for all of them beside the exec-built spaces.
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
        region,
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
        // there. (`deactivate_user` flushes the TLB, which is the other half of the same
        // problem: without it the stale translations survive the table.)
        if mmu::current_user_root() == self.root.addr() {
            mmu::deactivate_user();
        }

        // One call: revoke anything delegated out of this region (nothing can be: the region
        // has no capability, so userspace could never retype from it), then return the whole
        // run, root and tables and leaves alike, to the allocator. This is the
        // "reclaim-on-process-death" wiring §13 deferred; the frame list it replaced is gone.
        crate::untyped::destroy(self.region);

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
/// Split out from [`exec_elf`] on purpose: this is the part that can fail, so it is the part a
/// test can call without dying.
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
/// crickerfs image carrying init plus the programs init loads. The milestone tour and the
/// kernel-side service demos still run a role of the one `hello` binary, so they ask for `"init"`;
/// `spawn_init` and `boot_via_init` instead take the whole archive, because init parses the rest
/// itself. Returns `None` if there is no initrd, it will not parse, or it holds no such program.
// Used by the milestone tour, the kernel-wired virtio/console/shell demos, and the tests that load
// a user program; dead only in the bench boot, which runs no user programs.
#[cfg_attr(feature = "bench", allow(dead_code))]
pub fn program(name: &str) -> Option<&'static [u8]> {
    crickerfs::Fs::parse(initrd()?).ok()?.read(name)
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

/// Load the initrd program and become it, with nothing but a fresh stack. Never returns.
#[allow(dead_code)] // the bare-client path, exercised by tests
///
/// The bare case: no capabilities, no extra mappings, no argument. It can run its own code and
/// touch its own memory, and it can name nothing else in the system.
pub fn exec_elf(image: &[u8]) -> ! {
    run(
        image,
        Spawn {
            arg0: 0,
            arg1: 0,
            arg2: 0,
            grants: &[],
            maps: &[],
        },
    )
}

/// Where the kernel maps the initrd read-only into init's address space (milestone 19d): init
/// reads the ELF to parse it here. High enough not to collide with init's own segments (0x40_0000)
/// or its stack (0x50_0000).
#[cfg_attr(not(test), allow(dead_code))] // becomes the boot path at 19d.2; test-driven until then
pub const INITRD_VA: u64 = 0x2000_0000;

/// **Spawn the init task** (milestone 19d): load `image` as an ordinary user process, but also
/// map the whole initrd read-only at [`INITRD_VA`] so init can parse it, and hand init a building
/// budget (an untyped, slot 0) plus `report` (slot 1, `WRITE|GRANT` so init can endow a child).
/// init enters with `x0` = `role` and `x1` = the initrd length. This is the one program the kernel
/// still loads; init loads the rest (design/init-and-granular-spawn.md).
/// The software-generated interrupt the kernel routes to init for the IRQ-delegation test
/// (19d.2b): SGI 3, distinct from the scheduler's RESCHED (0) and the older endpoint SGIs (1, 2).
#[cfg_attr(not(test), allow(dead_code))]
pub const INIT_TEST_SGI: u32 = 3;

/// The PL011 receive interrupt on QEMU `virt`: SPI 1 = INTID 33. init routes and delegates it so
/// the input driver it builds (19d.2c) can wait on keystrokes.
#[cfg_attr(not(test), allow(dead_code))]
pub const UART_RX_INTID: u32 = 33;

/// Init's stack, in pages (19d.2c): init loads whole ELFs with deep call chains, so its stack is
/// larger than an ordinary process's one page. 8 pages (32 KiB) is generous.
#[cfg_attr(not(test), allow(dead_code))]
const INIT_STACK_PAGES: u64 = 8;

#[cfg_attr(not(test), allow(dead_code))] // becomes the boot path at 19d.2; test-driven until then
pub fn spawn_init(image: &'static [u8], role: u64, report: crate::sched::EpId) {
    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd to hand init");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);

    // Route the test interrupt (19d.2b) BEFORE spawning init: the test raises the SGI as soon as
    // this returns, and an interrupt that fires before it is routed is dropped ("unexpected
    // interrupt"), not queued. Setting up the route here means the fire is counted on the routed
    // endpoint even though the init-built child is not yet waiting; the child's WAIT drains it.
    crate::sched::bind_irq(INIT_TEST_SGI, crate::sched::create_endpoint());
    crate::arch::irq::enable(INIT_TEST_SGI);
    // And the UART receive interrupt (19d.2c): the input driver init builds waits on it. Route and
    // enable it here, so init can delegate the Irq cap to that driver.
    crate::sched::bind_irq(UART_RX_INTID, crate::sched::create_endpoint());
    crate::arch::irq::enable(UART_RX_INTID);

    // The initrd is a crickerfs archive (milestone 19f), not a bare ELF: it carries init plus the
    // programs init will load. The kernel reads only the one entry it must, "init". This is the same
    // "honest residue" as before (something has to load the first program), now naming that program
    // through a fixed archive index instead of assuming it sits at offset 0. Every other program is
    // init's to parse. See notes/init-and-loading.md.
    //
    // Read and MEASURE it here, on the boot path, before anything is spawned (milestone 22 phase
    // B.1): the check has to be the thing that decides whether a thread is created at all, not
    // something the new thread does to itself. `trust::require` halts on a mismatch, so past this
    // line the bytes are the ones this kernel image was built against.
    let init_bytes = match crickerfs::Fs::parse(image) {
        Ok(fs) => match fs.read("init") {
            Some(bytes) => bytes,
            None => {
                crate::println!("  boot archive has no 'init' program");
                crate::arch::halt();
            }
        },
        Err(e) => {
            crate::println!("  boot archive is not a crickerfs image: {e:?}");
            crate::arch::halt();
        }
    };
    crate::trust::require("init", init_bytes);

    crate::sched::spawn(move || {
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

        // init's building budget: a large untyped it retypes the child's aspace, frames, and TCB
        // from. Sized for a full copy of the initrd program plus its tables and init's scratch.
        let build_region = crate::untyped::create(2048).expect("no building budget for init");

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
            0x0900_0000,
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
        // The UART receive interrupt (slot 4), for the input driver init builds (19d.2c).
        crate::sched::grant(crate::cap::irq_cap_rights(
            UART_RX_INTID,
            crate::cap::Rights::READ.union(crate::cap::Rights::GRANT),
        ))
        .expect("grant uart rx irq");

        enter_frame(elf.entry(), USER_STACK_TOP, role, initrd_len, 0)
    })
    .expect("could not spawn init");
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
    const INIT_BOOT_ROLE: u64 = 27;
    let report = crate::sched::create_endpoint();
    spawn_init(image, INIT_BOOT_ROLE, report);
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

/// Load a program, and become it. Never returns.
///
/// # Safety
/// `program` must be position-independent aarch64 machine code that begins at its first byte.
// The hand-written demos live in the tour; the shell, initboot, and bench boots skip it.
#[cfg_attr(
    any(feature = "shell", feature = "bench", feature = "initboot"),
    allow(dead_code)
)]
pub unsafe fn exec(program: &[u8]) -> ! {
    assert!(
        program.len() as u64 <= FRAME_SIZE,
        "the 7a loader is one page. An ELF loader is 7c."
    );

    let mut space = AddressSpace::new(2).expect("no memory for a user address space");

    let code = space
        .map_new(USER_CODE_VA, Flags::user_code())
        .expect("could not map the user's code");
    code[..program.len()].copy_from_slice(program);

    space
        .map_new(USER_STACK_VA, Flags::user_data())
        .expect("could not map the user's stack");

    // The code page is `user_code()`: readable and executable by EL0, and **PXN**, so the
    // kernel cannot execute it even by accident. A bug that jumped EL1 into a user page would
    // otherwise run user-controlled instructions at EL1, which is a total compromise. That is
    // one bit in a page descriptor, set by a decision made at milestone 4.
    //
    // The instructions we just wrote went out through the DATA path. The instruction fetcher
    // has its own cache and has never heard of them. On aarch64 the I-cache is not coherent
    // with the D-cache, so without this the CPU can fetch whatever was in that frame *before*
    // we wrote the program into it.
    sync_icache(code.as_ptr() as u64, program.len());

    crate::sched::adopt_address_space(space);

    enter_at(USER_CODE_VA, 0, 0, 0)
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

    // Where the trap frame goes. aarch64 puts it at the very top of the kernel stack: its entry
    // paths are deep enough that this function's own frame is already well below it. RISC-V's TCB
    // entry path (trampoline -> user_thread_entry -> enter_frame) is shallow, so a frame at the top
    // would OVERLAP and corrupt this function's stack (the `frame` pointer itself) as `frame.write`
    // runs, sending the sret to a garbage sepc. Put it just below the live `sp` instead; `sscratch`
    // is armed to `frame + size`, so every re-entry rebuilds it at the same spot. See
    // notes/riscv-port.md.
    #[cfg(target_arch = "aarch64")]
    let slot = top - size_of::<TrapFrame>() as u64;
    #[cfg(target_arch = "riscv64")]
    let slot = (crate::arch::current_sp().min(top) - size_of::<TrapFrame>() as u64) & !15;
    let frame = slot as *mut TrapFrame;

    // And prove it, rather than trusting the reasoning above. This is one check, once per
    // exec, against a bug whose symptom is a nested fault storm that eats the kernel image.
    assert!(
        mmu::translate(frame as u64).is_some_and(|(_, f)| f.is_writable()),
        "the user's TrapFrame at {frame:p} is not in writable memory",
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

// --- the programs ---
//
// Hand-written aarch64, assembled into `.rodata` and copied into a user page at load time.
// There is no ELF loader yet (that is 7c) and no filesystem to load from (that is milestone 9),
// so the "binary" rides along inside the kernel image. Honest scaffolding, and it goes away.
//
// aarch64-only: these are literal aarch64 machine code, and using `global_asm!` here is a standing
// exception to rule 1 (it predates the RISC-V port). They are exercised only by aarch64 EL0 tests
// and the boot tour, both of which are themselves aarch64-gated. RISC-V reaches EL0 through the ELF
// loader, not these. See notes/riscv-port.md, leak #3.

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
.section .rodata.user_programs, "a"
.balign 4

// Go to EL0, come back, go again. Proves the round trip, not just the departure.
.global USER_HELLO_START
USER_HELLO_START:
    mov     x8,  #1             // SYS_YIELD. Until 7d a bare `svc` meant nothing; now the
    svc     #0                  // syscall number is in x8, and 0 would mean SYS_EXIT.
    mov     x8,  #1
    svc     #0                  // if we reach here, `eret` PUT US BACK at EL0
1:  b       1b                  // and now spin, so the timer can preempt us
.global USER_HELLO_END
USER_HELLO_END:

// A hostile program. It yields nothing, calls nothing, and asks for nothing.
//
// This is DECISIONS §5's arbitrary ELF binary, in the flesh: "it has its own stack, it never
// yields, and it will loop forever because we will write a bug." The ONLY thing in the universe
// that can take the CPU back is a timer interrupt landing between these two instructions.
.global USER_SPIN_START
USER_SPIN_START:
1:  add     x0,  x0,  #1
    b       1b
.global USER_SPIN_END
USER_SPIN_END:

// An outlaw. It reaches for a KERNEL address.
//
// 0xffff_0000_4008_0000 is in the direct map, and it IS mapped, and it IS readable. Just not by
// EL0: `Flags::kernel_data()` sets AP such that EL1 may read and write and EL0 may do neither.
//
// So this is not a translation fault (there is a translation), it is a PERMISSION fault, and
// that distinction is the entire privilege boundary. The hardware picks TTBR1 from bits 63:48,
// walks the kernel's own tables, finds the page, reads the AP bits, and says no.
.global USER_OUTLAW_START
USER_OUTLAW_START:
    movz    x0,  #0x4008, lsl #16
    movk    x0,  #0xffff, lsl #48       // x0 = 0xffff_0000_4008_0000
    ldr     x1,  [x0]                   // <- data abort, EC 0x24, from a lower EL
1:  b       1b                          // never reached
.global USER_OUTLAW_END
USER_OUTLAW_END:
"#
);

#[cfg(target_arch = "aarch64")]
macro_rules! user_program {
    ($name:ident, $start:ident, $end:ident) => {
        /// `allow(dead_code)` because 7c handed the demo over to the real ELF from the initrd,
        /// and these hand-written programs are now exercised only by the tests. They stay
        /// because they test things the real binary cannot: `outlaw` deliberately commits a
        /// privilege violation, and `spin` is a program with no `.data`, no stack use, and
        /// nothing but a loop, which is the purest form of DECISIONS §5's hostile binary.
        #[allow(dead_code)]
        pub fn $name() -> &'static [u8] {
            unsafe extern "C" {
                static $start: u8;
                static $end: u8;
            }
            let start = (&raw const $start) as usize;
            let end = (&raw const $end) as usize;

            // SAFETY: both symbols are in .rodata, in this image, and the assembler emitted
            // them in this order.
            unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
        }
    };
}

#[cfg(target_arch = "aarch64")]
user_program!(hello, USER_HELLO_START, USER_HELLO_END);
#[cfg(target_arch = "aarch64")]
user_program!(spin, USER_SPIN_START, USER_SPIN_END);
#[cfg(target_arch = "aarch64")]
user_program!(outlaw, USER_OUTLAW_START, USER_OUTLAW_END);

// The RISC-V hand-written demo program (the aarch64 ones above are aarch64 machine code). Same shape
// as USER_HELLO: yield, come back, yield again (proving the round trip through U-mode), then exit.
// Syscall ABI: number in a7, `ecall`. See DECISIONS §10/§17.
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
.section .rodata.user_programs, "a"
.balign 4
.global USER_HELLO_START
USER_HELLO_START:
    li      a7, 1           // SYS_YIELD
    ecall
    li      a7, 1           // SYS_YIELD again: if we get here, sret PUT US BACK at U-mode
    ecall
    li      a7, 0           // SYS_EXIT
    ecall
1:  j       1b              // never reached
.global USER_HELLO_END
USER_HELLO_END:
"#
);

/// The RISC-V demo program's bytes (the `hello` counterpart on riscv64; the aarch64 build gets its
/// own `hello` from the `user_program!` macro above).
#[cfg(target_arch = "riscv64")]
pub fn hello() -> &'static [u8] {
    unsafe extern "C" {
        static USER_HELLO_START: u8;
        static USER_HELLO_END: u8;
    }
    let start = (&raw const USER_HELLO_START) as usize;
    let end = (&raw const USER_HELLO_END) as usize;
    // SAFETY: both symbols are in .rodata, in this image, emitted in this order.
    unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
}

/// The word the RISC-V reporter program SENDs home (matches the program below).
#[cfg(target_arch = "riscv64")]
pub const RISCV_REPORT_WORD: u64 = 0xC4;

// A RISC-V reporter program: invoke the endpoint capability granted in slot 0 to SEND one word, then
// exit. The syscall ABI (DECISIONS §17): a7 = number, a0.. = args; SYS_INVOKE takes (slot, method,
// w0, w1, w2) in a0..a4. This exercises the capability boundary from U-mode, not just yield/exit.
#[cfg(target_arch = "riscv64")]
core::arch::global_asm!(
    r#"
.section .rodata.user_programs, "a"
.balign 4
.global USER_REPORTER_START
USER_REPORTER_START:
    li      a0, 0           // slot 0: the granted report capability
    li      a1, 0           // endpoint::SEND
    li      a2, 0xC4        // the word to send (RISCV_REPORT_WORD)
    li      a3, 0
    li      a4, 0
    li      a7, 2           // SYS_INVOKE
    ecall
    li      a7, 0           // SYS_EXIT
    ecall
1:  j       1b              // never reached
.global USER_REPORTER_END
USER_REPORTER_END:
"#
);

/// Build a user process from parts, grant it WRITE on a fresh endpoint in slot 0, run the RISC-V
/// reporter program, and receive the word it SENDs. Returns the word (`RISCV_REPORT_WORD` on
/// success). This is the RISC-V counterpart of the aarch64 build-start-run-a-child test: it proves
/// the capability invocation path (`SYS_INVOKE` -> endpoint SEND) works from U-mode.
#[cfg(target_arch = "riscv64")]
pub fn riscv_capability_demo() -> u64 {
    unsafe extern "C" {
        static USER_REPORTER_START: u8;
        static USER_REPORTER_END: u8;
    }
    let code = {
        let start = (&raw const USER_REPORTER_START) as usize;
        let end = (&raw const USER_REPORTER_END) as usize;
        // SAFETY: both symbols are in .rodata, in this image, emitted in this order.
        unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
    };

    // The child's address space and the frames for its code and stack.
    let as_region = crate::untyped::create(8).expect("no aspace region");
    let aspace = user_aspace_create(as_region).expect("no aspace");
    let frames_region = crate::untyped::create(2).expect("no frame region");

    let code_phys = crate::untyped::retype_page(frames_region).expect("no code frame");
    // SAFETY: a fresh frame we own, direct-mapped; copy the program in and make it fetchable.
    unsafe {
        core::ptr::copy_nonoverlapping(
            code.as_ptr(),
            mmu::phys_to_virt(code_phys) as *mut u8,
            code.len(),
        );
    }
    crate::arch::sync_icache(mmu::phys_to_virt(code_phys), code.len());
    user_aspace_map(aspace, USER_CODE_VA, code_phys, Flags::user_code()).expect("map code");

    let stack_phys = crate::untyped::retype_page(frames_region).expect("no stack frame");
    user_aspace_map(aspace, USER_STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

    // The child's one authority: WRITE on a report endpoint (it may SEND, not receive).
    let report = crate::sched::create_endpoint();
    let report_cap = crate::cap::endpoint_cap(report, crate::cap::Rights::WRITE);

    // Build the thread from parts: a TCB, the cap in slot 0, then configure and start.
    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    let slot = crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
    assert_eq!(slot, 0, "the reporter's cap must land in slot 0");
    crate::sched::configure_tcb(tid, USER_CODE_VA, USER_STACK_TOP, aspace).expect("configure");
    crate::sched::start_tcb(tid, [0; 3]).expect("start");

    crate::sched::ipc_recv(report)[0]
}

/// **Load and run a real compiled ELF at U-mode on RISC-V** (milestone 20, the user-ELF step).
///
/// Unlike [`riscv_capability_demo`], which copies a hand-written machine-code blob, this takes the
/// bytes of the `worker` program (a Rust binary compiled to a riscv64 ELF, delivered as the initrd)
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
/// The initrd is a crickerfs archive holding `init` (the portable `builder` program) plus `worker`.
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
    let fs = crickerfs::Fs::parse(archive).expect("initrd is not a crickerfs archive");
    let init_bytes = fs.read("init").expect("archive has no 'init' program");
    // Measured boot (milestone 22 phase B.1): the boot program is checked against the digest compiled
    // into this kernel image before its address space exists, and a mismatch halts. Same check, same
    // trust root, same place in the sequence as aarch64's `spawn_init`; the parity gate (§19) asks
    // for exactly that.
    crate::trust::require("init", init_bytes);
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

    // The word the child SENDs home (init built the pipe; the child sent through it).
    Ok(crate::sched::ipc_recv(report)[0])
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

    let fs = crickerfs::Fs::parse(archive).expect("initrd is not a crickerfs archive");
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
/// `spawn_init` + `init_boot`: load `sysinit` (the portable system builder) as the boot process, map
/// the whole initrd into it, and grant it three capabilities: a large untyped budget (slot 0), the
/// NS16550's registers as a device cap (slot 1), and the UART receive interrupt as an `Irq` cap (slot
/// 2). From those, `sysinit` builds the console server, the input driver, and the shell out of its
/// own budget and wires them together; the kernel touches none of it. Unlike the other demos this
/// does not block: `sysinit` and its children run on the scheduler while the boot thread parks.
#[cfg(target_arch = "riscv64")]
pub fn riscv_shell_boot(archive: &'static [u8], uart_irq: u32) -> Result<(), LoadError> {
    use crate::cap::Rights;
    const UART_PHYS: u64 = 0x1000_0000; // the NS16550 on QEMU virt

    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);

    let fs = crickerfs::Fs::parse(archive).expect("initrd is not a crickerfs archive");
    let init_bytes = fs
        .read("sysinit")
        .expect("archive has no 'sysinit' program");
    // Measured boot (milestone 22 phase B.1): `sysinit` is riscv's boot program, so it is in the
    // trust root under its own name and checked here, before its address space is built.
    crate::trust::require("sysinit", init_bytes);
    let elf = Elf::parse(init_bytes).map_err(LoadError::NotLoadable)?;

    // sysinit's address space: its segments, a deep stack (it runs an ELF loader that builds three
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
    let aspace_name = readopt_user_aspace(space).expect("register sysinit aspace");

    // Route the UART receive interrupt to an endpoint; the input driver's Irq cap will WAIT on it.
    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(uart_irq, irq_ep);
    let build_region = crate::untyped::create(2048).expect("no building budget for sysinit");

    let tcb_region = crate::untyped::create(2).expect("no tcb region");
    let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
    // slot 0: the delegable root budget (milestone 31), GRANT included so sysinit can split off a
    // budget for the shell and hand it on; rights only narrow downward. slot 1: the NS16550
    // registers, WRITE|GRANT so sysinit maps them into the console and input drivers. slot 2: the
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
    crate::sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace_name).expect("configure");
    crate::sched::start_tcb(tid, [0, initrd_len, 0]).expect("start"); // a1 = the archive length

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
#[allow(dead_code)] // the demo payload: exercised by the boot demo, mechanism unit-tested
pub mod console_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};
    use crate::sched::EpId;

    /// The PL011's physical address on QEMU `virt`. The kernel maps it for its own debug output;
    /// here we hand a *second* mapping of the same registers to the userspace server. On real
    /// hardware you would give the server exclusive ownership; in QEMU both mappings are fine,
    /// and the kernel's is now used only for panics and boot, not for anyone's `print`.
    const PL011_PHYS: u64 = 0x0900_0000;

    /// Printing-client role (`x0`), matching user/src/hello.rs. (The server is its own binary now,
    /// 19f.3, so it has no role; only the demo client is still a role of hello.)
    const ROLE_CLIENT: u64 = 2;

    /// What a client needs to talk to the console server: two endpoints and the shared page.
    #[derive(Clone, Copy)]
    pub struct Console {
        pub request: EpId,
        pub reply: EpId,
        pub shared_phys: u64,
    }

    /// Spawn the console server as a user process and return a handle for wiring up clients.
    ///
    /// The server holds: `RECV` on `request` (slot 0), `SEND` on `reply` (slot 1), the shared
    /// page mapped **read-only** (it only reads what clients wrote), and the **UART's registers**
    /// mapped as user device memory. That last mapping is the whole milestone: a driver, at EL0,
    /// holding its hardware.
    pub fn start() -> Console {
        // The console server is its own binary now (19f.3), loaded from the archive by name rather
        // than entered as a role of hello.
        let image = program("console").expect("no console program in the initrd");
        let request = crate::sched::create_endpoint();
        let reply = crate::sched::create_endpoint();
        let shared_phys = crate::memory::alloc()
            .expect("no frame for the shared console buffer")
            .addr();

        // Zero the shared page so a client's first print cannot leak stale RAM.
        // SAFETY: freshly allocated, reachable through the direct map, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(shared_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0, // no role selector: the console is its own binary
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(request, Rights::READ), // slot 0: RECV requests
                        endpoint_cap(reply, Rights::WRITE),  // slot 1: SEND acks
                    ],
                    maps: &[
                        Mapping {
                            va: SHARED_VA,
                            phys: shared_phys,
                            flags: Flags::user_rodata(),
                        },
                        Mapping {
                            va: UART_VA,
                            phys: PL011_PHYS,
                            flags: Flags::user_device(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the console server");

        Console {
            request,
            reply,
            shared_phys,
        }
    }

    /// Spawn a client wired to `console`: `SEND` on request (slot 0), `RECV` on reply (slot 1),
    /// and the shared page mapped **read/write** (it writes the text it wants printed).
    pub fn spawn_client(image: &'static [u8], console: Console) {
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_CLIENT,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(console.request, Rights::WRITE), // slot 0: SEND
                        endpoint_cap(console.reply, Rights::READ),    // slot 1: RECV ack
                    ],
                    maps: &[Mapping {
                        va: SHARED_VA,
                        phys: console.shared_phys,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn a console client");
    }

    /// The user VAs the client and server agree on. Kept here so the kernel and the binary have
    /// one source of truth; they must match user/src/hello.rs.
    const SHARED_VA: u64 = 0x0000_0000_0060_0000;
    const UART_VA: u64 = 0x0000_0000_0070_0000;
}

/// Bringing the virtio block driver up in userspace.
///
/// **Milestone 9's headline.** The kernel enumerates the bus (kernel/src/virtio.rs) to find the
/// block device, then hands a userspace driver everything it needs and nothing it does not: the
/// device's registers, a DMA page, an interrupt, and an endpoint to report what it read. The
/// kernel does not touch the device.
#[allow(dead_code)] // the demo payload; the mechanism is unit-tested
pub mod virtio_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
    use crate::sched::EpId;

    /// Where the driver expects its DMA page. Must match user/src/virtio.rs. The device registers
    /// are NOT mapped to the driver any more: it drives the device through a `Virtio` capability,
    /// so it cannot point the device outside this DMA region.
    const DMA_VA: u64 = 0x0000_0000_0090_0000;

    const ROLE_VIRTIO_BLK: u64 = 3;
    /// The write-path roles (milestone 32 phase 1); must match user/src/hello.rs and blk.rs.
    const ROLE_VIRTIO_BLK_WRITE: u64 = 30;
    const ROLE_VIRTIO_BLK_WRITE_ABANDON: u64 = 31;
    /// The virtio-net driver role (milestone 30); must match user/src/hello.rs and blk.rs.
    const ROLE_VIRTIO_NET: u64 = 40;

    /// Start a driver role against a discovered transport. The shared body of [`start`] (mmio),
    /// [`start_pci`] (PCIe), and the writer starters: everything from here on, the DMA region,
    /// the Irq routing, the confined `Virtio` capability, the spawn, is bus-agnostic, which is
    /// the transport seam doing its job, and role-agnostic, because every role gets the same
    /// world and differs only in what it does with it. `rid` is the PCIe requester id the IOMMU
    /// keys its tables on; mmio devices have no IOMMU in front of them and pass `None`.
    fn wire(
        image: &'static [u8],
        transport: crate::virtio::Transport,
        intid: u32,
        role: u64,
        rid: Option<u32>,
    ) -> EpId {
        // A DMA page: physical memory the device can reach, mapped into the driver, whose
        // physical address the driver must know (a process sees only virtual addresses). We hand
        // that physical address over in `arg1`.
        let dma = crate::memory::alloc()
            .expect("no DMA frame for the virtio driver")
            .addr();
        // SAFETY: fresh frame, reachable through the direct map. Zero it so stale RAM cannot look
        // like a valid descriptor to the device before the driver writes the real ones.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }

        // Route the device's interrupt to an endpoint and enable it, so the driver's `WAIT` on
        // its Irq capability will receive it. See milestone 9a.
        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(intid, irq_ep);
        crate::arch::irq::enable(intid);

        // Where the driver reports the bytes it read.
        let report = crate::sched::create_endpoint();

        // Register the device's transport with the kernel: the kernel owns the registers and the
        // DMA-critical operations, and confines the device to this DMA region. The driver gets a
        // `Virtio` capability, not the registers.
        let vid = crate::virtio::register(transport, dma, FRAME_SIZE, rid);

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: role,
                    arg1: dma, // the DMA region's PHYSICAL address (still needed to build requests)
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE), // slot 0: SEND the result
                        irq_cap(intid),                      // slot 1: WAIT / ACK the interrupt
                        virtio_cap(vid),                     // slot 2: drive the device, confined
                    ],
                    maps: &[Mapping {
                        va: DMA_VA,
                        phys: dma,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the virtio driver");

        report
    }

    /// Start the driver against the virtio-mmio disk. Returns the endpoint it will report its
    /// result on, or `None` if there is no disk attached to enumerate.
    pub fn start(image: &'static [u8]) -> Option<EpId> {
        start_role(image, ROLE_VIRTIO_BLK)
    }

    /// Start the SAME driver binary against the PCIe disk (the PCIe transport, DECISIONS §18):
    /// enumeration and bring-up by kernel/src/pci.rs, INTx through the interrupt controller, the
    /// identical confinement, now backed by the IOMMU when the machine has one (§20). The driver
    /// cannot tell which bus it is on, and that is the point.
    pub fn start_pci(image: &'static [u8]) -> Option<EpId> {
        start_role_pci(image, ROLE_VIRTIO_BLK)
    }

    /// Start the write-path driver (milestone 32 phase 1) against the mmio disk: it writes a
    /// pattern to the scratch block, reads it back, re-checks the filesystem around it, and
    /// reports the read-back bytes.
    pub fn start_writer(image: &'static [u8]) -> Option<EpId> {
        start_role(image, ROLE_VIRTIO_BLK_WRITE)
    }

    /// The same writer over the PCIe transport.
    pub fn start_writer_pci(image: &'static [u8]) -> Option<EpId> {
        start_role_pci(image, ROLE_VIRTIO_BLK_WRITE)
    }

    /// Start the abandoning writer (the kill-mid-write case): it submits a validated write and
    /// dies on purpose before collecting the completion. Reports 1 after the submit, so the test
    /// knows the request genuinely left before the death.
    pub fn start_write_abandoner(image: &'static [u8]) -> Option<EpId> {
        start_role(image, ROLE_VIRTIO_BLK_WRITE_ABANDON)
    }

    /// Start the virtio-net driver against the mmio NIC (milestone 30). It does a DHCP round trip
    /// over QEMU user-mode networking and reports the offered address. `None` if no NIC is attached.
    /// The same `wire` machinery as the disk: the DMA confinement now polices two queues, and the
    /// driver drives receive and transmit through the one `Virtio` capability.
    pub fn start_net(image: &'static [u8]) -> Option<EpId> {
        let dev = crate::virtio::find_net_device()?;
        Some(wire(
            image,
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dev.intid,
            ROLE_VIRTIO_NET,
            None, // virtio-mmio has no IOMMU in front of it on either board
        ))
    }

    /// The same net driver over the PCIe transport, behind the IOMMU (milestone 30, §20): the NIC
    /// is confined in hardware to its DMA region and shadow page, `iommu_platform=on`, exactly the
    /// disk's pattern. The driver cannot tell which bus it is on.
    pub fn start_net_pci(image: &'static [u8]) -> Option<EpId> {
        let d = crate::pci::find_net_device()?;
        Some(wire(
            image,
            crate::virtio::Transport::pci(&d),
            d.intid,
            ROLE_VIRTIO_NET,
            Some(d.rid),
        ))
    }

    /// The heap budget the net server (smoltcp) draws from, in pages: the socket set, per-frame
    /// transmit buffers, and caches, plus the program's own page tables. netd caps its heap at 128
    /// pages, so 192 leaves headroom without being unbounded.
    const NET_SERVER_BUDGET_PAGES: u64 = 192;
    /// smoltcp builds packets on the stack; one mapped stack page is not enough. Eight extra keeps
    /// the poll loop clear (allocdemo needed three for `alloc` collections; smoltcp asks more).
    const NET_SERVER_STACK_PAGES: u64 = 8;

    /// Start the **net server** (milestone 30, piece 3): the `netd` binary, which runs smoltcp over
    /// the confined NIC and does DHCP. Like [`wire`] it hands the confined `Virtio` capability, the
    /// interrupt, a DMA page, and a report endpoint; unlike it, the server also gets an **untyped
    /// budget** (slot 3) for the heap smoltcp allocates against, and extra stack pages. Returns the
    /// endpoint the server reports its acquired address on, or `None` if no NIC is attached.
    pub fn start_net_server(image: &'static [u8]) -> Option<EpId> {
        let dev = crate::virtio::find_net_device()?;
        Some(
            wire_net_server(
                image,
                crate::virtio::Transport::Mmio {
                    mmio_phys: dev.mmio_phys,
                },
                dev.intid,
                None,
            )
            .0,
        )
    }

    /// The net server over the PCIe transport, behind the IOMMU (§20).
    pub fn start_net_server_pci(image: &'static [u8]) -> Option<EpId> {
        let d = crate::pci::find_net_device()?;
        Some(
            wire_net_server(
                image,
                crate::virtio::Transport::pci(&d),
                d.intid,
                Some(d.rid),
            )
            .0,
        )
    }

    /// [`wire`] for the net server: the same confined transport, interrupt, DMA page, and report
    /// endpoint, plus an untyped budget for the heap, extra stack pages, and a **`Stack` endpoint**
    /// (slot 4) where clients' socket-contract requests arrive. `netd` is its own binary (loaded by
    /// name), so no role selector is passed. Returns `(report endpoint, stack endpoint)`; a caller
    /// that has no client (the phase-A DHCP tests) ignores the stack, and netd simply blocks on it.
    fn wire_net_server(
        image: &'static [u8],
        transport: crate::virtio::Transport,
        intid: u32,
        rid: Option<u32>,
    ) -> (EpId, EpId) {
        use crate::cap::untyped_cap;

        let dma = crate::memory::alloc()
            .expect("no DMA frame for the net server")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so stale RAM cannot look like a valid
        // descriptor to the device before the driver writes the real ones.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }

        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(intid, irq_ep);
        crate::arch::irq::enable(intid);

        let report = crate::sched::create_endpoint();
        let stack = crate::sched::create_endpoint();
        let vid = crate::virtio::register(transport, dma, FRAME_SIZE, rid);
        let budget = crate::untyped::create(NET_SERVER_BUDGET_PAGES).expect("no untyped for netd");

        // The DMA mapping plus the extra stack pages, in one array the spawn closure owns.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; NET_SERVER_STACK_PAGES as usize + 1];
        maps[0] = Mapping {
            va: DMA_VA,
            phys: dma,
            flags: Flags::user_data(),
        };
        for k in 0..NET_SERVER_STACK_PAGES as usize {
            let phys = crate::memory::alloc()
                .expect("no frame for the net server stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            maps[k + 1] = Mapping {
                va: USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE,
                phys,
                flags: Flags::user_data(),
            };
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0, // netd is its own binary; no role selector
                    arg1: dma,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE), // slot 0: report the acquired address
                        irq_cap(intid),                      // slot 1: WAIT / ACK the interrupt
                        virtio_cap(vid),                     // slot 2: the confined transport
                        untyped_cap(budget),                 // slot 3: the heap's budget
                        endpoint_cap(stack, Rights::READ),   // slot 4: serve clients' requests
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the net server");

        (report, stack)
    }

    /// The client's budget, in pages: it mints one shared frame and pays for that frame's own page
    /// table plus its mapping; small and fixed.
    const NET_CLIENT_BUDGET_PAGES: u64 = 16;

    /// **Spawn the net server and a client of its socket contract** (milestone 30, piece 3 phase B).
    /// Both are the `netd` binary (`image`): the server is entry role 0, the client is a nonzero
    /// role (the client rides in the same binary to keep the initrd under its 15-file directory
    /// limit). They share a `Stack` endpoint: netd holds `READ` (it serves), the client holds
    /// `WRITE` (it requests). The client also gets its own untyped (to mint and delegate the shared
    /// frame) and a report endpoint. `cli_arg` selects which exchange the client drives (UDP DNS or
    /// TCP echo). Returns the client's report endpoint, or `None` if no NIC is attached.
    pub fn start_net_stack(image: &'static [u8], cli_arg: u64, pci: bool) -> Option<EpId> {
        use crate::cap::untyped_cap;

        let (transport, intid, rid) = if pci {
            let d = crate::pci::find_net_device()?;
            (crate::virtio::Transport::pci(&d), d.intid, Some(d.rid))
        } else {
            let dev = crate::virtio::find_net_device()?;
            (
                crate::virtio::Transport::Mmio {
                    mmio_phys: dev.mmio_phys,
                },
                dev.intid,
                None,
            )
        };

        let (netd_report, stack) = wire_net_server(image, transport, intid, rid);

        // The client: WRITE on the shared stack endpoint, its own untyped, a report endpoint. Two
        // extra stack pages cover its DNS-query building and IPC; it links no heap.
        let cli_report = crate::sched::create_endpoint();
        let cli_budget =
            crate::untyped::create(NET_CLIENT_BUDGET_PAGES).expect("no untyped for the net client");
        let mut cli_stack = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; 2];
        for (k, m) in cli_stack.iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for the net client stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = phys;
        }

        crate::sched::spawn(move || {
            run(
                image, // the netd binary again; a nonzero entry role runs its client half
                Spawn {
                    arg0: cli_arg,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(cli_report, Rights::WRITE), // slot 0: report the verdict
                        // slot 1: the stack endpoint, WRITE to send requests and to delegate the
                        // shared frame onto it (the frame it mints already carries GRANT).
                        endpoint_cap(stack, Rights::WRITE),
                        untyped_cap(cli_budget), // slot 2: mint and map the shared frame
                    ],
                    maps: &cli_stack,
                },
            )
        })
        .expect("could not spawn the net client");

        // netd reports its DHCP lease with a blocking `send`; drain it here so netd unblocks and
        // enters its serve loop (the client's first request blocks until it does). This also
        // confirms DHCP completed before the client's exchange runs. The client, spawned above,
        // waits at its first request meanwhile.
        crate::sched::ipc_recv(netd_report);

        Some(cli_report)
    }

    /// The networked std client's heap budget and extra stack, both larger than the hand-written
    /// client's: it is a full std program (formatting, `Vec`, `String`), so it needs the same
    /// generous heap and stack the `hellostd` demo does.
    const STD_NET_HEAP_PAGES: u64 = 256;
    const STD_NET_STACK_PAGES: u64 = 32;

    /// **Spawn the net server and a `std::net` client** (milestone 27 phase two): the same
    /// `hellostd` std binary, but now given the network, so its `UdpSocket::bind` probe succeeds
    /// and it drives a real UDP DNS query and a TCP echo round trip through `std::net`, whose PAL
    /// binds to this same netd socket contract. netd is `netd_image` (entry role 0, holding the
    /// NIC and `READ` on the `Stack` endpoint); `std_image` is the ordinary std ELF given the std
    /// slot convention (heap untyped at 0, stdout at 1) plus the two net slots (the `Stack`
    /// endpoint `WRITE` at 2, an untyped budget for its per-socket shared frames at 3). Over the
    /// mmio transport; the PAL sits above the transport, so proving it on one is proving it. The
    /// same binary spawned without slots 2 and 3 runs the offline transcript instead, which is what
    /// makes "no ambient network" visible: authority, not the code, decides. Returns the program's
    /// stdout endpoint for the test to reassemble, or `None` if no NIC is attached.
    pub fn start_net_std(netd_image: &'static [u8], std_image: &'static [u8]) -> Option<EpId> {
        use crate::cap::untyped_cap;

        let dev = crate::virtio::find_net_device()?;
        let transport = crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        };
        let (netd_report, stack) = wire_net_server(netd_image, transport, dev.intid, None);

        let report = crate::sched::create_endpoint();
        let heap =
            crate::untyped::create(STD_NET_HEAP_PAGES).expect("no untyped for the std net heap");
        let frames = crate::untyped::create(NET_CLIENT_BUDGET_PAGES)
            .expect("no untyped for the std net frames");

        let mut stackmaps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; STD_NET_STACK_PAGES as usize];
        for (k, m) in stackmaps.iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for the std net stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = phys;
        }

        crate::sched::spawn(move || {
            run(
                std_image,
                Spawn {
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(heap),                   // slot 0: the heap's budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: stdout/stderr
                        endpoint_cap(stack, Rights::WRITE),  // slot 2: the Stack endpoint
                        untyped_cap(frames), // slot 3: mint per-socket shared frames
                    ],
                    maps: &stackmaps,
                },
            )
        })
        .expect("could not spawn the networked std program");

        // Same discipline as start_net_stack: drain netd's blocking DHCP report so it reaches its
        // serve loop before the std program's first request, and confirm DHCP completed.
        crate::sched::ipc_recv(netd_report);

        Some(report)
    }

    /// [`wire`] against the enumerated mmio disk, at `role`.
    fn start_role(image: &'static [u8], role: u64) -> Option<EpId> {
        let dev = crate::virtio::find_block_device()?;
        Some(wire(
            image,
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dev.intid,
            role,
            None, // virtio-mmio has no IOMMU in front of it on either board
        ))
    }

    /// [`wire`] against the enumerated PCIe disk, at `role`.
    fn start_role_pci(image: &'static [u8], role: u64) -> Option<EpId> {
        let d = crate::pci::find_block_device()?;
        Some(wire(
            image,
            crate::virtio::Transport::pci(&d),
            d.intid,
            role,
            Some(d.rid), // the PCIe requester id, the IOMMU keys its tables on it
        ))
    }

    const ROLE_VIRTIO_ATTACK: u64 = 8;

    /// Spawn a MALICIOUS driver that tries to DMA over kernel memory, for the security test. It
    /// holds a real `Virtio` capability and its own DMA region, and points a descriptor at the
    /// kernel image. Returns the endpoint on which it reports whether the kernel refused it.
    pub fn start_attacker(image: &'static [u8]) -> Option<EpId> {
        let dev = crate::virtio::find_block_device()?;
        let dma = crate::memory::alloc().expect("no DMA frame").addr();
        // SAFETY: fresh frame via the direct map.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }
        let vid = crate::virtio::register(
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dma,
            FRAME_SIZE,
            None,
        );
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_VIRTIO_ATTACK,
                    arg1: dma,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE),
                        irq_cap(dev.intid), // slot 1 (unused by the attacker; keeps virtio at slot 2)
                        virtio_cap(vid),    // slot 2
                    ],
                    maps: &[Mapping {
                        va: DMA_VA,
                        phys: dma,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the virtio attacker");

        Some(report)
    }

    const ROLE_VIRTIO_ATTACK_INDIRECT: u64 = 13;

    /// Spawn a malicious driver that tries the **indirect-descriptor** escape: it negotiates
    /// `INDIRECT_DESC` and submits one descriptor flagged indirect whose inner table aims the
    /// device at the kernel image. Same wiring as [`start_attacker`], different role. The kernel
    /// strips the feature and refuses the flag, so the attacker reports `1` (refused). Returns the
    /// report endpoint.
    pub fn start_attacker_indirect(image: &'static [u8]) -> Option<EpId> {
        let dev = crate::virtio::find_block_device()?;
        let dma = crate::memory::alloc().expect("no DMA frame").addr();
        // SAFETY: fresh frame via the direct map.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
        }
        let vid = crate::virtio::register(
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dma,
            FRAME_SIZE,
            None,
        );
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_VIRTIO_ATTACK_INDIRECT,
                    arg1: dma,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE),
                        irq_cap(dev.intid), // slot 1 (unused; keeps virtio at slot 2)
                        virtio_cap(vid),    // slot 2
                    ],
                    maps: &[Mapping {
                        va: DMA_VA,
                        phys: dma,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the indirect virtio attacker");

        Some(report)
    }
}

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
/// The service drives the **second** mmio block disk (the RedoxFS image); the first is the crickerfs
/// disk the phase-1 driver tests use. `None` if there is no such disk attached to this run.
#[allow(dead_code)] // spawned only by the phase-2 test, like every other service module here
pub mod fs_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, untyped_cap, virtio_cap};
    use crate::sched::EpId;

    /// The block server's role in the driver binary (must match user/src/{hello,blk}.rs and virtio.rs).
    const ROLE_BLK_SERVER: u64 = 32;

    /// The heap budget the FS server draws RedoxFS's allocations from. RedoxFS keeps a 128 KiB
    /// compress buffer, block buffers, and small tree structures for the images phase 2 serves; 8 MiB
    /// is comfortable and is the process's hard ceiling (its `HEAP_MAX` matches).
    const FS_BUDGET_PAGES: u64 = 2048;

    /// Extra stack pages for the FS server, below the single page `run` maps. RedoxFS's recursive
    /// tree/htree/transaction code needs far more than 4 KiB; 32 pages (128 KiB) is generous and
    /// costs 32 frames once per boot.
    const FS_STACK_PAGES: u64 = 32;

    // The VAs each process expects its mappings at. Each MUST match that program's source.
    const DMA_VA: u64 = 0x0000_0000_0090_0000; // block server DMA region, 2 pages (user/src/virtio.rs)
    const BLK_PAGE_FS: u64 = 0x5000_0000; // FS server's block page (fsserver.rs BLK_PAGE)
    const FILE_PAGE_FS: u64 = 0x5000_1000; // FS server's file page (fsserver.rs FILE_PAGE)
    const FILE_VA_CLIENT: u64 = 0x0000_0000_0060_0000; // client's file page (fsclient.rs FILE_VA)

    /// A std program's half of the same agreement (notes/abi.md §4, notes/std.md). Both constants
    /// MUST match the std PAL's `sys/pal/cricker/rt.rs`: the slot it looks for the FS-service
    /// endpoint in, and the VA it expects the shared file page at. A std program's slot layout
    /// differs from the hand-written client's because std already owes slots 0 and 1 to its heap and
    /// its stdout, and 2 and 3 to `std::net`.
    const FS_DIR_SLOT: u64 = 4;
    const FS_PAGE_STD: u64 = 0x0000_0000_1100_0000;

    /// A fresh, zeroed frame, returned by physical address. Zeroed so no stale RAM is ever visible
    /// across a share, and (for the DMA frame) so the device never reads a stale descriptor.
    fn frame() -> u64 {
        let p = crate::memory::alloc()
            .expect("no frame for the fs service")
            .addr();
        // SAFETY: fresh frame, reachable through the direct map.
        unsafe { core::ptr::write_bytes(mmu::phys_to_virt(p) as *mut u8, 0, FRAME_SIZE as usize) };
        p
    }

    /// **One boot, one FS service**, remembered here.
    ///
    /// The block server owns the RedoxFS device: a second wiring would put a second driver on the
    /// same virtio slot and re-bind its interrupt, so the two client tests (the hand-written
    /// `fsclient` and the std program) share one wired service instead. Whichever runs first pays
    /// for the wiring and receives the two readiness endpoints; the other sees `None` for them,
    /// because a readiness sentinel is sent once and has already been taken. Plain atomics rather
    /// than a lock: the only writer is the boot/test thread that calls these functions.
    static WIRED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
    static FILE_EP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    static FILE_SHARED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

    /// The wired service: the file-service endpoint clients `CALL`, the physical frame they share
    /// with the FS server, and (only on the call that did the wiring) the block server's and FS
    /// server's readiness endpoints.
    type Service = (EpId, u64, Option<(EpId, EpId)>);

    /// Wire the block server and the FS server if this boot has not already, else hand back what is
    /// already running. `None` means no RedoxFS disk is attached.
    fn ensure(blk_image: &'static [u8], fsserver_image: &'static [u8]) -> Option<Service> {
        use core::sync::atomic::Ordering;

        if WIRED.load(Ordering::Acquire) {
            return Some((
                FILE_EP.load(Ordering::Relaxed),
                FILE_SHARED.load(Ordering::Relaxed),
                None,
            ));
        }
        let (blk_ready, ready, file_ep, file_shared) = wire_servers(blk_image, fsserver_image)?;
        FILE_EP.store(file_ep, Ordering::Relaxed);
        FILE_SHARED.store(file_shared, Ordering::Relaxed);
        WIRED.store(true, Ordering::Release);
        Some((file_ep, file_shared, Some((blk_ready, ready))))
    }

    /// Wire and spawn the block server and the FS server. `blk_image` is the driver binary carrying
    /// the block-server role (hello on aarch64, `blk` on riscv); `fsserver_image` is the same on
    /// both ISAs. Returns `(blk_ready, ready, file_ep, file_shared)`.
    fn wire_servers(
        blk_image: &'static [u8],
        fsserver_image: &'static [u8],
    ) -> Option<(EpId, EpId, EpId, u64)> {
        let dev = crate::virtio::find_block_device_n(1)?;

        // The block server's DMA region is TWO contiguous pages: page 0 for the rings, request
        // header and status (block-server-private), page 1 for the 4096-byte data buffer. Page 1 is
        // ALSO the block page shared with the FS server, so the device DMAs a whole filesystem block
        // straight into the FS server's page, one request per block, no copy. The other shared page
        // (client <-> FS server, names and file bytes) is a single frame.
        let dma = crate::memory::alloc_contiguous(2)
            .expect("no 2-page DMA region for the block server")
            .addr();
        // SAFETY: two fresh contiguous frames via the direct map; zero so neither stale descriptors
        // nor stale file bytes are ever visible to the device or the FS server.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(dma) as *mut u8,
                0,
                2 * FRAME_SIZE as usize,
            )
        };
        let blk_shared = dma + FRAME_SIZE; // page 1 of the region is the shared block page
        let file_shared = frame();

        // The endpoints. Rights split each into a request side and an answer side.
        let blk_ep = crate::sched::create_endpoint(); // FS server WRITE (CALL) -> block server READ
        let file_ep = crate::sched::create_endpoint(); // client WRITE (CALL) -> FS server READ
        let ready = crate::sched::create_endpoint(); // FS server WRITE -> the kernel test RECVs
        let blk_ready = crate::sched::create_endpoint(); // block server WRITE -> the kernel test RECVs

        // --- the block server: its 2-page DMA region, the device's interrupt, the confined
        // transport, the blk request endpoint. Same confinement as any driver, just a bigger region.
        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(dev.intid, irq_ep);
        crate::arch::irq::enable(dev.intid);
        let vid = crate::virtio::register(
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dma,
            2 * FRAME_SIZE, // both pages: the device may touch the rings AND the data buffer
            None,           // virtio-mmio has no IOMMU in front of it
        );
        crate::sched::spawn(move || {
            run(
                blk_image,
                Spawn {
                    arg0: ROLE_BLK_SERVER,
                    arg1: dma, // the DMA region's physical address
                    arg2: 0,
                    grants: &[
                        endpoint_cap(blk_ep, Rights::READ), // slot 0: RECV blk requests
                        irq_cap(dev.intid),                 // slot 1: the device interrupt
                        virtio_cap(vid),                    // slot 2: the confined transport
                        endpoint_cap(blk_ready, Rights::WRITE), // slot 3: signal readiness once
                    ],
                    maps: &[
                        // Both DMA pages, contiguous at DMA_VA; page 1 (DMA_VA + FRAME_SIZE) is the
                        // shared block buffer, the same frame the FS server maps at BLK_PAGE_FS.
                        Mapping {
                            va: DMA_VA,
                            phys: dma,
                            flags: Flags::user_data(),
                        },
                        Mapping {
                            va: DMA_VA + FRAME_SIZE,
                            phys: blk_shared,
                            flags: Flags::user_data(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the block server");

        // --- the FS server: a heap budget, the block-service endpoint (client side), the
        // file-service endpoint (server side), and both shared pages. No device, no DMA. ---
        //
        // It also gets a DEEP stack. `run` maps one stack page (enough for the shallow programs),
        // but RedoxFS recurses through its tree and htree and commits transactions on the stack, and
        // one 4 KiB page overflows immediately (the first `open` faults ~4.2 KiB down). So map extra
        // stack pages below USER_STACK_VA out of fresh frames. These are shared-style mappings (not
        // freed on death), a one-time cost paid once per boot for the single FS server the test runs.
        let budget =
            crate::untyped::create(FS_BUDGET_PAGES).expect("no heap budget for the FS server");
        let mut stack = [0u64; FS_STACK_PAGES as usize];
        for f in stack.iter_mut() {
            *f = frame();
        }
        crate::sched::spawn(move || {
            // Build the mapping list: the two shared pages, then the extra stack pages.
            let mut maps = [Mapping {
                va: 0,
                phys: 0,
                flags: Flags::user_data(),
            }; 2 + FS_STACK_PAGES as usize];
            maps[0] = Mapping {
                va: BLK_PAGE_FS,
                phys: blk_shared,
                flags: Flags::user_data(),
            };
            maps[1] = Mapping {
                va: FILE_PAGE_FS,
                phys: file_shared,
                flags: Flags::user_data(),
            };
            for (i, &phys) in stack.iter().enumerate() {
                maps[2 + i] = Mapping {
                    va: super::USER_STACK_VA - (i as u64 + 1) * FRAME_SIZE,
                    phys,
                    flags: Flags::user_data(),
                };
            }
            run(
                fsserver_image,
                Spawn {
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(budget),                 // slot 0: the heap's untyped budget
                        endpoint_cap(blk_ep, Rights::WRITE), // slot 1: CALL the block server
                        endpoint_cap(file_ep, Rights::READ), // slot 2: RECV file requests
                        endpoint_cap(ready, Rights::WRITE),  // slot 3: signal readiness once
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the FS server");

        Some((blk_ready, ready, file_ep, file_shared))
    }

    /// Wire the service (or reuse this boot's) and spawn the hand-written client
    /// (`user/src/fsclient.rs`): the file-service endpoint, which IS its directory capability, the
    /// report endpoint, and its view of the shared file page. It names nothing else in the system.
    ///
    /// Returns `(readiness, report)`: the two readiness endpoints if this call wired the service,
    /// and the endpoint the client reports on.
    pub fn start(
        blk_image: &'static [u8],
        fsserver_image: &'static [u8],
        client_image: &'static [u8],
        client_role: u64,
    ) -> Option<(Option<(EpId, EpId)>, EpId)> {
        let (file_ep, file_shared, readiness) = ensure(blk_image, fsserver_image)?;
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                client_image,
                Spawn {
                    arg0: client_role, // 0 = the end-to-end proof; 1 = the fs_read benchmark loop
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                        endpoint_cap(report, Rights::WRITE),  // slot 1: report to the kernel
                    ],
                    maps: &[Mapping {
                        va: FILE_VA_CLIENT,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the FS client");

        Some((readiness, report))
    }

    /// The `std::fs` client's heap budget and extra stack. Same magnitudes as the networked std
    /// program: it is a full std program (formatting, `Vec`, `String`, `read_to_string`), so it
    /// needs the generous heap and the deep stack std's machinery wants.
    const STD_FS_HEAP_PAGES: u64 = 256;
    const STD_FS_STACK_PAGES: u64 = 32;

    /// **Wire the service and endow a std program with a directory capability** (milestone 27 phase
    /// two, the FS half).
    ///
    /// This is the one spawn site that makes `std::fs` work: an ordinary std ELF, given the std slot
    /// convention (heap untyped at 0, stdout at 1) **plus the FS-service endpoint at slot 4** and
    /// the page it shares with the FS server, mapped at the VA the PAL expects
    /// (`sys/pal/cricker/rt.rs::FS_PAGE`). Slots 2 and 3 are deliberately left EMPTY, which is why
    /// the grants go in by explicit slot instead of in order: this program holds a filesystem and no
    /// network, and `std::net` must be able to tell.
    ///
    /// The same binary spawned without slot 4 gets `Unsupported` from every `std::fs` call. That is
    /// the whole point: the code never chose to have a filesystem, its cspace did.
    ///
    /// Returns `(readiness, stdout)`: the readiness endpoints if this call wired the service, and
    /// the program's stdout endpoint for the test to reassemble.
    pub fn start_std(
        blk_image: &'static [u8],
        fsserver_image: &'static [u8],
        std_image: &'static [u8],
    ) -> Option<(Option<(EpId, EpId)>, EpId)> {
        let (file_ep, file_shared, readiness) = ensure(blk_image, fsserver_image)?;
        let report = crate::sched::create_endpoint();
        let heap =
            crate::untyped::create(STD_FS_HEAP_PAGES).expect("no untyped for the std fs heap");

        // The shared file page, then the deep stack std needs. `run` maps one stack page; std's
        // startup and formatting overflow it immediately, the same reason the other std spawns map
        // extra pages below it.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; 1 + STD_FS_STACK_PAGES as usize];
        maps[0] = Mapping {
            va: FS_PAGE_STD,
            phys: file_shared,
            flags: Flags::user_data(),
        };
        for (k, m) in maps[1..].iter_mut().enumerate() {
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = frame();
        }

        crate::sched::spawn(move || {
            // The directory capability goes in at its named slot BEFORE `run` grants in order, so
            // `run`'s two grants land at 0 and 1 and slots 2 and 3 stay empty. See `grant_at`.
            crate::sched::grant_at(FS_DIR_SLOT, endpoint_cap(file_ep, Rights::WRITE))
                .expect("the std fs slot was already occupied");
            run(
                std_image,
                Spawn {
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(heap),                   // slot 0: the heap's budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: stdout/stderr
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the std fs program");

        Some((readiness, report))
    }
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
#[allow(dead_code)] // spawned only by the milestone-29 test, like every other service module here
pub mod display_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
    use crate::sched::EpId;

    /// Where the driver maps its whole DMA region. Must match user/src/gpud.rs `DMA_VA`.
    const DMA_VA: u64 = 0x0000_0000_0090_0000;
    /// Where the client maps the surface. Must match user/src/painter.rs `SURFACE_VA`.
    const SURFACE_VA_CLIENT: u64 = 0x0000_0000_0060_0000;

    /// The DMA region, in frames: one for the rings and control buffers, then the surface.
    const DMA_FRAMES: u64 = 1 + gfx_proto::SURFACE_FRAMES as u64;

    /// The driver binary's escape-attempt role; must match user/src/gpud.rs `ROLE_BACKING_ESCAPE`.
    const ROLE_BACKING_ESCAPE: u64 = 1;

    /// **Wire and spawn the display driver and the painting client.** Returns
    /// `(driver report, client report)`, or `None` if no virtio-gpu function is on the bus.
    ///
    /// One spawn site for two processes on purpose: they are only meaningful together (a driver with
    /// no client serves nobody, a client with no driver blocks on its first CALL), and the endpoint
    /// and the shared frames that join them are created here, in the one place that is allowed to
    /// know both halves.
    pub fn start(driver_image: &'static [u8], client_image: &'static [u8]) -> Option<(EpId, EpId)> {
        let (driver_report, display_ep, surface) = wire_driver(driver_image, 0, 0)?;

        // --- the client: an endpoint and the pixels. Nothing else, which is the point. ---
        let client_report = crate::sched::create_endpoint();
        let mut client_maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; gfx_proto::SURFACE_FRAMES as usize];
        for (k, m) in client_maps.iter_mut().enumerate() {
            m.va = SURFACE_VA_CLIENT + k as u64 * FRAME_SIZE;
            m.phys = surface + k as u64 * FRAME_SIZE;
        }
        crate::sched::spawn(move || {
            run(
                client_image,
                Spawn {
                    arg0: 0,
                    arg1: 0, // no physical address: a client has no business knowing one
                    arg2: 0,
                    grants: &[
                        endpoint_cap(client_report, Rights::WRITE), // slot 0: its verdict
                        endpoint_cap(display_ep, Rights::WRITE),    // slot 1: CALL the driver
                    ],
                    maps: &client_maps,
                },
            )
        })
        .expect("could not spawn the painting client");

        Some((driver_report, client_report))
    }

    /// **Spawn a driver that attacks its own confinement** (user/src/gpud.rs `run_backing_escape`):
    /// it asks the device to read pixels out of a frame outside its grant. Returns
    /// `(report endpoint, the victim frame's physical address)`, or `None` if no GPU is on the bus.
    ///
    /// It gets exactly the honest driver's world, no more: the same confined transport, the same
    /// region, the same interrupt. That is what makes it a fair test of the barrier rather than of a
    /// missing capability. No client, because it never serves one.
    ///
    /// The **kernel** picks the victim frame and hands it over in `arg2`, the same way milestone 16b's
    /// confinement test picks its own escape frame: the caller has to know the exact address to look
    /// for in the IOMMU's fault queue, and a driver guessing at "the frame past my region" guesses
    /// wrong (the shadow page is allocated right after it, and that frame IS in the domain). The frame
    /// is deliberately never freed: it is an escape target, and handing it back to the allocator while
    /// a device has been told to read it is the use-after-free-by-hardware notes/dma.md warns about.
    pub fn start_backing_escape(driver_image: &'static [u8]) -> Option<(EpId, u64)> {
        let victim = crate::memory::alloc()
            .expect("no victim frame for the backing-escape test")
            .addr();
        let (report, _, _) = wire_driver(driver_image, ROLE_BACKING_ESCAPE, victim)?;
        Some((report, victim))
    }

    /// The shared half of both spawns: find the GPU, build the DMA region, route the interrupt,
    /// register the confined transport, and spawn `driver_image` at `role` with `arg2`. Returns
    /// `(report endpoint, display endpoint, the surface's physical base)`.
    fn wire_driver(driver_image: &'static [u8], role: u64, arg2: u64) -> Option<(EpId, EpId, u64)> {
        let d = crate::pci::find_gpu_device()?;

        // The DMA region: contiguous, because the surface must be one run of physical frames for the
        // device's backing to be a single memory entry and for the IOMMU domain to cover it as one
        // range. Zeroed, so neither a stale descriptor nor a stale pixel is ever visible to the
        // device or to the client.
        let dma = crate::memory::alloc_contiguous(DMA_FRAMES as usize)
            .expect("no contiguous DMA region for the display driver")
            .addr();
        // SAFETY: a fresh contiguous run of frames, reachable through the direct map, owned by
        // nobody else.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(dma) as *mut u8,
                0,
                (DMA_FRAMES * FRAME_SIZE) as usize,
            );
        }
        let surface = dma + FRAME_SIZE; // page 1 onward: the frames the client also maps

        // The device's interrupt, routed to an endpoint so the driver's WAIT receives it as a
        // message (milestone 9a).
        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(d.intid, irq_ep);
        crate::arch::irq::enable(d.intid);

        // Register the transport: the kernel keeps the registers and the two DMA-critical powers,
        // and confines the device in hardware to exactly this region plus the shadow page.
        let vid = crate::virtio::register(
            crate::virtio::Transport::pci(&d),
            dma,
            DMA_FRAMES * FRAME_SIZE,
            Some(d.rid), // the PCIe requester id the IOMMU keys its tables on
        );

        let display_ep = crate::sched::create_endpoint(); // client WRITE (CALL) -> driver READ
        let driver_report = crate::sched::create_endpoint();

        // --- the driver: the confined transport, the interrupt, the whole DMA region, and the
        // display endpoint's serving half. ---
        let mut driver_maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; DMA_FRAMES as usize];
        for (k, m) in driver_maps.iter_mut().enumerate() {
            m.va = DMA_VA + k as u64 * FRAME_SIZE;
            m.phys = dma + k as u64 * FRAME_SIZE;
        }
        crate::sched::spawn(move || {
            run(
                driver_image,
                Spawn {
                    arg0: role, // 0 = the display driver; 1 = the escape attempt
                    arg1: dma,  // the DMA region's PHYSICAL base: descriptors speak physical
                    arg2,       // the escape role's victim frame; unused (0) by the display driver
                    grants: &[
                        endpoint_cap(driver_report, Rights::WRITE), // slot 0: status
                        irq_cap(d.intid),                           // slot 1: the completion IRQ
                        virtio_cap(vid), // slot 2: the confined transport
                        endpoint_cap(display_ep, Rights::READ), // slot 3: serve clients
                    ],
                    maps: &driver_maps,
                },
            )
        })
        .expect("could not spawn the display driver");

        Some((driver_report, display_ep, surface))
    }
}

/// Console **input** in userspace: the receive half of the terminal.
#[allow(dead_code)]
pub mod input_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap};
    use crate::sched::EpId;

    const UART_VA: u64 = 0x0000_0000_00a0_0000;
    const LINE_VA: u64 = 0x0000_0000_00b0_0000;
    const PL011_PHYS: u64 = 0x0900_0000;
    /// The PL011 on QEMU `virt` is SPI 1, which is INTID 33 (SPIs start at 32).
    const UART_INTID: u32 = 33;

    /// Spawn the input driver, wired to the UART and its receive interrupt. Returns (line endpoint,
    /// line-buffer physical address). The driver is its own binary now (19f.4), loaded by name.
    pub fn spawn_wired() -> (EpId, u64) {
        let image = program("input").expect("no input program in the initrd");
        let line = crate::sched::create_endpoint();

        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(UART_INTID, irq_ep);
        crate::arch::irq::enable(UART_INTID);

        // The line buffer the driver assembles into. Shared with the reader (the shell) later; a
        // scratch page for the standalone validator.
        let line_phys = crate::memory::alloc().expect("no line-buffer frame").addr();
        // SAFETY: fresh frame, direct-mapped, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(line_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0, // no role selector: input is its own binary
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(line, Rights::WRITE), // slot 0: SEND completed lines
                        irq_cap(UART_INTID),               // slot 1: WAIT / ACK the RX interrupt
                    ],
                    maps: &[
                        Mapping {
                            va: UART_VA,
                            phys: PL011_PHYS,
                            flags: Flags::user_device(),
                        },
                        Mapping {
                            va: LINE_VA,
                            phys: line_phys,
                            flags: Flags::user_data(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the input driver");

        (line, line_phys)
    }
}

/// The shell, and everything it talks to. **Milestone 10: proof the whole stack works.**
///
/// Wires up four processes and the channels between them: the console server (output), the input
/// driver (a line of text at a time), the shell itself, and a kernel-side spawn service that
/// starts worker processes on the shell's request. When it returns, an interactive shell is
/// running at EL0, and everything the user types is a conversation between processes.
#[allow(dead_code)]
pub mod shell_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};

    const OUT_VA: u64 = 0x0000_0000_0060_0000; // shell <-> console server
    const LINE_VA: u64 = 0x0000_0000_00b0_0000; // shell <-> input driver

    /// **How many children the shell may have alive at once.** The bound that stops a spawn flood
    /// (or workers that block forever without exiting) from exhausting kernel memory: each live
    /// child costs a `Thread`, a 16 KiB kernel stack, and an address space, and there can be at
    /// most this many. A child returns its slot when it is reaped. See notes/quotas.md.
    static SPAWN_QUOTA: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(8);

    pub fn start() {
        // Every program the shell stack needs is its own binary now (19f.2-5), loaded by name.
        let worker = program("worker").expect("no worker program in the initrd");
        let shell = program("shell").expect("no shell program in the initrd");

        // Output: the console server (milestone 8), and the shell as its client.
        let console = console_service::start();

        // Input: the receive driver (milestone 10), delivering lines on `line`.
        let (line, line_phys) = input_service::spawn_wired();

        // The shell asks for spawns here; it receives worker results here.
        let spawn_ep = crate::sched::create_endpoint();
        let result_ep = crate::sched::create_endpoint();

        // The spawn service: a kernel thread that starts a worker process for each request. This
        // is the kernel acting as the "process server"; a full capability system would compose
        // spawn from Untyped/Tcb capabilities in userspace (milestone 11), but the shell does not
        // care where the service lives, only that it can name it.
        crate::sched::spawn(move || {
            loop {
                let n = crate::sched::ipc_recv(spawn_ep)[0];
                let spawned = crate::sched::spawn_with_quota(&SPAWN_QUOTA, move || {
                    run(
                        worker,
                        Spawn {
                            arg0: 0, // no role selector: worker is its own binary
                            arg1: n, // the worker's input, in x1
                            arg2: 0,
                            grants: &[endpoint_cap(result_ep, Rights::WRITE)],
                            maps: &[],
                        },
                    )
                });
                // **Do not panic on out-of-memory.** A spawn flood must degrade, not kill the
                // machine: if the kernel is out of memory we cannot make the worker, so we tell
                // the shell its request failed (a sentinel result) and carry on serving. The
                // security audit flagged the old `.expect(...)` here as a userspace-triggerable
                // kernel panic. (Per-process spawn quotas are the real fix, and they now exist as
                // `QuotaToken` in thread.rs, so this path is bounded as well as panic-free. See
                // notes/quotas.md. Not panicking remains the cheap, honest floor beneath the quota.)
                if spawned.is_none() {
                    // u64::MAX is the "could not spawn" sentinel the shell recognises.
                    crate::sched::ipc_send(result_ep, [u64::MAX, 0, 0]);
                }
            }
        })
        .expect("could not spawn the process service"); // once, at boot, not attacker-reachable

        // The shell itself, its own binary now (19f.5).
        crate::sched::spawn(move || {
            run(
                shell,
                Spawn {
                    arg0: 0, // no role selector: shell is its own binary
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(console.request, Rights::WRITE), // 0: print
                        endpoint_cap(console.reply, Rights::READ),    // 1: console ack
                        endpoint_cap(line, Rights::READ),             // 2: read a line
                        endpoint_cap(spawn_ep, Rights::WRITE),        // 3: request a spawn
                        endpoint_cap(result_ep, Rights::READ),        // 4: worker result
                    ],
                    maps: &[
                        Mapping {
                            va: OUT_VA,
                            phys: console.shared_phys,
                            flags: Flags::user_data(),
                        },
                        Mapping {
                            va: LINE_VA,
                            phys: line_phys,
                            flags: Flags::user_rodata(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the shell");
    }
}

/// Milestone 11: hand a process an untyped budget and let it spend it.
#[allow(dead_code)]
pub mod untyped_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    const ROLE_UNTYPED_DEMO: u64 = 7;

    /// Carve `pages` of memory into an untyped region, hand it to a fresh process, and return the
    /// region id and the endpoint the process reports on. The kernel's ONE allocation is the
    /// untyped itself; everything the process maps afterward spends that, not the allocator.
    pub fn start(image: &'static [u8], pages: u64) -> Option<(u64, EpId)> {
        let region = crate::untyped::create(pages)?;
        let report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_UNTYPED_DEMO,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(region),                 // slot 0: the memory budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: report the result
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the untyped demo");

        Some((region, report))
    }
}

/// **The untyped-backed userspace heap** (milestone 27): spawn the `allocdemo` workload, the
/// first program that links `extern crate alloc`, with an untyped budget (slot 0) and a report
/// endpoint (slot 1). The program wires `user_rt::heap` as its global allocator, churns
/// `Vec`/`String`/`BTreeMap` with frees in arbitrary order, asserts every intermediate result
/// itself (a wrong value faults), and reports a magic word plus how many bytes of heap it
/// committed. Portable: the same test runs the riscv64 ELF on riscv and the aarch64 ELF on
/// aarch64, out of each arch's own initrd.
#[cfg(test)]
pub mod alloc_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    /// The demo caps its own heap at 64 pages; the budget must also cover the program's page
    /// tables and the heap's, so 96 pages is comfortable without being unbounded.
    pub const BUDGET_PAGES: u64 = 96;

    /// `load` maps one stack page, which suits the hand-sized programs; `alloc` collections
    /// (BTreeMap nodes, the fmt machinery behind `assert!`) burn more than 4 KiB of stack, so
    /// map three more pages below it. The demo found this the honest way: a data abort at
    /// 0x4ffff8, one word below the mapped page.
    const EXTRA_STACK_PAGES: u64 = 3;

    pub fn start(image: &'static [u8]) -> EpId {
        let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for allocdemo");
        let report = crate::sched::create_endpoint();

        let mut stack = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; EXTRA_STACK_PAGES as usize];
        for (k, m) in stack.iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for allocdemo stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the new process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = phys;
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(budget),                 // slot 0: the heap's budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: report the verdict
                    ],
                    maps: &stack,
                },
            )
        })
        .expect("could not spawn allocdemo");

        report
    }
}

#[cfg(test)]
mod heap_tests {
    use super::*;

    /// The full `alloc` surface holds on a real budget: collections allocate, free in arbitrary
    /// order, and freed memory is reused rather than leaked. The workload asserts each value
    /// internally and faults on any lie, so this test's magic-word check is the "it all ran"
    /// bit; the committed count proves the heap both grew (it allocated more than zero pages)
    /// and stayed inside its own 64-page cap, i.e. growth is demand-driven, not budget-eating.
    #[test_case]
    fn a_process_runs_alloc_collections_on_its_own_untyped() {
        let image = program("allocdemo").expect("no allocdemo program in the initrd archive");
        let report = alloc_service::start(image);
        let words = crate::sched::ipc_recv(report);
        assert_eq!(
            words[0], 0xA110_C0DE,
            "allocdemo did not complete its heap workout",
        );
        let committed = words[1];
        assert!(committed > 0, "the heap never grew: nothing was allocated?");
        assert!(
            committed <= 64 * 4096,
            "the heap grew past its own cap: growth policy is broken",
        );
        assert!(
            committed.is_multiple_of(4096),
            "committed bytes must be whole pages",
        );
    }
}

/// **The display: virtio-gpu, a confined driver, and a client that draws** (milestone 29, rung one
/// of the display ladder).
///
/// Arch-neutral on purpose, unlike most of the device tests here: the driver and the client are
/// portable binaries in both archives, the transport is the same PCIe seam on both boards, and the
/// contract is one host-tested crate, so **both ISAs run literally this test** rather than two
/// copies of it that can drift (DECISIONS §19: parity is a gate).
#[cfg(test)]
mod display_tests {
    use super::*;
    use crate::sched;
    use gfx_proto as gfx;

    /// **A confined userspace driver puts a known pattern in a scanout framebuffer.**
    ///
    /// # What this proves, precisely
    ///
    /// The pattern is a per-coordinate function ([`gfx_proto::pixel`]), not a fill, and the digest is
    /// position sensitive, so a blank, stale, shifted, transposed, or truncated surface cannot pass
    /// (crates/gfx_proto's host tests assert exactly those properties of the pattern itself). Two
    /// independent witnesses report it, from two different address spaces: the **client** digests the
    /// surface after the flush through its own mapping, and the **driver** digests it through a
    /// different mapping after the device reported the transfer complete. The kernel compares both
    /// against a value it computed itself from the contract, so neither process is grading its own
    /// homework.
    ///
    /// It also proves the device could reach those exact frames and no others: the surface lives
    /// inside the driver's registered DMA region, the IOMMU domain maps exactly that region, and
    /// `RESOURCE_ATTACH_BACKING` naming it succeeded, which under translation only happens if the
    /// address translated.
    ///
    /// # What this test does NOT prove, and what does
    ///
    /// **This test proves the framebuffer, not the scanout.** The suite runs `-display none`, and
    /// nothing inside the guest can read back QEMU's host-side surface, so "the bytes we handed the
    /// device are the bytes it read out of our frames" is as far as an *in-guest* test reaches. A
    /// wrong pixel *format* or a wrong scanout rectangle would pass this while showing garbage on a
    /// real screen.
    ///
    /// The scanout is proven **from the host instead**, because only the host can see it:
    /// `cargo xtask`'s scanout check drives QEMU's monitor beside this suite, dumps the scanout with
    /// `screendump` (which works headlessly), and compares the PPM against `gfx_proto::pixel` pixel
    /// for pixel, on both ISAs. Together the two halves cover the whole path. See
    /// notes/framebuffer-contract.md, "Proving the scanout, from the host".
    #[test_case]
    fn a_confined_userspace_driver_puts_a_known_pattern_in_a_framebuffer() {
        let gpud = program("gpud").expect("no gpud program in the initrd archive");
        let painter = program("painter").expect("no painter program in the initrd archive");
        let threads_before = sched::thread_count();

        // A GPU asked for but not enumerated is a build-order mistake wearing a machine fact's
        // clothes, the hazard the runners were taught to fail loudly on. The test legs always attach
        // one, so absence is a failure, not a skip.
        let (driver_report, client_report) = display_service::start(gpud, painter).expect(
            "no virtio-gpu-pci function on the bus: is CRICKER_GPU missing from the test leg, or \
             the -device virtio-gpu-pci line from the runner?",
        );

        // And a GPU present while the IOMMU is not means every pixel read is bypassing translation.
        // That matters more for a GPU than for a disk: its backing addresses ride in a device-level
        // command payload, not in a descriptor, so the transport's validator never sees them and the
        // IOMMU is the only thing bounding them (notes/framebuffer-contract.md).
        assert!(
            crate::iommu::active(),
            "a virtio-gpu is present but the IOMMU is not active: the GPU's pixel reads are \
             unconfined (is iommu=smmuv3 / -device riscv-iommu-pci or iommu_platform=on missing?)",
        );

        // 1. The driver came up: device enumerated, resource created and backed, scanout set. Taken
        //    first because these are rendezvous SENDs, so the driver is parked here until we look.
        let [tag, geometry, display, ..] = sched::ipc_recv(driver_report);
        assert_eq!(
            tag,
            gfx::status::UP,
            "the display driver did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte \
             is the bring-up step that failed, see user/src/gpud.rs)",
        );
        assert_eq!(
            geometry,
            gfx::WIDTH as u64 | ((gfx::HEIGHT as u64) << 32),
            "the driver created a surface of the wrong geometry",
        );
        assert!(
            (display & 0xffff_ffff) >= gfx::WIDTH as u64,
            "the device reported a display narrower than our surface: {display:#x}",
        );

        // 2. The driver's own account of what it handed the device, digested in the driver's address
        //    space after the device completed the transfer. This must be taken BEFORE the client's
        //    verdict: the driver blocks in this SEND right after replying to the first flush, so a
        //    test that waited on the client first would deadlock against the client's second CALL.
        let [tag, driver_digest, pixels, ..] = sched::ipc_recv(driver_report);
        assert_eq!(
            tag,
            gfx::status::FLUSHED,
            "the driver never served a flush (it reported {tag:#x})",
        );
        assert_eq!(
            pixels,
            gfx::PIXELS as u64,
            "the driver flushed a different surface size"
        );
        assert_eq!(
            driver_digest,
            gfx::expected_checksum(),
            "the driver's digest of the frames it handed the device is not the pattern: the pixels \
             the device read are not the pixels the client painted",
        );

        // 3. The client's verdict: the surface read back through its own mapping after the flush.
        let [tag, client_digest, mismatch, ..] = sched::ipc_recv(client_report);
        assert_eq!(
            tag,
            gfx::status::PAINTED,
            "the painting client did not report a verdict (it reported {tag:#x}; a 0xDEAD_.. word's \
             low byte names the step, see user/src/painter.rs)",
        );
        assert_eq!(
            mismatch,
            gfx::NO_MISMATCH,
            "the client read back a wrong pixel at index {mismatch} of {}",
            gfx::PIXELS,
        );
        assert_eq!(
            client_digest,
            gfx::expected_checksum(),
            "the client's read-back digest is not the pattern it painted",
        );

        // The two witnesses must agree. They are digests of the same frames taken from different
        // address spaces at different moments, so a disagreement would mean the surface changed under
        // one of them, which is the mapping bug this shared-frame contract exists to not have.
        assert_eq!(
            driver_digest, client_digest,
            "the driver and the client disagree about the surface's contents",
        );

        // **Wait for the one-shot client to be reaped before returning.** Two reasons, and the second
        // is why this is not optional. It proves a client that finished is reaped rather than leaked,
        // the discipline every one-shot program here follows. And a process's frames come back
        // asynchronously at reap, so a client still unreaped when this test returns drops ~20 frames
        // into whatever the *next* test measures; `destroy_force_kills_a_runaway_and_reclaims_its_
        // region` asserts an exact free-frame count and failed on precisely that. The driver is a
        // long-lived server and never exits, so the target is one thread above the baseline.
        //
        // Clock-based, not a yield count: with work spread across cores a yield on an idle core
        // returns instantly and a fixed count elapses in almost no real time (DECISIONS §28).
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline && sched::thread_count() > threads_before + 1 {
            sched::yield_now();
        }
        assert!(
            sched::thread_count() <= threads_before + 1,
            "the painting client reported its verdict but was never reaped (threads {} > {})",
            sched::thread_count(),
            threads_before + 1,
        );
    }

    /// **The device is refused a framebuffer it was not granted.**
    ///
    /// The GPU raises a confinement question a disk and a NIC do not, and this is the test that
    /// answers it. Everywhere else, every address a device will touch arrives in a virtqueue
    /// descriptor, so the kernel validates it and copies it into a shadow ring the driver cannot
    /// reach (notes/dma.md). A virtio-gpu's *backing* addresses arrive inside a device-level command
    /// payload instead. The kernel bounds the descriptor that carries the command, but the addresses
    /// within it are bytes it does not parse, and it deliberately does not start parsing them:
    /// teaching the transport to read virtio-gpu commands would put device knowledge in the one place
    /// §18 keeps device-neutral.
    ///
    /// So the IOMMU is the barrier, and this proves it holds **in hardware**, the same way and with
    /// the same evidence milestone 16b's confinement test does: the fault the IOMMU recorded. A driver
    /// with exactly the honest driver's authority asks the device to read pixels out of a frame the
    /// kernel deliberately left out of its domain, and then to transfer from it. The IOMMU must fault
    /// at that frame.
    ///
    /// **The device's response code is deliberately not the assertion, and that is a finding.** The
    /// first version of this test asserted the command came back refused, and it did not: QEMU's DMA
    /// layer answers a translation failure by handing the device a *bounce buffer* instead of failing
    /// the mapping, so `RESOURCE_ATTACH_BACKING` returns OK while the bytes the device actually gets
    /// are not the victim frame's. The confinement held; only the error reporting did not survive the
    /// trip. So the fault queue is the fact, the response code is printed for the record, and the
    /// nuance is written down rather than smoothed over (notes/framebuffer-contract.md).
    ///
    /// **Runs BEFORE the happy-path test, and the name is what makes that true.** This test resets
    /// and re-registers the same physical GPU (each driver programs the device from scratch, and a
    /// virtio reset destroys every resource and scanout), the same way the disk's attacker tests share
    /// one device with the honest driver. If it ran second it would wipe the pattern the pixel test
    /// put on the scanout, and the host-side scanout check (`cargo xtask`'s `gpu_shot`, which dumps
    /// the scanout while the suite runs) would find nothing to match. Sorting first is why this is
    /// named `a_backing...` rather than `the_iommu...`. A reordering does not corrupt anything; it
    /// fails the scanout check loudly, which is the right way to be wrong.
    #[test_case]
    fn a_backing_outside_the_grant_is_refused_by_the_iommu() {
        let gpud = program("gpud").expect("no gpud program in the initrd archive");

        // Drain any stale fault first, so what we observe is this test's.
        while crate::iommu::take_fault().is_some() {}

        let (report, victim) = display_service::start_backing_escape(gpud).expect(
            "no virtio-gpu-pci function on the bus: is CRICKER_GPU missing from the test leg?",
        );
        assert!(
            crate::iommu::active(),
            "a virtio-gpu is present but the IOMMU is not active: nothing would refuse this escape, \
             so the test would pass or fail on a fiction",
        );

        let [tag, response, ..] = sched::ipc_recv(report);
        assert_eq!(
            tag,
            gfx::status::BACKING,
            "the escape driver did not reach its attach (it reported {tag:#x}; a 0xDEAD_.. word's \
             low byte names the bring-up step, see user/src/gpud.rs)",
        );

        // The evidence. QEMU records the fault as it processes the command under TCG, so a bounded
        // spin is plenty; the bound turns "no fault ever" into a failure rather than a hang.
        let mut fault = None;
        for _ in 0..2_000_000 {
            if let Some(f) = crate::iommu::take_fault() {
                fault = Some(f);
                break;
            }
            core::hint::spin_loop();
        }
        let f = fault.unwrap_or_else(|| {
            panic!(
                "the GPU was pointed at {victim:#x}, outside its DMA region, and the IOMMU recorded \
                 no fault (the device answered the attach with {response:#x}): a backing address \
                 rides in a command payload the transport validator cannot see, so if the IOMMU is \
                 not bounding it, nothing is",
            )
        });
        assert_eq!(
            f.addr & !0xfff,
            victim & !0xfff,
            "the IOMMU faulted, but on {:#x} (code {:#x}, rid {:#x}), not the frame the GPU was \
             pointed at ({victim:#x})",
            f.addr,
            f.code,
            f.rid,
        );

        // Leave the fault queue as we found it. Not tidiness: the RISC-V IOMMU's queue holds 128
        // records and the driver does not clear its overflow bit, so records left behind here cost a
        // later test its own fault assertion. The escape above is sized to produce one fault for the
        // same reason (user/src/gpud.rs).
        while crate::iommu::take_fault().is_some() {}
    }
}

/// **Rust `std` on the native ABI** (milestone 27): spawn the `hellostd` demo, an ordinary Rust
/// program (no `no_std`, no attributes) built for the `*-unknown-cricker` custom target with std's
/// PAL implemented directly over the capability ABI. It gets the same two grants as `allocdemo`,
/// an untyped budget (slot 0, which the std `GlobalAlloc` draws the heap from) and an endpoint
/// (slot 1, which `println!` SENDs to). Its stdout is a fixed, deterministic transcript the test
/// reassembles from the endpoint and checks byte for byte. Portable: the aarch64 ELF runs on
/// aarch64 and the riscv64 ELF on riscv, out of each arch's own initrd.
#[cfg(test)]
pub mod std_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    /// The heap high-water for the demo's Vec/String/HashMap workout plus std's own runtime
    /// allocations and the heap's page tables is well under 1 MiB; 256 pages is comfortable, and
    /// the initial region only needs to be contiguous at spawn, when memory is unfragmented.
    pub const BUDGET_PAGES: u64 = 256;

    /// std's startup, formatting machinery, and collection code use far more stack than a
    /// hand-written `no_std` worker. `load` maps one stack page; map 32 more below it (128 KiB
    /// total), generous so a stack-depth surprise is not what a first std bring-up debugs.
    const EXTRA_STACK_PAGES: u64 = 32;

    pub fn start(image: &'static [u8]) -> EpId {
        let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for hellostd");
        let report = crate::sched::create_endpoint();

        let mut stack = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; EXTRA_STACK_PAGES as usize];
        for (k, m) in stack.iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for hellostd stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the new process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = phys;
        }

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(budget),                 // slot 0: the heap's budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: stdout/stderr
                    ],
                    maps: &stack,
                },
            )
        })
        .expect("could not spawn hellostd");

        report
    }
}

#[cfg(test)]
mod std_tests {
    use super::*;

    /// Reassemble a std program's stdout off its endpoint until `want` bytes have arrived, and
    /// compare byte for byte. The PAL SENDs 16 bytes per message (w0 = byte count, w1|w2 = the
    /// bytes, little-endian), and `SEND` blocks until a receiver takes it, so taking exactly
    /// `want.len()` bytes consumes exactly the messages the program sent, and the program has
    /// reached `SYS_EXIT` by the time the last one lands.
    ///
    /// Shared by every std test on both ISAs (the arch-gated test modules reach it here), so all of
    /// them assert the same way and a drift in one is a diff in one place.
    pub(super) fn assert_std_transcript(report: crate::sched::EpId, want: &[u8], what: &str) {
        let mut got = [0u8; 512];
        let mut len = 0usize;
        while len < want.len() {
            let words = crate::sched::ipc_recv(report);
            let count = words[0] as usize;
            assert!(
                (1..=16).contains(&count),
                "{what}: stdout message with a bad byte count: {count}"
            );
            let mut chunk = [0u8; 16];
            chunk[..8].copy_from_slice(&words[1].to_le_bytes());
            chunk[8..].copy_from_slice(&words[2].to_le_bytes());
            for &b in &chunk[..count] {
                assert!(len < got.len(), "{what}: printed more than the transcript");
                got[len] = b;
                len += 1;
            }
        }
        assert_eq!(
            &got[..len],
            want,
            "{what}: stdout did not match the transcript"
        );
    }

    /// Consume the FS service's two readiness sentinels, if this caller is the one that wired it.
    ///
    /// One boot has one FS service (the block server owns the device), so the hand-written client's
    /// test and the `std::fs` test share it, and only the first of them to run gets the sentinels.
    /// Asserting on them where they exist is what separates a hang in the mount from one in the
    /// serve path.
    pub(super) fn assert_fs_service_ready(
        readiness: Option<(crate::sched::EpId, crate::sched::EpId)>,
    ) {
        let Some((blk_ready, ready)) = readiness else {
            return;
        };
        assert_eq!(
            crate::sched::ipc_recv(blk_ready)[0],
            fs_proto::fixture::READY,
            "the block server did not bring the RedoxFS device up",
        );
        assert_eq!(
            crate::sched::ipc_recv(ready)[0],
            fs_proto::fixture::READY,
            "the FS server did not open the RedoxFS image",
        );
    }

    /// Build the exact bytes `hellostd` prints when it is granted a directory capability, into
    /// `buf`; returns the length. Not a `const` because the motd's contents are spliced in from the
    /// shared fixture, and that is the load-bearing part: those bytes came off the RedoxFS image,
    /// through the FS server, through `std::fs`, and out the stdout endpoint.
    pub(super) fn std_fs_expected(buf: &mut [u8; 512]) -> usize {
        // The lengths spelled out below are the motd's; if the fixture changes, fail here rather
        // than in a byte comparison nobody can read.
        assert_eq!(
            fs_proto::fixture::MOTD.len(),
            70,
            "the motd fixture changed; the expected transcript's lengths must change with it",
        );
        let mut n = 0;
        for part in [
            b"std fs on cricker-os\n".as_slice(),
            fs_proto::fixture::MOTD,
            b"read_to_string 70\nmetadata len 70\n".as_slice(),
            b"absolute refused\ndotdot refused\nnested refused\n".as_slice(),
            b"missing not found\ncreate unsupported\n".as_slice(),
            b"write readback ok\nfs ok\n".as_slice(),
        ] {
            buf[n..n + part.len()].copy_from_slice(part);
            n += part.len();
        }
        n
    }

    /// The exact bytes `hellostd` prints, in order. `println!` is line-buffered and every line
    /// ends in `\n`, so the whole transcript is flushed by the time the program exits. Pinned here
    /// so a drift in std's behaviour, the PAL, or the demo is a loud diff rather than a mystery.
    /// `os cricker` proves `std::env::consts::OS` resolves through the patched `env_consts`; the
    /// two `unsupported` lines prove `fs`/`net` refuse honestly rather than pretend.
    const EXPECTED: &[u8] = b"hello from std on cricker-os\n\
        os cricker\n\
        vec sum 149985000\n\
        string len 800\n\
        map lookup 1369\n\
        fs honestly unsupported\n\
        net honestly unsupported\n\
        instant monotonic ok\n";

    /// A whole Rust `std` program runs on the native ABI and its output is exactly right.
    ///
    /// Granted only a heap and a stdout endpoint, so both `fs` and `net` refuse: the two
    /// `unsupported` lines in the transcript are "no ambient filesystem" and "no ambient network"
    /// felt from inside std, on a binary that also runs both for real when it is granted them.
    #[test_case]
    fn a_whole_std_program_runs_on_the_native_abi() {
        let image = program("hellostd").expect("no hellostd program in the initrd archive");
        let report = std_service::start(image);
        assert_std_transcript(report, EXPECTED, "hellostd");
    }
}

/// **Capability delegation: authority moves between processes at runtime.**
///
/// Every other capability in cricker-os is minted by the kernel and handed to a process at spawn.
/// That made the kernel a central authority-granting oracle, which is the ambient-authority shape
/// §10 argued against, just relocated. A capability system's defining move is that a process can
/// pass authority it holds to another process, narrowing it on the way, and only if it was trusted
/// to (`GRANT`). This wires the smallest scenario that exercises all three: a *granter* delegates a
/// resource capability to a *receiver* over a channel, narrowed to `WRITE` (no `GRANT`); the
/// receiver uses it and then cannot pass it on. See user/src/hello.rs granter()/receiver().
/// **Frame capabilities: shared memory a process holds, maps, and delegates.**
///
/// The payoff of delegation applied to memory. A *producer* retypes a page out of its own untyped
/// into a `Frame` capability, maps it, writes into it, and delegates a READ-only view to a
/// *consumer*, which maps the same physical page and reads what the producer wrote. The kernel
/// copies nothing and pre-arranges nothing: the two processes compose the sharing themselves, and
/// the read-only narrowing means the consumer can look but not write. See user/src/hello.rs
/// frame_producer()/frame_consumer().
#[cfg(test)]
pub mod frame_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    const ROLE_PRODUCER: u64 = 11;
    const ROLE_CONSUMER: u64 = 12;

    /// Spawn the pair, each with its own untyped budget, and return the endpoint the consumer
    /// reports its verdict on. Eight pages of untyped apiece covers one frame plus the page tables
    /// each side needs to map it.
    pub fn wire(image: &'static [u8]) -> EpId {
        let channel = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();
        let prod_ut = crate::untyped::create(8).expect("no untyped for the frame producer");
        let cons_ut = crate::untyped::create(8).expect("no untyped for the frame consumer");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_PRODUCER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(prod_ut),                 // slot 0: retype the frame + page tables
                        endpoint_cap(channel, Rights::WRITE), // slot 1: delegate the frame
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the frame producer");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_CONSUMER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::READ), // slot 0: receive the frame
                        untyped_cap(cons_ut),                // slot 1: page tables for its mappings
                        endpoint_cap(report, Rights::WRITE), // slot 2: report the verdict
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the frame consumer");

        report
    }
}

#[cfg(test)]
pub mod delegation_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};
    use crate::sched::EpId;

    const ROLE_GRANTER: u64 = 9;
    const ROLE_RECEIVER: u64 = 10;

    /// The word the receiver sends back through the delegated capability, so a test can confirm a
    /// capability minted by one process works when invoked by another.
    pub const USED_WORD: u64 = 0x5A;

    /// Spawn the pair and return `(resource endpoint, report endpoint)`. The granter delegates its
    /// `resource` capability (held `WRITE | GRANT`) to the receiver, narrowed to `WRITE`. The
    /// receiver `SEND`s [`USED_WORD`] on the received capability (a `RECV` on `resource` collects
    /// it) and reports a two-bit verdict on `report`.
    pub fn wire(image: &'static [u8]) -> (EpId, EpId) {
        let channel = crate::sched::create_endpoint(); // granter SEND_CAP -> receiver RECV_CAP
        let resource = crate::sched::create_endpoint(); // the capability being delegated
        let loopback = crate::sched::create_endpoint(); // the receiver's refused re-delegation target
        let report = crate::sched::create_endpoint(); // the receiver's verdict

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_GRANTER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::WRITE), // slot 0: SEND_CAP over it
                        endpoint_cap(resource, Rights::WRITE.union(Rights::GRANT)), // slot 1: delegate this
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the delegation granter");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_RECEIVER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::READ),   // slot 0: RECV_CAP
                        endpoint_cap(report, Rights::WRITE),   // slot 1: report the verdict
                        endpoint_cap(loopback, Rights::WRITE), // slot 2: attempt re-delegation here
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the delegation receiver");

        (resource, report)
    }
}

/// **Milestone 19a: a process mints an endpoint from its own memory, at EL0.** The maker holds
/// an untyped budget and a channel; the peer holds the channel and a report line. Everything
/// else, the endpoint itself included, is created at runtime by the maker out of its own pages
/// and delegated. See user/src/hello.rs ep_maker()/ep_user().
#[cfg(test)]
pub mod retype_ep_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    const ROLE_MAKER: u64 = 17;
    const ROLE_USER: u64 = 18;

    /// Spawn the pair; returns the report endpoint carrying the word that crossed the minted
    /// endpoint.
    pub fn wire(image: &'static [u8]) -> EpId {
        let channel = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();
        let region = crate::untyped::create(4).expect("no region for the maker's budget");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_MAKER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(region),                  // slot 0: the budget to mint from
                        endpoint_cap(channel, Rights::WRITE), // slot 1: delegate the mint here
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the endpoint maker");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_USER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(channel, Rights::READ), // slot 0: receive the delegation
                        endpoint_cap(report, Rights::WRITE), // slot 1: report the word
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the endpoint user");

        report
    }
}

/// **Milestone 19b: a process builds an address space, at EL0.** One role: an untyped budget
/// and a report line; everything else it constructs. See user/src/hello.rs aspace_builder().
#[cfg(test)]
pub mod aspace_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    const ROLE_BUILDER: u64 = 19;

    /// Spawn the builder; returns the report endpoint carrying its verdict bits.
    pub fn wire(image: &'static [u8]) -> EpId {
        let report = crate::sched::create_endpoint();
        let region = crate::untyped::create(8).expect("no region for the builder");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_BUILDER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(region),                 // slot 0: the budget
                        endpoint_cap(report, Rights::WRITE), // slot 1: the verdict
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the aspace builder");

        report
    }
}

/// **Milestone 12: Call/Reply, at EL0.** One request endpoint, a server that answers a caller it was
/// never wired to, and the one-shot reply capability proven across the boundary. See
/// user/src/hello.rs call_server()/call_client().
#[cfg(test)]
pub mod call_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};
    use crate::sched::EpId;

    const ROLE_SERVER: u64 = 14;
    const ROLE_CLIENT: u64 = 15;

    /// Spawn the pair, sharing one request endpoint. Returns `(client reply report, server one-shot
    /// report)`: the client publishes the reply it got, the server publishes whether a second reply
    /// was refused.
    pub fn wire(image: &'static [u8]) -> (EpId, EpId) {
        let ep = crate::sched::create_endpoint(); // client CALL <-> server RECV_CAP
        let call_report = crate::sched::create_endpoint();
        let oneshot_report = crate::sched::create_endpoint();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_SERVER,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(ep, Rights::READ),              // slot 0: RECV calls
                        endpoint_cap(oneshot_report, Rights::WRITE), // slot 1: report the verdict
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the call server");

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_CLIENT,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(ep, Rights::WRITE),          // slot 0: CALL
                        endpoint_cap(call_report, Rights::WRITE), // slot 1: report the reply
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the call client");

        (call_report, oneshot_report)
    }
}

/// **Milestone 13: revoke a frame, at EL0.** One process with an untyped budget retypes a frame,
/// maps it, revokes it, and reports whether the revoke deleted its own capability. See
/// user/src/hello.rs revoke_demo().
#[cfg(test)]
pub mod revoke_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    const ROLE_REVOKE_DEMO: u64 = 16;

    /// Spawn the demo with an 8-page untyped budget; returns the endpoint it reports its verdict on.
    pub fn wire(image: &'static [u8]) -> EpId {
        let region = crate::untyped::create(8).expect("no untyped for the revoke demo");
        let report = crate::sched::create_endpoint();
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_REVOKE_DEMO,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        untyped_cap(region),                 // slot 0: retype + page tables
                        endpoint_cap(report, Rights::WRITE), // slot 1: report the verdict
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn the revoke demo");
        report
    }
}

// The in-kernel userspace test suite is aarch64-specific: every test drives a hand-written aarch64
// program (`outlaw`, `spin`, `forged_elf`) through `exec`, and reads aarch64 fault registers
// (`ESR`/`FAR`). RISC-V's userspace path is exercised by the boot tour instead (the `worker`,
// `builder`, and `driver` programs; see the riscv block in main.rs and notes/riscv-parity-scope.md).
// Porting these to riscv would mean hand-writing 37 programs' worth of riscv machine code for no new
// coverage of portable logic, so they stay aarch64.
#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::*;
    use crate::arch::exceptions::{
        LAST_USER_FAULT_ESR, LAST_USER_FAULT_FAR, SVC_COUNT, USER_FAULTS,
    };
    use crate::arch::timer;
    use crate::sched;
    // The std-transcript and FS-readiness assertions live with the std tests so both ISAs share one
    // copy; see `std_tests`.
    use super::std_tests::{assert_fs_service_ready, assert_std_transcript, std_fs_expected};
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// The `init` program's ELF bytes, pulled out of the initrd archive by name (milestone 19f). A
    /// test that loads a real user program wants the program's bytes, not the whole crickerfs
    /// archive; only the `spawn_init` tests pass the archive, because init parses it itself. Named
    /// to avoid the module's `hello` (a tiny hand-written 7a program, `user_program!` at the top).
    fn init_image() -> &'static [u8] {
        program("init").expect("no init program in the initrd archive")
    }

    /// The `worker` program's ELF bytes (milestone 19f.2), a distinct binary in the archive, not a
    /// role of the init/hello binary. `_start(x0, x1, x2)` reads its input in `x1` and needs no
    /// role selector.
    fn worker_image() -> &'static [u8] {
        program("worker").expect("no worker program in the initrd archive")
    }

    /// The `netd` program's ELF bytes (milestone 30, piece 3): the smoltcp net server, a distinct
    /// binary loaded by name.
    fn netd_image() -> &'static [u8] {
        program("netd").expect("no netd program in the initrd archive")
    }

    /// The net client's test selectors and its success word, matching user/src/netcli.rs. The
    /// client is a nonzero entry role of the `netd` binary, so it needs no image of its own.
    const NET_TEST_UDP_DNS: u64 = 1;
    const NET_TEST_TCP_ECHO: u64 = 2;
    const NET_TEST_TCP_REOPEN: u64 = 3;
    const NET_CLIENT_OK: u64 = 1;

    /// Spin the scheduler until `done()`, or give up after a wall-clock deadline. Returns whether it
    /// happened. **Time-based, not a fixed yield count** (DECISIONS §28): with work spread across
    /// cores, the test thread's own core is often idle, so a yield returns at once and a fixed count
    /// of them elapses in almost no real time, timing out before a parallel result on another core
    /// lands. A ~2 s deadline gives the other cores real time to finish while staying far under the
    /// 60 s hang watchdog, so a genuine hang still fails.
    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if done() {
                return true;
            }
            sched::yield_now();
        }
        done()
    }

    /// **We are running on `SP_EL1`, and the whole trap frame depends on it.**
    ///
    /// At EL1 the name `sp` means `SP_EL1` if `SPSel.SP == 1`, and `SP_EL0` if it is 0. Every
    /// `SAVE_CONTEXT` in the kernel does `sub sp, sp, #272` and every user entry does
    /// `msr sp_el0, x3`. If `SPSel` were 0, those two would be **the same register**, and the
    /// kernel would restore a user stack pointer straight into its own stack pointer.
    ///
    /// This has been true since boot.s and we never checked it. A test that can only fail if
    /// the world is upside down is still worth having when the failure is silent.
    #[test_case]
    fn el1_runs_on_sp_el1() {
        let spsel: u64;
        // SAFETY: reading SPSel has no side effects.
        unsafe { core::arch::asm!("mrs {}, spsel", out(reg) spsel, options(nostack, nomem)) };

        assert_eq!(
            spsel & 1,
            1,
            "SPSel says EL1 is using SP_EL0: the trap frame's sp_el0 field aliases the \
             kernel's own stack pointer"
        );
    }

    /// EL0. The boundary.
    ///
    /// Two `svc`s, not one, and that is the point: the second can only happen if the `eret`
    /// **put us back at EL0** after the first. One `svc` proves we left. Two prove we came back.
    #[test_case]
    fn a_user_program_reaches_el0_and_returns_twice() {
        let before = SVC_COUNT.load(Ordering::Relaxed);

        sched::spawn(|| unsafe { exec(hello()) }).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) >= before + 2),
            "saw {} svc from EL0, wanted 2",
            SVC_COUNT.load(Ordering::Relaxed) - before,
        );
    }

    /// **The privilege boundary is real, and it is a PERMISSION fault, not a missing page.**
    ///
    /// The address the user reaches for is mapped, and readable, and the kernel reads it all
    /// day. The hardware picks `TTBR1` from bits 63:48, walks the kernel's own tables, finds
    /// the page, reads the `AP` bits, and says no.
    ///
    /// So `DFSC = 0b001111` (permission fault) rather than a translation fault is the whole
    /// assertion. A translation fault would mean we had merely failed to map something, which
    /// would pass a sloppier test and prove nothing at all.
    #[test_case]
    fn a_user_program_cannot_read_a_kernel_address() {
        const KERNEL_ADDR: u64 = 0xffff_0000_4008_0000;

        let before = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > before),
            "the user program read a kernel address and was NOT stopped",
        );

        let esr = LAST_USER_FAULT_ESR.load(Ordering::Relaxed);
        let far = LAST_USER_FAULT_FAR.load(Ordering::Relaxed);

        assert_eq!((esr >> 26) & 0x3f, 0x24, "not a data abort from a lower EL");
        assert_eq!(esr & 0x3f, 0x0f, "not a PERMISSION fault: esr {esr:#x}");
        assert_eq!(esr & (1 << 6), 0, "not a read");
        assert_eq!(far, KERNEL_ADDR, "faulted on the wrong address");

        // And the kernel is executing this line, which is the other half of the claim.
    }

    /// DECISIONS §5's arbitrary ELF binary, at EL0, in the flesh.
    ///
    /// A program with no yield, no syscall, and not even a function call. The **only** thing in
    /// the universe that can take the CPU back from it is a timer interrupt landing between two
    /// of its instructions. Milestone 6 proved this for a kernel thread we compiled. This is the
    /// case that actually mattered.
    #[test_case]
    fn a_user_program_that_never_yields_is_preempted_anyway() {
        let preemptions = sched::preemptions();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(|| unsafe { exec(spin()) }).expect("spawn failed");

        // Give it the CPU and then take it back, without asking.
        timer::spin_for(timer::frequency() / 10);

        assert!(
            sched::preemptions() > preemptions,
            "nothing was preempted while a user thread spun at EL0",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the spinning user thread faulted; it was supposed to just spin",
        );

        // And we are here, running, having taken the CPU back from a program that never
        // offered it.
    }

    /// Forge an ELF64 header by hand, so a test can ask for something no linker would emit.
    ///
    /// A fixed buffer, since the kernel it tests has no heap (milestone 14 phase C): one ELF
    /// header, one program header, sixteen bytes of code. The ELF **names its own load
    /// address**, and this is the file that names the kernel's.
    fn forged_elf(vaddr: u64, flags: u32) -> [u8; 136] {
        const EHDR: usize = 64;
        const PHDR: usize = 56;
        let code: [u8; 16] = [0; 16];

        let mut out = [0u8; EHDR + PHDR + 16];
        out[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        out[4] = 2; // ELFCLASS64
        out[5] = 1; // little-endian
        out[6] = 1; // EV_CURRENT
        out[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        out[18..20].copy_from_slice(&183u16.to_le_bytes()); // EM_AARCH64
        out[24..32].copy_from_slice(&vaddr.to_le_bytes()); // e_entry
        out[32..40].copy_from_slice(&(EHDR as u64).to_le_bytes()); // e_phoff
        out[54..56].copy_from_slice(&(PHDR as u16).to_le_bytes());
        out[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        let p = EHDR;
        out[p..p + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        out[p + 4..p + 8].copy_from_slice(&flags.to_le_bytes());
        out[p + 8..p + 16].copy_from_slice(&((EHDR + PHDR) as u64).to_le_bytes()); // p_offset
        out[p + 16..p + 24].copy_from_slice(&vaddr.to_le_bytes()); // p_vaddr
        out[p + 32..p + 40].copy_from_slice(&(code.len() as u64).to_le_bytes()); // p_filesz
        out[p + 40..p + 48].copy_from_slice(&(code.len() as u64).to_le_bytes()); // p_memsz
        out
    }

    /// **A binary that asks to be loaded over the kernel.**
    ///
    /// This is the attack. An ELF names its own load address, so a hostile one simply names
    /// `0xffff_0000_4008_0000` and waits to see whether the loader is credulous.
    ///
    /// It is refused **by construction, not by a check we remembered to write**: the user
    /// `Mapper` is built with `Half::Low`, and a high address is not a thing it can express. The
    /// same `WrongHalf` guard has been in `paging` since milestone 4, put there because a *host*
    /// test discovered that bits 63:48 are not translated. It has been waiting for this file.
    #[test_case]
    fn an_elf_that_asks_to_be_loaded_over_the_kernel_is_refused() {
        let image = forged_elf(0xffff_0000_4008_0000, elf::PF_R | elf::PF_X);

        assert_eq!(
            load(&image).err(),
            Some(LoadError::Unmappable(MapError::WrongHalf)),
            "the kernel agreed to map a user program on top of itself",
        );
    }

    /// And a binary asking for a page that is both writable and executable.
    ///
    /// Caught in `crates/elf`, on the host, in microseconds. But assert it end-to-end too: the
    /// value of the host test is that it is fast, not that it is the only line of defence.
    #[test_case]
    fn an_elf_that_asks_for_a_writable_executable_page_is_refused() {
        let image = forged_elf(0x40_0000, elf::PF_R | elf::PF_W | elf::PF_X);

        assert_eq!(
            load(&image).err(),
            Some(LoadError::NotLoadable(elf::Error::WritableAndExecutable)),
        );
    }

    /// Junk is refused, and refusing it does not take the kernel down.
    #[test_case]
    fn a_bad_binary_is_refused_rather_than_panicking() {
        assert!(load(b"#!/bin/sh\necho hi\n").is_err());
        assert!(load(&[]).is_err());
        assert!(load(&[0u8; 4096]).is_err());
        // And we are still executing, which is the assertion.
    }

    /// The initrd is there, and it is the program we built.
    #[test_case]
    fn the_initrd_holds_an_aarch64_executable() {
        let image = init_image();
        let e = elf::Elf::parse(image).expect("the initrd is not a loadable aarch64 ELF");

        assert_eq!(e.entry(), 0x40_0000, "linked somewhere unexpected");

        // Three segments, and NONE of them writable-and-executable. Counted straight off the
        // iterator: the kernel this test rides in has no heap to collect into (milestone 14).
        assert!(
            e.segments().count() >= 3,
            "expected .text, .rodata and .data"
        );
        assert!(e.segments().any(|s| s.is_executable() && !s.is_writable()));
        assert!(e.segments().any(|s| s.is_writable() && !s.is_executable()));

        // And one of them has a .bss: memsz > filesz. If this is not true, the test below is
        // vacuous, and we would never know.
        assert!(
            e.segments().any(|s| s.memsz as usize > s.data.len()),
            "no segment has a .bss, so the zero-fill is untested",
        );
    }

    /// **The whole of 7c.** A separately compiled binary, arriving in the initrd, running at EL0.
    ///
    /// The program checks its own image and speaks with the only two words it has: `svc` if
    /// every expectation about its own memory holds, `brk` if not. **No data crosses the
    /// boundary**, because there is no ABI yet and we are not going to invent one by accident.
    ///
    /// So `svc` and no fault means: `.text` executed, `.rodata` was readable, `.data` was copied
    /// from the file, `.bss` was zeroed (the file does not contain those bytes), and the stack
    /// worked well enough to recurse eight frames.
    #[test_case]
    fn a_real_elf_from_the_initrd_runs_at_el0_and_verifies_itself() {
        let svc = SVC_COUNT.load(Ordering::Relaxed);
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(|| exec_elf(init_image())).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) > svc),
            "the program never reached its `svc`",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the program reached EL0 and then FAILED its own self-check: one of \
             .text/.rodata/.data/.bss/stack was not what the ELF asked for",
        );
    }

    /// **The milestone 15 witness: address spaces stay apart with NO flush on switch.**
    ///
    /// Two spaces map the *same* virtual address to different frames holding different bytes.
    /// We install A and read through the VA (loading A's translation, tagged with A's ASID,
    /// into the TLB), then install B, which since milestone 15 flushes nothing, and read
    /// again. If user mappings were still global, or the ASID did not ride TTBR0, or two
    /// spaces shared a tag, B's read would hit A's still-cached entry and see A's byte: one
    /// process reading another's memory, the exact bug the sledgehammer flush used to prevent.
    #[test_case]
    fn asid_tagging_keeps_address_spaces_apart_without_flushes() {
        let mut a = AddressSpace::new(2).expect("no space A");
        let mut b = AddressSpace::new(2).expect("no space B");

        let (asid_a, asid_b) = (a.ttbr0() >> 48, b.ttbr0() >> 48);
        assert_ne!(asid_a, asid_b, "two live spaces share an ASID");
        assert_ne!(asid_a, 0, "a user space got the kernel's ASID 0");
        assert_ne!(asid_b, 0, "a user space got the kernel's ASID 0");

        const VA: u64 = 0x40_0000;
        a.map_new(VA, Flags::user_data()).expect("map A")[0] = 0xAA;
        b.map_new(VA, Flags::user_data()).expect("map B")[0] = 0xBB;

        // SAFETY: nothing is at EL0; we are a kernel thread mid-test, and each space outlives
        // its activation. The reads go through the live TTBR0 translation, which is the point.
        let (read_a, read_b) = unsafe {
            mmu::activate_user(a.ttbr0());
            let ra = core::ptr::read_volatile(VA as *const u8);
            mmu::activate_user(b.ttbr0()); // milestone 15: this flushes NOTHING
            let rb = core::ptr::read_volatile(VA as *const u8);
            mmu::deactivate_user();
            (ra, rb)
        };

        assert_eq!(read_a, 0xAA);
        assert_eq!(
            read_b, 0xBB,
            "B read A's byte: a stale TLB entry crossed address spaces, so the nG/ASID              tagging is broken",
        );
    }

    /// The loader honours the file's permissions, and does not widen them.
    ///
    /// An ELF's `.rodata` segment is `PF_R` alone. The tempting shortcut is to map every
    /// non-executable segment as `user_data()`, which is **writable** — quietly granting the
    /// program authority its own file never asked for.
    #[test_case]
    fn a_read_only_segment_is_mapped_read_only() {
        let image = init_image();
        let (space, _) = load(image).expect("the initrd did not load");

        let rodata = elf::Elf::parse(image)
            .unwrap()
            .segments()
            .find(|s| s.is_readable() && !s.is_writable() && !s.is_executable())
            .expect("the test binary has no read-only segment");

        // Install it so we can ask the CPU's own tables, rather than our record of them.
        // SAFETY: nothing is at EL0 right now; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.ttbr0()) };

        let (_, flags) = mmu::translate_user(rodata.vaddr).expect(".rodata is not mapped at all");

        assert!(
            flags.is_user_accessible(),
            "EL0 cannot read its own .rodata"
        );
        assert!(!flags.is_writable(), "the loader made .rodata WRITABLE");
        assert!(!flags.is_user_executable(), ".rodata is executable at EL0");
        assert!(
            !flags.is_kernel_executable(),
            ".rodata is executable at EL1"
        );

        mmu::deactivate_user();
        drop(space);
    }

    /// **The question the kernel must ask, asked of the hardware.**
    ///
    /// `AT S1E0R` means *translate this address as EL0 would, for a read*. One instruction, and
    /// it is the difference between a kernel and a confused deputy.
    ///
    /// Note the precondition assertion. Without it the test is vacuous: "EL0 cannot read the
    /// kernel's text" proves nothing if the kernel's text is not mapped in the first place.
    #[test_case]
    fn the_hardware_says_el0_cannot_read_the_kernels_memory() {
        const KERNEL_TEXT: u64 = 0xffff_0000_4008_0000;

        let (space, _) = load(init_image()).expect("the initrd did not load");

        // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.ttbr0()) };

        // The precondition, and it is what gives the assertion below its teeth: that address IS
        // mapped, and the KERNEL can read it. It reads it all day.
        assert!(
            mmu::translate(KERNEL_TEXT).is_some(),
            "the kernel's text is not mapped, so this test proves nothing",
        );

        // And EL0 cannot. Not "we decline to"; the silicon says no.
        assert!(
            !mmu::user_can_read(KERNEL_TEXT),
            "the hardware says EL0 could read the kernel's own text",
        );
        assert!(!mmu::user_can_write(KERNEL_TEXT));

        // It can read its own code, or the check is a rubber stamp that says no to everything.
        assert!(
            mmu::user_can_read(0x40_0000),
            "EL0 cannot read its own .text, so the check refuses everything and proves nothing",
        );

        // And not an address in its own half that nobody mapped.
        assert!(!mmu::user_can_read(0x7000_0000));

        mmu::deactivate_user();
        drop(space);
    }

    /// **A program with the capability can print. The same program without it cannot.**
    ///
    /// The binary is byte-identical. Nothing about it changed. What changed is what it was
    /// *handed*, and that is the entire content of DECISIONS §10.
    ///
    /// It reports by `brk`, which the kernel treats as a fault: the program expects `NoSuchSlot`
    /// from an empty slot and expects `BadPointer` when it asks the kernel to read the kernel's
    /// own memory, and it kills itself if either is wrong. So **no fault** means every one of
    /// those held.
    #[test_case]
    fn a_user_client_moves_data_through_shared_memory() {
        // What the client prints first. Must match user/src/hello.rs.
        const FIRST_LINE: &[u8] =
            b"      hello from EL0, printed by a driver that also runs at EL0.\n";
        const SHARED_VA: u64 = 0x0000_0000_0060_0000;

        static CAPTURED: AtomicBool = AtomicBool::new(false);
        static LEN: AtomicU64 = AtomicU64::new(0);
        static mut BUF: [u8; 128] = [0; 128];

        let image = init_image();
        let request = sched::create_endpoint();
        let reply = sched::create_endpoint();

        // The shared page, owned by the test (not by either address space), so `map_physical`
        // will not free it. Deliberately leaked: the client spins forever, so there is no safe
        // moment to reclaim it, and one page is a fine price for the test.
        let shared = crate::memory::alloc().expect("no shared frame").addr();

        // The server: a kernel thread that reads the shared page and records the first message.
        sched::spawn(move || {
            loop {
                let m = sched::ipc_recv(request);
                let len = m[0].min(128);
                if !CAPTURED.load(Ordering::SeqCst) {
                    // SAFETY: the shared frame is ours via the direct map; the client wrote `len`
                    // bytes before sending. Single-threaded capture.
                    let src = crate::arch::mmu::phys_to_virt(shared) as *const u8;
                    let dst = &raw mut BUF as *mut u8;
                    for i in 0..len as usize {
                        // SAFETY: both pointers are in range; BUF is 128 bytes and len <= 128.
                        unsafe {
                            core::ptr::write_volatile(
                                dst.add(i),
                                core::ptr::read_volatile(src.add(i)),
                            )
                        };
                    }
                    LEN.store(len, Ordering::SeqCst);
                    CAPTURED.store(true, Ordering::SeqCst);
                }
                sched::ipc_send(reply, [0, 0, 0]); // ack, so the client reuses the buffer
            }
        })
        .expect("spawn failed");

        // The client: the real binary, client role, wired to the endpoints and the shared page.
        let faults = USER_FAULTS.load(Ordering::Relaxed);
        sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 2, // printing-client role (matches user/src/hello.rs)
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        crate::cap::endpoint_cap(request, crate::cap::Rights::WRITE),
                        crate::cap::endpoint_cap(reply, crate::cap::Rights::READ),
                    ],
                    maps: &[Mapping {
                        va: SHARED_VA,
                        phys: shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("spawn failed");

        assert!(
            wait_for(|| CAPTURED.load(Ordering::SeqCst)),
            "the server never received a message through shared memory",
        );
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the client faulted instead of printing cleanly",
        );

        let len = LEN.load(Ordering::SeqCst) as usize;
        // SAFETY: written by the server thread, which stopped touching BUF once CAPTURED.
        let got = unsafe { core::slice::from_raw_parts(&raw const BUF as *const u8, len) };
        assert_eq!(
            got, FIRST_LINE,
            "the wrong bytes arrived through shared memory"
        );
    }

    /// `map_physical` puts one physical frame into an address space at a chosen VA, with exactly
    /// the permissions asked for and no more. The mechanism a driver leaves the kernel on.
    #[test_case]
    fn map_physical_maps_a_shared_frame_and_a_device_page() {
        const DATA_VA: u64 = 0x0000_0000_0060_0000;
        const DEV_VA: u64 = 0x0000_0000_0070_0000;
        const PL011_PHYS: u64 = 0x0900_0000;

        let mut space = AddressSpace::new(2).expect("no address space");
        let frame = crate::memory::alloc().expect("no frame").addr();

        space
            .map_physical(DATA_VA, frame, Flags::user_data())
            .expect("shared map failed");
        space
            .map_physical(DEV_VA, PL011_PHYS, Flags::user_device())
            .expect("device map failed");

        // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.ttbr0()) };

        let (data_pa, data_f) = mmu::translate_user(DATA_VA).expect("shared page not mapped");
        assert_eq!(data_pa, frame, "shared page maps the wrong frame");
        assert!(data_f.is_user_accessible() && data_f.is_writable());
        assert!(!data_f.is_user_executable());

        let (dev_pa, dev_f) = mmu::translate_user(DEV_VA).expect("device page not mapped");
        assert_eq!(
            dev_pa, PL011_PHYS,
            "device page maps the wrong physical address"
        );
        assert!(dev_f.is_user_accessible() && dev_f.is_writable());

        mmu::deactivate_user();
        crate::memory::free(Frame::from_addr(frame));
        drop(space);
    }

    /// A thread can name nothing until somebody hands it something.
    #[test_case]
    fn a_new_thread_holds_no_capabilities() {
        use crate::cap::Error;

        // The current thread is a kernel thread, spawned by the harness, and was handed nothing.
        for slot in 0..16 {
            assert_eq!(
                sched::current_cap(slot).err(),
                Some(Error::NoSuchSlot),
                "slot {slot} is not empty in a thread nobody granted anything",
            );
        }
    }

    /// **A userspace driver reads a file off a real virtio disk.** Milestone 9, end to end.
    ///
    /// The kernel enumerates the bus and hands a driver at EL0 the device registers, a DMA page,
    /// and an interrupt. The driver sets up a virtqueue, reads the superblock by DMA, parses the
    /// crickerfs directory, reads the `motd` file, and reports its first bytes. We check them
    /// against the known contents, which proves real disk data crossed DMA and the EL0 boundary.
    ///
    /// It also proves the interrupt path (9a) carried the completion: `ROUTED_IRQS` counts device
    /// interrupts turned into messages, and it must rise. And it proves the idle thread works: the
    /// driver blocks waiting for that interrupt with nothing else to run, and the scheduler idles
    /// rather than declaring a deadlock.
    #[test_case]
    fn a_userspace_driver_reads_a_file_from_a_virtio_disk() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let report = match virtio_service::start(init_image()) {
            Some(r) => r,
            None => {
                // No disk attached to this run. Nothing to test; do not fail.
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };

        let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);

        // Blocks until the driver has done the whole read. If the driver faults, it never sends,
        // and the scheduler idles; the QEMU-level timeout is the backstop.
        let word = sched::ipc_recv(report)[0];

        assert_eq!(
            &word.to_le_bytes(),
            b"cricker-",
            "the driver reported the wrong file contents",
        );
        assert!(
            ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
            "the read completed but no device interrupt was delivered as a message",
        );
    }

    /// **`std::fs` end to end over the FS-service contract** (milestone 27 phase two, the FS half).
    ///
    /// An ordinary Rust program, granted **one directory capability** and nothing else that names a
    /// filesystem, opens the file the host-made RedoxFS image ships, reads it with `Read` and
    /// `read_to_string`, stats it, and gets refused when it tries to name anything outside that
    /// directory. The bytes it prints are the file's own, so the assertion covers the whole path:
    /// disk, DMA-confined block server, FS server running an engine we did not write, the file
    /// contract, std's PAL, and the stdout endpoint.
    ///
    /// What it proves that the hand-written client's test does not: `std::fs::File::open` has no
    /// global namespace to resolve against, and the mapping to a granted directory holds from inside
    /// std, including the refusal of `..`, of an absolute path, and of a nested path. And the same
    /// binary run without slot 4 gets `Unsupported`, which the offline std test asserts.
    #[test_case]
    fn std_fs_reads_a_file_through_a_granted_directory_capability() {
        let (readiness, report) = match fs_service::start_std(
            init_image(),
            program("fsserver").expect("no fsserver program in the initrd archive"),
            hellostd_image(),
        ) {
            Some(r) => r,
            None => {
                crate::println!("    (no RedoxFS disk attached; skipping)");
                return;
            }
        };
        assert_fs_service_ready(readiness);

        let mut want = [0u8; 512];
        let n = std_fs_expected(&mut want);
        assert_std_transcript(report, &want[..n], "std fs");
    }

    /// **The RedoxFS filesystem service, end to end** (milestone 32 phase 2, the flagship
    /// userspace-reuse story). Three confined processes: a block server drives the RedoxFS disk over
    /// DMA, an FS server mounts it over blk IPC and serves files from its own heap, and a client
    /// opens `motd` through a granted directory capability, reads it, writes a pattern to `scratch`
    /// and reads it back, then reports. The client names nothing but its directory endpoint, so a
    /// success here is the whole capability contract holding: designation is authorization, the
    /// handle is a server-minted token, and a real CoW filesystem we did not write runs confined.
    #[test_case]
    fn the_fs_server_serves_redoxfs_over_a_capability_contract() {
        let (readiness, report) = match fs_service::start(
            init_image(),
            program("fsserver").expect("no fsserver program in the initrd archive"),
            program("fsclient").expect("no fsclient program in the initrd archive"),
            0, // the end-to-end proof role, not the benchmark loop
        ) {
            Some(r) => r,
            None => {
                // No RedoxFS disk attached to this run. Nothing to test; do not fail.
                crate::println!("    (no RedoxFS disk attached; skipping)");
                return;
            }
        };

        // The two servers' readiness sentinels, if this test is the one that wired them (the
        // `std::fs` test shares the same service, and each sentinel is sent exactly once).
        assert_fs_service_ready(readiness);

        // Then: the client has read motd, round-tripped scratch, and reported. If any of the three
        // processes faults, it never sends and the QEMU-level timeout is the backstop.
        let [head, status, ..] = sched::ipc_recv(report);
        assert_eq!(
            status,
            fs_proto::fixture::SUCCESS,
            "the client did not report success: a check in the read or write path failed",
        );
        assert_eq!(
            &head.to_le_bytes()[..],
            &fs_proto::fixture::MOTD[..8],
            "the client read the wrong motd bytes off the RedoxFS image",
        );
    }

    /// **A userspace driver completes a DHCP round trip over virtio-net.** Milestone 30, end to
    /// end, and the proof the multi-queue confinement carries a real NIC.
    ///
    /// The kernel enumerates the NIC and hands a driver at EL0 a confined `Virtio` capability, a DMA
    /// page, and an interrupt. The driver brings up BOTH virtqueues (receive = 0, transmit = 1),
    /// posts a receive buffer, transmits a hand-built DHCP DISCOVER, and waits for QEMU user-mode
    /// networking's OFFER. It reports the offered address, which must land in slirp's 10.0.2.0/24.
    /// Because a valid OFFER for our transaction is the only path to that report, a match proves the
    /// DISCOVER left (TX) and the OFFER returned (RX), across both queues and both directions of the
    /// confinement, with no TCP/IP stack in the loop.
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net() {
        let report = match virtio_service::start_net(init_image()) {
            Some(r) => r,
            None => {
                // No NIC on this run (a bare boot). The test runners always attach one, so this
                // branch is not the parity gate. See scripts/qemu-runner*.sh (CRICKER_NET).
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };

        let yiaddr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            yiaddr & 0xffff_ff00,
            0x0A00_0200,
            "the DHCP OFFER's yiaddr {yiaddr:#010x} is not in QEMU slirp's 10.0.2.0/24: the round \
             trip did not complete correctly",
        );
        // We do NOT assert a fresh routed interrupt here, unlike the disk read test. The net
        // driver's completion is the used ring advancing, not one interrupt per operation (the same
        // discipline the disk driver's complete loop follows, notes/dma.md), and the net test suite
        // shares one NIC across many drivers and servers (piece 3): a leftover completion from a
        // prior operator can be counted before this test's baseline and then consumed as a stale
        // wakeup, so a strict interrupt-delta is unreliable. The OFFER round trip above is the proof
        // that the interrupt path carried the completion.
    }

    /// The same DHCP round trip over the PCIe transport, behind the IOMMU (milestone 30, §20): the
    /// NIC is confined in hardware to its DMA region, and the driver binary is byte-identical to the
    /// mmio one. Proves the multi-queue confinement and the net driver work over the bus real
    /// hardware uses.
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net_pci() {
        let report = match virtio_service::start_net_pci(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };

        let yiaddr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            yiaddr & 0xffff_ff00,
            0x0A00_0200,
            "the DHCP OFFER's yiaddr {yiaddr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// **The net server: smoltcp running DHCP over the confined NIC** (milestone 30, piece 3). The
    /// integration proof and the thesis headline for networking: a real, reused TCP/IP stack
    /// (smoltcp, not hand-built) runs entirely at EL0, brings the NIC up through the `Virtio`
    /// capability, and completes a DHCP handshake against QEMU user-mode networking. The kernel
    /// knows nothing about DHCP; it owns only the DMA confinement. The server reports the acquired
    /// address, which must land in slirp's 10.0.2.0/24, so only a real DHCP round trip driven by
    /// smoltcp over the confined NIC can produce it.
    #[test_case]
    fn the_net_server_acquires_a_dhcp_lease_over_smoltcp() {
        let report = match virtio_service::start_net_server(netd_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let addr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            addr & 0xffff_ff00,
            0x0A00_0200,
            "smoltcp's DHCP lease {addr:#010x} is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// The net server over the PCIe transport, behind the IOMMU (milestone 30, §20): smoltcp drives
    /// a NIC confined in hardware and still gets its lease.
    #[test_case]
    fn the_net_server_acquires_a_dhcp_lease_over_smoltcp_pci() {
        let report = match virtio_service::start_net_server_pci(netd_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };
        let addr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            addr & 0xffff_ff00,
            0x0A00_0200,
            "smoltcp's DHCP lease {addr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// **The socket contract, UDP end to end** (milestone 30, piece 3 phase B; DECISIONS §25). A
    /// client process holds a `Stack` endpoint and its own untyped, mints a shared frame, delegates
    /// it, opens a UDP socket by id, and sends a real DNS query to slirp's built-in resolver
    /// (10.0.2.3:53), verifying the response is a reply to its own transaction. No ambient network:
    /// the client acts only through the capability it was granted, and the bytes cross in the shared
    /// frame, never in a message. Proves the whole path, client to netd to smoltcp to the confined
    /// NIC, over the mmio transport.
    #[test_case]
    fn a_client_resolves_dns_through_the_socket_contract() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_UDP_DNS, false) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP DNS exchange through the socket contract failed (client code {verdict:#x})",
        );
    }

    /// The same UDP DNS exchange over the PCIe transport, behind the IOMMU.
    #[test_case]
    fn a_client_resolves_dns_through_the_socket_contract_pci() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_UDP_DNS, true) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP DNS exchange over PCIe failed (client code {verdict:#x})",
        );
    }

    /// **The socket contract, TCP end to end** (milestone 30, piece 3 phase B). A client opens a TCP
    /// socket by id, connects to slirp's guestfwd echo peer (10.0.2.9:7777, piped to `/bin/cat`),
    /// sends a payload, receives the echo, and closes. The full round trip, handshake through
    /// bidirectional data to teardown, deterministic and zero-host-setup (nothing outlives QEMU),
    /// through the client, netd, smoltcp, and the confined NIC.
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_TCP_ECHO, false) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the TCP echo round trip through the socket contract failed (client code {verdict:#x})",
        );
    }

    /// The same TCP echo round trip over the PCIe transport, behind the IOMMU.
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract_pci() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_TCP_ECHO, true) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the TCP echo round trip over PCIe failed (client code {verdict:#x})",
        );
    }

    /// **Regression: reusing a socket id is safe** (the ephemeral-port fix). A client opens a TCP
    /// socket on id 0, connects to the echo peer, closes it, then reopens the same id and connects
    /// again. netd derived the local port from the socket id, so the reopen reused the exact port and
    /// the second connect stalled on a slirp flow that had not cleared; the rotating allocator hands
    /// the reopen a fresh port, so both connects complete. The client reports OK only if they do.
    #[test_case]
    fn a_reopened_socket_id_connects_again_over_tcp() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_TCP_REOPEN, false)
        {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "reopening a socket id and connecting again failed (client code {verdict:#x}): the \
             ephemeral local port is not independent of the socket id",
        );
    }

    /// The `hellostd` std program's ELF bytes. The same binary the offline std test spawns; given
    /// the network here, its `UdpSocket::bind` probe succeeds and it runs the net transcript.
    fn hellostd_image() -> &'static [u8] {
        program("hellostd").expect("no hellostd program in the initrd archive")
    }

    /// The exact transcript `hellostd` prints when it is granted the network. Pinned so a drift in
    /// the net PAL, the contract, or the demo is a loud diff rather than a mystery.
    const STD_NET_EXPECTED: &[u8] = b"std net on cricker-os\ndns ok\ntcp echo ok\n";

    /// **`std::net` end to end over the socket contract** (milestone 27 phase two): the `hellostd`
    /// std binary, given the network, does a real UDP DNS query and a TCP echo round trip through
    /// `std::net::{UdpSocket, TcpStream}`, whose PAL binds to netd's contract. The program never
    /// sees a capability or a socket id; it writes to a socket and reads from it. This closes the
    /// `net honestly unsupported` gap from phase one: std's networking runs on the native ABI,
    /// reaching the same path the hand-written client does through std's blocking API. Its stdout
    /// is reassembled off the endpoint and compared byte for byte, the `hellostd` discipline.
    #[test_case]
    fn std_net_runs_over_the_socket_contract() {
        let report = match virtio_service::start_net_std(netd_image(), hellostd_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };

        assert_std_transcript(report, STD_NET_EXPECTED, "std net");
    }

    /// **The shell's `run` mechanism: spawn a process, get its answer.** Milestone 10's core.
    ///
    /// A worker process is started at EL0 with an argument, computes `n*n`, reports the result on
    /// an endpoint it was handed, and exits. The whole lifecycle a shell drives when you type
    /// `run n` — minus the interactive loop, which is exercised by the piped demo instead.
    #[test_case]
    fn a_spawned_worker_process_computes_and_reports() {
        let result = sched::create_endpoint();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        sched::spawn(move || {
            run(
                worker_image(), // its own binary now (19f.2), not a role of hello
                Spawn {
                    arg0: 0, // no role selector; the input is in x1
                    arg1: 9, // the worker computes 9*9
                    arg2: 0,
                    grants: &[crate::cap::endpoint_cap(result, crate::cap::Rights::WRITE)],
                    maps: &[],
                },
            )
        })
        .expect("spawn failed");

        let answer = sched::ipc_recv(result)[0];
        assert_eq!(answer, 81, "the spawned worker computed the wrong answer");
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the worker faulted instead of computing cleanly",
        );
    }

    /// **The kernel stops allocating.** Milestone 11's whole point, as one number.
    ///
    /// We carve an untyped region, then a process maps page after page out of it until the region
    /// is exhausted. The assertion that matters: the kernel's used-frame count **does not change
    /// while the process allocates**, because every page comes from the untyped, not the kernel
    /// allocator. A process cannot make the kernel allocate, so it cannot exhaust kernel memory;
    /// it runs out of its own budget and stops, cleanly, with the kernel untouched.
    #[test_case]
    fn a_process_spends_untyped_and_the_kernel_never_allocates() {
        let used = || crate::memory::stats().expect("no allocator").used;

        const PAGES: u64 = 24;
        let (region, report) = untyped_service::start(init_image(), PAGES)
            .expect("could not create the untyped region");

        // The process sends a "ready" signal once it is fully loaded (its ELF and stack are
        // kernel-allocated, like any process). We measure the frame count THERE, so the window we
        // check contains only what it does next: map pages out of its untyped.
        sched::ipc_recv(report); // ready
        let baseline = used();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        let mapped = sched::ipc_recv(report)[0]; // the count, after it exhausted the untyped

        assert_eq!(
            used(),
            baseline,
            "the kernel allocated {} frames while a process mapped {mapped} pages: untyped is not \
             backing the process's memory",
            used() as i64 - baseline as i64,
        );
        assert!(mapped > 0, "the process mapped nothing");
        assert_eq!(
            USER_FAULTS.load(Ordering::Relaxed),
            faults,
            "the process faulted instead of exhausting its budget cleanly",
        );

        // And the untyped is genuinely spent: the process mapped until it ran dry.
        let (watermark, total) = crate::untyped::usage(region).expect("region vanished");
        assert_eq!(total, PAGES);
        assert!(
            watermark >= mapped,
            "the process mapped {mapped} pages but the untyped only advanced {watermark}",
        );
        assert!(
            total - watermark < 4,
            "the untyped had {} pages left unspent; the process gave up early",
            total - watermark,
        );
    }

    /// **The DMA-confinement fix, end to end.** A malicious driver at EL0 holds a real `Virtio`
    /// capability and its own DMA region, and points a descriptor at the kernel's image, asking
    /// the device to write there. Because the device has no IOMMU, this would succeed if the
    /// driver could ring it directly. The kernel validates every descriptor on submit and refuses
    /// this one, so the device is never told to go and never touches the kernel. The driver
    /// reports `1` when it was refused.
    #[test_case]
    fn the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region() {
        let report = match virtio_service::start_attacker(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let refused = sched::ipc_recv(report)[0];
        assert_eq!(
            refused, 1,
            "a malicious driver's descriptor pointing at kernel memory was NOT refused: the \
             device could have DMA'd over the kernel",
        );
    }

    /// **The indirect-descriptor escape, end to end.** The direct-descriptor test above proves the
    /// obvious case. This proves the subtle one: a driver that negotiates `INDIRECT_DESC` and
    /// submits an indirect descriptor whose inner table (in its own region) aims the device at the
    /// kernel. A validator that walked only the flat chain would pass the outer descriptor and let
    /// the device follow the table out. The kernel strips the feature and refuses the flag, so the
    /// device is never rung. The driver reports `1` when it was refused.
    #[test_case]
    fn the_kernel_refuses_an_indirect_descriptor_escape() {
        let report = match virtio_service::start_attacker_indirect(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let refused = sched::ipc_recv(report)[0];
        assert_eq!(
            refused, 1,
            "an indirect descriptor whose inner table pointed at kernel memory was NOT refused: \
             the device could have followed it out of the driver's region",
        );
    }

    /// **The PCIe transport on aarch64, end to end** (DECISIONS §18): the same driver reads the
    /// same file off the disk QEMU attached as `virtio-blk-pci`, found by ECAM enumeration in
    /// the highmem window, BARs placed by the kernel, the completion arriving as INTx through
    /// the GIC (SPI 3 + swizzle). The riscv twin proved the seam on the PLIC board; this proves
    /// the same subsystem, from the same portable crate and seam, on the second bus of the
    /// second interrupt controller.
    #[test_case]
    fn a_userspace_driver_reads_a_file_over_the_pcie_transport() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let report = match virtio_service::start_pci(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-pci disk on the bus; skipping)");
                return;
            }
        };

        let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);
        let word = sched::ipc_recv(report)[0];

        assert_eq!(
            &word.to_le_bytes(),
            b"cricker-",
            "the driver reported the wrong file contents over pci",
        );
        assert!(
            ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
            "the read completed but no INTx interrupt was delivered through the GIC",
        );
    }

    /// **A userspace driver writes a block and reads it back.** Milestone 32 phase 1: the write
    /// verb, end to end, through the same validated transport as the read path. The driver
    /// writes a pattern to the scratch block, wipes its buffer, reads the block back, verifies
    /// every byte in-process, re-checks the superblock and directory around it, and reports the
    /// read-back head. A matching report therefore certifies the round trip AND that the write
    /// landed only on its own block.
    #[test_case]
    fn a_userspace_driver_writes_a_block_and_reads_it_back() {
        let report = match virtio_service::start_writer(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            &word.to_le_bytes(),
            b"CRKWRIT1",
            "the driver did not read back the pattern it wrote",
        );
    }

    /// The same write round trip over the PCIe transport (DECISIONS §18): the write verb must
    /// hold on both buses, exactly as the read path does, or the transport seam has a
    /// direction-shaped hole.
    #[test_case]
    fn a_userspace_driver_writes_a_block_over_the_pcie_transport() {
        let report = match virtio_service::start_writer_pci(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-pci disk on the bus; skipping)");
                return;
            }
        };
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            &word.to_le_bytes(),
            b"CRKWRIT1",
            "the driver did not read back the pattern it wrote over pci",
        );
    }

    /// **A driver killed mid-write leaves the device and the transport sane.** Errors here eat
    /// filesystems, so this is the write path's teardown proof: a driver submits a validated
    /// write and dies (panics, is killed, is reaped) without ever collecting the completion,
    /// acknowledging the interrupt, or advancing its ring bookkeeping. The device still owes a
    /// completion into the dead driver's DMA region, which is safe precisely because that frame
    /// is kernel-allocated and deliberately never reclaimed on thread death (`map_physical`'s
    /// "Drop leaves it alone" rule): the DMA lands in memory the allocator never re-issued.
    /// Then the full writer runs against the SAME device, resets it, and must complete its own
    /// round trip, which proves the abandoned request wedged nothing: not the device, not the
    /// validator's per-registration state, not the disk.
    #[test_case]
    fn a_driver_killed_mid_write_leaves_the_device_and_transport_sane() {
        let faults = USER_FAULTS.load(Ordering::Relaxed);
        let report = match virtio_service::start_write_abandoner(init_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };

        // 1 = the kernel validated the write and rang the device; the request is genuinely in
        // flight (or already complete) when the driver dies.
        assert_eq!(
            sched::ipc_recv(report)[0],
            1,
            "the abandoner never got its write submitted",
        );

        // The deliberate death: panic -> brk -> killed. Wait for the kill so the survivor below
        // runs against a device whose previous operator is really gone.
        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "the abandoner never died; nothing was killed mid-write",
        );

        // The survivor: the same full write-verify driver, same physical device. It must succeed
        // from a clean device reset, in-flight completion and all.
        let report = virtio_service::start_writer(init_image())
            .expect("the disk vanished between the abandoner and the survivor");
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            &word.to_le_bytes(),
            b"CRKWRIT1",
            "after a mid-write kill, a fresh driver could not use the device",
        );
    }

    /// A dead user thread's address space is freed, all of it, including its page tables.
    ///
    /// The milestone 6 reaper test found that stack VAs were bump-allocated and never reused,
    /// because `unmap_page` leaves intermediate tables standing. An `AddressSpace` sidesteps
    /// that entirely: it dies **all at once**, so it never unmaps anything. It records every
    /// frame the mapper hands it, leaves and tables alike, and frees the lot.
    ///
    /// The assertion is exact, not approximate. Approximate would have hidden the milestone 6
    /// bug.
    #[test_case]
    fn a_dead_user_thread_frees_its_whole_address_space() {
        let used = || crate::memory::stats().expect("no allocator").used;

        // The steady-state thread count to return to after each spawned thread is reaped. It is
        // NOT a constant: the boot thread and core 0's idle are two, plus one idle thread per
        // secondary core (SMP, §11). Capture it dynamically so the test does not bake in a core
        // count.
        let baseline = sched::thread_count();

        // Warm up: the first user thread ever created pays for page tables in a region of
        // kernel VA that nothing has touched. Measure the STEADY state, which is the one that
        // has to hold forever.
        //
        // Snapshot the fault count BEFORE spawning (as the loop below always did). The old
        // order, spawn then snapshot, was a race: on SMP the outlaw can fault in that gap, the
        // snapshot swallows its fault, and the wait below times out on a count that will never
        // move again. Latent until milestone 14 phase A.2/A.3 made spawn-to-fault fast enough
        // to lose the race about once in seven runs.
        // Pin the outlaws to THIS core (DECISIONS §28 made `spawn` scatter them). Frame accounting
        // must be exact to catch a leak (the milestone-6 bug this test guards), but a thread's frames
        // are freed by `finish_switch` on whatever core reaps it, *after* it leaves the thread table
        // and outside SCHED. Scattered across cores, that free is asynchronous, so `used()` fluctuates
        // and never reads exact. Kept on the test's own core, each outlaw's fault, reap, and frame
        // free happen synchronously under the test's own yields, so `used()` is exact again. This
        // tests the reaper, not placement, so pinning costs nothing.
        let here = crate::cpu::id();
        let outlaw_here = || sched::spawn_on(here, || unsafe { exec(outlaw()) });

        let f0 = USER_FAULTS.load(Ordering::Relaxed);
        outlaw_here().expect("spawn failed");
        assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f0));
        assert!(wait_for(|| sched::thread_count() <= baseline));

        let before = used();

        for _ in 0..4 {
            let f = USER_FAULTS.load(Ordering::Relaxed);
            outlaw_here().expect("spawn failed");
            assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f));
            assert!(wait_for(|| sched::thread_count() <= baseline));
        }

        // Exact, but allow the asynchronous reap to settle. Pinning the outlaws with `spawn_on` is a
        // placement HINT, not a pin (DECISIONS §28): an idle core can steal one before this core runs
        // it, and then the frame free (finish_switch dropping the address space, after the thread
        // leaves the table and outside SCHED) lands on that core a beat after `thread_count` already
        // fell. So wait for `used()` to return to `before` rather than reading it the instant the
        // count drops. Still exact and still a leak trap: a real leak (the milestone-6 bug) never
        // gives the frames back, so this wait times out and fails; only cross-core reap lag is
        // tolerated, not a missing frame.
        assert!(
            wait_for(|| used() == before),
            "four user address spaces came and went and {} frames did not come back",
            used() as i64 - before as i64,
        );
    }

    /// **Milestone 19b: a user-built address space is a first-class citizen of every memory
    /// mechanism.** Retype a space out of a region, map a frame into it, and check the three
    /// things that make it real: the CPU's walker sees the mapping with the exact flags asked
    /// for; §13 revocation reaches into the user-built space (the record was paid and filed, so
    /// `revoke_frame` unmaps it there like anywhere); and destroying the pinned backing region
    /// frees nothing while the space lives in it.
    #[test_case]
    fn a_user_built_aspace_maps_translates_and_revokes() {
        let region = crate::untyped::create(8).expect("no region");
        let name = user_aspace_create(region).expect("no aspace");
        let root = user_aspace_root(name).expect("aspace has no root");

        let frame_region = crate::untyped::create(2).expect("no frame region");
        let phys = crate::untyped::retype_page(frame_region).expect("no frame");
        let va = 0x40_0000u64;

        user_aspace_map(name, va, phys, Flags::user_rodata()).expect("map_into failed");

        let (mapped_pa, flags) = mmu::translate_at(root, va).expect("the walker sees no mapping");
        assert_eq!(mapped_pa, phys, "mapped the wrong frame");
        assert!(!flags.is_writable(), "asked read-only, got writable");
        assert!(
            !flags.is_global(),
            "a user mapping in a built space must be ASID-tagged"
        );

        // Same va twice: refused, the break-before-make contract holds for built spaces too.
        assert!(
            user_aspace_map(name, va, phys, Flags::user_rodata()).is_err(),
            "double-map at one va was allowed"
        );

        // The reach of §13: revoking the frame unmaps it from the space nobody exec'd.
        crate::revoke::revoke_frame(phys);
        assert!(
            mmu::translate_at(root, va).is_none(),
            "revocation does not reach a user-built address space",
        );

        // The pin: the backing region hosts a live root, so destroy must free nothing.
        let free_before = crate::memory::stats().unwrap().free();
        crate::untyped::destroy(region);
        assert_eq!(
            crate::memory::stats().unwrap().free(),
            free_before,
            "destroy reclaimed the region under a live user-built space",
        );

        // The frame region is unpinned (it only ever produced a plain frame), so destroy
        // reclaims it whole, the already-revoked frame included. No manual free: the region
        // owns its pages, and freeing one twice is the allocator's double-free panic.
        crate::untyped::destroy(frame_region);
    }

    /// **Milestone 19d.2b: init delegates an interrupt to a driver it builds.** The last
    /// delegatable device authority after endpoints and device MMIO: an interrupt capability.
    /// init holds one for a test SGI (the kernel routed it), builds a child, hands it the Irq
    /// cap, and starts it. The child blocks in the interrupt's WAIT; the test raises the SGI; the
    /// interrupt is delivered as a message through the delegated capability, the child wakes and
    /// reports. A hang would mean the interrupt never reached the init-built child, so a passing
    /// test is the proof. Completes the "init delegates every authority kind" story the
    /// interrupt-driven drivers (input, virtio) rest on.
    #[test_case]
    fn userspace_init_delegates_an_interrupt_to_a_child() {
        const IRQ_WORD: u64 = 0x1590;
        const INIT_IRQ_ROLE: u64 = 25;

        let report = crate::sched::create_endpoint();
        spawn_init(initrd().expect("no initrd"), INIT_IRQ_ROLE, report);

        // Raise the test interrupt. The endpoint counts it if the child is not waiting yet (it is
        // still being built), and the child's WAIT drains that pending signal, so there is no race.
        crate::drivers::gic::send_sgi(INIT_TEST_SGI, crate::cpu::id());

        let word = crate::sched::ipc_recv(report)[0];
        assert_eq!(
            word, IRQ_WORD,
            "the interrupt never reached the init-built child through the delegated Irq cap",
        );
    }

    /// **Milestone 19d.2b: userspace init brings up the real console server.** Past 19d.2a's
    /// ID-read probe: init builds the *actual* print server as a child, wires it a request/reply
    /// channel and a shared page and the UART, then plays the client, asking it to print a line.
    /// The server prints to the real UART (visible in the QEMU log) and acks the length; init
    /// reports that length. Receiving the exact message length proves a userspace-built console
    /// works end to end: a driver init constructed, on a channel init created, driving hardware
    /// init delegated. This is the shape the real boot path (19d.2c) uses.
    #[test_case]
    fn userspace_init_brings_up_the_console_server() {
        // The message length the init_console role prints and the server acks. Kept in sync with
        // user/src/hello.rs init_console (the b"..." there); a mismatch fails loudly, not silently.
        const MSG_LEN: u64 = 72;
        const INIT_CONSOLE_ROLE: u64 = 24;

        let report = crate::sched::create_endpoint();
        spawn_init(initrd().expect("no initrd"), INIT_CONSOLE_ROLE, report);

        let acked = crate::sched::ipc_recv(report)[0];
        assert_eq!(
            acked, MSG_LEN,
            "the init-built console server did not print-and-ack: {acked} bytes, expected {MSG_LEN}",
        );
    }

    /// **Milestone 19d.2: userspace init builds a device driver and hands it the hardware.**
    /// The step beyond 19d.1: not just a child, but a child that touches a *device*. init holds a
    /// UART **device capability** (a new delegatable authority to map MMIO device-typed), builds
    /// a driver child, and maps the UART's registers into it. The child reads the PL011's
    /// PrimeCell identification registers, whose value is the fixed `0xB105F00D` every real PL011
    /// returns. Receiving that constant proves the whole chain: device access is a capability the
    /// kernel minted and init delegated, `MAP_INTO` mapped it device-typed (not cached normal
    /// memory, which would corrupt MMIO), and a userspace-init-built driver drove real hardware.
    #[test_case]
    fn userspace_init_builds_a_driver_that_reads_real_hardware() {
        const PL011_PRIMECELL_ID: u64 = 0xB105_F00D;
        const INIT_DEV_ROLE: u64 = 23;

        let report = crate::sched::create_endpoint();
        spawn_init(initrd().expect("no initrd"), INIT_DEV_ROLE, report);

        let id = crate::sched::ipc_recv(report)[0];
        assert_eq!(
            id, PL011_PRIMECELL_ID,
            "the init-built driver did not read the PL011's id: device delegation or the              device-typed mapping is broken",
        );
    }

    /// **Milestone 19d: userspace init parses a real ELF and builds a running process from it.**
    /// The kernel loads exactly one program, init (a role of the same binary), and hands it the
    /// initrd mapped read-only plus a building budget and a report endpoint. init parses that
    /// ELF *in userspace* (the `elf` crate, no longer in the kernel's trusted core) and loads it
    /// as a child through the granular verbs: retype an address space, copy each segment into
    /// retyped frames and map them, retype and endow a TCB, configure, start. The child runs code
    /// the kernel never parsed and reports the agreed word. Receiving it is the whole thesis of
    /// milestone 19 working end to end: a verified kernel that runs a workload it did not load.
    #[test_case]
    fn userspace_init_parses_an_elf_and_builds_a_running_child() {
        const CHILD_WORD: u64 = 0xC0FFEE;
        const INIT_ROLE: u64 = 20;

        let report = crate::sched::create_endpoint();
        spawn_init(initrd().expect("no initrd"), INIT_ROLE, report);

        let word = crate::sched::ipc_recv(report)[0];
        assert_eq!(
            word, CHILD_WORD,
            "init did not build a running child from the ELF it parsed in userspace",
        );
    }

    /// **Milestone 19e: init builds a worker, passes it an argument, and gets the answer back.**
    /// Every child before this took only its role in `x0`. A worker computes on an input, so 19e
    /// widened `START` to carry three initial registers. init builds a worker, starts it with the
    /// input in `x1`, and the worker squares it and reports. Receiving `n*n` (not `n`, not garbage)
    /// proves the argument crossed `START` into a fresh EL0 thread's registers intact. This is the
    /// mechanism a real spawn service runs on: a workload parameterized by data, not just identity.
    #[test_case]
    fn init_builds_a_worker_and_passes_it_an_argument() {
        const INIT_WORKER_ROLE: u64 = 28;
        const WORKER_INPUT: u64 = 7;

        let report = crate::sched::create_endpoint();
        spawn_init(initrd().expect("no initrd"), INIT_WORKER_ROLE, report);

        let answer = crate::sched::ipc_recv(report)[0];
        assert_eq!(
            answer,
            WORKER_INPUT * WORKER_INPUT,
            "the worker did not receive its START argument: expected n*n back",
        );
    }

    /// **Milestone 19e: init runs a real compute workload and it comes out right.** The worker's
    /// `n*n` proved the mechanism; this proves a *substantial* program. init builds the `"coremark"`
    /// binary (a CoreMark-derived run: list sort, matrix multiply, state machine, folded into a CRC),
    /// starts it, and the workload SENDs the run's checksum home. Receiving `coremark::PINNED_CRC_64`
    /// (`0x7954`, the value the host `coremark` test also pins) proves a real workload ran correctly
    /// against the native ABI, and that the same computation gives the same answer on the kernel's
    /// target as on the host, which is the property a cross-OS comparison will rest on.
    #[test_case]
    fn init_runs_the_coremark_workload_and_it_checks_out() {
        const INIT_COREMARK_ROLE: u64 = 29;

        let report = crate::sched::create_endpoint();
        spawn_init(initrd().expect("no initrd"), INIT_COREMARK_ROLE, report);

        let [crc, ticks, freq, _, _] = crate::sched::ipc_recv(report);
        assert_eq!(
            crc,
            coremark::PINNED_CRC_64 as u64,
            "the CoreMark workload computed the wrong checksum",
        );
        // The workload self-timed via CNTVCT_EL0. Nonzero ticks and a real frequency prove EL0 can
        // read the virtual counter (CNTKCTL_EL1.EL0VCTEN), the foundation the primitive suite needs.
        // (Under TCG the magnitude is icount fiction, but it still advances, so the read works.)
        assert!(
            ticks > 0,
            "the workload's self-timing read a frozen counter"
        );
        assert!(freq > 0, "CNTFRQ_EL0 read as zero at EL0");
    }

    /// **Milestone 19c.3, the whole point: one process builds and starts another, and it runs.**
    /// The kernel drives the four verbs the way init eventually will: retype an address space and
    /// a TCB, map a code page (containing a hand-assembled EL0 stub) and a stack into the space,
    /// insert a report endpoint into the child's cspace, configure the TCB (entry, stack, space),
    /// and START it. The child, code no wiring wrote and a thread no `spawn` created, drops to
    /// EL0, invokes the capability it was granted to SEND a word home, and exits. Receiving that
    /// word proves every verb: the retype, the maps, the cap insert, the configure, the start,
    /// and a real EL0 thread built from parts.
    #[test_case]
    fn a_process_can_build_start_and_run_a_child_thread() {
        const CODE_VA: u64 = 0x40_0000;
        const STACK_VA: u64 = 0x50_0000;
        const REPORT_WORD: u64 = 0x42;

        // The child's program, hand-assembled: SEND(slot 0, endpoint::SEND=0, REPORT_WORD),
        // then EXIT. Nine instructions; the child's first granted cap lands in slot 0.
        let code: [u32; 9] = [
            0xD280_0000,                                       // movz x0, #0        (report cap slot)
            0xD280_0001, // movz x1, #0        (endpoint::SEND)
            0xD280_0000 | ((REPORT_WORD as u32) << 5) | 2, // movz x2, #REPORT_WORD
            0xD280_0003, // movz x3, #0
            0xD280_0004, // movz x4, #0
            0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8, // movz x8, #SYS_INVOKE
            0xD400_0001, // svc #0             (the SEND)
            0xD280_0008, // movz x8, #0        (SYS_EXIT)
            0xD400_0001, // svc #0             (exit; never returns)
        ];

        // The child's address space, and a region to carve its code and stack frames from.
        let as_region = crate::untyped::create(8).expect("no aspace region");
        let aspace = user_aspace_create(as_region).expect("no aspace");
        let frames_region = crate::untyped::create(2).expect("no frame region");

        let code_phys = crate::untyped::retype_page(frames_region).expect("no code frame");
        // Write the program through the direct map, then make it coherent for the fetcher.
        // SAFETY: a fresh frame we own, direct-mapped.
        unsafe {
            let dst = mmu::phys_to_virt(code_phys) as *mut u32;
            for (i, &insn) in code.iter().enumerate() {
                dst.add(i).write(insn);
            }
        }
        sync_icache(mmu::phys_to_virt(code_phys), size_of_val(&code));
        user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

        let stack_phys = crate::untyped::retype_page(frames_region).expect("no stack frame");
        user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

        // The child's one authority: WRITE on a report endpoint, so it can SEND but not receive.
        let report = crate::sched::create_endpoint();
        let report_cap = crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        );

        // Build the thread from parts.
        let tcb_region = crate::untyped::create(2).expect("no tcb region");
        let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
        let slot = crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
        assert_eq!(
            slot, 0,
            "the child's first cap must land in slot 0 (the code assumes it)"
        );

        // Not before it is whole: START must refuse an unconfigured embryo.
        assert!(
            crate::sched::start_tcb(tid, [0; 3]).is_err(),
            "START ran a half-built thread (no address space, no entry)",
        );

        crate::sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
            .expect("configure");
        crate::sched::start_tcb(tid, [0; 3]).expect("start");

        // And starting twice must refuse: it is no longer an embryo.
        assert!(
            crate::sched::start_tcb(tid, [0; 3]).is_err(),
            "START ran a thread that was already running",
        );

        let got = crate::sched::ipc_recv(report)[0];
        assert_eq!(
            got, REPORT_WORD,
            "the child never reported: a built-from-parts thread did not reach EL0 and run",
        );
    }

    /// **Object revocation, piece 3: a started thread and its bound address space are reclaimed
    /// after it exits.** Build and start a child as above, but carve its code and stack from the
    /// *same* region as its address space, so one region holds the child's whole world (root,
    /// tables, code, stack) and its TCB is in another. Two properties: while the child is still
    /// runnable, reclaiming its region is refused (a live thread occupies it, and its owner must let
    /// it finish); once it has run, exited, and been reaped, both regions reclaim and the free-frame
    /// count returns *exactly* to baseline. The bound address space died with the thread (the `Drop`
    /// chain), leaving its region object-free for `reclaim_region` to unpin and free.
    #[test_case]
    fn reclaim_frees_a_started_then_exited_childs_regions() {
        const CODE_VA: u64 = 0x40_0000;
        const STACK_VA: u64 = 0x50_0000;
        const REPORT_WORD: u64 = 0x43;

        // SEND(slot 0, endpoint::SEND, REPORT_WORD) then EXIT, the same stub as the test above.
        let code: [u32; 9] = [
            0xD280_0000,
            0xD280_0001,
            0xD280_0000 | ((REPORT_WORD as u32) << 5) | 2,
            0xD280_0003,
            0xD280_0004,
            0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8,
            0xD400_0001,
            0xD280_0008,
            0xD400_0001,
        ];

        // The report endpoint is created before the baseline: it lives in the kernel's own pinned
        // endpoint region (never reclaimed here; endpoint revocation is a later piece), so it must
        // not count against the frame accounting.
        let report = crate::sched::create_endpoint();
        let frames_before = crate::memory::free_frames();
        let threads_before = crate::sched::thread_count();

        // The child's whole address space in one region: root, tables, code, and stack.
        let as_region = crate::untyped::create(8).expect("no aspace region");
        let aspace = user_aspace_create(as_region).expect("no aspace");

        let code_phys = crate::untyped::retype_page(as_region).expect("no code frame");
        // SAFETY: a fresh frame we own, direct-mapped.
        unsafe {
            let dst = mmu::phys_to_virt(code_phys) as *mut u32;
            for (i, &insn) in code.iter().enumerate() {
                dst.add(i).write(insn);
            }
        }
        sync_icache(mmu::phys_to_virt(code_phys), size_of_val(&code));
        user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

        let stack_phys = crate::untyped::retype_page(as_region).expect("no stack frame");
        user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

        let report_cap = crate::cap::endpoint_cap(
            report,
            crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
        );
        let tcb_region = crate::untyped::create(2).expect("no tcb region");
        let tid = crate::sched::create_tcb(tcb_region).expect("no tcb");
        crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
        crate::sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
            .expect("configure");
        crate::sched::start_tcb(tid, [0; 3]).expect("start");

        // Ready but not yet run (single core, we have not yielded): reclaiming its region must be
        // refused while a live thread occupies it. The refusal leaves the region untouched.
        assert!(
            crate::sched::reclaim_region(tcb_region).is_err(),
            "reclaim must refuse a region that still holds a live thread",
        );

        // Let it run: it SENDs the word and exits. Receiving proves it reached EL0.
        let got = crate::sched::ipc_recv(report)[0];
        assert_eq!(got, REPORT_WORD, "the child never reported");

        // Let the reaper collect the now-Finished child. A Finished thread is removed when its own
        // core switches away from it, and DECISIONS §28's placement can have put this child on
        // ANOTHER core, so yielding on THIS core cannot make that happen: a hundred cheap yields
        // complete long before the remote core's next timer tick. So wait on the clock, not on a
        // yield count. Still a leak trap rather than a masked failure: a child that is never reaped
        // times out and fails; only cross-core reap lag is tolerated. This wait was a yield count
        // until it failed about one full-suite run in four once §28 started scattering threads;
        // clock-based, it survived four consecutive runs. Two sibling waits had the same defect.
        assert!(
            wait_for(|| crate::sched::thread_count() == threads_before),
            "the exited child was never reaped",
        );

        // Both regions reclaim now: the TCB's, and the address space's (its bound space died with
        // the thread, so the region is object-free, needing only unpin and free).
        crate::sched::reclaim_region(tcb_region).expect("reclaim the TCB region after exit");
        crate::sched::reclaim_region(as_region)
            .expect("reclaim the address-space region after exit");

        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "every frame the child used must come back to baseline",
        );
    }

    /// **Spawn-to-reap repeats without leaking: the whole milestone's payoff.** Build, start, run,
    /// exit, reap, and reclaim a region-backed EL0 child, in a loop. Every iteration returns the
    /// free-frame count to the same baseline, and the region slots are reused (generational), so the
    /// loop neither leaks memory nor exhausts the region table. This is the property "spawn's
    /// prerequisite" was always about: not retype (that had shipped), but reclamation, so a workload
    /// can come and go over and over. A few iterations under TCG is enough to catch any per-cycle
    /// leak; the real magnitudes wait on the EL0 lat_proc benchmark.
    #[test_case]
    fn spawn_to_reap_repeats_without_leaking() {
        const CODE_VA: u64 = 0x40_0000;
        const STACK_VA: u64 = 0x50_0000;
        const REPORT_WORD: u64 = 0x44;
        let code: [u32; 9] = [
            0xD280_0000,
            0xD280_0001,
            0xD280_0000 | ((REPORT_WORD as u32) << 5) | 2,
            0xD280_0003,
            0xD280_0004,
            0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8,
            0xD400_0001,
            0xD280_0008,
            0xD400_0001,
        ];

        let report = crate::sched::create_endpoint();
        let baseline = crate::memory::free_frames();

        for round in 0..6 {
            let threads_before = crate::sched::thread_count();

            let as_region = crate::untyped::create(8).expect("aspace region");
            let aspace = user_aspace_create(as_region).expect("aspace");
            let code_phys = crate::untyped::retype_page(as_region).expect("code frame");
            // SAFETY: a fresh frame we own, direct-mapped.
            unsafe {
                let dst = mmu::phys_to_virt(code_phys) as *mut u32;
                for (i, &insn) in code.iter().enumerate() {
                    dst.add(i).write(insn);
                }
            }
            sync_icache(mmu::phys_to_virt(code_phys), size_of_val(&code));
            user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");
            let stack_phys = crate::untyped::retype_page(as_region).expect("stack frame");
            user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

            let report_cap = crate::cap::endpoint_cap(
                report,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            );
            let tcb_region = crate::untyped::create(2).expect("tcb region");
            let tid = crate::sched::create_tcb(tcb_region).expect("tcb");
            crate::sched::tcb_insert_cap(tid, report_cap, None).expect("cap insert");
            crate::sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
                .expect("configure");
            crate::sched::start_tcb(tid, [0; 3]).expect("start");

            assert_eq!(
                crate::sched::ipc_recv(report)[0],
                REPORT_WORD,
                "round {round}: the child never reported"
            );
            // On the clock, not on yields, for the reason spelled out in the test above: §28 can
            // place the child on another core and only that core's switch reaps it. A lagging reap
            // here would surface as the reclaim below refusing a region that still holds a thread.
            assert!(
                wait_for(|| crate::sched::thread_count() == threads_before),
                "round {round}: the child was never reaped",
            );
            crate::sched::reclaim_region(tcb_region).expect("reclaim tcb region");
            crate::sched::reclaim_region(as_region).expect("reclaim aspace region");

            assert_eq!(
                crate::memory::free_frames(),
                baseline,
                "round {round}: spawn-to-reap leaked; the cycle does not return to baseline",
            );
        }
    }

    /// **Milestone 19b, end to end: a process constructs an address space from EL0.** The
    /// builder retypes a space and a frame from its own budget, maps the frame in, and checks
    /// the kernel enforces break-before-make inside the space it built. Verdict 0b111 or bust.
    #[test_case]
    fn a_process_can_build_an_address_space_from_el0() {
        let report = aspace_service::wire(init_image());
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, 0b111,
            "aspace build verdict {verdict:#b}: bit0 retype, bit1 map_into, bit2 double-map refused",
        );
    }

    /// **Milestone 19a, end to end: an endpoint minted by a process out of its own memory
    /// carries IPC between processes.** The maker retypes a page of its untyped into an endpoint
    /// (`RETYPE_OBJ`), delegates a READ view to a peer it has never met, and sends a word into
    /// it; the peer listens on the received capability and reports what arrived. No kernel
    /// wiring created the endpoint: budget, mint, delegation, and rendezvous are all the
    /// processes' own acts. The word arriving is the whole granular-construction story of 19a
    /// working at EL0.
    #[test_case]
    fn a_process_can_mint_an_endpoint_and_ipc_flows_over_it() {
        let report = retype_ep_service::wire(init_image());
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            word, 0x77,
            "the word never crossed the process-minted endpoint",
        );
    }

    /// **Capability delegation, end to end.** A granter process passes a resource capability to a
    /// receiver process over an IPC channel, narrowed to `WRITE`. Three things must hold, and this
    /// checks all three: the receiver *gets* the capability, the receiver can *use* it (a
    /// capability minted for it by another process works when it invokes it), and the receiver
    /// *cannot pass it on* because it was handed the capability without `GRANT`. This is the
    /// operation that makes the capability model composable by processes instead of brokered by the
    /// kernel at spawn. See user/src/hello.rs and user::delegation_service.
    #[test_case]
    fn a_capability_can_be_delegated_over_ipc_and_grant_gates_re_delegation() {
        let image = init_image();
        let (resource, report) = delegation_service::wire(image);

        // The receiver invoked the *delegated* capability to SEND this word. Collecting it here is
        // proof the capability the granter minted for the receiver actually carries authority.
        let used = sched::ipc_recv(resource)[0];
        assert_eq!(
            used,
            delegation_service::USED_WORD,
            "a delegated capability did not work when its recipient invoked it",
        );

        // The receiver's own two-bit verdict: bit 0 it received a capability, bit 1 the kernel
        // refused its attempt to re-delegate a capability it holds without GRANT.
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict & 0b01,
            0b01,
            "the receiver never received the delegated capability",
        );
        assert_eq!(
            verdict & 0b10,
            0b10,
            "a capability held WITHOUT grant was allowed to be re-delegated: rights did not gate it",
        );
    }

    /// **Milestone 12: a process calls a server it was never wired to, and the reply cap is
    /// one-shot.** The client `CALL`s across the boundary; the server `RECV_CAP`s the request plus a
    /// kernel-minted reply capability naming the caller, answers through it (the round trip through
    /// the real syscall path), then tries to answer a second time and reports that the kernel
    /// refused. This is what a pre-wired reply endpoint cannot guarantee.
    #[test_case]
    fn a_process_calls_a_server_and_the_reply_is_one_shot() {
        let (call_report, oneshot_report) = call_service::wire(init_image());

        let reply = sched::ipc_recv(call_report)[0];
        assert_eq!(
            reply, 42,
            "the CALL did not return the server's reply (40 + 2)"
        );

        let one_shot = sched::ipc_recv(oneshot_report)[0];
        assert_eq!(
            one_shot, 1,
            "the server's second reply was NOT refused: the reply capability is not one-shot",
        );
    }

    /// **Milestone 13: a process revokes a frame across the boundary.** It retypes a page, maps it,
    /// then `REVOKE`s it; the kernel unmaps the page and deletes every capability to it, the
    /// process's own included, so a second operation on that slot finds nothing there. This exercises
    /// the REVOKE syscall path (rights, unmap, cap deletion). The multi-address-space unmapping and
    /// the safe reclamation are proven directly in kernel/src/revoke.rs.
    #[test_case]
    fn a_process_revokes_a_frame_and_loses_the_capability() {
        let report = revoke_service::wire(init_image());
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, 1,
            "REVOKE did not both succeed and leave the frame slot empty",
        );
    }

    /// **Frame capabilities, end to end.** A producer retypes a page into a `Frame`, maps it, writes
    /// a sentinel, and delegates a READ-only view to a consumer. Two things must hold: the consumer
    /// reads the producer's sentinel through its *own* mapping of the same physical page (the memory
    /// is genuinely shared, and the kernel copied nothing), and the consumer *cannot* map that page
    /// writable, because it was handed the frame with `READ` alone. This is §10's "shared memory
    /// carries data" done by the processes rather than wired by the kernel at spawn. See
    /// user/src/hello.rs and user::frame_service.
    #[test_case]
    fn a_frame_capability_shares_a_page_and_a_read_only_view_cannot_write_it() {
        let image = init_image();
        let report = frame_service::wire(image);

        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict & 0b01,
            0b01,
            "the consumer did not read the producer's sentinel through the shared frame: the page was not shared",
        );
        assert_eq!(
            verdict & 0b10,
            0b10,
            "a frame delegated READ-only was mappable writable: rights did not confine the mapping",
        );
    }
}

/// **Forcible teardown: `DESTROY` tears a runaway down** (DECISIONS §16 amendment, §24's second-`^C`
/// tier). A child spinning at EL0, never yielding and never checking an endpoint, cannot be waited
/// out; its region's owner must be able to reclaim it anyway. This is the one cross-ISA test in this
/// file, because the mechanism it proves is pure portable scheduler logic: the only per-architecture
/// part is the single spin instruction (`b .` / `j .`), and the whole capability dance around it is
/// the same code both ISAs run. It is separate from the aarch64 module above precisely so it can run
/// on both, which the parity gate (DECISIONS §19) asks of every kernel capability.
#[cfg(test)]
mod force_kill_tests {
    use super::*;
    use crate::sched;

    const CODE_VA: u64 = 0x40_0000;
    const STACK_VA: u64 = 0x50_0000;

    /// A one-instruction runaway: branch (aarch64) or jump (riscv) to self, forever. It never
    /// yields, never syscalls, never touches an endpoint, so nothing cooperative can end it and the
    /// forcible tier is the only thing that can.
    #[cfg(target_arch = "aarch64")]
    const SPIN_STUB: &[u32] = &[0x1400_0000]; // b .
    #[cfg(target_arch = "riscv64")]
    const SPIN_STUB: &[u32] = &[0x0000_006F]; // j .  (jal x0, 0)

    /// Build a runaway from parts (aspace, code, stack, TCB all in one region), start it, then
    /// reclaim its region while it still spins, and assert the region comes back whole.
    #[test_case]
    fn destroy_force_kills_a_runaway_and_reclaims_its_region() {
        let frames_before = crate::memory::free_frames();
        let threads_before = sched::thread_count();

        // The runaway's whole world in one region: the address space's root and tables, its code
        // page, its stack, and its TCB, so a single `DESTROY` reclaims all of it.
        let region = crate::untyped::create(16).expect("no region for the runaway");
        let aspace = user_aspace_create(region).expect("no aspace");

        let code_phys = crate::untyped::retype_page(region).expect("no code frame");
        // SAFETY: a fresh frame we own, direct-mapped; write the spin loop and make it fetchable.
        unsafe {
            let dst = mmu::phys_to_virt(code_phys) as *mut u32;
            for (i, &insn) in SPIN_STUB.iter().enumerate() {
                dst.add(i).write(insn);
            }
        }
        sync_icache(
            mmu::phys_to_virt(code_phys),
            core::mem::size_of_val(SPIN_STUB),
        );
        user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

        let stack_phys = crate::untyped::retype_page(region).expect("no stack frame");
        user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

        let tid = sched::create_tcb(region).expect("no tcb");
        sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
            .expect("configure");
        sched::start_tcb(tid, [0; 3]).expect("start");

        // Let the runaway actually reach EL0 and start spinning, so we tear down a running thread,
        // not an embryo. A few yields is plenty; it is preemptible the instant it lands.
        for _ in 0..8 {
            sched::yield_now();
        }

        // The forcible tier: reclaim the region while the runaway is still live. The first pass arms
        // the kill and refuses; the runaway is converted to a corpse at its next preemption; the
        // retry reclaims. The wait is time-based, not a fixed spin count, because since DECISIONS §28
        // the runaway may be placed on another core, where only that core's own timer tick converts
        // it (the kill is bounded by the tick, §28.3 / §16). A tight yield loop on this core would
        // finish inside one 10 ms tick and never give the remote core a chance; a one-second deadline
        // spans ~100 ticks, ample, while still failing a real bug rather than hanging the emulator.
        let deadline = crate::arch::timer::now() + crate::arch::timer::frequency();
        let mut reclaimed = false;
        while crate::arch::timer::now() < deadline {
            if sched::reclaim_region(region).is_ok() {
                reclaimed = true;
                break;
            }
            sched::yield_now();
        }
        assert!(
            reclaimed,
            "DESTROY never tore down a runaway: the killed flag did not convert it to a corpse",
        );

        assert!(
            sched::thread_count() <= threads_before,
            "the force-killed runaway was reclaimed but never actually reaped",
        );
        assert_eq!(
            crate::memory::free_frames(),
            frames_before,
            "reclaiming a force-killed runaway did not return its frames to baseline",
        );
    }
}

/// **An init that gives its authority away, and a supervision tree that outlives it** (milestone 22
/// phase B.2).
///
/// Cross-ISA, because every piece is portable: the whole tree is four ordinary user programs
/// (`rootsup`, `spawner`, `subsup`, `flaky`) built out of the capability verbs, and the kernel's only
/// part is the fault endpoint phase A already built.
///
/// The kernel spawns `rootsup` the way it spawns init: the archive mapped read-only, one untyped
/// budget, one report endpoint. rootsup then builds a construction sub-server and a supervisor, hands
/// each exactly what it needs, and **deletes its own budget**. From then on the tree runs without it:
/// the sub-server crashes, its supervisor hears about it, reaps it through the spawner, and asks for a
/// replacement, which runs and exits cleanly. init could not have done any of that, and that is what
/// these two tests prove.
#[cfg(test)]
mod authority_tests {
    use super::*;
    use crate::sched;

    /// The report protocol, matching user/src/suptree.rs (the same convention the net client's
    /// selectors follow: userspace owns the definition, the test mirrors it).
    const REPORT_INIT_DROPPED: u64 = 1;
    const REPORT_SERVER_RAN: u64 = 2;
    const REPORT_SUP_SAW_DEATH: u64 = 3;
    const REPORT_SUP_GAVE_UP: u64 = 4;
    const REPORT_FAILED: u64 = 9;

    /// Pages in rootsup's construction budget. It builds two servers out of this, splits the
    /// spawner's budget from it, and then deletes it; the spawner's split is the only memory the tree
    /// spends afterwards.
    const ROOT_BUDGET_PAGES: u64 = 1024;

    /// **Spawn the tree's root the way the kernel spawns init**, and return the report endpoint every
    /// process in the tree holds a WRITE view of.
    ///
    /// Deliberately the same endowment `spawn_init` gives (`INITRD_VA`, an untyped in slot 0, a report
    /// endpoint in slot 1) so what is being tested is rootsup's *choices*, not a privileged shortcut.
    fn spawn_tree() -> sched::EpId {
        let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
        let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
        let bytes = program("rootsup").expect("no rootsup program in the initrd archive");
        let elf = Elf::parse(bytes).expect("rootsup is not loadable");

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
        let mut space = AddressSpace::new(content).expect("no memory for rootsup");
        map_segments(&mut space, &elf).expect("could not lay out rootsup");
        for k in 0..INIT_STACK_PAGES {
            space
                .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
                .expect("could not map rootsup's stack");
        }
        for i in 0..initrd_pages {
            space
                .map_physical(
                    INITRD_VA + i * FRAME_SIZE,
                    initrd_start + i * FRAME_SIZE,
                    Flags::user_rodata(),
                )
                .expect("could not map the initrd");
        }
        let aspace = readopt_user_aspace(space).expect("register the rootsup aspace");

        let report = sched::create_endpoint();
        let budget = crate::untyped::create(ROOT_BUDGET_PAGES).expect("no budget for rootsup");
        let tcb_region = crate::untyped::create(2).expect("no tcb region");
        let tid = sched::create_tcb(tcb_region).expect("no tcb");
        let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
            .expect("insert budget");
        assert_eq!(s0, 0, "rootsup's budget must land in slot 0");
        let s1 = sched::tcb_insert_cap(
            tid,
            crate::cap::endpoint_cap(
                report,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            ),
            None,
        )
        .expect("insert report");
        assert_eq!(s1, 1, "rootsup's report endpoint must land in slot 1");
        sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
        sched::start_tcb(tid, [0, initrd_len, 0]).expect("start");
        report
    }

    /// How many reports a healthy run of the tree makes: init's drop, the first instance running, its
    /// crash reaching the supervisor, the replacement running, and the replacement's clean exit
    /// reaching the supervisor. Exactly five, which is itself an assertion: a sixth would mean the
    /// supervisor restarted something it should have left finished, or a tier-one server died.
    const EXPECTED_REPORTS: usize = 5;

    /// **Run one tree from spawn to quiescence**, returning every report it made.
    ///
    /// Runs it to the end rather than stopping at the first interesting message, for a reason worth
    /// recording: a half-run tree keeps building processes in the background, and the first version of
    /// these tests left one running, which broke a *later* test's thread accounting
    /// (`destroy_force_kills_a_runaway` counts threads before and after). A test that leaves work
    /// running is a test that fails somebody else.
    ///
    /// The order of the five is not fixed (init's drop races the sub-server's first run), so callers
    /// filter by kind; within a kind the order is causal and asserted.
    fn run_tree() -> [[u64; 5]; EXPECTED_REPORTS] {
        let report = spawn_tree();
        let mut msgs = [[0u64; 5]; EXPECTED_REPORTS];
        for slot in msgs.iter_mut() {
            let msg = sched::ipc_recv(report);
            assert_ne!(
                msg[0], REPORT_FAILED,
                "the supervision tree could not be built: stage {}",
                msg[1]
            );
            assert_ne!(
                msg[0], REPORT_SUP_GAVE_UP,
                "the supervisor exhausted its retry budget ({} restarts): the replacement should \
                 have survived",
                msg[1],
            );
            *slot = msg;
        }

        // Let the tree settle, then prove it has nothing more to say. A parked sender here means a
        // sixth report exists, which is how "the supervisor did not restart a finished server" is
        // proven without a blocking receive that would hang when the code is right.
        for _ in 0..400 {
            sched::yield_now();
        }
        assert_eq!(
            sched::endpoint_waiting_senders(report),
            0,
            "the tree made more than {EXPECTED_REPORTS} reports: something acted after the \
             sub-server finished",
        );
        msgs
    }

    /// Every report of one kind, in arrival order.
    fn of_kind(msgs: &[[u64; 5]; EXPECTED_REPORTS], kind: u64) -> impl Iterator<Item = &[u64; 5]> {
        msgs.iter().filter(move |m| m[0] == kind)
    }

    /// **init drops its construction authority, and the drop is real.**
    ///
    /// rootsup builds its two servers, deletes the wiring capabilities and then the untyped budget
    /// itself, and immediately tries the two primitives that build things: retype a page, and retype a
    /// kernel object. Both must fail, and they must fail with `NoSuchSlot` (there is nothing there)
    /// rather than `NotPermitted` (there is something there and you may not use it), because the
    /// capability is *gone*, not narrowed. That distinction is the whole difference between "we asked
    /// init not to" and "init cannot."
    ///
    /// It is reported from inside the process on purpose: what matters is what the *holder* can do,
    /// and only the holder can ask.
    #[test_case]
    fn init_drops_its_construction_authority_and_cannot_build_again() {
        let msgs = run_tree();
        let dropped = of_kind(&msgs, REPORT_INIT_DROPPED)
            .next()
            .expect("init never reported dropping its budget");
        assert_eq!(
            dropped[1], 1,
            "init still built a page or a kernel object after deleting its untyped: the authority \
             was not actually dropped",
        );
        assert_eq!(
            dropped[2], 1,
            "using the dropped budget failed with error {} (negated), not NoSuchSlot: the slot \
             should be empty, not merely restricted",
            dropped[2],
        );
    }

    /// **A dead sub-server is restarted by its own supervisor, in userspace, and init cannot have
    /// helped.**
    ///
    /// The sequence: the sub-server runs as attempt 0 and crashes on a load from an unmapped address;
    /// its supervisor receives the kernel's fault message, reaps the corpse through the spawner (§16
    /// revocation), and asks for attempt 1; attempt 1 runs and exits cleanly; the supervisor reads
    /// EXIT as "finished" and does **not** restart it again. Every decision in that paragraph is code
    /// in an unprivileged process that holds no memory at all, and the kernel's whole contribution is
    /// one message.
    ///
    /// **How "without init's involvement" is proven, and why it is not a timing argument.** init has
    /// no construction authority by then: it deleted its untyped, and the companion test above
    /// confirms it can no longer use it. A process that cannot retype a page cannot have built the
    /// replacement. Authority, not scheduling order, is the evidence.
    #[test_case]
    fn a_dead_sub_server_is_restarted_by_its_supervisor_not_by_init() {
        let msgs = run_tree();

        let mut ran = of_kind(&msgs, REPORT_SERVER_RAN);
        let first = ran.next().expect("the sub-server never ran at all");
        assert_eq!(first[1], 0, "the first instance should be attempt 0");
        let second = ran
            .next()
            .expect("the crashed sub-server was never restarted");
        assert_eq!(
            second[1], 1,
            "the replacement was not started as attempt 1: the supervisor's restart policy did not \
             run, or ran with the wrong state",
        );
        assert!(
            ran.next().is_none(),
            "a third instance ran: the supervisor restarted a server that had finished",
        );

        let mut deaths = of_kind(&msgs, REPORT_SUP_SAW_DEATH);
        let crash = deaths.next().expect("the supervisor saw no death");
        assert_eq!(
            crash[2],
            abi::fault::EVENT_FAULT,
            "the crash should reach the supervisor as a FAULT event",
        );
        assert_ne!(
            crash[1], 0,
            "the fault message carried no tid: the supervisor cannot tell who died",
        );
        // The other half of §26's "both events flow": a clean exit must arrive as EXIT, because that
        // is what lets a userspace policy tell "finished" from "crashed" without guessing.
        let finished = deaths
            .next()
            .expect("the replacement's clean exit never reached the supervisor");
        assert_eq!(
            finished[2],
            abi::fault::EVENT_EXIT,
            "attempt 1 exited cleanly, so the supervisor must see EXIT, not FAULT",
        );
    }
}

/// **Measured boot: the kernel refuses to enter an init it was not built for** (milestone 22 phase
/// B.1, DECISIONS §22).
///
/// Cross-ISA, because the check is portable: one hash implementation (`crates/measure`), one trust
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
mod measured_boot_tests {
    use super::*;

    /// The boot-program entry both architectures' kernels load themselves. (riscv's shell boot loads
    /// `sysinit` instead, measured under that name by the same trust root; `init` is the entry both
    /// ISAs have, so it is the one this test can assert on portably.)
    const BOOT_PROGRAM: &str = "init";

    /// **The initrd in RAM is the initrd this kernel was built against.** The end-to-end build
    /// composition check: nothing here is hard-coded, the digest comes out of the kernel's own
    /// `.rodata` and the bytes come out of the archive QEMU loaded, and they have to agree. If the
    /// build ever writes the manifest after compiling the kernel, or measures the wrong entry, or the
    /// archive is repacked without a kernel relink, this fails.
    #[test_case]
    fn the_boot_program_measures_to_the_compiled_in_trust_root() {
        let bytes = program(BOOT_PROGRAM).expect("no boot program in the initrd archive");
        assert!(
            crate::trust::expected(BOOT_PROGRAM).is_some(),
            "the kernel image carries no measurement for '{BOOT_PROGRAM}': the build's measurement \
             step did not run, and an unmeasured boot would be refused at boot time",
        );
        assert_eq!(
            crate::trust::verify(BOOT_PROGRAM, bytes),
            Ok(()),
            "the boot program in RAM is not the one this kernel image was built against",
        );
    }

    /// **One flipped bit is refused, and an unmeasured name is refused too.** The tamper is measured
    /// by streaming (flip the first byte, then the rest untouched) rather than by copying a
    /// 300 KiB ELF, because there is no heap to copy it into; the digest is the same one a real
    /// tampered initrd would produce.
    #[test_case]
    fn a_tampered_boot_program_and_an_unmeasured_name_are_both_refused() {
        let bytes = program(BOOT_PROGRAM).expect("no boot program in the initrd archive");
        let mut h = measure::Sha256::new();
        h.update(&[bytes[0] ^ 1]);
        h.update(&bytes[1..]);
        let tampered = h.finalize();

        assert_eq!(
            measure::verify_digest(crate::trust::TRUST_ROOT, BOOT_PROGRAM, &tampered),
            Err(measure::VerifyError::Mismatch),
            "a boot program with one bit flipped still satisfied the trust root",
        );
        // Fail-closed on the other axis: a program the trust root says nothing about is refused, not
        // waved through. This is what makes an empty or stale trust root safe.
        assert_eq!(
            crate::trust::verify("no-such-program", bytes),
            Err(measure::VerifyError::Unmeasured),
            "the kernel vouched for a program it has no measurement for",
        );
    }
}

/// Parity C: the virtio-blk driver, its two attackers, and the DMA confinement, on RISC-V.
///
/// **The fault endpoint: a supervisor watches a child die and reap it** (milestone 22, DECISIONS
/// §26). These are the cross-ISA tests, because the mechanism is portable: a supervised child that
/// faults (or exits) turns into a five-word message on its supervision endpoint, its corpse persists
/// until the supervisor reaps it with §16 revocation, and a fresh child runs in its place. The only
/// per-architecture parts are the two tiny code stubs (a null load that faults, and a `SEND` + exit),
/// and even those are the same shape both ISAs already use elsewhere in this file. The kernel is the
/// only sender on the fault endpoint, so the tid the supervisor reads is trustworthy without a badge.
#[cfg(test)]
mod supervision_tests {
    use super::*;
    use crate::sched;
    use abi::fault::{EVENT_EXIT, EVENT_FAULT, FAULT_EP_SLOT};

    const CODE_VA: u64 = 0x40_0000;
    const STACK_VA: u64 = 0x50_0000;
    /// The unmapped address the fault stub loads from. Distinctive, so the delivered fault address
    /// proves the message carries real fault-time state and not a zero placeholder.
    const BAD_ADDR: u64 = 0x00A5_0000;
    /// The word the report stub SENDs, so a test can tell "the child ran" from "the child faulted."
    const REPORT_WORD: u64 = 0x42;

    /// A child that faults on its very first memory access: load from [`BAD_ADDR`], which nothing
    /// maps. Two instructions; the faulting one is the second, so the reported pc is `CODE_VA + 4`.
    #[cfg(target_arch = "aarch64")]
    const FAULT_STUB: &[u32] = &[
        0xD2A0_14A0, // movz x0, #0xA5, lsl #16   (x0 = 0x00A5_0000)
        0xF940_0001, // ldr  x1, [x0]             (data abort: nothing maps BAD_ADDR)
    ];
    #[cfg(target_arch = "riscv64")]
    const FAULT_STUB: &[u32] = &[
        0x00A5_0537, // lui a0, 0xA50             (a0 = 0x00A5_0000)
        0x0005_3583, // ld  a1, 0(a0)             (load page fault: nothing maps BAD_ADDR)
    ];

    /// A child that SENDs [`REPORT_WORD`] on the endpoint in slot 0, then exits cleanly. The same
    /// nine-instruction shape the region-reclaim tests use, so "it ran" is the SEND arriving.
    #[cfg(target_arch = "aarch64")]
    const REPORT_STUB: &[u32] = &[
        0xD280_0000,                                       // movz x0, #0            (slot 0)
        0xD280_0001, // movz x1, #0            (endpoint::SEND)
        0xD280_0000 | ((REPORT_WORD as u32) << 5) | 2, // movz x2, #REPORT_WORD
        0xD280_0003, // movz x3, #0
        0xD280_0004, // movz x4, #0
        0xD280_0000 | ((abi::SYS_INVOKE as u32) << 5) | 8, // movz x8, #SYS_INVOKE
        0xD400_0001, // svc #0                 (SEND)
        0xD280_0008, // movz x8, #0            (SYS_EXIT)
        0xD400_0001, // svc #0                 (exit)
    ];
    #[cfg(target_arch = "riscv64")]
    const REPORT_STUB: &[u32] = &[
        0x0000_0513,                                    // li a0, 0            (slot 0)
        0x0000_0593,                                    // li a1, 0            (endpoint::SEND)
        0x0000_0613 | ((REPORT_WORD as u32) << 20),     // li a2, REPORT_WORD
        0x0000_0693,                                    // li a3, 0
        0x0000_0713,                                    // li a4, 0
        0x0000_0893 | ((abi::SYS_INVOKE as u32) << 20), // li a7, SYS_INVOKE
        0x0000_0073,                                    // ecall               (SEND)
        0x0000_0893 | ((abi::SYS_EXIT as u32) << 20),   // li a7, SYS_EXIT
        0x0000_0073,                                    // ecall               (exit)
    ];

    /// Build a child from `stub` with its whole world in one region (aspace, code, stack, TCB), so a
    /// single `DESTROY` reclaims it. `report` goes in slot 0 (what the report stub SENDs on);
    /// `fault_ep`, if given, goes in the reserved fault slot, so `START` records it as the child's
    /// supervision endpoint. Returns `(child_tid, region)`.
    fn build_child(
        stub: &[u32],
        report: Option<sched::EpId>,
        fault_ep: Option<sched::EpId>,
    ) -> (u64, u64) {
        let region = crate::untyped::create(16).expect("no region for the child");
        let aspace = user_aspace_create(region).expect("no aspace");

        let code_phys = crate::untyped::retype_page(region).expect("no code frame");
        // SAFETY: a fresh frame we own, direct-mapped; write the stub and make it fetchable.
        unsafe {
            let dst = mmu::phys_to_virt(code_phys) as *mut u32;
            for (i, &insn) in stub.iter().enumerate() {
                dst.add(i).write(insn);
            }
        }
        sync_icache(mmu::phys_to_virt(code_phys), core::mem::size_of_val(stub));
        user_aspace_map(aspace, CODE_VA, code_phys, Flags::user_code()).expect("map code");

        let stack_phys = crate::untyped::retype_page(region).expect("no stack frame");
        user_aspace_map(aspace, STACK_VA, stack_phys, Flags::user_data()).expect("map stack");

        let tid = sched::create_tcb(region).expect("no tcb");
        if let Some(rep) = report {
            let cap = crate::cap::endpoint_cap(
                rep,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            );
            let slot = sched::tcb_insert_cap(tid, cap, None).expect("insert report");
            assert_eq!(
                slot, 0,
                "the report cap must land in slot 0 (the stub assumes it)"
            );
        }
        if let Some(fe) = fault_ep {
            // The spawn-slot convention: the supervision endpoint goes in the reserved fault slot.
            // Rights do not matter here (the kernel reads only the endpoint name and consumes the
            // slot at START, so the child cannot forge fault messages on it); READ is the minimum.
            let cap = crate::cap::endpoint_cap(fe, crate::cap::Rights::READ);
            sched::tcb_insert_cap(tid, cap, Some(FAULT_EP_SLOT)).expect("insert fault ep");
        }
        sched::configure_tcb(tid, CODE_VA, STACK_VA + frames::FRAME_SIZE, aspace)
            .expect("configure");
        sched::start_tcb(tid, [0; 3]).expect("start");
        (tid, region)
    }

    /// **A crash becomes a message; the corpse survives until reaped; a fresh child runs.** The whole
    /// supervision cycle in one test: spawn a child holding a fault endpoint, let it crash, receive
    /// the fault message with the right tid and fault address, confirm the corpse still holds its
    /// fault-time state (dead until reaped), reap it with revocation, and respawn a child that runs.
    #[test_case]
    fn a_faulting_child_reports_to_its_supervisor_and_is_reaped_then_respawned() {
        let fault_ep = sched::create_endpoint();
        let (child, region) = build_child(FAULT_STUB, None, Some(fault_ep));

        // The child faults on its first load. Its death arrives here, kernel-stamped.
        let msg = sched::ipc_recv(fault_ep);
        assert_eq!(msg[0], EVENT_FAULT, "a crash must report as a FAULT event");
        assert_eq!(msg[1], child, "the fault message named the wrong thread");
        assert_eq!(
            msg[2],
            CODE_VA + 4,
            "the faulting pc was not the load instruction"
        );
        assert_eq!(
            msg[3], BAD_ADDR,
            "the faulting address was not carried in the message"
        );

        // Dead until reaped: the corpse is still in the table, still holding its fault-time state,
        // and it never runs again. This is what makes postmortem (and a future resume) possible.
        assert_eq!(
            sched::corpse_fault_msg(child),
            Some(msg),
            "the corpse did not retain its fault message: it was reaped too early, or lost its state",
        );

        // Reap it with §16 revocation, the supervisor's explicit act. The corpse is Dead, not live,
        // so the region reclaims without a force-kill.
        sched::reclaim_region(region).expect("reaping the corpse's region failed");
        assert_eq!(
            sched::corpse_fault_msg(child),
            None,
            "the corpse outlived its region: revocation did not reap it",
        );

        // Respawn: a fresh child, in a fresh region, runs to completion where the crashed one died.
        let report = sched::create_endpoint();
        let (_c2, region2) = build_child(REPORT_STUB, Some(report), None);
        assert_eq!(
            sched::ipc_recv(report)[0],
            REPORT_WORD,
            "the respawned child never ran: the supervision cycle did not recover",
        );
        // The respawn exits unsupervised, so it is reaped by the scheduler; reclaim once it is gone.
        for _ in 0..2000 {
            if sched::reclaim_region(region2).is_ok() {
                break;
            }
            sched::yield_now();
        }
    }

    /// **A clean exit flows too, distinguished by the event code.** The other half of §26's "both
    /// faults and exits": a supervised child that SENDs its word and exits normally reports an EXIT
    /// event (not FAULT), with no fault pc or address, so a restart policy can tell "finished" from
    /// "crashed."
    #[test_case]
    fn a_clean_exit_reports_the_exit_event_not_a_fault() {
        let report = sched::create_endpoint();
        let fault_ep = sched::create_endpoint();
        let (child, region) = build_child(REPORT_STUB, Some(report), Some(fault_ep));

        // It runs (the SEND proves it reached EL0), then exits cleanly.
        assert_eq!(
            sched::ipc_recv(report)[0],
            REPORT_WORD,
            "the child never ran before exiting",
        );
        let msg = sched::ipc_recv(fault_ep);
        assert_eq!(
            msg[0], EVENT_EXIT,
            "a clean exit must report EXIT, not FAULT"
        );
        assert_eq!(msg[1], child, "the exit message named the wrong thread");
        assert_eq!(msg[2], 0, "a clean exit has no faulting pc");
        assert_eq!(msg[3], 0, "a clean exit has no faulting address");

        // A cleanly-exited supervised child is dead until reaped, exactly like a crashed one.
        sched::reclaim_region(region).expect("reaping the exited corpse's region failed");
    }
}

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
mod riscv_virtio_tests {
    use super::*;
    // Shared with the aarch64 module so both ISAs assert a std transcript the same way.
    use super::std_tests::{assert_fs_service_ready, assert_std_transcript, std_fs_expected};
    use crate::sched;
    use core::sync::atomic::Ordering;

    /// The `blk` driver's ELF bytes from the riscv initrd archive. Absent means the initrd was
    /// built without it (or the aarch64 archive was handed to a riscv boot, the mix-up the xtask
    /// riscv test leg exists to prevent); fail loudly rather than skip.
    fn blk_image() -> &'static [u8] {
        program("blk").expect("no blk program in the initrd archive")
    }

    /// The `netd` program's ELF bytes (milestone 30, piece 3): the smoltcp net server.
    fn netd_image() -> &'static [u8] {
        program("netd").expect("no netd program in the initrd archive")
    }

    /// The net client's test selectors and success word, matching user/src/netcli.rs. The client is
    /// a nonzero entry role of the `netd` binary, so it needs no image of its own.
    const NET_TEST_UDP_DNS: u64 = 1;
    const NET_TEST_TCP_ECHO: u64 = 2;
    const NET_TEST_TCP_REOPEN: u64 = 3;
    const NET_CLIENT_OK: u64 = 1;

    /// Spin the scheduler until `done()`, or give up after a wall-clock deadline. The aarch64
    /// module's helper, re-declared because that module is aarch64-gated; time-based for the same
    /// reason (DECISIONS §28: a fixed yield count elapses in no real time on an idle core).
    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if done() {
                return true;
            }
            sched::yield_now();
        }
        done()
    }

    /// **A faulting riscv user thread dies, and the kernel does not.** DECISIONS §10's promise
    /// ("a driver bug is a crashed process, not a dead machine"), proven on the second ISA for
    /// the first time.
    ///
    /// This test exists because the promise was NOT kept here: the riscv trap dispatcher stepped
    /// over a U-mode `ebreak` (so a panicking driver resumed its own panic loop, alive forever)
    /// and panicked the kernel on any other U-mode fault, behind a comment claiming user threads
    /// could not run on RISC-V yet. Every riscv userspace binary's panic handler ends in `ebreak`
    /// expecting to die, and no test had ever made one fault. The kill-mid-write test (below)
    /// needs a driver to genuinely die, which is what flushed this out.
    ///
    /// The blk binary's `_start` panics on an unknown role, so spawning it with one is the
    /// smallest honest fault: panic, `ebreak`, killed, reaped.
    #[test_case]
    fn a_faulting_user_thread_is_killed_and_the_kernel_survives() {
        use crate::arch::exceptions::USER_FAULTS;

        let faults = USER_FAULTS.load(Ordering::Relaxed);
        let threads = sched::thread_count();

        sched::spawn(move || {
            run(
                blk_image(),
                Spawn {
                    arg0: 0xDEAD, // no such role: _start panics, and the panic handler ebreaks
                    arg1: 0,
                    arg2: 0,
                    grants: &[],
                    maps: &[],
                },
            )
        })
        .expect("spawn failed");

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "the faulting user thread was never killed",
        );
        assert!(
            wait_for(|| sched::thread_count() <= threads),
            "the killed thread was never reaped",
        );
    }

    /// The headline, on the second ISA: an unprivileged process drives a real block device over
    /// DMA and reads a file off it, with the kernel owning only the confinement. Interrupt
    /// delivery is asserted too: the completion reached the driver as a message through its Irq
    /// capability, via the PLIC rather than the GIC.
    #[test_case]
    fn a_userspace_driver_reads_a_file_from_a_virtio_disk() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let report = match virtio_service::start(blk_image()) {
            Some(r) => r,
            None => {
                // No disk attached to this run. Nothing to test; do not fail.
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };

        let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);
        let word = sched::ipc_recv(report)[0];

        assert_eq!(
            &word.to_le_bytes(),
            b"cricker-",
            "the driver reported the wrong file contents",
        );
        assert!(
            ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
            "the read completed but no device interrupt was delivered as a message",
        );
    }

    /// **The RedoxFS filesystem service, end to end, on the second ISA** (milestone 32 phase 2).
    /// The aarch64 twin's contract, proven identically on riscv by the same suite (the parity gate):
    /// a block server, an FS server over blk IPC, and a client that opens a file through a granted
    /// directory capability, reads it, round-trips a write, and reports. The block-server role rides
    /// the portable `blk` binary here instead of hello.
    #[test_case]
    fn the_fs_server_serves_redoxfs_over_a_capability_contract() {
        let (readiness, report) = match fs_service::start(
            blk_image(),
            program("fsserver").expect("no fsserver program in the initrd archive"),
            program("fsclient").expect("no fsclient program in the initrd archive"),
            0, // the end-to-end proof role, not the benchmark loop
        ) {
            Some(r) => r,
            None => {
                crate::println!("    (no RedoxFS disk attached; skipping)");
                return;
            }
        };

        assert_fs_service_ready(readiness);
        let [head, status, ..] = sched::ipc_recv(report);
        assert_eq!(
            status,
            fs_proto::fixture::SUCCESS,
            "the client did not report success: a check in the read or write path failed",
        );
        assert_eq!(
            &head.to_le_bytes()[..],
            &fs_proto::fixture::MOTD[..8],
            "the client read the wrong motd bytes off the RedoxFS image",
        );
    }

    /// **`std::fs` over the FS-service contract, on the second ISA** (milestone 27 phase two, the
    /// parity gate). The aarch64 twin's proof, same binary, same contract, same transcript: a std
    /// program granted one directory capability reads the file the RedoxFS image ships and is
    /// refused every path that would leave that directory. See the aarch64 twin for what it proves.
    #[test_case]
    fn std_fs_reads_a_file_through_a_granted_directory_capability() {
        let (readiness, report) = match fs_service::start_std(
            blk_image(),
            program("fsserver").expect("no fsserver program in the initrd archive"),
            program("hellostd").expect("no hellostd program in the initrd archive"),
        ) {
            Some(r) => r,
            None => {
                crate::println!("    (no RedoxFS disk attached; skipping)");
                return;
            }
        };
        assert_fs_service_ready(readiness);

        let mut want = [0u8; 512];
        let n = std_fs_expected(&mut want);
        assert_std_transcript(report, &want[..n], "std fs");
    }

    /// The virtio-net DHCP round trip, on the second ISA (milestone 30): a driver at EL0 brings up
    /// both queues, transmits a DHCP DISCOVER, and receives slirp's OFFER, all behind the multi-queue
    /// confinement, with the completion delivered via the PLIC. Parity with the aarch64 net test.
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net() {
        let report = match virtio_service::start_net(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };

        let yiaddr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            yiaddr & 0xffff_ff00,
            0x0A00_0200,
            "the DHCP OFFER's yiaddr {yiaddr:#010x} is not in QEMU slirp's 10.0.2.0/24",
        );
        // No fresh-interrupt assertion here; see the aarch64 twin. The net completion is the used
        // ring, not one interrupt per operation, and the shared-NIC test suite makes a strict
        // interrupt-delta unreliable. The OFFER round trip is the proof.
    }

    /// The riscv net round trip over PCIe, behind the RISC-V IOMMU (milestone 30, §20).
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net_pci() {
        let report = match virtio_service::start_net_pci(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };

        let yiaddr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            yiaddr & 0xffff_ff00,
            0x0A00_0200,
            "the DHCP OFFER's yiaddr {yiaddr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// The net server (smoltcp) acquiring a DHCP lease over the confined NIC, on the second ISA
    /// (milestone 30, piece 3). A reused userspace TCP/IP stack, driving a kernel-confined device.
    #[test_case]
    fn the_net_server_acquires_a_dhcp_lease_over_smoltcp() {
        let report = match virtio_service::start_net_server(netd_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let addr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            addr & 0xffff_ff00,
            0x0A00_0200,
            "smoltcp's DHCP lease {addr:#010x} is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// The riscv net server over PCIe, behind the RISC-V IOMMU (milestone 30, §20).
    #[test_case]
    fn the_net_server_acquires_a_dhcp_lease_over_smoltcp_pci() {
        let report = match virtio_service::start_net_server_pci(netd_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };
        let addr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            addr & 0xffff_ff00,
            0x0A00_0200,
            "smoltcp's DHCP lease {addr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// The socket contract, UDP end to end on the second ISA (milestone 30, piece 3 phase B): a
    /// client resolves a real DNS name through slirp's resolver over the granted `Stack` endpoint
    /// and shared frame.
    #[test_case]
    fn a_client_resolves_dns_through_the_socket_contract() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_UDP_DNS, false) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP DNS exchange through the socket contract failed (client code {verdict:#x})",
        );
    }

    /// The riscv UDP DNS exchange over PCIe, behind the RISC-V IOMMU.
    #[test_case]
    fn a_client_resolves_dns_through_the_socket_contract_pci() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_UDP_DNS, true) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP DNS exchange over PCIe failed (client code {verdict:#x})",
        );
    }

    /// The socket contract, TCP end to end on the second ISA: connect to slirp's guestfwd echo peer,
    /// send, receive the echo, close, the full round trip through the confined NIC.
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_TCP_ECHO, false) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the TCP echo round trip through the socket contract failed (client code {verdict:#x})",
        );
    }

    /// The riscv TCP echo round trip over PCIe, behind the RISC-V IOMMU.
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract_pci() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_TCP_ECHO, true) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net-pci device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the TCP echo round trip over PCIe failed (client code {verdict:#x})",
        );
    }

    /// Regression on the second ISA: reopening a socket id and connecting again completes (the
    /// ephemeral-port fix). See the aarch64 twin for the finding.
    #[test_case]
    fn a_reopened_socket_id_connects_again_over_tcp() {
        let report = match virtio_service::start_net_stack(netd_image(), NET_TEST_TCP_REOPEN, false)
        {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "reopening a socket id and connecting again failed (client code {verdict:#x})",
        );
    }

    /// The `hellostd` std program's ELF bytes from the riscv initrd. Given the network here, its
    /// `UdpSocket::bind` probe succeeds and it runs the net transcript.
    fn hellostd_image() -> &'static [u8] {
        program("hellostd").expect("no hellostd program in the initrd archive")
    }

    /// The exact transcript `hellostd` prints when it is granted the network.
    const STD_NET_EXPECTED: &[u8] = b"std net on cricker-os\ndns ok\ntcp echo ok\n";

    /// **`std::net` end to end over the socket contract, on the second ISA** (milestone 27 phase
    /// two): the riscv twin of the aarch64 std-net test. The `hellostd` std binary, given the
    /// network, does a real UDP DNS query and a TCP echo round trip through `std::net`, whose PAL
    /// binds to netd's contract, proving std's networking runs on the native ABI on both
    /// architectures (the §19 parity gate). Its stdout is reassembled and compared byte for byte.
    #[test_case]
    fn std_net_runs_over_the_socket_contract() {
        let report = match virtio_service::start_net_std(netd_image(), hellostd_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-net device attached; skipping)");
                return;
            }
        };

        assert_std_transcript(report, STD_NET_EXPECTED, "std net");
    }

    /// The DMA confinement holds on riscv: a descriptor aimed at kernel memory is refused and
    /// the device is never rung. The attacker reports `1` (refused).
    #[test_case]
    fn the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region() {
        let report = match virtio_service::start_attacker(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let refused = sched::ipc_recv(report)[0];
        assert_eq!(
            refused, 1,
            "a malicious driver's descriptor pointing at kernel memory was NOT refused: the \
             device could have DMA'd over the kernel",
        );
    }

    /// The indirect-descriptor escape is refused on riscv too; see the aarch64 twin for why the
    /// subtle case needs its own test.
    #[test_case]
    fn the_kernel_refuses_an_indirect_descriptor_escape() {
        let report = match virtio_service::start_attacker_indirect(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let refused = sched::ipc_recv(report)[0];
        assert_eq!(
            refused, 1,
            "an indirect descriptor whose inner table pointed at kernel memory was NOT refused: \
             the device could have followed it out of the driver's region",
        );
    }

    /// **The PCIe transport, end to end.** The identical driver binary reads the same file off
    /// the disk QEMU attached as `virtio-blk-pci`: found by ECAM enumeration, BARs placed by the
    /// kernel, registers reached through the virtio-pci common-config block, the completion
    /// interrupt arriving as INTx through the PLIC (the swizzled line, so this is also P3's
    /// proof), and the same shadow-ring confinement in the path. The driver cannot tell which
    /// bus it is on; this test is the transport seam's contract, held against real device
    /// behaviour on both sides.
    #[test_case]
    fn a_userspace_driver_reads_a_file_over_the_pcie_transport() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let report = match virtio_service::start_pci(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-pci disk on the bus; skipping)");
                return;
            }
        };

        let irqs_before = ROUTED_IRQS.load(Ordering::Relaxed);
        let word = sched::ipc_recv(report)[0];

        assert_eq!(
            &word.to_le_bytes(),
            b"cricker-",
            "the driver reported the wrong file contents over pci",
        );
        assert!(
            ROUTED_IRQS.load(Ordering::Relaxed) > irqs_before,
            "the read completed but no INTx interrupt was delivered through the PLIC",
        );
    }

    /// The write round trip on the second ISA (milestone 32 phase 1); see the aarch64 twin for
    /// what the report certifies.
    #[test_case]
    fn a_userspace_driver_writes_a_block_and_reads_it_back() {
        let report = match virtio_service::start_writer(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            &word.to_le_bytes(),
            b"CRKWRIT1",
            "the driver did not read back the pattern it wrote",
        );
    }

    /// The write round trip over the PCIe transport, on the second ISA.
    #[test_case]
    fn a_userspace_driver_writes_a_block_over_the_pcie_transport() {
        let report = match virtio_service::start_writer_pci(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio-pci disk on the bus; skipping)");
                return;
            }
        };
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            &word.to_le_bytes(),
            b"CRKWRIT1",
            "the driver did not read back the pattern it wrote over pci",
        );
    }

    /// Kill-mid-write on the second ISA; see the aarch64 twin for the full argument. This is
    /// also the test that made the riscv user-fault kill path exist: the abandoner's deliberate
    /// death is a U-mode `ebreak`, which used to be silently stepped over.
    #[test_case]
    fn a_driver_killed_mid_write_leaves_the_device_and_transport_sane() {
        use crate::arch::exceptions::USER_FAULTS;

        let faults = USER_FAULTS.load(Ordering::Relaxed);
        let report = match virtio_service::start_write_abandoner(blk_image()) {
            Some(r) => r,
            None => {
                crate::println!("    (no virtio disk attached; skipping)");
                return;
            }
        };

        assert_eq!(
            sched::ipc_recv(report)[0],
            1,
            "the abandoner never got its write submitted",
        );
        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "the abandoner never died; nothing was killed mid-write",
        );

        let report = virtio_service::start_writer(blk_image())
            .expect("the disk vanished between the abandoner and the survivor");
        let word = sched::ipc_recv(report)[0];
        assert_eq!(
            &word.to_le_bytes(),
            b"CRKWRIT1",
            "after a mid-write kill, a fresh driver could not use the device",
        );
    }
}

/// **No test may leak a runnable thread** (the regression proxy for the test-thread starvation that
/// made the RedoxFS mount overrun the hang watchdog under the net boot). A one-shot driver that
/// spins forever instead of exiting stays `Ready`/`Running` for the rest of the boot; enough of them
/// crammed onto core 0 (the scheduler places every spawn and wake on the current core, DECISIONS
/// "Open design ideas": the SMP placement gap) starve a later heavy test past the 60 s watchdog.
///
/// This module is deliberately the **last** thing in the file so it runs after every driver test on
/// both ISAs, catching an accumulated leak wherever it came from. It quiesces first (yielding lets a
/// just-finished thread be reaped by the next context switch), then asserts nothing but the idle
/// threads and this probe is still runnable. A leak fails here with the offending thread in the dump,
/// on the test that leaked's own turf, rather than as a mysterious watchdog trip three tests later.
#[cfg(test)]
mod no_leaked_threads {
    use crate::sched;

    #[test_case]
    fn the_suite_left_no_runnable_thread_spinning() {
        // Quiesce on the CLOCK, not on a yield count. A Finished thread is reaped when its OWN core
        // switches away from it, and DECISIONS §28 scatters threads across cores, so yields on this
        // core do not make a remote core reap: two hundred cheap yields can all complete before
        // another core's next timer tick. Give every core a couple of ticks to get there, then judge.
        // What stays runnable after that is still a genuine leak (a thread that never blocks and
        // never exits), so this tolerates cross-core lag without masking the thing it guards.
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            // Scoped so nothing from `current()` is held across the yield.
            if {
                let me = sched::current();
                sched::runnable_non_idle_count(&me)
            } == 0
            {
                break;
            }
            sched::yield_now();
        }
        let me = sched::current();
        let leaked = sched::runnable_non_idle_count(&me);
        if leaked != 0 {
            sched::dump_threads();
        }
        assert_eq!(
            leaked, 0,
            "{leaked} thread(s) are still runnable after the suite quiesced: a test spawned a \
             thread that never exits. A leaked spinner starves later heavy tests past the watchdog; \
             make the one-shot role exit() after it reports instead of looping forever.",
        );
    }
}
