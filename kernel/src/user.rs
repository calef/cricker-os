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
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "aarch64")]
pub const UART_RX_INTID: u32 = 33;
#[cfg_attr(not(test), allow(dead_code))]
#[cfg(target_arch = "riscv64")]
pub const UART_RX_INTID: u32 = 10;

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

// The aarch64 test module is the only caller: 19d.2 shipped, and the shape that actually became
// the boot path is `init_boot` below. RISC-V boots the same system through `riscv_shell_boot`.
#[cfg_attr(not(test), allow(dead_code))]
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
    // enable it here, so init can delegate the Irq cap to that driver. On RISC-V these are the SAME
    // source (see INIT_TEST_SGI), and binding it twice would leave the first endpoint routed to
    // nothing while the test waits on it, so bind once and grant twice.
    if UART_RX_INTID != INIT_TEST_SGI {
        crate::sched::bind_irq(UART_RX_INTID, crate::sched::create_endpoint());
        crate::arch::irq::enable(UART_RX_INTID);
    }

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
        Ok(fs) => match fs.read(INIT_ROLES_ENTRY) {
            Some(bytes) => bytes,
            None => {
                crate::println!("  boot archive has no '{INIT_ROLES_ENTRY}' program");
                crate::arch::halt();
            }
        },
        Err(e) => {
            crate::println!("  boot archive is not a crickerfs image: {e:?}");
            crate::arch::halt();
        }
    };
    crate::trust::require(INIT_ROLES_ENTRY, init_bytes);

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
        // The UART receive interrupt (slot 4), for the input driver init builds (19d.2c).
        crate::sched::grant(crate::cap::irq_cap_rights(
            UART_RX_INTID,
            crate::cap::Rights::READ.union(crate::cap::Rights::GRANT),
        ))
        .expect("grant uart rx irq");
        // The file service (slot 5) and the page its clients share with it (slot 6), when this boot
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
//   - aarch64 `spin`    (loop, no syscall, no stack)      -> the `spinner` binary (milestone 24)
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
/// `spawn_init` + `init_boot`: load `system_initializer` (the portable system builder) as the boot process, map
/// the whole initrd into it, and grant it three capabilities: a large untyped budget (slot 0), the
/// NS16550's registers as a device cap (slot 1), and the UART receive interrupt as an `Irq` cap (slot
/// 2). From those, `system_initializer` builds the console server, the input driver, and the shell out of its
/// own budget and wires them together; the kernel touches none of it. Unlike the other demos this
/// does not block: `system_initializer` and its children run on the scheduler while the boot thread parks.
#[cfg(target_arch = "riscv64")]
#[cfg_attr(not(feature = "shell"), allow(dead_code))] // the `shell` boot mode is the only caller
pub fn riscv_shell_boot(archive: &'static [u8], uart_irq: u32) -> Result<(), LoadError> {
    use crate::cap::Rights;
    const UART_PHYS: u64 = 0x1000_0000; // the NS16550 on QEMU virt

    let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
    let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);

    let fs = crickerfs::Fs::parse(archive).expect("initrd is not a crickerfs archive");
    let init_bytes = fs
        .read("system_initializer")
        .expect("archive has no 'system_initializer' program");
    // Measured boot (milestone 22 phase B.1): `system_initializer` is riscv's boot program, so it is in the
    // trust root under its own name and checked here, before its address space is built.
    crate::trust::require("system_initializer", init_bytes);
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
    // The file service (slot 3) and the page its clients share with it (slot 4), when this boot has
    // a filesystem (milestone 50). GRANT on both, because init's job with them is to delegate: it
    // narrows the endpoint into the shell and maps the frame into its address space. `a2` carries
    // the rights the endpoint holds, which is also how init is told there is one at all. `None` is
    // the ordinary case for a run with no RedoxFS disk attached.
    let fs_rights = match program("fs_server")
        .and_then(|fs_server| fs_service::root_directory(fs_service::blk_server_image(), fs_server))
    {
        Some((file_ep, file_shared)) => {
            let s3 = crate::sched::tcb_insert_cap(
                tid,
                crate::cap::endpoint_cap(file_ep, Rights::WRITE.union(Rights::GRANT)),
                None,
            )
            .expect("insert the file service");
            assert_eq!(s3, 3);
            let s4 = crate::sched::tcb_insert_cap(
                tid,
                crate::cap::frame_cap(file_shared, Rights::WRITE.union(Rights::GRANT)),
                None,
            )
            .expect("insert the shared file page");
            assert_eq!(s4, 4);
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
#[cfg_attr(not(test), allow(dead_code))] // the tour spawns it; the tests drive it
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
    /// transmit buffers, and caches, plus the program's own page tables. `net_stack` caps its heap at 128
    /// pages, so 192 leaves headroom without being unbounded.
    const NET_SERVER_BUDGET_PAGES: u64 = 192;
    /// smoltcp builds packets on the stack; one mapped stack page is not enough. Eight extra keeps
    /// the poll loop clear (`allocator_exerciser` needed three for `alloc` collections; smoltcp asks more).
    const NET_SERVER_STACK_PAGES: u64 = 8;

    /// Start the **net server** (milestone 30, piece 3): the `net_stack` binary, which runs smoltcp over
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
    /// (slot 4) where clients' socket-contract requests arrive. `net_stack` is its own binary (loaded by
    /// name), so no role selector is passed. Returns `(report endpoint, stack endpoint)`; a caller
    /// that has no client (the phase-A DHCP tests) ignores the stack, and `net_stack` simply blocks on it.
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
        let budget =
            crate::untyped::create(NET_SERVER_BUDGET_PAGES).expect("no untyped for net_stack");

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
                    arg0: 0, // net_stack is its own binary; no role selector
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
    /// Both are the `net_stack` binary (`image`): the server is entry role 0, the client is a nonzero
    /// role (the client rides in the same binary to keep the initrd under its 15-file directory
    /// limit). They share a `Stack` endpoint: `net_stack` holds `READ` (it serves), the client holds
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

        let (net_stack_report, stack) = wire_net_server(image, transport, intid, rid);

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
                image, // the net_stack binary again; a nonzero entry role runs its client half
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

        // net_stack reports its DHCP lease with a blocking `send`; drain it here so net_stack unblocks and
        // enters its serve loop (the client's first request blocks until it does). This also
        // confirms DHCP completed before the client's exchange runs. The client, spawned above,
        // waits at its first request meanwhile.
        crate::sched::ipc_recv(net_stack_report);

        Some(cli_report)
    }

    /// The networked std client's heap budget and extra stack, both larger than the hand-written
    /// client's: it is a full std program (formatting, `Vec`, `String`), so it needs the same
    /// generous heap and stack the `std_exerciser` demo does.
    const STD_NET_HEAP_PAGES: u64 = 256;
    const STD_NET_STACK_PAGES: u64 = 32;

    /// **Spawn the net server and a `std::net` client** (milestone 27 phase two): the same
    /// `std_exerciser` std binary, but now given the network, so its `UdpSocket::bind` probe succeeds
    /// and it drives a real UDP DNS query and a TCP echo round trip through `std::net`, whose PAL
    /// binds to this same `net_stack` socket contract. `net_stack` is `net_stack_image` (entry role 0, holding the
    /// NIC and `READ` on the `Stack` endpoint); `std_image` is the ordinary std ELF given the std
    /// slot convention (heap untyped at 0, stdout at 1) plus the two net slots (the `Stack`
    /// endpoint `WRITE` at 2, an untyped budget for its per-socket shared frames at 3). Over the
    /// mmio transport; the PAL sits above the transport, so proving it on one is proving it. The
    /// same binary spawned without slots 2 and 3 runs the offline transcript instead, which is what
    /// makes "no ambient network" visible: authority, not the code, decides. Returns the program's
    /// stdout endpoint for the test to reassemble, or `None` if no NIC is attached.
    pub fn start_net_std(net_stack_image: &'static [u8], std_image: &'static [u8]) -> Option<EpId> {
        use crate::cap::untyped_cap;

        let dev = crate::virtio::find_net_device()?;
        let transport = crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        };
        let (net_stack_report, stack) =
            wire_net_server(net_stack_image, transport, dev.intid, None);

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

        // Same discipline as start_net_stack: drain net_stack's blocking DHCP report so it reaches its
        // serve loop before the std program's first request, and confirm DHCP completed.
        crate::sched::ipc_recv(net_stack_report);

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
#[cfg_attr(not(test), allow(dead_code))] // spawned only by the phase-2 test
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

    /// **Extra stack pages for the FS server**, below the single page `run` maps, so the process
    /// gets `1 + FS_STACK_PAGES` pages in one contiguous run down from [`USER_STACK_TOP`].
    ///
    /// RedoxFS recurses through its tree, htree and transaction code with **8 KiB frames** (one
    /// `read_block::<TreeList<..>>` activation carries a whole 4096-byte block plus scratch), so this
    /// is not a "generous round number" question, it is a measured one. It used to be 32, and 32 was
    /// **528 bytes short** the moment milestone 31 phase 2 added `CREATE` and `TRUNCATE`: the FS
    /// server took one more level of tree recursion, ran off the bottom of its stack, and was killed
    /// by a data abort mid-request, which left every client blocked on a `CALL` that would never be
    /// answered. See [`fs_stack_used`] for the instrument that now measures this instead of guessing,
    /// and `the_fs_servers_stack_still_has_headroom` for the test that fails the day it is too small
    /// again. notes/fs-server.md carries the story.
    ///
    /// 96 is chosen against the measurement, not above it: the high-water is **135,696 bytes** (and
    /// 127,408 as measured by milestone 37, an unattributed 8 KiB lower; notes/fs-server.md carries
    /// both and the reasoning), and
    /// `1 + 96` pages is 397,312, which leaves room for roughly thirty more 8 KiB activations. That
    /// margin is the point, because recursion depth here tracks the *tree* depth, which grows with
    /// the image; a size proven on a 16 MiB fixture is not proven on a real disk. 384 KiB of frames
    /// once per boot is cheap next to the FS server's 8 MiB heap budget.
    const FS_STACK_PAGES: u64 = 96;

    /// The pattern every FS-server stack page is filled with before the process starts. Nothing but
    /// the FS server's own stack writes ever touch these frames, so a word that still reads as this
    /// is a word the stack never reached, and the deepest changed word is the high-water mark. The
    /// value is deliberately not 0 and not a plausible pointer, so a poisoned word cannot be mistaken
    /// for real data (or vice versa) in a dump.
    const STACK_POISON: u64 = 0xC71C_5E57_C71C_5E57;

    // The VAs each process expects its mappings at. Each MUST match that program's source.
    const DMA_VA: u64 = 0x0000_0000_0090_0000; // block server DMA region, 2 pages (user/src/virtio.rs)
    const BLK_PAGE_FS: u64 = 0x5000_0000; // FS server's block page (fs_server.rs BLK_PAGE)
    const FILE_PAGE_FS: u64 = 0x5000_1000; // FS server's file page (fs_server.rs FILE_PAGE)
    const FILE_VA_CLIENT: u64 = 0x0000_0000_0060_0000; // client's file page (fs_test_client.rs FILE_VA)

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

    /// How many FS servers one boot can start, and therefore how many banks of poisoned stack pages
    /// [`FS_STACK_PHYS`] holds: the ordinary one, and milestone 37's two (the server that is killed
    /// mid-transaction, and the one that mounts the disk it left behind).
    ///
    /// They cannot share stack frames. The killed server is still executing its trap when the
    /// recovery server starts, so reusing its pages would have one process writing another's stack,
    /// and the bug would present as the recovery mount failing for no reason.
    const FS_SERVERS: usize = 3;

    /// A fresh frame filled with [`STACK_POISON`], for one of the FS server's stack pages, and
    /// remembered in [`FS_STACK_PHYS`] so the depth actually reached can be read back afterwards.
    fn poisoned_stack_frame(server: usize, index: usize) -> u64 {
        let p = crate::memory::alloc()
            .expect("no frame for the fs server's stack")
            .addr();
        // SAFETY: fresh frame, reachable through the direct map, exactly FRAME_SIZE bytes.
        let words = unsafe {
            core::slice::from_raw_parts_mut(
                mmu::phys_to_virt(p) as *mut u64,
                FRAME_SIZE as usize / 8,
            )
        };
        words.fill(STACK_POISON);
        FS_STACK_PHYS[server][index].store(p, core::sync::atomic::Ordering::Relaxed);
        p
    }

    /// The physical frames behind every FS server's extra stack pages: one bank per server, index 0
    /// being the page directly below [`USER_STACK_VA`]. Written once at wiring, read by
    /// [`fs_stack_used`]. Zero means "this boot never started that server".
    #[allow(clippy::declare_interior_mutable_const)] // an atomic array is exactly what this is for
    static FS_STACK_PHYS: [[core::sync::atomic::AtomicU64; FS_STACK_PAGES as usize]; FS_SERVERS] =
        [const { [const { core::sync::atomic::AtomicU64::new(0) }; FS_STACK_PAGES as usize] };
            FS_SERVERS];

    /// **How deep the FS server's stack actually went**, in bytes below [`USER_STACK_TOP`], and how
    /// much it was given. `None` if this boot wired no FS service.
    ///
    /// Read by scanning the poison: the deepest word that is no longer [`STACK_POISON`] is the
    /// deepest the process ever wrote. This is a measurement of the whole run so far, not a sample,
    /// because nothing ever un-writes a stack word. The base page `run` maps is counted as fully used
    /// (it holds the entry frame and cannot be scanned from here), which is true and is why the
    /// number starts at one page.
    ///
    /// The point of it is that a stack size is otherwise a number nobody can defend. The one before
    /// this was 528 bytes too small, and the way we found out was a mystery hang.
    ///
    /// The high-water is a **maximum over every FS server this boot started**, which is why
    /// milestone 37's recovery mount is covered by it too. A mount that has to walk back a
    /// generation is the case most likely to recurse further than a clean one, so it is exactly the
    /// case this instrument should be watching.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn fs_stack_used() -> Option<(u64, u64)> {
        use core::sync::atomic::Ordering;
        let total = (FS_STACK_PAGES + 1) * FRAME_SIZE;
        let mut deepest = FRAME_SIZE; // the base page, always used
        let mut wired = false;
        for bank in FS_STACK_PHYS.iter() {
            for (i, slot) in bank.iter().enumerate() {
                let phys = slot.load(Ordering::Relaxed);
                if phys == 0 {
                    continue;
                }
                wired = true;
                // SAFETY: a frame this module allocated and still owns, via the direct map.
                let words = unsafe {
                    core::slice::from_raw_parts(
                        mmu::phys_to_virt(phys) as *const u64,
                        FRAME_SIZE as usize / 8,
                    )
                };
                // Page `i` spans [USER_STACK_VA - (i+1)*FRAME, USER_STACK_VA - i*FRAME). Word `w` in
                // it sits that far above the page's base, so a touched word means at least this much
                // depth.
                if let Some(w) = words.iter().position(|&x| x != STACK_POISON) {
                    let depth = (i as u64 + 2) * FRAME_SIZE - w as u64 * 8;
                    deepest = deepest.max(depth);
                }
            }
        }
        wired.then_some((deepest, total))
    }

    /// **One boot, one FS service**, remembered here.
    ///
    /// The block server owns the RedoxFS device: a second wiring would put a second driver on the
    /// same virtio slot and re-bind its interrupt, so the two client tests (the hand-written
    /// `fs_test_client` and the std program) share one wired service instead. Whichever runs first pays
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
    fn ensure(blk_image: &'static [u8], fs_server_image: &'static [u8]) -> Option<Service> {
        use core::sync::atomic::Ordering;

        if WIRED.load(Ordering::Acquire) {
            return Some((
                FILE_EP.load(Ordering::Relaxed),
                FILE_SHARED.load(Ordering::Relaxed),
                None,
            ));
        }
        let (blk_ready, ready, file_ep, file_shared) = wire_servers(blk_image, fs_server_image)?;
        FILE_EP.store(file_ep, Ordering::Relaxed);
        FILE_SHARED.store(file_shared, Ordering::Relaxed);
        WIRED.store(true, Ordering::Release);
        Some((file_ep, file_shared, Some((blk_ready, ready))))
    }

    /// Wire and spawn the block server and the FS server. `blk_image` is the driver binary carrying
    /// the block-server role (hello on aarch64, `blk` on riscv); `fs_server_image` is the same on
    /// both ISAs. Returns `(blk_ready, ready, file_ep, file_shared)`.
    fn wire_servers(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
    ) -> Option<(EpId, EpId, EpId, u64)> {
        let (blk_ep, blk_ready, blk_shared) =
            spawn_block_server(blk_image, crate::virtio::find_block_device_n(1)?);
        let file_shared = frame();
        let file_ep = crate::sched::create_endpoint(); // client WRITE (CALL) -> FS server READ
        let ready = crate::sched::create_endpoint(); // FS server WRITE -> the kernel test RECVs
        spawn_fs_server(
            fs_server_image,
            FsServer {
                slot: 0,
                blk_ep,
                blk_shared,
                file_ep,
                file_shared,
                ready,
                budget_pages: FS_BUDGET_PAGES,
                crash: (0, 0, 0),
            },
        );
        Some((blk_ready, ready, file_ep, file_shared))
    }

    /// **Spawn a block server on one virtio block device.** Extracted from [`wire_servers`] because
    /// milestone 37's crash test needs a second one, on its own disk, so that a test which
    /// deliberately leaves a filesystem half-written cannot touch the image every other FS test
    /// depends on. Returns `(blk_ep, blk_ready, blk_shared)`.
    ///
    /// The DMA region is TWO contiguous pages: page 0 for the rings, request header and status
    /// (block-server-private), page 1 for the 4096-byte data buffer. Page 1 is ALSO the block page
    /// shared with the FS server, so the device DMAs a whole filesystem block straight into the FS
    /// server's page, one request per block, no copy.
    fn spawn_block_server(
        blk_image: &'static [u8],
        dev: crate::virtio::VirtioMmioDevice,
    ) -> (EpId, EpId, u64) {
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
            );
        };
        let blk_shared = dma + FRAME_SIZE; // page 1 of the region is the shared block page

        let blk_ep = crate::sched::create_endpoint(); // FS server WRITE (CALL) -> block server READ
        let blk_ready = crate::sched::create_endpoint(); // block server WRITE -> the kernel test RECVs

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

        (blk_ep, blk_ready, blk_shared)
    }

    /// Everything one FS-server process is, as a value, because eight positional arguments of which
    /// four are bare `u64` endpoint ids is a call nobody can read and a caller can silently get the
    /// wrong way round.
    struct FsServer {
        /// Which bank of [`FS_STACK_PHYS`] this process's poisoned stack is recorded in. One per
        /// FS server a boot can start, so the high-water instrument covers all of them.
        slot: usize,
        blk_ep: EpId,
        blk_shared: u64,
        file_ep: EpId,
        file_shared: u64,
        ready: EpId,
        /// The untyped budget this server's heap draws from, in frames. The ordinary server gets
        /// [`FS_BUDGET_PAGES`]; milestone 37's two get a fraction of it, because an untyped is
        /// **reserved** rather than merely capped and three 8 MiB reservations do not fit in this
        /// machine's 128 MiB (the first symptom was init failing to get its own budget, several
        /// tests later, which is a long way from the cause). The measured high-water of a real mount
        /// under this allocator is 352 KiB (DECISIONS §27), so [`CRASH_BUDGET_PAGES`] is still five
        /// times the number rather than a guess trimmed until it fit.
        budget_pages: u64,
        /// Milestone 37's crash injection, straight into the process's START arguments:
        /// `(which WRITE request to die in, block writes to allow first, bytes of the last one that
        /// reach the platter)`. All zero disables it, which is every FS server but the crash test's
        /// first one. See `fs_server/src/bin/fs_server.rs`.
        crash: (u64, u64, u64),
    }

    /// Spawn one FS server: a heap budget, the block-service endpoint (client side), the
    /// file-service endpoint (server side), and both shared pages. No device, no DMA.
    ///
    /// It also gets a DEEP stack. `run` maps one stack page (enough for the shallow programs), but
    /// RedoxFS recurses through its tree and htree and commits transactions on the stack, and one
    /// 4 KiB page overflows immediately (the first `open` faults ~4.2 KiB down). So map extra stack
    /// pages below `USER_STACK_VA` out of fresh frames. These are shared-style mappings (not freed on
    /// death), a one-time cost per FS server a boot starts.
    fn spawn_fs_server(fs_server_image: &'static [u8], cfg: FsServer) {
        let budget =
            crate::untyped::create(cfg.budget_pages).expect("no heap budget for the FS server");
        let mut stack = [0u64; FS_STACK_PAGES as usize];
        for (i, f) in stack.iter_mut().enumerate() {
            *f = poisoned_stack_frame(cfg.slot, i);
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
                phys: cfg.blk_shared,
                flags: Flags::user_data(),
            };
            maps[1] = Mapping {
                va: FILE_PAGE_FS,
                phys: cfg.file_shared,
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
                fs_server_image,
                Spawn {
                    arg0: cfg.crash.0,
                    arg1: cfg.crash.1,
                    arg2: cfg.crash.2,
                    grants: &[
                        untyped_cap(budget),                     // slot 0: the heap's untyped budget
                        endpoint_cap(cfg.blk_ep, Rights::WRITE), // slot 1: CALL the block server
                        endpoint_cap(cfg.file_ep, Rights::READ), // slot 2: RECV file requests
                        endpoint_cap(cfg.ready, Rights::WRITE),  // slot 3: signal readiness once
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the FS server");
    }

    // =======================================================================================
    // Milestone 37: the crash test's own service, on its own disk (DECISIONS §34 condition 1)
    // =======================================================================================

    /// What [`start_crash`] hands back: the two readiness endpoints, and the endpoint the driver
    /// reports its acknowledged write on.
    ///
    /// `fs_ready` carries **two** messages in sequence, and that is the design rather than an
    /// accident: the FS server's ordinary readiness sentinel when the mount is up, and then
    /// `fixture::crash::CUT` from inside the injector, immediately before it traps. The second one
    /// is what tells the test the kill was the injector's doing and not something else going wrong,
    /// and it is what gives the recovery mount a defined moment to start at instead of a guess.
    #[cfg_attr(not(test), allow(dead_code))]
    pub struct CrashRun {
        pub blk_ready: EpId,
        pub fs_ready: EpId,
        pub driver_report: EpId,
    }

    /// The heap budget each of milestone 37's two FS servers draws from. Smaller than
    /// [`FS_BUDGET_PAGES`] on purpose: an untyped is a reservation, and three 8 MiB ones do not fit
    /// beside everything else this boot builds in 128 MiB. 2 MiB is five times the 352 KiB
    /// high-water a real mount under this allocator actually reaches (DECISIONS §27's measurement),
    /// and the crash workload is one open and two 66-byte writes.
    const CRASH_BUDGET_PAGES: u64 = 512;

    /// The crash disk's block-service endpoint and shared block page, remembered between the two
    /// halves of the test: the recovery server is a **different process** on the **same** block
    /// server, which is endpoint-only naming doing its job. The block server never learns that its
    /// client died and was replaced, because it never knew who its client was.
    static CRASH_BLK_EP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    static CRASH_BLK_SHARED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

    /// **Phase one: wire a filesystem on its own disk and kill it mid-transaction** (milestone 37).
    ///
    /// Three processes on virtio block device 2, which nothing else in the boot touches. The disk is
    /// dedicated on purpose: this test deliberately leaves a filesystem half-written, and pointing it
    /// at the shared fixture would couple every later test to whether this one ran first. DECISIONS
    /// §27 records what an order-coupled gate costs (three investigations, three incompatible root
    /// causes, none of them real), so the fixture is regenerated per run and owned by one test.
    ///
    /// The FS server is spawned **armed**: die one block write into the second `WRITE` request, with
    /// that block torn in half. One is the count that cannot miss, because a write transaction always
    /// issues at least one block write and a larger count is a server that never dies and a test that
    /// hangs.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_crash(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        client_image: &'static [u8],
    ) -> Option<CrashRun> {
        use core::sync::atomic::Ordering;
        let (blk_ep, blk_ready, blk_shared) =
            spawn_block_server(blk_image, crate::virtio::find_block_device_n(2)?);
        CRASH_BLK_EP.store(blk_ep, Ordering::Relaxed);
        CRASH_BLK_SHARED.store(blk_shared, Ordering::Relaxed);

        let file_shared = frame();
        let file_ep = crate::sched::create_endpoint();
        let fs_ready = crate::sched::create_endpoint();
        spawn_fs_server(
            fs_server_image,
            FsServer {
                slot: 1,
                blk_ep,
                blk_shared,
                file_ep,
                file_shared,
                ready: fs_ready,
                budget_pages: CRASH_BUDGET_PAGES,
                // Die one block write into the SECOND write request, tearing that block at 2048
                // bytes. The first write is acknowledged and must survive; the second is the one the
                // property is about.
                crash: (2, 1, 2048),
            },
        );

        let driver_report = spawn_fs_client(client_image, file_ep, file_shared, 3, 0, 0, 0);
        Some(CrashRun {
            blk_ready,
            fs_ready,
            driver_report,
        })
    }

    /// **Phase two: mount the disk the dead server left behind** (milestone 37).
    ///
    /// A fresh FS-server process, on the same block server and the same block page, with its own file
    /// endpoint, its own file page and its own stack. It carries nothing over from the process that
    /// died: what it recovers, it recovers from the platter, through the ordinary `Server::open` every
    /// FS server in this system mounts with. Its readiness sentinel arriving at all is the first half
    /// of the result, because that open fails outright on an image it cannot make sense of.
    ///
    /// Returns `(ready, report)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn recover_crash(
        fs_server_image: &'static [u8],
        client_image: &'static [u8],
    ) -> (EpId, EpId) {
        use core::sync::atomic::Ordering;
        let file_shared = frame();
        let file_ep = crate::sched::create_endpoint();
        let ready = crate::sched::create_endpoint();
        spawn_fs_server(
            fs_server_image,
            FsServer {
                slot: 2,
                blk_ep: CRASH_BLK_EP.load(Ordering::Relaxed),
                blk_shared: CRASH_BLK_SHARED.load(Ordering::Relaxed),
                file_ep,
                file_shared,
                ready,
                budget_pages: CRASH_BUDGET_PAGES,
                crash: (0, 0, 0), // this one is not armed: it is the one that has to survive
            },
        );
        let report = spawn_fs_client(client_image, file_ep, file_shared, 4, 0, 0, 0);
        (ready, report)
    }

    /// The most extra stack pages [`spawn_fs_client`] will map below the one `run` gives every
    /// program. Small on purpose: a client that needs more than this is a client whose frames want
    /// looking at, not a number that wants raising.
    const CLIENT_EXTRA_STACK: usize = 8;

    /// Spawn one client holding exactly a file-service endpoint, a report endpoint and its view of
    /// the shared page. Returns the report endpoint.
    ///
    /// `extra_stack` is pages **below** the single one `run` maps. The hand-written `fs_test_client` roles
    /// need none: they hold a handle and a small buffer. The navigating shell (milestone 47's
    /// commands) needs some, and the number is a measurement rather than a guess: with none it
    /// overflowed by 192 bytes, which presented as a data abort on its own `sp` and then as the
    /// 60-second lost-wakeup watchdog, because the kernel test was still waiting for a report from a
    /// process that had died. A shell carries a path stack (eight levels of name), a parsed path,
    /// and a listing buffer, all by value, so 4 KiB is genuinely tight for it. Same discipline as
    /// the FS server's stack (DECISIONS §27): sized by what it did, not by what looks generous.
    ///
    /// **Milestone 50 moved it again, the same way.** `Endowment` grew a sink and a source, each of
    /// which can carry a whole `FileGrant`, and the shell now holds one endowment per pipeline
    /// stage; four extra pages overflowed by 48 bytes at the same `sp` and with the same symptom.
    /// Recorded because it will happen a third time: the shell's stack is sized by the largest value
    /// `grant_plan` hands it, so a field added there is a page needed here.
    fn spawn_fs_client(
        client_image: &'static [u8],
        file_ep: EpId,
        file_shared: u64,
        role: u64,
        arg: u64,
        arg2: u64,
        extra_stack: usize,
    ) -> EpId {
        assert!(
            extra_stack <= CLIENT_EXTRA_STACK,
            "an FS client asked for more stack than this wiring maps",
        );
        let report = crate::sched::create_endpoint();
        crate::sched::spawn(move || {
            let mut maps = [Mapping {
                va: FILE_VA_CLIENT,
                phys: file_shared,
                flags: Flags::user_data(),
            }; 1 + CLIENT_EXTRA_STACK];
            for (k, m) in maps[1..=extra_stack].iter_mut().enumerate() {
                m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
                m.phys = frame();
            }
            run(
                client_image,
                Spawn {
                    arg0: role,
                    arg1: arg,
                    // A third word, because the `rm` program is started the way a **grant** is
                    // (`fs_proto::grant`): a spec word and two words of name. Every other client
                    // here takes a role and one number and leaves this zero.
                    arg2,
                    grants: &[
                        endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                        endpoint_cap(report, Rights::WRITE),  // slot 1: report to the kernel
                    ],
                    maps: &maps[..1 + extra_stack],
                },
            )
        })
        .expect("could not spawn the FS client");
        report
    }

    /// Wire the service (or reuse this boot's) and spawn the hand-written client
    /// (`user/src/fs_test_client.rs`): the file-service endpoint, which IS its directory capability, the
    /// report endpoint, and its view of the shared file page. It names nothing else in the system.
    ///
    /// Returns `(readiness, report)`: the two readiness endpoints if this call wired the service,
    /// and the endpoint the client reports on.
    pub fn start(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        client_image: &'static [u8],
        client_role: u64,
    ) -> Option<(Option<(EpId, EpId)>, EpId)> {
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
        // 0 = the end-to-end proof; 1 = the fs_read benchmark loop.
        let report = spawn_fs_client(client_image, file_ep, file_shared, client_role, 0, 0, 0);
        Some((readiness, report))
    }

    /// **Wire a per-file grant and the program that holds it** (milestone 31 phase 2,
    /// notes/grant-expression.md). This is what `run wc report.txt` resolves to, wired by the
    /// kernel's test suite instead of by the shell, so the mechanism is gated on both ISAs.
    ///
    /// Three processes, and the shape is the point:
    ///
    /// ```text
    ///   FS server ──file IPC──► fs_file_caretaker ──narrowed file IPC──► the confined program
    ///                (a directory)          (one file, one direction)
    /// ```
    ///
    /// The caretaker holds the directory capability. The program holds an endpoint to the caretaker
    /// and **nothing that names the FS server**, so "it cannot reach a second file" is a statement
    /// about its cspace, not about a check it is trusted to pass. The narrowing is an address
    /// space, which is why the attacker test below is a witness rather than an assertion.
    ///
    /// One frame is shared by all three. Every request on both hops is a blocking `CALL`, so the
    /// client is parked inside its own call for the whole time the caretaker is using the page; a
    /// second frame would buy a copy and no isolation, since the client is entitled to the bytes
    /// either way.
    ///
    /// The grant, as one value, because its four fields are one decision: which file, in which
    /// direction, handed to which program started how. Splitting them across a long argument list
    /// invites a caller to get `rights` and `role` the wrong way round, and both are bare integers.
    #[cfg_attr(not(test), allow(dead_code))]
    pub struct Grant {
        /// The one name the caretaker will answer for. Must fit [`fs_proto::grant::MAX_NAME`].
        pub name: &'static str,
        /// `grant::READ`, or `READ | WRITE`.
        pub rights: u64,
        /// The confined program's `arg0` (its role) and `arg1`.
        pub role: u64,
        pub arg: u64,
    }

    /// **Drain the FS service's two readiness sentinels**, which is a sequencing act and not only an
    /// assertion.
    ///
    /// Both servers announce with a blocking `SEND`, so each is parked inside its own announcement
    /// until somebody receives it. Nothing they serve can be answered before that. Every caller that
    /// needs the service to be *running* rather than merely spawned has to come through here first,
    /// and [`wait_for_caretaker`] documents what that ordering is load-bearing for.
    ///
    /// `None` means an earlier caller in this boot already wired the service and drained these.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn wait_for_service(readiness: Option<(EpId, EpId)>) {
        let Some((blk_ready, fs_ready)) = readiness else {
            return;
        };
        assert_eq!(
            crate::sched::ipc_recv(blk_ready)[0],
            fs_proto::fixture::READY,
            "the block server did not bring the RedoxFS device up",
        );
        assert_eq!(
            crate::sched::ipc_recv(fs_ready)[0],
            fs_proto::fixture::READY,
            "the FS server did not open the RedoxFS image",
        );
    }

    /// **Wait for a caretaker's startup request to have been answered, before the program it
    /// confines exists at all.**
    ///
    /// This is a correction, and the bug it fixes is worth stating where it was made rather than
    /// only in the note. All three processes share **one frame** (the module comment above argues
    /// that is sound because every request on both hops is a blocking `CALL`, so the client is
    /// parked inside its own call for the whole time the caretaker is using the page). That
    /// argument holds once the caretaker is serving. It does not hold at **startup**, where the
    /// caretaker stages the granted name in the page and then blocks in a `CALL` to the FS server:
    /// a confined program that already exists writes its own first name over that page, and the FS
    /// server resolves whatever it finds there.
    ///
    /// It is not even a race in the common case. When this call is the one that wires the service,
    /// the FS server is parked inside its readiness `SEND`, so the caretaker's descent cannot be
    /// answered until [`wait_for_service`] drains it, and the client has that whole window to
    /// clobber the page. The caretaker then dies (it will not serve a hole) and its client blocks
    /// forever on a call nobody will answer, which is how this presented: a userspace `ebreak`
    /// followed by the 60 s lost-wakeup watchdog. It passed on aarch64 and failed on riscv, which
    /// is the shape of a timing bug and was one.
    ///
    /// The fix is ordering, not a second page: drain the service, wait for the caretaker's own
    /// sentinel, and only then spawn the confined program.
    #[cfg_attr(not(test), allow(dead_code))]
    fn wait_for_caretaker(caretaker_ready: EpId) {
        assert_eq!(
            crate::sched::ipc_recv(caretaker_ready)[0],
            fs_proto::fixture::READY,
            "the caretaker could not open what it was granted, so there is nothing to attenuate",
        );
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_granted(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        caretaker_image: &'static [u8],
        client_image: &'static [u8],
        grant: Grant,
    ) -> Option<EpId> {
        let Grant {
            name,
            rights,
            role: client_role,
            arg: client_arg,
        } = grant;
        assert!(
            fs_proto::grant::fits(name.as_bytes()),
            "a granted name rides in two argument words; this one does not fit",
        );
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
        let narrow_ep = crate::sched::create_endpoint();
        let caretaker_ready = crate::sched::create_endpoint();

        let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
        let spec = fs_proto::grant::spec(name.len(), rights);

        // The caretaker: the directory capability, the narrowed endpoint it serves, and the shared
        // page. The grant itself (the name and the direction) rides in its START arguments, so a
        // per-file grant costs no frame at all.
        crate::sched::spawn(move || {
            run(
                caretaker_image,
                Spawn {
                    arg0: lo,
                    arg1: hi,
                    arg2: spec,
                    grants: &[
                        endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                        endpoint_cap(narrow_ep, Rights::READ), // slot 1: serve the narrowed capability
                        endpoint_cap(caretaker_ready, Rights::WRITE), // slot 2: readiness, once
                    ],
                    maps: &[Mapping {
                        va: FILE_VA_CLIENT,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the file caretaker");

        // Both handshakes before the client exists, in this order and for [`wait_for_caretaker`]'s
        // reason: the servers are parked inside their announcements until they are drained, and the
        // caretaker's own request is staged in the page all three share.
        wait_for_service(readiness);
        wait_for_caretaker(caretaker_ready);

        // The confined program. Its slot 0 looks exactly like a directory capability from inside, and
        // is not one: same protocol, same page, a namespace of one name.
        Some(spawn_fs_client(
            client_image,
            narrow_ep,
            file_shared,
            client_role,
            client_arg,
            0,
            0,
        ))
    }

    /// **Wire a per-directory grant and the program confined to it** (milestone 47,
    /// notes/dir-capability.md). The same three-process shape [`start_granted`] wires, one rung up:
    ///
    /// ```text
    ///   FS server ──file IPC──► fs_subtree_caretaker ──narrowed file IPC──► the confined program
    ///            (the image root)         (one subtree, one rights set)
    /// ```
    ///
    /// The caretaker holds the root directory capability and the confined program holds an endpoint
    /// to the caretaker and **nothing that names the FS server**, which is what makes "it cannot
    /// reach the parent directory or a sibling" a statement about its cspace rather than about a
    /// branch. That argument is `fs_file_caretaker`'s, and it is load-bearing here for an extra
    /// reason: the FS server's handle table is per *server*, so a rights-carrying handle on its own
    /// would not confine a program that could still name [`fs_proto::fs::ROOT`].
    ///
    /// `rights` is a [`fs_proto::dir`] mask. It is what the caretaker *asks* for; the FS server
    /// intersects it with the root's and refuses if the answer is smaller, so a wiring that asked
    /// for more than exists fails at the caretaker's first request rather than silently serving
    /// less.
    #[cfg_attr(not(test), allow(dead_code))]
    pub struct DirGrant {
        /// The directory the caretaker descends into, one component under the image root. Must fit
        /// [`fs_proto::grant::MAX_NAME`].
        pub name: &'static str,
        /// The [`fs_proto::dir`] rights the subtree capability is to carry.
        pub rights: u64,
        /// The confined program's `arg0` (its role) and `arg1`.
        pub role: u64,
        pub arg: u64,
        /// Its `arg2`. Zero for every client that takes a role and a number; the `rm` program is
        /// started with a **grant's** three words instead (a spec and two of name), so it uses all
        /// three and its "role" word is the spec.
        pub arg2: u64,
        /// Stack pages beyond the one `run` maps, for a confined program that needs them. The
        /// hand-written attacker needs none; a shell does (see [`spawn_fs_client`]).
        pub stack_pages: usize,
    }

    /// **The binary carrying the block server's role**, which is the one thing the two ISAs
    /// disagree about here: on aarch64 it is a role of the `init`/hello binary, on riscv the
    /// dedicated `blk` one. Every caller goes through this so the disagreement is one `cfg` rather
    /// than a second copy of every wiring.
    ///
    /// It panics rather than returning `None` because a boot archive without it is a build that did
    /// not finish, not a machine without a disk; the disk's absence is [`root_directory`]'s `None`.
    pub fn blk_server_image() -> &'static [u8] {
        #[cfg(target_arch = "aarch64")]
        return program("init").expect("no init program in the initrd archive");
        #[cfg(target_arch = "riscv64")]
        return program("blk").expect("no blk program in the initrd archive");
    }

    /// **Wire the filesystem and hand back the root directory capability**, for a boot rather than
    /// for a test (milestone 50).
    ///
    /// This is the one entry point the interactive boot uses. It brings up the block server and the
    /// FS server, drains both readiness sentinels (so the service is *running* and not merely
    /// spawned by the time init exists), and returns `(the file-service endpoint, the physical frame
    /// its clients map)`. `None` means no RedoxFS disk is attached to this run, which is the normal
    /// case for a plain `cargo xtask run`, and every caller treats it as "this boot has no
    /// filesystem" rather than as an error.
    ///
    /// The endpoint **is** the directory capability (DECISIONS §27), rooted at the image root. A
    /// boot hands it to the shell unnarrowed on purpose: it is the machine's own prompt, and the
    /// interesting confinement claims are about what the shell then hands to the programs it spawns.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn root_directory(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
    ) -> Option<(EpId, u64)> {
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
        wait_for_service(readiness);
        Some((file_ep, file_shared))
    }

    /// **Wire a subtree caretaker and hand back the narrowed capability**, without spawning
    /// whatever is going to hold it.
    ///
    /// [`start_granted_dir`]'s first half, split out because milestone 50's redirection witness is
    /// a shell that holds a directory **and** a terminal, a spawn channel and a budget, so its
    /// client half is nothing like [`spawn_fs_client`]'s. What it shares with every other confined
    /// program is exactly what is here: a caretaker between it and the FS server, and the page they
    /// both map.
    ///
    /// Returns `(narrow_ep, file_shared)`, with both handshakes already drained for
    /// [`wait_for_caretaker`]'s reason, so a caller may spawn its client the moment this returns.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn narrow_dir(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        caretaker_image: &'static [u8],
        name: &'static str,
        rights: u64,
    ) -> Option<(EpId, u64)> {
        assert!(
            fs_proto::grant::fits(name.as_bytes()),
            "a granted name rides in two argument words; this one does not fit",
        );
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
        let narrow_ep = crate::sched::create_endpoint();
        let caretaker_ready = crate::sched::create_endpoint();

        let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
        let spec = fs_proto::grant::spec(name.len(), rights);

        crate::sched::spawn(move || {
            run(
                caretaker_image,
                Spawn {
                    arg0: lo,
                    arg1: hi,
                    arg2: spec,
                    grants: &[
                        endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                        endpoint_cap(narrow_ep, Rights::READ), // slot 1: serve the narrowed capability
                        endpoint_cap(caretaker_ready, Rights::WRITE), // slot 2: readiness, once
                    ],
                    maps: &[Mapping {
                        va: FILE_VA_CLIENT,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the subtree caretaker");

        // Both handshakes before the client exists. See [`wait_for_caretaker`]: this is the call
        // that found the bug, because a caretaker whose descent is answered only after the service
        // is drained gives its client an unbounded window to write over the staged name.
        wait_for_service(readiness);
        wait_for_caretaker(caretaker_ready);
        Some((narrow_ep, file_shared))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_granted_dir(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        caretaker_image: &'static [u8],
        client_image: &'static [u8],
        grant: DirGrant,
    ) -> Option<EpId> {
        let DirGrant {
            name,
            rights,
            role: client_role,
            arg: client_arg,
            arg2: client_arg2,
            stack_pages,
        } = grant;
        let (narrow_ep, file_shared) =
            narrow_dir(blk_image, fs_server_image, caretaker_image, name, rights)?;

        // The confined program. Its slot 0 looks exactly like a directory capability from inside,
        // and is one: the same protocol, the same page, a namespace of one subtree.
        Some(spawn_fs_client(
            client_image,
            narrow_ep,
            file_shared,
            client_role,
            client_arg,
            client_arg2,
            stack_pages,
        ))
    }

    /// Where the nameset caretaker expects its read-only name-set page
    /// (`user/src/fs_nameset_caretaker.rs`'s `SET_VA`).
    const SET_VA_CARETAKER: u64 = 0x0000_0000_0070_0000;

    /// **Wire a set grant and the program confined to it** (milestone 47's globbing lane,
    /// notes/glob-grant.md). [`start_granted_dir`]'s shape with a narrower namespace:
    ///
    /// ```text
    ///   FS server ──file IPC──► fs_nameset_caretaker ──narrowed file IPC──► the confined program
    ///            (the image root)         (one directory, and only the names in the set)
    /// ```
    ///
    /// The interesting difference from every other grant here is **where the grant lives**. A name
    /// rides in two `START` argument words; a set does not fit in any number of registers, so it is
    /// encoded into a **frame of its own and mapped read-only** into the caretaker, which copies it
    /// into a local before it does anything else. That is the honest place for `ARG_MAX` to
    /// reappear: it is the size of a capability now, not the size of a buffer, and it is bounded by
    /// `fs_proto::nameset::MAX_NAMES` at both ends.
    ///
    /// The set is written **before the caretaker is spawned**, into a frame nothing else has ever
    /// been handed, which is why it needs none of [`wait_for_caretaker`]'s ordering care: unlike
    /// the shared page, no client can reach it at all.
    #[cfg_attr(not(test), allow(dead_code))]
    pub struct SetGrant<'a> {
        /// The directory the caretaker descends into, one component under the image root. The set's
        /// names are the names *in* it that the grant designates.
        pub dir: &'static str,
        /// The set, as `(name, is_dir)`: what the shell's expansion produced. At most
        /// `fs_proto::nameset::MAX_NAMES` of them, and an over-long set is a panic here because the
        /// shell refuses it at the prompt (`grant_plan::Refusal::TooManyNames`), so one arriving means
        /// the wiring built a grant no command line could have expressed.
        pub names: &'a [(&'a [u8], bool)],
        /// The [`fs_proto::dir`] rights the caretaker asks for on its descent.
        pub rights: u64,
        /// The confined program's three `START` words. `rm` is started with a grant's spec and two
        /// name words rather than a role and a number; see [`DirGrant`].
        pub role: u64,
        pub arg: u64,
        pub arg2: u64,
        /// Stack pages beyond the one `run` maps.
        pub stack_pages: usize,
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_granted_set(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        caretaker_image: &'static [u8],
        client_image: &'static [u8],
        grant: SetGrant<'_>,
    ) -> Option<EpId> {
        let SetGrant {
            dir,
            names,
            rights,
            role: client_role,
            arg: client_arg,
            arg2: client_arg2,
            stack_pages,
        } = grant;
        assert!(
            fs_proto::grant::fits(dir.as_bytes()),
            "a granted directory's name rides in two argument words; this one does not fit",
        );
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
        let narrow_ep = crate::sched::create_endpoint();
        let caretaker_ready = crate::sched::create_endpoint();

        // The set, encoded into its own frame before anything can see it. `encode` refuses rather
        // than truncating, and a truncated set would be a capability nobody planned.
        let set_phys = frame();
        let mut encoded = [0u8; fs_proto::nameset::BYTES];
        let n = fs_proto::nameset::encode(names, &mut encoded)
            .expect("this set does not fit one grant, so no command line could have named it");
        // SAFETY: a fresh frame of FRAME_SIZE bytes reachable through the direct map, and `n` is at
        // most `nameset::BYTES`, which is far smaller.
        unsafe {
            core::ptr::copy_nonoverlapping(
                encoded.as_ptr(),
                mmu::phys_to_virt(set_phys) as *mut u8,
                n,
            );
        };

        let (lo, hi) = fs_proto::grant::pack_name(dir.as_bytes());
        let spec = fs_proto::grant::spec(dir.len(), rights);

        crate::sched::spawn(move || {
            run(
                caretaker_image,
                Spawn {
                    arg0: lo,
                    arg1: hi,
                    arg2: spec,
                    grants: &[
                        endpoint_cap(file_ep, Rights::WRITE), // slot 0: CALL the FS server
                        endpoint_cap(narrow_ep, Rights::READ), // slot 1: serve the narrowed capability
                        endpoint_cap(caretaker_ready, Rights::WRITE), // slot 2: readiness, once
                    ],
                    maps: &[
                        Mapping {
                            va: FILE_VA_CLIENT,
                            phys: file_shared,
                            flags: Flags::user_data(),
                        },
                        // **Read-only, and that is not decoration.** The set is the namespace this
                        // process serves; a writable mapping would let the one program that must not
                        // be able to widen its own grant do exactly that.
                        Mapping {
                            va: SET_VA_CARETAKER,
                            phys: set_phys,
                            flags: Flags::user_rodata(),
                        },
                    ],
                },
            )
        })
        .expect("could not spawn the nameset caretaker");

        wait_for_service(readiness);
        wait_for_caretaker(caretaker_ready);

        Some(spawn_fs_client(
            client_image,
            narrow_ep,
            file_shared,
            client_role,
            client_arg,
            client_arg2,
            stack_pages,
        ))
    }

    /// **Put a file behind a byte sink** (milestone 50, notes/sink-protocol.md).
    ///
    /// Wires the FS service (or reuses this boot's) and spawns `user/src/sink.rs` in its file role:
    /// it holds the FS-service endpoint, a report endpoint, and the page it shares with the FS
    /// server, and it serves one endpoint whose only expressible request is "append these bytes".
    ///
    /// `sink` is the capability a program's output slot gets, and **that endpoint is the whole of
    /// what the writer holds**, which is the property the milestone rests on: it is created here
    /// and handed out with `WRITE` to whoever is redirected and `READ` to the sink.
    #[cfg_attr(not(test), allow(dead_code))]
    pub struct FileSink {
        /// The FS service's two readiness endpoints, if this call is the one that wired it.
        pub readiness: Option<(EpId, EpId)>,
        /// The byte sink. Goes into a program's output slot with `WRITE`.
        pub sink: EpId,
        /// Readiness first, then `DONE` and the byte total at end of stream.
        pub report: EpId,
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_file_sink(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        sink_image: &'static [u8],
    ) -> Option<FileSink> {
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
        let sink = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();
        crate::sched::spawn(move || {
            run(
                sink_image,
                Spawn {
                    arg0: SINK_ROLE_FILE,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(sink, Rights::READ), // slot 0: the byte sink it serves
                        endpoint_cap(file_ep, Rights::WRITE), // slot 1: CALL the FS server
                        endpoint_cap(report, Rights::WRITE), // slot 2: readiness and the total
                    ],
                    maps: &[Mapping {
                        va: FILE_VA_CLIENT,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the file sink");
        Some(FileSink {
            readiness,
            sink,
            report,
        })
    }

    /// **Read back what the file sink wrote**, in a different process with a different FS session.
    ///
    /// Spawned only after the sink has reported that it closed the file, because the two share the
    /// FS server's one file page (the [`wait_for_caretaker`] lesson: one page is sound between
    /// parties that are never using it at once, and sequencing is what makes that true).
    ///
    /// It streams the file's contents out **over the sink contract**, so the bytes that reach the
    /// test arrive in the same sixteen-byte framing a `println!` does. Returns `(out, report)`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn start_sink_verify(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        sink_image: &'static [u8],
    ) -> Option<(EpId, EpId)> {
        let (file_ep, file_shared, _) = ensure(blk_image, fs_server_image)?;
        let out = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();
        crate::sched::spawn(move || {
            run(
                sink_image,
                Spawn {
                    arg0: SINK_ROLE_VERIFY,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(out, Rights::WRITE), // slot 0: where the file's bytes go
                        endpoint_cap(file_ep, Rights::WRITE), // slot 1: CALL the FS server
                        endpoint_cap(report, Rights::WRITE), // slot 2: the size it found
                    ],
                    maps: &[Mapping {
                        va: FILE_VA_CLIENT,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    }],
                },
            )
        })
        .expect("could not spawn the sink verifier");
        Some((out, report))
    }

    /// `user/src/sink.rs`'s roles. Kept in sync with that file by name and by this comment; a
    /// mismatch spawns the wrong role and hangs, which is why they are named here rather than
    /// spelled as bare integers at the two call sites.
    const SINK_ROLE_FILE: u64 = 1;
    const SINK_ROLE_VERIFY: u64 = 2;

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
        fs_server_image: &'static [u8],
        std_image: &'static [u8],
    ) -> Option<(Option<(EpId, EpId)>, EpId)> {
        let (file_ep, file_shared, readiness) = ensure(blk_image, fs_server_image)?;
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
pub mod display_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
    use crate::sched::EpId;

    /// Where the driver maps its whole DMA region. Must match user/src/display.rs `DMA_VA`.
    const DMA_VA: u64 = 0x0000_0000_0090_0000;
    /// Where the client maps the surface. Must match user/src/painter.rs `SURFACE_VA`.
    const SURFACE_VA_CLIENT: u64 = 0x0000_0000_0060_0000;

    /// The DMA region, in frames: one for the rings and control buffers, then the surface.
    const DMA_FRAMES: u64 = 1 + gfx_proto::SURFACE_FRAMES as u64;

    /// The driver binary's escape-attempt role; must match user/src/display.rs `ROLE_BACKING_ESCAPE`.
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

    /// **Spawn a driver that attacks its own confinement** (user/src/display.rs `run_backing_escape`):
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

    /// **Wire and spawn the display driver alone**, with no client: `(report endpoint, display
    /// endpoint, the surface's physical base)`, or `None` if no virtio-gpu is on the bus.
    ///
    /// For rung two (milestone 33). The compositor takes `painter`'s place at this seam exactly as the
    /// contract promised it would, so what it needs from rung one is a display endpoint to CALL and the
    /// frames the device scans out. Nothing about the driver changes, which is the claim
    /// notes/framebuffer-contract.md made when it said routing was by endpoint.
    pub fn start_driver(driver_image: &'static [u8]) -> Option<(EpId, EpId, u64)> {
        wire_driver(driver_image, 0, 0)
    }

    /// Where the display terminal maps the page an application writes text into. Must match
    /// `user/src/display_terminal.rs` `OUT_VA`.
    const OUT_VA_TERM: u64 = 0x0000_0000_0068_0000;

    /// What the kernel keeps after wiring a display terminal onto the scanout.
    pub struct TerminalWiring {
        /// The display driver's status endpoint.
        pub driver_report: EpId,
        /// The terminal's status endpoint.
        pub term_report: EpId,
        /// The endpoint the terminal serves. The kernel holds WRITE, so it can play **both** classes
        /// of sender: an application (`OP_WRITE`) and an input source (`OP_BYTES`).
        pub term: EpId,
        /// The application's output page, so the kernel can put the bytes of an `OP_WRITE` there.
        pub out: u64,
        /// The scanout frames, so the kernel can read the picture back through the direct map and
        /// grade it against a value it computed itself.
        pub surface: u64,
    }

    /// **Wire and spawn the display driver with a terminal on the whole scanout** (milestone 29's
    /// remaining increment). `None` if no virtio-gpu function is on the bus.
    ///
    /// The terminal takes `painter`'s place at the display seam with **exactly `painter`'s
    /// authority**: a report endpoint, the display endpoint, and the surface frames. It holds no
    /// device, no interrupt, and no physical address, and `display` cannot tell it from the client that
    /// drew a test pattern. That is the answer to "did the framebuffer contract need changing to
    /// carry text?", and it is an answer made of a spawn literal rather than an argument.
    ///
    /// What it adds over `painter`'s wiring is two things, and both are the terminal contract's, not
    /// the framebuffer's: an endpoint it **serves** (the terminal contract's IPC half), and a page an
    /// application writes bytes into (DECISIONS §10's control-by-message, bulk-by-shared-page split).
    pub fn start_terminal(
        driver_image: &'static [u8],
        term_image: &'static [u8],
    ) -> Option<TerminalWiring> {
        // A scanout that is not a whole number of character cells would leave a strip the terminal
        // never paints. A build error, not a runtime surprise.
        const _: () = assert!(
            gfx_proto::WIDTH.is_multiple_of(bitfont::GLYPH_W)
                && gfx_proto::HEIGHT.is_multiple_of(bitfont::GLYPH_H),
            "the scanout is not a whole number of character cells",
        );
        // And the script's geometry is the scanout's, checked here rather than trusted, because the
        // script is what three independent parties predict the picture from.
        const _: () = assert!(
            gfx_proto::WIDTH / bitfont::GLYPH_W == video_terminal::script::COLS
                && gfx_proto::HEIGHT / bitfont::GLYPH_H == video_terminal::script::ROWS,
            "video_terminal::script's geometry and the scanout's have drifted apart",
        );

        let (driver_report, display_ep, surface) = wire_driver(driver_image, 0, 0)?;

        let out = crate::memory::alloc()
            .expect("no output-page frame for the display terminal")
            .addr();
        // SAFETY: a fresh frame, direct-mapped, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(out) as *mut u8, 0, FRAME_SIZE as usize);
        };

        let term_report = crate::sched::create_endpoint();
        let term = crate::sched::create_endpoint();

        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; gfx_proto::SURFACE_FRAMES as usize + 1];
        for (k, m) in maps
            .iter_mut()
            .take(gfx_proto::SURFACE_FRAMES as usize)
            .enumerate()
        {
            m.va = SURFACE_VA_CLIENT + k as u64 * FRAME_SIZE;
            m.phys = surface + k as u64 * FRAME_SIZE;
        }
        maps[gfx_proto::SURFACE_FRAMES as usize] = Mapping {
            va: OUT_VA_TERM,
            phys: out,
            flags: Flags::user_data(),
        };

        crate::sched::spawn(move || {
            run(
                term_image,
                Spawn {
                    arg0: video_terminal::status::MODE_DISPLAY,
                    arg1: 0, // no physical address: a terminal has no business knowing one
                    arg2: 0,
                    grants: &[
                        endpoint_cap(term_report, Rights::WRITE), // slot 0: status
                        endpoint_cap(display_ep, Rights::WRITE),  // slot 1: CALL the driver
                        endpoint_cap(term, Rights::READ),         // slot 2: serve the terminal
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the display terminal");

        Some(TerminalWiring {
            driver_report,
            term_report,
            term,
            out,
            surface,
        })
    }

    impl TerminalWiring {
        /// **Play the application**: put `text` in the output page and `OP_WRITE` it.
        ///
        /// Returns when the terminal has drawn it and the display driver has put it on the scanout,
        /// because that is what the terminal contract says an `OP_WRITE` reply means (the bytes are
        /// on the console's side). So a test needs no polling and no sleep between writes.
        pub fn print(&self, text: &[u8]) {
            super::term_print(self.out, self.term, text);
        }

        /// **Play the input driver**: `OP_BYTES` these keystrokes, eight to a message.
        ///
        /// Byte for byte the framing `user/src/input.rs` sends and the compositor forwards
        /// (DECISIONS §33), which is the point: the display terminal is fed by the same driver half
        /// as the serial one, so neither contract had to grow anything to carry a keystroke to a
        /// screen.
        pub fn type_bytes(&self, bytes: &[u8]) {
            for chunk in bytes.chunks(8) {
                let mut w1 = 0u64;
                for (k, &b) in chunk.iter().enumerate() {
                    w1 |= (b as u64) << (8 * k);
                }
                let w0 = line_editor::proto::req(line_editor::proto::OP_BYTES, chunk.len() as u64);
                assert_eq!(
                    crate::sched::ipc_call(self.term, [w0, w1])[0],
                    0,
                    "the terminal refused a keystroke",
                );
            }
        }

        /// A scanout pixel, read by the **kernel** through the direct map: a witness that belongs to
        /// no process in userspace.
        pub fn screen_pixel(&self, x: u32, y: u32) -> u32 {
            let at = mmu::phys_to_virt(self.surface) + (y * gfx_proto::WIDTH + x) as u64 * 4;
            // SAFETY: inside the scanout frames this kernel allocated.
            unsafe { core::ptr::read_volatile(at as *const u32) }
        }

        /// **The scanout holds exactly the picture `expect` describes.** Compared pixel for pixel
        /// rather than by digest, so a failure names a coordinate.
        pub fn assert_screen_is(&self, expect: &video_terminal::Vt, what: &str) {
            for y in 0..gfx_proto::HEIGHT {
                for x in 0..gfx_proto::WIDTH {
                    let (got, want) = (self.screen_pixel(x, y), expect.pixel(x, y));
                    assert_eq!(
                        got,
                        want,
                        "{what}: the framebuffer is wrong at ({x},{y}) [cell ({},{})]: {got:#010x}, \
                         expected {want:#010x}",
                        x / bitfont::GLYPH_W,
                        y / bitfont::GLYPH_H,
                    );
                }
            }
        }
    }
}

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
pub mod compositor_service {
    use compositor::SCENE;

    use super::*;
    use crate::cap::{Rights, endpoint_cap};
    use crate::sched::EpId;

    // The compositor's address space. Must match user/src/compositor.rs.
    const SCREEN_VA: u64 = 0x0000_0000_0080_0000;
    const WLIST_VA: u64 = 0x0000_0000_0081_0000;
    const RING_VA: u64 = 0x0000_0000_0082_0000;
    const CLIENT_BASE: u64 = 0x0000_0000_0090_0000;
    const CLIENT_STRIDE: u64 = 0x0000_0000_0010_0000;

    // A client's address space. Must match user/src/window.rs. The same in every client, on purpose.
    const CTL_VA: u64 = 0x0000_0000_0060_0000;
    const SURFACE_VA: u64 = 0x0000_0000_0061_0000;
    const C_SCREEN_VA: u64 = 0x0000_0000_0070_0000;
    const C_WLIST_VA: u64 = 0x0000_0000_0078_0000;

    // Client roles. Must match user/src/window.rs.
    pub const ROLE_INPUT: u64 = 1 << 0;
    pub const ROLE_PROBE_INPUT: u64 = 1 << 1;
    pub const ROLE_PROBE_NEIGHBOUR: u64 = 1 << 2;
    pub const ROLE_PROBE_SCREEN: u64 = 1 << 3;
    pub const ROLE_CAPTURE: u64 = 1 << 4;
    pub const ROLE_VICTIM: u64 = 1 << 5;
    pub const ROLE_SMALL_DAMAGE: u64 = 1 << 6;

    /// The screen's frames, in the scanout's own geometry. The same run of frames rung one's driver
    /// scans out, because the screen *is* rung one's surface.
    const SCREEN_FRAMES: u64 = gfx_proto::SURFACE_FRAMES as u64;

    /// What the kernel keeps after wiring a scene: the endpoints it can ring or listen on, and the
    /// physical addresses it needs to be an independent witness of what the processes did.
    ///
    /// The physical addresses are the point of this struct. The kernel allocated these frames, so it
    /// can read them through the direct map without asking any process anything, which is what lets a
    /// test check a client's surface against a value it computed itself rather than against a number
    /// the client reported.
    pub struct Wiring {
        /// Where clients (and the input source) ring. The kernel holds WRITE so a test can play the
        /// input driver.
        pub doorbell: EpId,
        /// The compositor's report endpoint.
        pub report: EpId,
        /// The screen's frames.
        pub screen: u64,
        /// The window-list page the compositor publishes.
        pub wlist: u64,
        /// The input ring page, shared with the input source and nobody else.
        pub ring: u64,
        /// Each client's frames: its control page, then its surface.
        pub client: [u64; compositor::MAX_WINDOWS],
        /// Each client's report endpoint. One per client, so the kernel knows who is speaking: the
        /// kernel is the spawner and may hold per-client channels, which is exactly the identity the
        /// compositor deliberately does not have.
        pub client_report: [EpId; compositor::MAX_WINDOWS],
        /// Each focusable client's input endpoint (the compositor holds WRITE, the client READ).
        pub input: [EpId; compositor::MAX_WINDOWS],
        pub n: usize,
        pub focusable: usize,
        image: &'static [u8],
        /// The display terminal's image (milestone 29's text increment), so a scene can be built out
        /// of window clients, terminals, or both. A terminal is a window client with a different
        /// program inside it and exactly the same authority.
        term_image: &'static [u8],
        ring_tail: u32,
    }

    /// **Wire the scene and start the compositor.** `display` is an endpoint speaking the rung-one
    /// display contract (`display`, or a kernel stand-in for the tests that do not need a device), and
    /// `screen` the frames it scans out.
    ///
    /// Returns once the compositor is spawned; the caller should wait for `status::COMP_UP` on
    /// [`Wiring::report`] before spawning clients, which is also the reason a client's first act is a
    /// content-free `HELLO`: either order works.
    pub fn start(n: usize, focusable: usize, display: EpId, screen: u64) -> Wiring {
        assert!(n <= SCENE.len() && n <= compositor::MAX_WINDOWS && focusable <= n);
        let image = program("compositor").expect("no compositor program in the initrd archive");
        let client_image = program("window").expect("no window program in the initrd archive");
        let term_image =
            program("display_terminal").expect("no display_terminal program in the initrd archive");

        let wlist = zeroed_frame();
        let ring = zeroed_frame();
        let doorbell = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();

        // **One contiguous run for every client's frames.** Not a convenience: it is what makes the
        // neighbour attack real, because it puts a client's neighbour's pixels in the frame physically
        // after its own grant. A test that attacked a scattered allocation would prove only that it
        // could not find the neighbour.
        let mut per_client = [0u64; compositor::MAX_WINDOWS];
        let mut total = 0u64;
        for (i, win) in SCENE.iter().take(n).enumerate() {
            per_client[i] = 1 + win.frames() as u64; // a control page, then the surface
            total += per_client[i];
        }
        let run_base = crate::memory::alloc_contiguous(total as usize)
            .expect("no contiguous run for the compositor's client surfaces")
            .addr();
        // SAFETY: a fresh contiguous run of frames, reachable through the direct map, owned by nobody
        // else. Zeroed so no client and no test ever reads a stale pixel.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(run_base) as *mut u8,
                0,
                (total * FRAME_SIZE) as usize,
            );
        }

        let mut client = [0u64; compositor::MAX_WINDOWS];
        let mut client_report = [0; compositor::MAX_WINDOWS];
        let mut input = [0; compositor::MAX_WINDOWS];
        let mut at = run_base;
        for i in 0..n {
            client[i] = at;
            at += per_client[i] * FRAME_SIZE;
            client_report[i] = crate::sched::create_endpoint();
        }
        for ep in input.iter_mut().take(focusable) {
            *ep = crate::sched::create_endpoint();
        }

        // The compositor's world: the screen, the list it publishes, the ring it reads, and every
        // client's control page and surface. No device, no interrupt, no physical address.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; MAX_COMP_MAPS];
        let mut m = 0;
        for k in 0..SCREEN_FRAMES {
            maps[m] = Mapping {
                va: SCREEN_VA + k * FRAME_SIZE,
                phys: screen + k * FRAME_SIZE,
                flags: Flags::user_data(),
            };
            m += 1;
        }
        maps[m] = Mapping {
            va: WLIST_VA,
            phys: wlist,
            flags: Flags::user_data(),
        };
        m += 1;
        maps[m] = Mapping {
            va: RING_VA,
            phys: ring,
            flags: Flags::user_data(),
        };
        m += 1;
        for i in 0..n {
            for k in 0..per_client[i] {
                maps[m] = Mapping {
                    va: CLIENT_BASE + i as u64 * CLIENT_STRIDE + k * FRAME_SIZE,
                    phys: client[i] + k * FRAME_SIZE,
                    flags: Flags::user_data(),
                };
                m += 1;
            }
        }

        let mut grants = [endpoint_cap(report, Rights::WRITE); MAX_COMP_GRANTS];
        grants[1] = endpoint_cap(display, Rights::WRITE);
        grants[2] = endpoint_cap(doorbell, Rights::READ);
        for i in 0..focusable {
            grants[3 + i] = endpoint_cap(input[i], Rights::WRITE);
        }
        let ngrants = 3 + focusable;

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: n as u64,
                    arg1: focusable as u64,
                    arg2: 0,
                    grants: &grants[..ngrants],
                    maps: &maps[..m],
                },
            )
        })
        .expect("could not spawn the compositor");

        Wiring {
            doorbell,
            report,
            screen,
            wlist,
            ring,
            client,
            client_report,
            input,
            n,
            focusable,
            image: client_image,
            term_image,
            ring_tail: 0,
        }
    }

    impl Wiring {
        /// **Spawn window client `i` in `role`.** Its whole authority: a report endpoint, the doorbell,
        /// its own control page and surface, an input endpoint if it is focusable, and (only for
        /// [`ROLE_CAPTURE`]) a read-only mapping of the screen and the window list.
        pub fn spawn_client(&self, i: usize, role: u64) {
            let frames = SCENE[i].frames() as u64;
            let mut maps = [Mapping {
                va: 0,
                phys: 0,
                flags: Flags::user_data(),
            }; MAX_CLIENT_MAPS];
            let mut m = 0;
            maps[m] = Mapping {
                va: CTL_VA,
                phys: self.client[i],
                flags: Flags::user_data(),
            };
            m += 1;
            for k in 0..frames {
                maps[m] = Mapping {
                    va: SURFACE_VA + k * FRAME_SIZE,
                    phys: self.client[i] + (1 + k) * FRAME_SIZE,
                    flags: Flags::user_data(),
                };
                m += 1;
            }
            if role & ROLE_CAPTURE != 0 {
                // The screenshot and enumeration grant, and it is **read-only**: a thing that may look
                // at the screen may not draw on it. `Flags::user_rodata` is the difference between a
                // screenshot tool and a second compositor.
                for k in 0..SCREEN_FRAMES {
                    maps[m] = Mapping {
                        va: C_SCREEN_VA + k * FRAME_SIZE,
                        phys: self.screen + k * FRAME_SIZE,
                        flags: Flags::user_rodata(),
                    };
                    m += 1;
                }
                maps[m] = Mapping {
                    va: C_WLIST_VA,
                    phys: self.wlist,
                    flags: Flags::user_rodata(),
                };
                m += 1;
            }

            let mut grants = [endpoint_cap(self.client_report[i], Rights::WRITE); 3];
            grants[1] = endpoint_cap(self.doorbell, Rights::WRITE);
            let ngrants = if i < self.focusable {
                grants[2] = endpoint_cap(self.input[i], Rights::READ);
                3
            } else {
                // **Two grants, and the emptiness of slot 2 is load-bearing**: this client cannot
                // receive input, and its attempt to try is `NoSuchSlot` rather than a refusal from
                // anyone. See ROLE_PROBE_INPUT.
                2
            };

            let image = self.image;
            let probe = self.neighbour_probe_va(i);
            crate::sched::spawn(move || {
                run(
                    image,
                    Spawn {
                        arg0: role,
                        arg1: probe,
                        arg2: 0,
                        grants: &grants[..ngrants],
                        maps: &maps[..m],
                    },
                )
            })
            .expect("could not spawn a window client");
        }

        /// **The address at which client `i`'s neighbour's pixels really are**, in `i`'s own virtual
        /// address space: one page past the last frame of its surface, which the contiguous allocation
        /// above makes the neighbour's control page, and one further is the neighbour's first pixel
        /// page. The attacker is handed this so the test can assert on the exact faulting address, the
        /// same way milestone 29's escape test is handed its victim frame.
        pub fn neighbour_probe_va(&self, i: usize) -> u64 {
            SURFACE_VA + (SCENE[i].frames() as u64 + 1) * FRAME_SIZE
        }

        /// The physical frame the probe address would reach if it were mapped. The test asserts this is
        /// the neighbour's first pixel frame, which is what makes the attack a real one.
        pub fn neighbour_probe_phys(&self, i: usize) -> u64 {
            self.client[i] + (SCENE[i].frames() as u64 + 2) * FRAME_SIZE
        }

        /// Client `i`'s surface, digested by the **kernel** through the direct map: a witness that
        /// belongs to nobody in userspace.
        pub fn client_surface_digest(&self, i: usize) -> u64 {
            let base = mmu::phys_to_virt(self.client[i] + FRAME_SIZE);
            compositor::surface_checksum(SCENE[i].w, SCENE[i].h, |k| {
                // SAFETY: inside the frames this kernel allocated for client `i`'s surface, reached
                // through the direct map.
                unsafe { core::ptr::read_volatile((base + (k * 4) as u64) as *const u32) }
            })
        }

        /// The composed screen, read by the kernel through the direct map.
        pub fn screen_pixel(&self, x: u32, y: u32) -> u32 {
            let at = mmu::phys_to_virt(self.screen) + (y * compositor::SCREEN_W + x) as u64 * 4;
            // SAFETY: inside the scanout frames, reached through the direct map.
            unsafe { core::ptr::read_volatile(at as *const u32) }
        }

        /// Write `v` into the screen at `(x, y)`: the poison a damage test needs, so that "the
        /// compositor did not touch this" is an observation rather than an inference.
        pub fn poison_screen_pixel(&self, x: u32, y: u32, v: u32) {
            let at = mmu::phys_to_virt(self.screen) + (y * compositor::SCREEN_W + x) as u64 * 4;
            // SAFETY: as above; the kernel owns these frames and no device is reading them in the
            // tests that poison (the display is a kernel stand-in there).
            unsafe { core::ptr::write_volatile(at as *mut u32, v) };
        }

        /// Which window the compositor says has focus, read out of the page it publishes. The
        /// compositor's decision, witnessed rather than asked for.
        pub fn focused(&self) -> u32 {
            let at = mmu::phys_to_virt(self.wlist) + compositor::proto::wlist::FOCUSED;
            // SAFETY: inside the window-list frame this kernel allocated.
            unsafe { core::ptr::read_volatile(at as *const u32) }
        }

        /// **Play the input driver**: put `bytes` in the ring and ring the doorbell.
        ///
        /// This is what a virtio-keyboard driver would do, and the authority it exercises is the ring
        /// mapping, not the doorbell: any client can ring, and none of them can write here. The CALL
        /// returns once the compositor has processed the frame, so a test needs no polling.
        pub fn type_bytes(&mut self, bytes: &[u8]) {
            let base = mmu::phys_to_virt(self.ring);
            for &b in bytes {
                let at = base
                    + compositor::proto::ring::BYTES
                    + (self.ring_tail % compositor::proto::ring::CAPACITY) as u64;
                // SAFETY: inside the ring frame this kernel allocated and shares with the compositor.
                unsafe { core::ptr::write_volatile(at as *mut u8, b) };
                self.ring_tail = self.ring_tail.wrapping_add(1);
            }
            // The bytes must be visible before the tail that advertises them.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            // SAFETY: inside the ring frame.
            unsafe {
                core::ptr::write_volatile(
                    (base + compositor::proto::ring::TAIL) as *mut u32,
                    self.ring_tail,
                );
            };
            let w0 = compositor::proto::req(compositor::proto::COMMIT, 0);
            crate::sched::ipc_call(self.doorbell, [w0, 0]);
        }

        /// Ring the doorbell without typing anything: "look at the surfaces". Returns the reply's `r0`.
        ///
        /// The milestone tour uses it; the compositor tests all type something first.
        #[cfg_attr(test, allow(dead_code))]
        pub fn ring_doorbell(&self, op: u64) -> u64 {
            crate::sched::ipc_call(self.doorbell, [compositor::proto::req(op, 0), 0])[0]
        }

        /// **Spawn a display terminal as window `i`** (milestone 29's text increment).
        ///
        /// It takes a `window` client's place with **exactly a window client's authority**: a report
        /// endpoint, the doorbell, an input endpoint, and its own control page and surface. The
        /// compositor cannot tell it from the client that painted a coordinate pattern, which is the
        /// same claim rung two made about `display` one seam down, now made about a client.
        ///
        /// The one addition is an **output page**, and it belongs to the terminal contract rather
        /// than to the compositor's: it is where an application puts the bytes of an `OP_WRITE`
        /// (DECISIONS §10). The kernel holds the other end of that, playing the application.
        ///
        /// **Its input endpoint and its terminal endpoint are the same endpoint**, deliberately.
        /// This process has one wait point (DECISIONS §33), so an application printing and the
        /// compositor typing must arrive on one endpoint and be told apart by opcode, exactly as
        /// `line_editor` does for the serial terminal. The compositor holds WRITE on it because window `i`
        /// is focusable; the kernel holds it too, as the spawner.
        ///
        /// Returns the output page's physical address. `i` must be below `focusable`, or the
        /// terminal would hold no endpoint to serve and would park forever on its first receive.
        pub fn spawn_terminal(&self, i: usize) -> TermClient {
            assert!(
                i < self.focusable,
                "a display terminal must be focusable: its input endpoint is the endpoint it serves",
            );
            // The window must be a whole number of character cells, for the reason the scanout must
            // be: a strip the terminal never paints shows whatever the frame held.
            assert!(
                SCENE[i].w.is_multiple_of(bitfont::GLYPH_W)
                    && SCENE[i].h.is_multiple_of(bitfont::GLYPH_H),
                "window {i} is not a whole number of character cells",
            );

            let out = crate::memory::alloc()
                .expect("no output-page frame for a display terminal")
                .addr();
            // SAFETY: a fresh frame, direct-mapped, owned by nobody yet.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(out) as *mut u8, 0, FRAME_SIZE as usize);
            };

            let frames = SCENE[i].frames() as u64;
            let mut maps = [Mapping {
                va: 0,
                phys: 0,
                flags: Flags::user_data(),
            }; MAX_CLIENT_MAPS];
            let mut m = 0;
            maps[m] = Mapping {
                va: T_CTL_VA,
                phys: self.client[i],
                flags: Flags::user_data(),
            };
            m += 1;
            maps[m] = Mapping {
                va: T_OUT_VA,
                phys: out,
                flags: Flags::user_data(),
            };
            m += 1;
            for k in 0..frames {
                maps[m] = Mapping {
                    va: T_SURFACE_VA + k * FRAME_SIZE,
                    phys: self.client[i] + (1 + k) * FRAME_SIZE,
                    flags: Flags::user_data(),
                };
                m += 1;
            }

            let grants = [
                endpoint_cap(self.client_report[i], Rights::WRITE),
                endpoint_cap(self.doorbell, Rights::WRITE),
                endpoint_cap(self.input[i], Rights::READ),
            ];
            let image = self.term_image;
            crate::sched::spawn(move || {
                run(
                    image,
                    Spawn {
                        arg0: video_terminal::status::MODE_WINDOW,
                        arg1: 0,
                        arg2: 0,
                        grants: &grants,
                        maps: &maps[..m],
                    },
                )
            })
            .expect("could not spawn a display terminal");

            TermClient {
                out,
                ep: self.input[i],
            }
        }
    }

    // A display terminal's address space. Must match user/src/display_terminal.rs. Different numbers from a
    // `window` client's, because they are different programs; the kernel picks each binary's.
    const T_SURFACE_VA: u64 = 0x0000_0000_0060_0000;
    const T_OUT_VA: u64 = 0x0000_0000_0068_0000;
    const T_CTL_VA: u64 = 0x0000_0000_0069_0000;

    /// A display terminal running as a compositor client, from the spawner's side: the page it reads
    /// an application's bytes out of, and the endpoint it serves.
    pub struct TermClient {
        pub out: u64,
        pub ep: crate::sched::EpId,
    }

    impl TermClient {
        /// Play the application: `OP_WRITE` this text and return when it is on the screen.
        pub fn print(&self, text: &[u8]) {
            super::term_print(self.out, self.ep, text);
        }
    }

    /// The most mappings a compositor can need: the screen, the list, the ring, and every client's
    /// control page and surface.
    const MAX_COMP_MAPS: usize = SCREEN_FRAMES as usize + 2 + compositor::MAX_WINDOWS * 4;
    /// The most a client can need: its control page, its surface, and (capture only) the screen and
    /// the window list.
    const MAX_CLIENT_MAPS: usize = 4 + SCREEN_FRAMES as usize + 1;
    /// Report, display, doorbell, and one input endpoint per focusable client.
    const MAX_COMP_GRANTS: usize = 3 + compositor::MAX_WINDOWS;

    /// A fresh zeroed frame, for a page the kernel hands two processes to share.
    fn zeroed_frame() -> u64 {
        let f = crate::memory::alloc()
            .expect("no frame for the compositor's shared pages")
            .addr();
        // SAFETY: a fresh frame, direct-mapped, owned by nobody yet.
        unsafe { core::ptr::write_bytes(mmu::phys_to_virt(f) as *mut u8, 0, FRAME_SIZE as usize) };
        f
    }
}

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
pub mod keyboard_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
    use crate::sched::EpId;

    /// Where the driver maps its DMA page and the input ring. Must match user/src/kbd.rs.
    const DMA_VA: u64 = 0x0000_0000_0090_0000;
    const RING_VA: u64 = 0x0000_0000_0082_0000;

    /// One page, like every other driver here except the display's. A keyboard's event queue is
    /// eight eight-byte records; there is nothing bulk about it, so the standing rule holds in the
    /// other direction too: **a device gets the grant it needs and no more.**
    const DMA_FRAMES: u64 = 1;

    pub struct Wiring {
        /// The driver's status endpoint.
        pub report: EpId,
        /// The doorbell the driver rings. The kernel holds READ here, playing the compositor.
        pub doorbell: EpId,
        /// The input ring's frame, so the kernel can read what was typed.
        pub ring: u64,
        head: u32,
    }

    /// **Wire and spawn the keyboard driver.** `None` if no virtio-input function is on the bus.
    ///
    /// The kernel keeps the doorbell's receiving half and the ring, so it can stand in for the
    /// compositor; a real system hands both to `compositor` instead and nothing about this driver
    /// changes, which is the same swap rung two made at the display seam.
    pub fn start(image: &'static [u8]) -> Option<Wiring> {
        let d = crate::pci::find_input_device()?;

        let dma = crate::memory::alloc_contiguous(DMA_FRAMES as usize)
            .expect("no DMA region for the keyboard driver")
            .addr();
        // SAFETY: a fresh frame, direct-mapped, owned by nobody else. Zeroed so no stale descriptor
        // and no stale event is ever visible to the device or to us.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(dma) as *mut u8,
                0,
                (DMA_FRAMES * FRAME_SIZE) as usize,
            );
        }
        let ring = crate::memory::alloc()
            .expect("no frame for the input ring")
            .addr();
        // SAFETY: as above.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(ring) as *mut u8, 0, FRAME_SIZE as usize);
        };

        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(d.intid, irq_ep);
        crate::arch::irq::enable(d.intid);

        let vid = crate::virtio::register(
            crate::virtio::Transport::pci(&d),
            dma,
            DMA_FRAMES * FRAME_SIZE,
            Some(d.rid),
        );

        let report = crate::sched::create_endpoint();
        let doorbell = crate::sched::create_endpoint();

        let maps = [
            Mapping {
                va: DMA_VA,
                phys: dma,
                flags: Flags::user_data(),
            },
            Mapping {
                va: RING_VA,
                phys: ring,
                flags: Flags::user_data(),
            },
        ];
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0,
                    arg1: dma, // the DMA region's PHYSICAL base: descriptors speak physical
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE),   // slot 0: status
                        irq_cap(d.intid),                      // slot 1: the event interrupt
                        virtio_cap(vid),                       // slot 2: the confined transport
                        endpoint_cap(doorbell, Rights::WRITE), // slot 3: ring the compositor
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the keyboard driver");

        Some(Wiring {
            report,
            doorbell,
            ring,
            head: 0,
        })
    }

    impl Wiring {
        /// **Take what the driver has typed into the ring**, advancing the head the way a compositor
        /// does. Returns how many bytes landed in `out`.
        pub fn take_typed(&mut self, out: &mut [u8]) -> usize {
            use compositor::proto::ring;
            let base = mmu::phys_to_virt(self.ring);
            // SAFETY: inside the ring frame this kernel allocated and shares with the driver.
            let tail = unsafe { core::ptr::read_volatile((base + ring::TAIL) as *const u32) };
            // The tail is published after the bytes it advertises; read it before them.
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            let mut n = 0;
            while self.head != tail && n < out.len() {
                let at = base + ring::BYTES + (self.head % ring::CAPACITY) as u64;
                // SAFETY: inside the ring frame.
                out[n] = unsafe { core::ptr::read_volatile(at as *const u8) };
                self.head = self.head.wrapping_add(1);
                n += 1;
            }
            // SAFETY: inside the ring frame; the head is ours to advance.
            unsafe { core::ptr::write_volatile((base + ring::HEAD) as *mut u32, self.head) };
            n
        }

        /// Answer the driver's `COMMIT`, the way a compositor would after compositing.
        pub fn answer_doorbell(&self) {
            let m = crate::sched::ipc_recv_cap(self.doorbell);
            let crate::cap::Object::Reply(caller) = crate::sched::current_cap(m[1])
                .expect("the keyboard driver's ring was not a CALL")
                .object
            else {
                panic!("the keyboard driver rang without a reply capability");
            };
            assert_eq!(
                compositor::proto::op(m[0]),
                compositor::proto::COMMIT,
                "the keyboard driver rang with something other than COMMIT",
            );
            crate::sched::ipc_reply(caller, [0, 0]);
            crate::sched::delete_current_cap(m[1]).expect("consume the one-shot reply");
        }
    }
}

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
#[cfg_attr(not(test), allow(dead_code))] // the tests are its only caller today
pub mod clock_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap};
    use crate::sched::EpId;

    /// Where the service expects its two mappings. Must match user/src/clock.rs.
    ///
    /// `CLOCK_VA` is public because it is the address the **set** authority lives at, and milestone
    /// 51's NTP tests aim a write there from a process that holds no such mapping. An attack on an
    /// address nobody uses would prove nothing.
    pub const CLOCK_VA: u64 = 0x00c0_0000;
    const RTC_VA: u64 = 0x00d0_0000;

    /// What the clock service was wired with, so a test (or a real init) can play its clients.
    pub struct Wiring {
        /// The service's startup report.
        pub report: EpId,
        /// The propose endpoint. A holder of `WRITE` here may ask; it may not tell.
        pub propose: EpId,
        /// The clock page's **physical** frame, so a reader can be given a read-only mapping of it
        /// (and so the kernel's own tests can read it through the direct map).
        pub page_phys: u64,
        /// Which RTC the machine turned out to have, one of `clock_proto::rtc`.
        pub kind: u64,
    }

    /// Wire and spawn the clock service.
    pub fn start(image: &'static [u8]) -> Wiring {
        let page_phys = crate::memory::alloc()
            .expect("no frame for the clock page")
            .addr();
        // Zeroed, which is also the honest starting state: a page nobody has published to reads as
        // `state::UNKNOWN` rather than as 1970 (clock_proto's `a_zeroed_page_reads_as_unknown`).
        // SAFETY: freshly allocated, reachable through the direct map, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(page_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
        };

        let report = crate::sched::create_endpoint();
        let propose = crate::sched::create_endpoint();

        let rtc = crate::memory::rtc_region();
        let kind = rtc.map_or(clock_proto::rtc::NONE, |(_, _, k)| k);

        // Two mappings, or one on a machine with no RTC. The device page is mapped and no
        // `DeviceFrame` capability is granted, exactly as the console server's UART is: the service
        // drives the registers and never delegates them onward, and §41's take-back revocation
        // reaches an address-space mapping whether or not a cspace slot names it too.
        let mut maps = [
            Mapping {
                va: CLOCK_VA,
                phys: page_phys,
                flags: Flags::user_data(), // read/WRITE: the service is a setter
            },
            Mapping {
                va: RTC_VA,
                phys: 0,
                flags: Flags::user_device(),
            },
        ];
        let n_maps = match rtc {
            Some((phys, _, _)) => {
                maps[1].phys = phys;
                2
            }
            None => 1,
        };

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: kind, // which register layout the MACHINE has, not which ISA we are
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(propose, Rights::READ), // slot 0: serve proposals
                        endpoint_cap(report, Rights::WRITE), // slot 1: the startup verdict
                    ],
                    maps: &maps[..n_maps],
                },
            )
        })
        .expect("could not spawn the clock service");

        Wiring {
            report,
            propose,
            page_phys,
            kind,
        }
    }

    impl Wiring {
        /// The clock page as the kernel sees it, through the direct map. A reader process would
        /// hold a read-only mapping instead; the seqlock and the layout are the same either way,
        /// because they come from the one contract crate.
        pub fn page(&self) -> clock_proto::ClockPage {
            // SAFETY: a frame this module allocated and still owns, named through the direct map.
            unsafe { clock_proto::ClockPage::new(mmu::phys_to_virt(self.page_phys)) }
        }

        /// Wall-clock nanoseconds as a reader would compute them: the page's offset plus the
        /// ambient monotonic counter. 0 when the machine does not know.
        pub fn wall_nanos(&self) -> u64 {
            let r = self.page().read();
            if clock_proto::state::known(r.state) {
                clock_proto::wall_nanos(r.offset_nanos, monotonic_nanos())
            } else {
                0
            }
        }

        /// Play a proposer: `CALL` the propose endpoint. Returns `(status, wall_nanos_after)`.
        pub fn propose_nanos(&self, unix_nanos: u64) -> (u64, u64) {
            let r = crate::sched::ipc_call(
                self.propose,
                [
                    clock_proto::propose::req(clock_proto::propose::PROPOSE),
                    unix_nanos,
                ],
            );
            (r[0], r[1])
        }
    }

    /// Monotonic nanoseconds since boot, the kernel's copy of the reader's arithmetic. Split into
    /// seconds and a remainder for the same reason the service's version is: the naive
    /// `ticks * 1_000_000_000` overflows a `u64` a few minutes into a boot.
    pub fn monotonic_nanos() -> u64 {
        let freq = crate::arch::timer::frequency();
        let ticks = crate::arch::timer::now();
        let secs = ticks / freq;
        let rem = ticks % freq;
        secs * clock_proto::NANOS_PER_SEC + rem * clock_proto::NANOS_PER_SEC / freq
    }
}

/// **Wall-clock time** (milestone 51 lane A, DECISIONS §43).
///
/// Arch-neutral on purpose: one portable binary carrying both RTC drivers, one host-tested
/// contract, and the machine's own device tree choosing between them, so **both ISAs run literally
/// these tests** rather than two copies that can drift (DECISIONS §19, parity is a gate).
#[cfg(test)]
mod clock_tests {
    use clock_proto::{policy, propose, state, status};

    use super::*;

    /// Start the service and take its startup report. `[RPT_READY, state, wall_nanos]`.
    fn start() -> (clock_service::Wiring, [u64; 5]) {
        let image = program("clock").expect("no clock program in the initrd archive");
        let w = clock_service::start(image);
        let report = crate::sched::ipc_recv(w.report);
        (w, report)
    }

    /// **The machine finds out what time it is, from its own device tree.**
    ///
    /// Two ISAs, two entirely different RTCs (a PL031 counting seconds at `0x9010000`, a Goldfish
    /// counting nanoseconds at `0x101000`), one binary, and the choice between them made from the
    /// `compatible` string rather than from `target_arch`. What the assertion actually catches:
    /// a wrong base address, a wrong register offset, the Goldfish halves swapped, and the
    /// seconds/nanoseconds unit confusion, all of which land outside a 74-year window.
    ///
    /// It is a **plausibility** check, not an accuracy one, and deliberately so. Nothing in the
    /// guest knows the host's clock to compare against, so the honest claim is "this is a time a
    /// machine running this code could be at", which is exactly what `policy::plausible` is for and
    /// exactly what the previous behaviour (1970 plus uptime) fails.
    #[test_case]
    fn the_clock_service_reads_a_plausible_wall_clock_from_the_rtc() {
        let (w, report) = start();

        assert_ne!(
            w.kind,
            clock_proto::rtc::NONE,
            "both QEMU virt boards have an RTC; finding none means the compatible match broke",
        );
        assert_eq!(
            report[1],
            state::RTC,
            "the clock should be set from the RTC"
        );
        assert!(
            policy::plausible(report[2]),
            "the RTC read back {} ns, which is outside the sanity window",
            report[2],
        );

        // And the page a reader would hold says the same thing, computed independently: the kernel
        // adds its own counter to the offset the service published, through the same seqlock.
        let r = w.page().read();
        assert_eq!(r.state, state::RTC);
        assert_eq!(r.generation, 1, "one publish: the RTC reading");
        let by_hand = clock_proto::wall_nanos(r.offset_nanos, clock_service::monotonic_nanos());
        assert!(
            by_hand >= report[2] && by_hand - report[2] < 10 * clock_proto::NANOS_PER_SEC,
            "a reader's own arithmetic ({by_hand}) should agree with the service's ({})",
            report[2],
        );
    }

    /// **A proposer can ask and cannot tell.**
    ///
    /// The kernel plays the network time client here: it holds nothing but the propose endpoint,
    /// and every attempt to move the clock somewhere the policy forbids comes back refused with the
    /// page's generation unchanged. The generation is the load-bearing half of the assertion; a
    /// refusal that had nevertheless written the offset would return the same status word.
    ///
    /// This is the milestone's demonstrable claim, and it is one Unix cannot make: `ntpd` runs as
    /// root and may set the clock to anything.
    #[test_case]
    fn a_proposer_can_ask_and_cannot_tell() {
        let (w, _) = start();
        let before = w.page().read();
        assert!(state::known(before.state), "needs a running clock to step");

        let now = w.wall_nanos();
        for (proposed, want, what) in [
            (0, status::REFUSED_IMPLAUSIBLE, "1970, the old lie itself"),
            (
                policy::NOT_AFTER_NANOS,
                status::REFUSED_IMPLAUSIBLE,
                "the far future, past every certificate expiry",
            ),
            (
                now + 2 * policy::MAX_STEP_FORWARD_NANOS,
                status::REFUSED_TOO_FAR_FORWARD,
                "plausible in the absolute, too big a step",
            ),
            (
                now - 2 * policy::MAX_STEP_BACKWARD_NANOS,
                status::REFUSED_TOO_FAR_BACKWARD,
                "backwards, where instants would happen twice",
            ),
        ] {
            let (got, _) = w.propose_nanos(proposed);
            assert_eq!(got, want, "proposing {what}");
            assert_eq!(
                w.page().read().generation,
                before.generation,
                "a refused proposal must not have written the page ({what})",
            );
        }

        // And the bounded case it exists to allow: a small correction is accepted, and the
        // provenance says it came from a proposal rather than from a human.
        let (got, after) = w.propose_nanos(now + clock_proto::NANOS_PER_SEC / 2);
        assert_eq!(got, status::ACCEPTED);
        assert!(after >= now);
        let r = w.page().read();
        assert_eq!(r.state, state::SYNCED, "an accepted proposal is not a SET");
        assert_eq!(r.generation, before.generation + 1);
    }

    /// **Adjusting the wall clock cannot perturb monotonic time, by construction.**
    ///
    /// The property the counter-plus-offset design buys, and the reason `Instant` was left alone.
    /// Unix reaches for `adjtime` slewing partly because stepping the clock backwards breaks things
    /// that assumed it only moved forward; here the step is an offset write and the counter never
    /// sees it, so there is nothing to slew *for correctness*. The assertion is deliberately blunt:
    /// step the wall clock by half a second and require the monotonic counter to have advanced by
    /// far less than that, which no implementation that fed the offset into the counter could pass.
    #[test_case]
    fn adjusting_the_wall_clock_leaves_the_monotonic_counter_alone() {
        let (w, _) = start();
        let step = clock_proto::NANOS_PER_SEC / 2;

        let mono_before = clock_service::monotonic_nanos();
        let wall_before = w.wall_nanos();
        let (got, wall_after) = w.propose_nanos(wall_before + step);
        assert_eq!(got, status::ACCEPTED);
        let mono_after = clock_service::monotonic_nanos();

        assert!(
            wall_after >= wall_before + step / 2,
            "the wall clock should have moved: {wall_before} -> {wall_after}",
        );
        let mono_moved = mono_after - mono_before;
        assert!(
            mono_moved < step / 4,
            "the monotonic counter moved {mono_moved} ns across a {step} ns wall-clock step; \
             it should have moved only by the cost of the round trip",
        );
    }

    /// **A malformed request is refused, and the service survives it.**
    ///
    /// The serve loop's other exit. It matters because the propose endpoint is the one thing a
    /// hostile component holds, so "an opcode nobody defined" has to be a reply rather than a fault
    /// or a wedge: the second, well-formed call proves the service is still there.
    #[test_case]
    fn an_unknown_opcode_is_answered_rather_than_fatal() {
        let (w, _) = start();
        let r = crate::sched::ipc_call(w.propose, [propose::req(0xff), 0]);
        assert_eq!(r[0], status::BAD_REQUEST);

        let r = crate::sched::ipc_call(w.propose, [propose::req(propose::STATE), 0]);
        assert_eq!(r[0], state::RTC, "still serving, and still knows the time");
    }
}

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
mod date_tests {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, frame_cap};
    use crate::sched::EpId;

    /// Where `date` expects the clock page, read-only. Must match user/src/date.rs's `CLOCK_VA`.
    const CLOCK_VA: u64 = 0x00c0_0000;

    // The `a0` format selector, and the `a1`/`a2` conventions. Must match user/src/date.rs.
    const FMT_HUMAN: u64 = 0;
    const FMT_RFC3339: u64 = 1;
    const FMT_UNIX: u64 = 4;
    const PROVENANCE: u64 = 1;

    /// Spawn `date` and return the endpoint its output arrives on.
    ///
    /// `page` is the whole of its clock authority: `Some(phys)` grants the frame with **`READ`**
    /// and maps it **read-only**, which is the read rung of §43's ladder and is what makes a
    /// `date -s` unbuildable rather than merely absent. `None` grants no clock at all, which is the
    /// other unknown-clock cause and a different message.
    fn spawn_date(page: Option<u64>, fmt: u64, offset_minutes: i64, provenance: u64) -> EpId {
        let image = program("date").expect("no date program in the initrd archive");
        let out = crate::sched::create_endpoint();
        crate::sched::spawn(move || match page {
            Some(phys) => run(
                image,
                Spawn {
                    arg0: fmt,
                    arg1: offset_minutes as u64,
                    arg2: provenance,
                    grants: &[
                        endpoint_cap(out, Rights::WRITE), // slot 0: stdout
                        frame_cap(phys, Rights::READ),    // slot 1: a READER, and nothing more
                    ],
                    maps: &[Mapping {
                        va: CLOCK_VA,
                        phys,
                        flags: Flags::user_rodata(),
                    }],
                },
            ),
            None => run(
                image,
                Spawn {
                    arg0: fmt,
                    arg1: offset_minutes as u64,
                    arg2: provenance,
                    grants: &[endpoint_cap(out, Rights::WRITE)],
                    maps: &[],
                },
            ),
        })
        .expect("could not spawn date");
        out
    }

    /// One line of `date`'s output, without its newline.
    ///
    /// The framing is the std PAL's stdout framing (`w0` = the byte count, `w1`|`w2` = the bytes,
    /// little-endian), deliberately, so there is one convention for "a program printed something"
    /// rather than two. `SEND` blocks until a receiver takes it, so stopping at the newline
    /// consumes exactly the messages that line was made of and leaves any following line queued.
    fn line(out: EpId, buf: &mut [u8; 128]) -> usize {
        let mut len = 0usize;
        loop {
            let words = crate::sched::ipc_recv(out);
            let count = words[0] as usize;
            assert!(
                (1..=16).contains(&count),
                "date: a stdout message with a bad byte count: {count}"
            );
            let mut chunk = [0u8; 16];
            chunk[..8].copy_from_slice(&words[1].to_le_bytes());
            chunk[8..].copy_from_slice(&words[2].to_le_bytes());
            for &b in &chunk[..count] {
                if b == b'\n' {
                    return len;
                }
                assert!(len < buf.len(), "date printed a line longer than a line");
                buf[len] = b;
                len += 1;
            }
        }
    }

    /// Start the clock service and take its startup report, so the page has been published to
    /// before anything reads it.
    fn clock() -> clock_service::Wiring {
        let image = program("clock").expect("no clock program in the initrd archive");
        let w = clock_service::start(image);
        let _ = crate::sched::ipc_recv(w.report);
        w
    }

    /// **`date` prints the time the machine actually knows, in a form that reads back.**
    ///
    /// Three formats over the one clock, and the assertion is not "it printed something shaped
    /// like a date". The kernel computes the wall clock itself, straight from the page's offset
    /// plus the ambient counter, and requires `date`'s output to name the same instant. So a wrong
    /// epoch, a nanoseconds/seconds confusion, a timezone applied twice, or a calendar that is
    /// simply wrong all fail here, and none of them would fail a regex.
    ///
    /// The window is ten seconds because the two readings are genuinely at different times (a
    /// process spawn sits between them) and the clock has one-second resolution. It is tight
    /// enough that the failures above miss it by decades.
    #[test_case]
    fn date_prints_the_wall_clock_it_was_granted() {
        let w = clock();
        let mut buf = [0u8; 128];

        // `Unix`, the format with nothing between the clock and the text: the kernel's own reading
        // of the same page, in seconds, must be within a few seconds of what date printed.
        let n = line(spawn_date(Some(w.page_phys), FMT_UNIX, 0, 0), &mut buf);
        let printed: i64 = core::str::from_utf8(&buf[..n])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("date printed {:?}, which is not a number", &buf[..n]));
        let ours = (w.wall_nanos() / clock_proto::NANOS_PER_SEC) as i64;
        assert!(
            (printed - ours).abs() < 10,
            "date printed {printed} seconds; the kernel reads {ours} from the same page",
        );

        // `Rfc3339`, the interchange form, parsed back by the same crate that printed it. The
        // round trip is the weak half of this; the strong half is that the instant it names must
        // still be the one above, so the text carries the time rather than merely being well-formed.
        let n = line(spawn_date(Some(w.page_phys), FMT_RFC3339, 0, 0), &mut buf);
        let s = core::str::from_utf8(&buf[..n]).expect("date printed non-UTF-8");
        let dt = calendar::DateTime::parse_rfc3339(s).unwrap_or_else(|e| {
            panic!("date printed {s:?}, which is not RFC 3339: {}", e.as_str())
        });
        assert_eq!(dt.offset().minutes(), 0, "no offset was asked for");
        assert!(
            (dt.to_unix() - ours).abs() < 10,
            "date printed {s}, which is not the {ours} the kernel reads",
        );
        assert!(
            s.ends_with('Z') && s.len() == 20,
            "{s:?} is not the 20-byte Z-terminated form RFC 3339 asks for at zero offset",
        );

        // `Human`, the default a person gets, at a non-zero offset so the offset is not merely
        // accepted and dropped. `Thu 2026-07-30 18:04:56 +05:30`: the weekday is the crate's whole
        // natural-language surface, and the trailing field is what says which clock this is.
        let n = line(spawn_date(Some(w.page_phys), FMT_HUMAN, 330, 0), &mut buf);
        let s = core::str::from_utf8(&buf[..n]).expect("date printed non-UTF-8");
        assert!(
            s.ends_with(" +05:30"),
            "{s:?} should carry the +05:30 offset it was asked for",
        );
        let day = &s[..3];
        assert!(
            ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].contains(&day),
            "{s:?} should start with a weekday abbreviation, not {day:?}",
        );
        // Same instant again, seen through a different rendering. `Human` is
        // `Fri 2026-07-31 14:59:43 +05:30`: drop the weekday and close the space before the offset,
        // and what is left is RFC 3339 (which permits the space in place of `T`, but not the one
        // before the offset). Doing it this way rather than eyeballing the digits is what makes the
        // check an assertion about the *instant* rather than about the layout.
        assert_eq!(s.len(), 30, "{s:?} is not the Human rendering at an offset");
        let mut iso = [0u8; 32];
        iso[..19].copy_from_slice(&s.as_bytes()[4..23]); // the date and time
        iso[19..25].copy_from_slice(&s.as_bytes()[24..]); // the offset, its space removed
        let iso = calendar::DateTime::parse_rfc3339_bytes(&iso[..25])
            .unwrap_or_else(|e| panic!("{s:?} does not reduce to RFC 3339: {}", e.as_str()));
        assert_eq!(iso.offset().minutes(), 330);
        assert!(
            (iso.to_unix() - ours).abs() < 10,
            "{s:?} names a different instant from the {ours} the kernel reads",
        );
    }

    /// **An unknown clock is a sentence, not 1970 and not a panic.**
    ///
    /// This is DECISIONS §42's no-silent-degradation rule applied where a person can see it, and it
    /// is the reason `date` reads `clock_proto::state` before it reads the offset. `std`'s
    /// `SystemTime::now()` has no error channel and must panic here; `date` has one, so it says
    /// which of the two causes it was:
    ///
    /// - **a frame nobody published to**, which is what a machine with no RTC (or one whose reading
    ///   the clock service refused) leaves its readers holding. §43 recorded this path as proven by
    ///   construction only, because both QEMU boards always have a working RTC. It is provable in
    ///   the guest after all: the page is the contract, so an unpublished page **is** that machine.
    /// - **no clock capability at all**, which is a different fix and therefore a different
    ///   sentence. Note there is no mapping either, so `date` has to answer without touching
    ///   `CLOCK_VA`; a program that read the page to find out whether it had one would fault.
    #[test_case]
    fn an_unknown_clock_is_said_plainly_rather_than_printed_as_1970() {
        let mut buf = [0u8; 128];

        // A clock page nobody has published to. Zeroed, which is what the frame allocator hands
        // out and what `clock_proto`'s `a_zeroed_page_reads_as_unknown` pins as UNKNOWN.
        let blank = crate::memory::alloc()
            .expect("no frame for a blank clock page")
            .addr();
        // SAFETY: freshly allocated, named through the direct map, owned by nobody else.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(blank) as *mut u8, 0, FRAME_SIZE as usize);
        };

        let out = spawn_date(Some(blank), FMT_HUMAN, 0, PROVENANCE);
        let n = line(out, &mut buf);
        let s = core::str::from_utf8(&buf[..n]).expect("date printed non-UTF-8");
        assert_eq!(
            s, "date: the time is unknown: the machine has no clock it believes",
            "an unknown clock must be reported, not guessed at",
        );
        // And the provenance line agrees rather than inventing a source for a time it does not have.
        let n = line(out, &mut buf);
        assert_eq!(
            core::str::from_utf8(&buf[..n]).unwrap(),
            "date: clock source: unknown, generation 0",
        );

        // The other cause: no capability in the slot, so no mapping either.
        let n = line(spawn_date(None, FMT_HUMAN, 0, 0), &mut buf);
        assert_eq!(
            core::str::from_utf8(&buf[..n]).unwrap(),
            "date: the time is unknown: this process holds no clock capability",
        );
    }

    /// **The provenance is readable, and it is the four-state model rather than a boolean.**
    ///
    /// "A human set it" and "an external source the service bounded accepted it" are different
    /// claims, and the difference is exactly what a caller weighing a certificate expiry wants
    /// (DECISIONS §43). Nothing else in the tree renders that distinction for a person, so if it is
    /// not asserted here it is asserted nowhere.
    ///
    /// The generation is the load-bearing half: it counts publishes, so a reader can see that the
    /// clock was stepped under it. Proposing a small correction moves `rtc, generation 1` to
    /// `synced, generation 2`, and a `date` that had cached or invented either would not follow.
    #[test_case]
    fn date_reports_where_the_time_came_from() {
        let w = clock();
        let mut buf = [0u8; 128];

        let out = spawn_date(Some(w.page_phys), FMT_UNIX, 0, PROVENANCE);
        let _ = line(out, &mut buf); // the time itself, asserted elsewhere
        let n = line(out, &mut buf);
        assert_eq!(
            core::str::from_utf8(&buf[..n]).unwrap(),
            "date: clock source: rtc, generation 1",
            "the clock was read from the RTC once and published once",
        );

        // Step it through the propose endpoint, which is an authority `date` does not hold, and the
        // provenance follows the page rather than the process.
        let (status, _) = w.propose_nanos(w.wall_nanos() + clock_proto::NANOS_PER_SEC / 2);
        assert_eq!(status, clock_proto::status::ACCEPTED);
        let out = spawn_date(Some(w.page_phys), FMT_UNIX, 0, PROVENANCE);
        let _ = line(out, &mut buf);
        let n = line(out, &mut buf);
        assert_eq!(
            core::str::from_utf8(&buf[..n]).unwrap(),
            "date: clock source: synced, generation 2",
            "an accepted proposal is a SYNCED clock one generation on, and date should say so",
        );
    }
}

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
pub mod entropy_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
    use crate::sched::EpId;

    /// Where the service maps its DMA page. Must match user/src/entropy.rs.
    const DMA_VA: u64 = 0x0000_0000_0090_0000;

    /// One page, and no more. The rings take 0x16e of it and the buffer 0x100; a device whose whole
    /// job is to write 256 bytes at a time has no business holding a larger grant, and "a device
    /// gets the grant it needs and no more" is the standing rule in both directions.
    const DMA_FRAMES: u64 = 1;

    /// Which bus to take the RNG from. Both `virt` machines offer both, and the milestone-56 test
    /// runs the same binary over each in turn, because a driver that works on one transport and
    /// silently not the other is exactly what DECISIONS §18's seam exists to prevent.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Bus {
        Mmio,
        Pci,
    }

    pub struct Wiring {
        /// The service's readiness endpoint, and **only on the call that did the wiring**: the
        /// report is sent once and whoever asked first has taken it.
        pub ready: Option<EpId>,
        /// The request endpoint. **This is the capability a client is given**, with WRITE; the
        /// service holds READ. Nothing about it names the device.
        pub request: EpId,
        /// Which bus this instance took its device from, so a failure names it.
        pub bus: Bus,
        /// True when the device sat behind an IOMMU, which on this machine means the PCIe wiring.
        pub confined_by_iommu: bool,
    }

    /// **One entropy service per device per boot**, for the same reason the FS service is wired
    /// once: a second service on the same device would reset it and reprogram its queue out from
    /// under the first, and the first would then wait forever for a completion the device was
    /// never told to make. Whoever asks first pays for the wiring and receives the readiness
    /// endpoint; later callers get the same request endpoint and `None` for it.
    ///
    /// Plain atomics rather than a lock: the only writer is the boot/test thread that calls this.
    static WIRED: [core::sync::atomic::AtomicBool; 2] = [
        core::sync::atomic::AtomicBool::new(false),
        core::sync::atomic::AtomicBool::new(false),
    ];
    static REQUEST: [core::sync::atomic::AtomicU64; 2] = [
        core::sync::atomic::AtomicU64::new(0),
        core::sync::atomic::AtomicU64::new(0),
    ];
    static CONFINED: [core::sync::atomic::AtomicBool; 2] = [
        core::sync::atomic::AtomicBool::new(false),
        core::sync::atomic::AtomicBool::new(false),
    ];

    impl Bus {
        const fn index(self) -> usize {
            match self {
                Bus::Mmio => 0,
                Bus::Pci => 1,
            }
        }
    }

    /// Wire the entropy service on `bus` if this boot has not already, else hand back what is
    /// already running. `None` means there is no virtio-rng function on that bus.
    pub fn ensure(image: &'static [u8], bus: Bus) -> Option<Wiring> {
        use core::sync::atomic::Ordering;

        let i = bus.index();
        if WIRED[i].load(Ordering::Acquire) {
            return Some(Wiring {
                ready: None,
                request: REQUEST[i].load(Ordering::Relaxed),
                bus,
                confined_by_iommu: CONFINED[i].load(Ordering::Relaxed),
            });
        }
        let w = start(image, bus)?;
        REQUEST[i].store(w.request, Ordering::Relaxed);
        CONFINED[i].store(w.confined_by_iommu, Ordering::Relaxed);
        WIRED[i].store(true, Ordering::Release);
        Some(w)
    }

    /// **Wire and spawn the entropy service.** `None` if `bus` has no virtio-rng function on it.
    fn start(image: &'static [u8], bus: Bus) -> Option<Wiring> {
        // The two buses differ in exactly two things: where the registers are, and whether there is
        // a requester id for the IOMMU to confine. Everything below is shared, which is the §18
        // seam doing its job.
        let (transport, intid, rid) = match bus {
            Bus::Mmio => {
                let d = crate::virtio::find_entropy_device()?;
                (
                    crate::virtio::Transport::Mmio {
                        mmio_phys: d.mmio_phys,
                    },
                    d.intid,
                    None,
                )
            }
            Bus::Pci => {
                let d = crate::pci::find_rng_device()?;
                (crate::virtio::Transport::pci(&d), d.intid, Some(d.rid))
            }
        };

        let dma = crate::memory::alloc_contiguous(DMA_FRAMES as usize)
            .expect("no DMA region for the entropy service")
            .addr();
        // SAFETY: a fresh frame, direct-mapped, owned by nobody else. Zeroed so no stale descriptor
        // is visible to the device, and so a buffer the service has not filled yet reads as zeros
        // rather than as somebody's old page contents pretending to be entropy.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(dma) as *mut u8,
                0,
                (DMA_FRAMES * FRAME_SIZE) as usize,
            );
        }

        let irq_ep = crate::sched::create_endpoint();
        crate::sched::bind_irq(intid, irq_ep);
        crate::arch::irq::enable(intid);

        let confined_by_iommu = rid.is_some() && crate::iommu::active();
        let vid = crate::virtio::register(transport, dma, DMA_FRAMES * FRAME_SIZE, rid);

        let ready = crate::sched::create_endpoint();
        let request = crate::sched::create_endpoint();

        let maps = [Mapping {
            va: DMA_VA,
            phys: dma,
            flags: Flags::user_data(),
        }];
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0,
                    arg1: dma, // the DMA region's PHYSICAL base: descriptors speak physical
                    arg2: 0,
                    grants: &[
                        endpoint_cap(request, Rights::READ), // slot 0: RECV client requests
                        irq_cap(intid),                      // slot 1: the completion interrupt
                        virtio_cap(vid),                     // slot 2: the confined transport
                        endpoint_cap(ready, Rights::WRITE),  // slot 3: signal readiness once
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the entropy service");

        Some(Wiring {
            ready: Some(ready),
            request,
            bus,
            confined_by_iommu,
        })
    }

    impl Wiring {
        /// Take the startup report: `[READY, first_refill_ok, bytes_in_hand]`, or a `0xDEAD_..`
        /// word whose low byte names the bring-up step that failed. `None` when this caller was not
        /// the one that wired the service, since the report is sent once.
        pub fn wait_for_ready(&self) -> Option<[u64; 5]> {
            self.ready.map(crate::sched::ipc_recv)
        }

        /// **Play a client**: ask for `n` random bytes over the request endpoint and copy out what
        /// arrives. Returns how many landed in `out`. This is the whole of a client's power, and
        /// the kernel deliberately exercises it through the same endpoint a userspace client would
        /// hold rather than reaching into the service.
        pub fn get(&self, n: u64, out: &mut [u8]) -> usize {
            let r = crate::sched::ipc_call(
                self.request,
                [entropy_proto::req(entropy_proto::GET, n), 0],
            );
            match entropy_proto::delivered(r[0]) {
                Some(count) => entropy_proto::take(count, r[1], out),
                None => 0,
            }
        }
    }
}

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
mod entropy_tests {
    use entropy_service::Bus;

    use super::*;

    /// Reach the service on `bus`, wiring it if this is the first test to ask, and check its
    /// startup report when this call is the one that wired it.
    fn start(bus: Bus) -> entropy_service::Wiring {
        let image = program("entropy").expect("no entropy program in the initrd archive");
        let w = entropy_service::ensure(image, bus).unwrap_or_else(|| {
            panic!(
                "no virtio-rng device on the {bus:?} bus: is CRICKER_RNG missing from the test leg, \
                 or the -device virtio-rng line from the runner?"
            )
        });
        if let Some(report) = w.wait_for_ready() {
            assert_eq!(
                report[0],
                entropy_proto::READY,
                "the entropy service did not come up on {bus:?} (it reported {:#x}; a 0xDEAD_.. \
                 word's low byte names the step, see user/src/entropy.rs)",
                report[0],
            );
            assert_eq!(
                report[1], 1,
                "the entropy service came up on {bus:?} but the device gave it no bytes at all",
            );
        }
        w
    }

    /// Draw `WORDS` eight-byte words through the request endpoint, asserting every draw is full.
    /// Deliberately more than one bufferful (the service fetches 256 bytes per device request), so
    /// this crosses the refill boundary and a cursor that wrapped instead of refilling shows up as
    /// a repeat below.
    const WORDS: usize = 64;

    fn draw(w: &entropy_service::Wiring) -> [u64; WORDS] {
        let mut words = [0u64; WORDS];
        for (i, slot) in words.iter_mut().enumerate() {
            let mut buf = [0u8; 8];
            let n = w.get(8, &mut buf);
            assert_eq!(
                n, 8,
                "draw {i} of {WORDS} on {:?} returned {n} bytes, not 8: the device ran dry, or the \
                 service failed to refill",
                w.bus,
            );
            *slot = u64::from_le_bytes(buf);
        }
        words
    }

    /// Every word distinct, and none of them zero. With a real source a collision among 64 draws is
    /// a 2^-58 event, so a failure here is a bug rather than bad luck: a stuck device, a buffer
    /// served twice, or a used ring the driver never re-read all present exactly this way.
    fn assert_unpredictable(words: &[u64; WORDS], what: &str) {
        for (i, &a) in words.iter().enumerate() {
            assert_ne!(
                a, 0,
                "{what}: draw {i} is all zeros, which is the DMA page unwritten"
            );
            for (j, &b) in words.iter().enumerate().skip(i + 1) {
                assert_ne!(a, b, "{what}: draws {i} and {j} are identical ({a:#018x})");
            }
        }
    }

    /// **The headline, over virtio-mmio.** A client that holds one endpoint and no device gets
    /// bytes off a real random-number generator, 512 of them, across a refill, all different.
    #[test_case]
    fn a_client_obtains_unpredictable_bytes_from_a_virtio_rng_over_mmio() {
        let w = start(Bus::Mmio);
        let words = draw(&w);
        assert_unpredictable(&words, "mmio");
    }

    /// **The same service, the same binary, over PCIe** (DECISIONS §18), and behind the IOMMU while
    /// it is there. An entropy source's buffer is the one page in memory whose contents must not be
    /// guessable, so an unconfined device writing it is worth asserting against rather than hoping.
    #[test_case]
    fn a_client_obtains_unpredictable_bytes_from_a_virtio_rng_over_pcie() {
        let w = start(Bus::Pci);
        assert!(
            w.confined_by_iommu,
            "the PCIe RNG is present but not behind the IOMMU: the buffer the device writes the \
             system's key material into is unconfined (is iommu_platform=on missing from the \
             runner's virtio-rng-pci line?)",
        );
        let words = draw(&w);
        assert_unpredictable(&words, "pcie");
    }

    /// **Two independent sources do not agree**, which is what says the bytes came from the devices
    /// rather than from anything shared underneath them (a fixed seed, a counter, the DMA page's
    /// previous contents). Also the cheapest proof that two services can hold two devices at once.
    #[test_case]
    fn two_entropy_services_on_two_devices_do_not_produce_the_same_bytes() {
        let mmio = start(Bus::Mmio);
        let pci = start(Bus::Pci);
        let a = draw(&mmio);
        let b = draw(&pci);
        for (i, (&x, &y)) in a.iter().zip(b.iter()).enumerate() {
            assert_ne!(
                x, y,
                "draw {i} is identical on both devices ({x:#018x}): these are not two sources",
            );
        }
    }

    /// **The count in a reply is the truth about the reply.** A short request gets exactly that
    /// many bytes and leaves the rest of the caller's buffer alone, so a caller cannot be handed
    /// padding it mistakes for entropy; an oversized one is clamped and answered rather than
    /// refused; and an opcode the service does not implement is answered with nothing rather than
    /// killing the service, which the draw afterwards proves.
    #[test_case]
    fn a_reply_never_delivers_more_bytes_than_it_says() {
        let w = start(Bus::Mmio);

        let mut buf = [0xAAu8; 8];
        assert_eq!(w.get(3, &mut buf), 3, "asked for three bytes");
        assert_eq!(
            &buf[3..],
            &[0xAA; 5],
            "the service wrote past the count it reported",
        );

        let mut big = [0u8; 8];
        assert_eq!(
            w.get(200, &mut big),
            entropy_proto::MAX_BYTES as usize,
            "an oversized request should be clamped and answered, not refused",
        );

        let r = crate::sched::ipc_call(w.request, [entropy_proto::req(0xff, 8), 0]);
        assert_eq!(
            r[0],
            entropy_proto::NO_ENTROPY,
            "an unknown opcode should be answered with no bytes",
        );

        let mut after = [0u8; 8];
        assert_eq!(
            w.get(8, &mut after),
            8,
            "the service stopped serving after an unknown opcode",
        );
    }
}

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
pub mod credential_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    /// Where the service maps the provisioner's page. Must match user/src/credentialer.rs.
    const PROV_VA: u64 = 0x0000_0000_00e0_0000;
    /// Where the service and a client map the verify page. Must match both programs.
    const VERIFY_VA: u64 = 0x0000_0000_00e1_0000;

    /// The service's untyped budget, in pages: 6 MiB. It pays for one thing, the Argon2id scratch,
    /// which is `cred::Cost::DEFAULT.blocks()` KiB (4 MiB today), plus the page tables that map it
    /// and the allocator's slack. Sized from the cost parameter rather than guessed, so raising the
    /// cost is a change in two places that fail loudly together rather than one that fails at run
    /// time under load.
    const CRED_BUDGET_PAGES: u64 = 1536;

    /// Extra stack pages for the service, and this is a **measured** number rather than a cautious
    /// one: with the default single page the service takes a data abort at `0x4fff00`, 256 bytes
    /// below the stack, on its first derivation. Argon2's inner loop copies whole 1 KiB `Block`s
    /// through locals (`block_r`, `block_tmp` in `fill_block`), so one page is not close to enough
    /// and no amount of care in *our* code would have made it enough. The net server takes 8 pages
    /// for smoltcp for the same kind of reason; this takes 16, because a KDF that overflows its
    /// stack fails as a killed process on every login rather than as a bad answer, and the pages
    /// are cheap next to the 4 MiB the scratch already costs.
    const CRED_STACK_PAGES: u64 = 16;

    /// The clients' budgets. A `credentialer_test_client` role holds no untyped at all: it maps one page the wiring
    /// placed and calls one endpoint. There is nothing for it to build.
    ///
    /// This constant does not exist. The absence is the point, and it is written down because a
    /// reader looking for "what memory does the attacker get" should find the answer rather than
    /// conclude it was overlooked.
    const _NO_CLIENT_BUDGET: () = ();

    /// The `credentialer_test_client` roles; must match `user/src/credentialer_test_client.rs`.
    pub const ROLE_HONEST: u64 = 0;
    pub const ROLE_ATTACKER: u64 = 1;
    pub const ROLE_PROVISIONER: u64 = 2;

    /// The report words `credentialer_test_client` and the service send, likewise.
    pub const RPT_DONE: u64 = 0x_c2ed_c11e_0000_0001;
    pub const RPT_READY: u64 = 0x_c2ed_0000_0000_0001;

    /// A running credential service and the endpoints that reach it.
    pub struct Wiring {
        /// The service's readiness endpoint. It reports **after** the seal, so receiving on this is
        /// also how a caller knows provisioning is over.
        pub ready: EpId,
        /// The verify endpoint. This is what a client is given, with WRITE.
        pub verify: EpId,
        /// The provision endpoint. Held here only so the provisioner can be spawned against it;
        /// after the seal, the service has deleted its receive end and a `CALL` here would block
        /// forever, which is why nothing sends on it afterwards.
        pub provision: EpId,
    }

    /// **Wire and spawn the credential service.** It blocks on its provision endpoint immediately,
    /// so nothing happens until [`provisioner`] runs.
    ///
    /// `entropy` is the entropy service's request endpoint (DECISIONS §44). It is not optional: a
    /// credential service that cannot draw a salt refuses to start, and passing it a dead endpoint
    /// is how that path gets tested.
    pub fn start(image: &'static [u8], entropy: EpId) -> Wiring {
        let provision = crate::sched::create_endpoint();
        let verify = crate::sched::create_endpoint();
        let ready = crate::sched::create_endpoint();
        let budget =
            crate::untyped::create(CRED_BUDGET_PAGES).expect("no untyped for the credential store");

        // The two shared pages, then the extra stack, in one array the spawn closure owns.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; CRED_STACK_PAGES as usize + 2];
        maps[0] = Mapping {
            va: PROV_VA,
            phys: frame(),
            flags: Flags::user_data(),
        };
        maps[1] = Mapping {
            va: VERIFY_VA,
            phys: frame(),
            flags: Flags::user_data(),
        };
        for k in 0..CRED_STACK_PAGES as usize {
            let phys = crate::memory::alloc()
                .expect("no frame for the credential service's stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the process starts clean, which
            // for this process also means it does not start with somebody else's bytes where its
            // key material will go.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            maps[k + 2] = Mapping {
                va: USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE,
                phys,
                flags: Flags::user_data(),
            };
        }
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0,
                    arg1: 0, // no physical address: this process touches no device
                    arg2: 0,
                    grants: &[
                        endpoint_cap(provision, Rights::READ), // slot 0: write the store, until SEAL
                        endpoint_cap(verify, Rights::READ),    // slot 1: answer questions, forever
                        endpoint_cap(entropy, Rights::WRITE),  // slot 2: salts, naming no device
                        untyped_cap(budget),                   // slot 3: the memory-hard scratch
                        endpoint_cap(ready, Rights::WRITE), // slot 4: one message, after the seal
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the credential service");

        Wiring {
            ready,
            verify,
            provision,
        }
    }

    /// **Spawn the provisioner** and wait for its report. It fills the store and seals it, so when
    /// this returns the service is in phase two and the provision endpoint is dead at both ends.
    ///
    /// Its endowment is the provision endpoint, a report endpoint, and the provisioner's page. It
    /// holds no verify endpoint, which is the mirror of the client's position: neither party can do
    /// the other's job, and neither is prevented from it by a check.
    pub fn provisioner(image: &'static [u8], w: &Wiring) -> [u64; 5] {
        spawn_cli(image, ROLE_PROVISIONER, w.provision, PROV_VA)
    }

    /// **Spawn a client** in `role` against the verify endpoint, and wait for its report.
    pub fn client(image: &'static [u8], w: &Wiring, role: u64) -> [u64; 5] {
        spawn_cli(image, role, w.verify, VERIFY_VA)
    }

    /// The one spawn site the three `credentialer_test_client` roles share, because the whole claim is that they
    /// differ in their endowment and not in their code. Changing `endpoint` and `va` here is the
    /// entire difference between a provisioner and an attacker.
    fn spawn_cli(image: &'static [u8], role: u64, endpoint: EpId, va: u64) -> [u64; 5] {
        let report = crate::sched::create_endpoint();
        let maps = [Mapping {
            va,
            phys: page_for(va),
            flags: Flags::user_data(),
        }];
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: role,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(endpoint, Rights::WRITE), // slot 0: the service
                        endpoint_cap(report, Rights::WRITE),   // slot 1: say what happened
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn a credential client");
        crate::sched::ipc_recv(report)
    }

    /// The two shared frames, allocated once and remembered, because the service and its
    /// counterparty must map the **same** physical frame and the spawns happen at different times.
    ///
    /// Plain atomics rather than a lock: the only writer is the boot/test thread.
    static FRAMES: [core::sync::atomic::AtomicU64; 2] = [
        core::sync::atomic::AtomicU64::new(0),
        core::sync::atomic::AtomicU64::new(0),
    ];

    /// Allocate the next shared frame, in the order the service's `maps` array wants them.
    fn frame() -> u64 {
        use core::sync::atomic::Ordering;
        let i = usize::from(FRAMES[0].load(Ordering::Acquire) != 0);
        let phys = crate::memory::alloc()
            .expect("no frame for a credential page")
            .addr();
        // SAFETY: a fresh frame, direct-mapped, owned by nobody else. Zeroed so a client's first
        // look at the page cannot find somebody else's memory, which for this contract would mean
        // finding it where a secret is supposed to go.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        // Release: the frame must be zeroed before another thread can observe the pointer to it.
        FRAMES[i].store(phys, Ordering::Release);
        phys
    }

    /// Which frame backs a given virtual address, for the counterparty's mapping.
    fn page_for(va: u64) -> u64 {
        use core::sync::atomic::Ordering;
        let i = usize::from(va == VERIFY_VA);
        let phys = FRAMES[i].load(Ordering::Acquire);
        assert_ne!(
            phys, 0,
            "the credential service was not wired before a client"
        );
        phys
    }

    /// Read the shared frame behind `va` directly, which is a thing only the kernel can do and is
    /// how a test checks a claim about what is *not* in a page.
    pub fn peek(va: u64, out: &mut [u8]) {
        use core::sync::atomic::Ordering;
        let i = usize::from(va == VERIFY_VA);
        let phys = FRAMES[i].load(Ordering::Acquire);
        // SAFETY: a frame this module allocated and still owns, read through the direct map.
        let page = unsafe {
            core::slice::from_raw_parts(mmu::phys_to_virt(phys) as *const u8, FRAME_SIZE as usize)
        };
        let n = out.len().min(page.len());
        out[..n].copy_from_slice(&page[..n]);
    }

    /// The verify page's address, for a test that wants to look at it.
    pub const fn verify_page_va() -> u64 {
        VERIFY_VA
    }

    /// Unpack the `k`th reply code from a `credentialer_test_client` report's second word. One byte per code; see
    /// `user/src/credentialer_test_client.rs` `Codes`.
    pub const fn nth(packed: u64, k: u32) -> u64 {
        (packed >> (8 * k)) & 0xff
    }
}

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
mod credential_tests {
    use credential_service as cs;

    use super::*;

    /// Wire the whole system once: the entropy service, the credential service, and the
    /// provisioner that fills and seals the store. Returns the wiring, the provisioner's report,
    /// and the service's readiness report.
    ///
    /// Once per boot, because the seal is irreversible: a second provisioner would `CALL` an
    /// endpoint whose receiver is gone and block forever, which is the correct behaviour and a
    /// terrible test.
    ///
    /// **The readiness message is taken here and not in the test that asserts on it**, which cost a
    /// hang to learn. A `SEND` on this kernel's endpoints is a rendezvous: it blocks until somebody
    /// receives. The credential service sends its readiness word between the seal and the serve
    /// loop, so a test that let a client `CALL` before draining that message would find a service
    /// that had not reached its serve loop, and the whole boot would deadlock with a sender and a
    /// caller both waiting on nobody. The entropy service does not show this because its wiring
    /// receives immediately.
    fn provisioned() -> (cs::Wiring, [u64; 3], [u64; 3]) {
        use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        // Plain atomics rather than a lock or a `Once`: the only writer is the test thread, and
        // the tree's `spin` is built without the `once` feature. Release/Acquire on `DONE` is what
        // publishes the nine words next to it (rule 4: assume weak ordering).
        static DONE: AtomicBool = AtomicBool::new(false);
        #[allow(clippy::declare_interior_mutable_const)]
        const ZERO: AtomicU64 = AtomicU64::new(0);
        static SAVED: [AtomicU64; 9] = [ZERO; 9];

        if !DONE.load(Ordering::Acquire) {
            let rng = program("entropy").expect("no entropy program in the initrd archive");
            let e = entropy_service::ensure(rng, entropy_service::Bus::Mmio)
                .expect("no virtio-rng device on the mmio bus");
            if let Some(r) = e.wait_for_ready() {
                assert_eq!(
                    r[0],
                    entropy_proto::READY,
                    "the entropy service did not come up, so no salt could be drawn",
                );
            }
            let svc =
                program("credentialer").expect("no credentialer program in the initrd archive");
            let cli = program("credentialer_test_client")
                .expect("no credentialer_test_client program in the initrd archive");
            let w = cs::start(svc, e.request);
            let report = cs::provisioner(cli, &w);
            let ready = crate::sched::ipc_recv(w.ready);
            for (slot, v) in SAVED.iter().zip([
                w.ready,
                w.verify,
                w.provision,
                report[0],
                report[1],
                report[2],
                ready[0],
                ready[1],
                ready[2],
            ]) {
                slot.store(v, Ordering::Relaxed);
            }
            DONE.store(true, Ordering::Release);
        }
        let v = |i: usize| SAVED[i].load(Ordering::Relaxed);
        (
            cs::Wiring {
                ready: v(0),
                verify: v(1),
                provision: v(2),
            },
            [v(3), v(4), v(5)],
            [v(6), v(7), v(8)],
        )
    }

    /// **Phase one lands, and the store's capacity is real.** Three identities go in, the fourth is
    /// refused with `FULL` rather than quietly replacing somebody, and the seal is accepted.
    ///
    /// The service's own readiness message is checked here too, because it is sent *after* the
    /// seal: receiving it is the kernel's evidence that the provisioning loop was left, not merely
    /// that a `SEAL` was answered.
    #[test_case]
    fn provisioning_fills_the_store_and_the_seal_closes_it() {
        let (_, report, ready) = provisioned();
        assert_eq!(report[0], cs::RPT_DONE, "the provisioner did not report");
        let codes = report[1];
        for k in 0..3 {
            assert_eq!(
                cs::nth(codes, k),
                cred_proto::OK,
                "identity {k} was not stored (reply {}), codes {codes:#018x}",
                cs::nth(codes, k),
            );
        }
        assert_eq!(
            cs::nth(codes, 3),
            cred_proto::FULL,
            "a fourth identity in a three-slot store must be refused, not silently accepted",
        );
        assert_eq!(
            cs::nth(codes, 4),
            cred_proto::OK,
            "the seal was not accepted"
        );
        assert_eq!(
            report[2], 1,
            "the provisioner's page still held bytes after the seal: a plaintext secret survived \
             in a frame the provisioner maps",
        );

        assert_eq!(
            ready[0],
            cs::RPT_READY,
            "the credential service did not reach phase two (it reported {:#x}; a 0xDEAD_.. word's \
             low byte names the step, see user/src/credentialer.rs)",
            ready[0],
        );
        assert_eq!(
            ready[1], 3,
            "the sealed store does not hold three identities"
        );
        assert!(
            ready[2] >= 1024,
            "the wired Argon2id cost is {} KiB, which is not memory-hard in any useful sense",
            ready[2],
        );
    }

    /// **The headline.** A userspace client holding one endpoint, no store, no entropy and no
    /// budget gets the right answer to four questions: the right secret, the wrong secret, an
    /// identity nobody provisioned, and one person's password against another person's account.
    ///
    /// The last two are one assertion in spirit and two in fact, because they are the failures that
    /// look alike from outside and come from different bugs: a lookup that ignores the identity,
    /// and a comparison that ignores the pairing.
    #[test_case]
    fn a_client_gets_a_correct_yes_or_no_and_nothing_else() {
        let (w, _, _) = provisioned();
        let cli = program("credentialer_test_client")
            .expect("no credentialer_test_client program in the initrd archive");
        let r = cs::client(cli, &w, cs::ROLE_HONEST);
        assert_eq!(r[0], cs::RPT_DONE, "the client did not report");
        let codes = r[1];
        assert_eq!(
            cs::nth(codes, 0),
            cred_proto::MATCH,
            "the right secret for a provisioned identity was refused, codes {codes:#018x}",
        );
        assert_eq!(
            cs::nth(codes, 1),
            cred_proto::MISMATCH,
            "the wrong secret was accepted, codes {codes:#018x}",
        );
        assert_eq!(
            cs::nth(codes, 2),
            cred_proto::MISMATCH,
            "an identity nobody provisioned was accepted, codes {codes:#018x}",
        );
        assert_eq!(
            cs::nth(codes, 3),
            cred_proto::MISMATCH,
            "one identity's secret opened another's account, codes {codes:#018x}",
        );
        assert_eq!(
            r[2], 1,
            "the shared page was not empty after the last reply: either the client's presented \
             secret or something of the store's is still sitting in a frame two processes map",
        );
    }

    /// **The same endowment, used to attack.** The attacker holds exactly what the honest client
    /// holds, and tries to write the store through it: `PUT`, `SEAL`, an undefined opcode, and a
    /// request whose lengths are outside the contract. Every one is refused, and the credential it
    /// tried to install does not work.
    ///
    /// It is refused because there is nothing to refuse: the provision endpoint was deleted at both
    /// ends before this program was spawned, so `PUT` here is not a privileged request arriving at
    /// a guard, it is a word arriving at a loop that implements one opcode.
    #[test_case]
    fn the_same_endowment_cannot_write_the_store() {
        let (w, _, _) = provisioned();
        let cli = program("credentialer_test_client")
            .expect("no credentialer_test_client program in the initrd archive");
        let r = cs::client(cli, &w, cs::ROLE_ATTACKER);
        assert_eq!(r[0], cs::RPT_DONE, "the attacker did not report");
        let codes = r[1];

        // **`PUT` gets MISMATCH, not MALFORMED, and that is the model working.** `provision::PUT`
        // and `verify::VERIFY` are both opcode 1: the two spaces are independent because the
        // *endpoint* gives a number its meaning. So what the attacker actually sent was a verify of
        // an identity and a secret, and the honest answer is no. There was never a privileged
        // request to refuse. This assertion was written expecting MALFORMED and the machine
        // corrected it, which is worth recording here rather than quietly renumbering the opcodes
        // to make the mistake more legible: renumbering would imply the service distinguishes a
        // forbidden opcode from an unknown one, and it does not.
        assert_eq!(
            cs::nth(codes, 0),
            cred_proto::MISMATCH,
            "a PUT on the verify endpoint is a verify of an identity nobody provisioned, so the \
             answer must be MISMATCH; codes {codes:#018x}",
        );
        for (k, what) in [
            (1, "SEAL"),
            (2, "an undefined opcode"),
            (3, "a request with lengths outside the contract"),
        ] {
            assert_eq!(
                cs::nth(codes, k),
                cred_proto::MALFORMED,
                "{what} on the verify endpoint was answered {} rather than MALFORMED, codes \
                 {codes:#018x}",
                cs::nth(codes, k),
            );
        }
        assert_eq!(
            cs::nth(codes, 4),
            cred_proto::MISMATCH,
            "the attacker installed a working credential for itself, codes {codes:#018x}",
        );
        assert_eq!(r[2], 1, "the attacker left bytes in the shared page");
    }

    /// **The service kept serving.** Every refusal above must be a reply, not a crash: a credential
    /// service that a malformed request can kill is a login outage anybody can cause, and it is the
    /// exact shape of the `argon2` cost-overflow panic `cred::Cost::new` exists to prevent.
    #[test_case]
    fn the_service_survives_everything_the_attacker_did() {
        let (w, _, _) = provisioned();
        let cli = program("credentialer_test_client")
            .expect("no credentialer_test_client program in the initrd archive");
        let _ = cs::client(cli, &w, cs::ROLE_ATTACKER);
        let r = cs::client(cli, &w, cs::ROLE_HONEST);
        assert_eq!(
            cs::nth(r[1], 0),
            cred_proto::MATCH,
            "the credential service stopped answering correctly after an attacker talked to it",
        );
    }

    /// **The kernel looks at the frame itself.** The client asserted the page was clean; this
    /// reads the physical frame through the direct map, which no userspace program could do, and
    /// checks that the shared page carries neither the secret that was presented nor any nonzero
    /// byte at all.
    ///
    /// Checking for the literal secret as well as for zero is not redundant. "All zero" is the
    /// property that holds today; "does not contain the secret" is the property that must hold if
    /// the wipe is ever narrowed, and a test that only checked the first would go green on a
    /// change that broke the second.
    #[test_case]
    fn the_shared_frame_holds_nothing_after_an_answer() {
        let (w, _, _) = provisioned();
        let cli = program("credentialer_test_client")
            .expect("no credentialer_test_client program in the initrd archive");
        let _ = cs::client(cli, &w, cs::ROLE_HONEST);

        let mut page = [0u8; 4096];
        cs::peek(cs::verify_page_va(), &mut page);
        let secret = b"correct horse battery staple";
        assert!(
            !page.windows(secret.len()).any(|s| s == secret),
            "the presented secret is still in the frame the client and the service share",
        );
        if let Some(i) = page.iter().position(|&b| b != 0) {
            panic!(
                "byte {i} of the shared frame is {:#04x} after the exchange: the service left \
                 something in a page a client maps",
                page[i],
            );
        }
    }
}

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
pub mod ntp_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    /// The roles of the `ntp` binary. Must match user/src/ntp.rs.
    pub const ROLE_CLIENT: u64 = 0;
    pub const ROLE_SERVER: u64 = 1;
    pub const ROLE_PROBE_CLOCK: u64 = 2;

    /// The words the `ntp` binary reports. Must match user/src/ntp.rs.
    ///
    /// The whole vocabulary is here even though the tests assert on five of the eight: the point of
    /// a mirror is that it is complete, and a failing test that prints `3` is only readable if `3`
    /// has a name on this side too.
    #[allow(dead_code)]
    pub mod rpt {
        pub const SYNCED: u64 = 1;
        pub const REJECTED: u64 = 2;
        pub const NO_REPLY: u64 = 3;
        pub const NO_ENTROPY: u64 = 4;
        pub const NET_ERROR: u64 = 5;
        pub const PROBING: u64 = 6;
        pub const SERVED: u64 = 7;
        pub const BAD_LOCAL_TIME: u64 = 8;
    }

    /// Which reply the test server sends. Must match `user/src/ntp.rs`'s `srv`.
    pub mod srv {
        pub const GOOD: u64 = 0;
        pub const BAD_ORIGIN: u64 = 1;
        pub const KISS_OF_DEATH: u64 = 2;
        pub const SHORT: u64 = 3;
    }

    /// The small integers the client reports a `ntp_proto::Reject` as. Must match
    /// `user/src/ntp.rs`'s `reject_code`; kept as distinct values rather than a bool because which
    /// check refused a packet is the difference between a broken server and an attack.
    pub mod reject {
        pub const LENGTH: u64 = 1;
        pub const KISS_OF_DEATH: u64 = 4;
        pub const ORIGIN_MISMATCH: u64 = 7;
    }

    /// How many requests the client makes before giving up. Must match `ATTEMPTS` in user/src/ntp.rs.
    pub const ATTEMPTS: u64 = 3;

    /// Each role mints or maps exactly one shared frame and pays for its page tables. Small and
    /// fixed: neither role links a heap.
    const BUDGET_PAGES: u64 = 16;
    /// Extra stack pages. The client builds a 48-byte packet and does IPC; the same three `socket_test_client`
    /// and its relatives get is plenty and leaves no doubt.
    const STACK_PAGES: u64 = 3;

    /// A running test server: the endpoint a client speaks the socket contract on, and the endpoint
    /// it reports the first request it saw on.
    pub struct Server {
        /// The socket contract's endpoint. The server holds `READ`; a client is given `WRITE`.
        pub stack: EpId,
        /// **Drain this before waiting on the client.** The server reports once, with a blocking
        /// `send`, and the client's `RECV` is queued behind it.
        pub report: EpId,
    }

    /// Extra stack for a spawned role, allocated and zeroed. Returned by value so the spawn closure
    /// owns it.
    fn stack_pages() -> [Mapping; STACK_PAGES as usize] {
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; STACK_PAGES as usize];
        for (k, m) in maps.iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for an ntp role's stack")
                .addr();
            // SAFETY: a fresh frame through the direct map; zero it so the process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = phys;
        }
        maps
    }

    /// Spawn the test server: `variant` selects the reply it builds, `claimed_nanos` is the
    /// wall-clock time it claims. The kernel supplies the claimed time because the server holds no
    /// clock capability of its own, which keeps every test's expectation an exact number rather
    /// than a window.
    pub fn start_server(image: &'static [u8], variant: u64, claimed_nanos: u64) -> Server {
        let stack = crate::sched::create_endpoint();
        let report = crate::sched::create_endpoint();
        let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for the ntp server");
        let maps = stack_pages();

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_SERVER,
                    arg1: variant,
                    arg2: claimed_nanos,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE), // slot 0: the one report
                        endpoint_cap(stack, Rights::READ),   // slot 1: serve the socket contract
                        untyped_cap(budget),                 // slot 2: map the client's frame
                    ],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the ntp test server");

        Server { stack, report }
    }

    /// **The client's endowment, in one place.** `entropy` is an `Option` on purpose: a client wired
    /// without it is the test that proves the refusal is loud, and the empty slot is what "no
    /// entropy capability" actually is.
    ///
    /// `role` picks the client proper or the clock-page probe, which is given **the same five
    /// slots** so that what the probe fails to reach, it fails to reach as a fully endowed NTP
    /// client rather than as a stripped-down one.
    fn spawn_role(
        image: &'static [u8],
        role: u64,
        a1: u64,
        a2: u64,
        stack: EpId,
        propose: EpId,
        entropy: Option<EpId>,
    ) -> EpId {
        let report = crate::sched::create_endpoint();
        let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for the ntp client");
        let maps = stack_pages();
        // Slot 4 is granted or it is not; there is no third state and no flag inside it. An
        // ungranted slot answers a `CALL` with `NoSuchSlot`, which is how the client tells "there
        // is no entropy service" from "the service has none".
        let n_grants = if entropy.is_some() { 5 } else { 4 };
        let entropy = entropy.unwrap_or(0);

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: role,
                    arg1: a1,
                    arg2: a2,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE),  // slot 0: the verdict
                        endpoint_cap(stack, Rights::WRITE),   // slot 1: the network
                        untyped_cap(budget),                  // slot 2: the shared frame
                        endpoint_cap(propose, Rights::WRITE), // slot 3: ask, never tell
                        endpoint_cap(entropy, Rights::WRITE), // slot 4: the nonce
                    ][..n_grants],
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn the ntp client");

        report
    }

    /// Spawn the NTP client against `stack`, told which server to ask (`ip` packed big-endian,
    /// `port`). Returns its report endpoint.
    pub fn start_client(
        image: &'static [u8],
        stack: EpId,
        propose: EpId,
        entropy: Option<EpId>,
        ip: u32,
        port: u16,
    ) -> EpId {
        spawn_role(
            image,
            ROLE_CLIENT,
            ip as u64,
            port as u64,
            stack,
            propose,
            entropy,
        )
    }

    /// Spawn the clock-page probe: the client's endowment, pointed at `va`.
    pub fn start_probe(
        image: &'static [u8],
        stack: EpId,
        propose: EpId,
        entropy: Option<EpId>,
        va: u64,
    ) -> EpId {
        spawn_role(image, ROLE_PROBE_CLOCK, va, 0, stack, propose, entropy)
    }
}

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
mod ntp_tests {
    use core::sync::atomic::Ordering;

    use clock_proto::{NANOS_PER_SEC, policy, state, status};
    use ntp_service::{ATTEMPTS, reject, rpt, srv};

    use super::*;
    use crate::arch::exceptions::USER_FAULTS;

    /// Bounded by wall clock rather than by a yield count: since DECISIONS §28 the work a test
    /// spawns runs on other cores, so a yield on an idle core elapses no real time. Two seconds is
    /// far under the 60 s hang watchdog, so a genuine lost wakeup still fails loudly.
    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if cond() {
                return true;
            }
            crate::sched::yield_now();
        }
        cond()
    }

    fn ntp_image() -> &'static [u8] {
        program("ntp").expect("no ntp program in the initrd archive")
    }

    /// **A fresh clock service per test.** Each one allocates its own page and reads the RTC again,
    /// so a step taken in one test cannot bias the next one's bounds, and each test's "before" is
    /// genuinely before.
    fn clock() -> clock_service::Wiring {
        let image = program("clock").expect("no clock program in the initrd archive");
        let w = clock_service::start(image);
        let report = crate::sched::ipc_recv(w.report);
        assert!(
            state::known(report[1]),
            "the clock service does not know the time, so there is nothing for NTP to correct",
        );
        w
    }

    /// The entropy service's request endpoint: the client's whole authority over randomness, and it
    /// names no device. `ensure` wires the service once per boot; whichever test asks first pays.
    fn entropy() -> crate::sched::EpId {
        let image = program("entropy").expect("no entropy program in the initrd archive");
        let w = entropy_service::ensure(image, entropy_service::Bus::Mmio).expect(
            "no virtio-rng device on the mmio bus: is CRICKER_RNG missing from the test leg?",
        );
        if let Some(report) = w.wait_for_ready() {
            assert_eq!(
                report[0],
                entropy_proto::READY,
                "the entropy service did not come up (it reported {:#x})",
                report[0],
            );
        }
        w.request
    }

    /// Where the client is told to send. Nothing listens there; the test server is behind the
    /// endpoint, not behind the address. The address is asserted anyway, because a client that
    /// invented its own destination would be one that ignores its wiring.
    const SERVER_IP: u32 = 0x0a00_0202; // 10.0.2.2, slirp's gateway
    const SERVER_PORT: u16 = ntp_proto::PORT;

    /// Run one exchange: a fresh server at `claimed_nanos`, a fresh client, both reports drained in
    /// the order the blocking `send` requires. Returns `(the server's report, the client's report)`.
    fn exchange(
        clock: &clock_service::Wiring,
        variant: u64,
        claimed_nanos: u64,
    ) -> ([u64; 5], [u64; 5]) {
        let server = ntp_service::start_server(ntp_image(), variant, claimed_nanos);
        let client = ntp_service::start_client(
            ntp_image(),
            server.stack,
            clock.propose,
            Some(entropy()),
            SERVER_IP,
            SERVER_PORT,
        );
        // The server blocks in its one `send` until this returns, and the client's RECV is queued
        // behind it, so the order here is load-bearing rather than stylistic. The bounded wait is
        // what turns "the client never sent a request" into a failure that names itself instead of
        // a sixty-second watchdog hang: `ipc_recv` on an endpoint nobody will ever send on does not
        // come back.
        assert!(
            wait_for(|| crate::sched::endpoint_waiting_senders(server.report) > 0),
            "the test server never saw a request: the client failed before it reached the network",
        );
        let served = crate::sched::ipc_recv(server.report);
        assert!(
            wait_for(|| crate::sched::endpoint_waiting_senders(client) > 0),
            "the client never reported: it is still blocked somewhere in the exchange",
        );
        let reported = crate::sched::ipc_recv(client);
        (served, reported)
    }

    /// **The exchange lands as a proposal, and the service is what decides.**
    ///
    /// The whole path in one test: eight bytes of real entropy become a nonce, a well-formed NTPv4
    /// client packet goes out to port 123, a server reply comes back, `Query::accept` produces a
    /// sample, and the correction reaches the clock **through the propose endpoint**. The proof that
    /// it went that way rather than by a write is the state word: an accepted proposal publishes
    /// `SYNCED`, which nothing but the service can produce.
    #[test_case]
    fn an_ntp_exchange_reaches_the_clock_as_a_proposal() {
        let clock = clock();
        let before = clock.page().read();
        let step = NANOS_PER_SEC / 2;
        let wall_before = clock.wall_nanos();
        let claimed = wall_before + step;

        let (served, reported) = exchange(&clock, srv::GOOD, claimed);

        // What the server saw on the wire: the packet is ours and it is addressed where it was told.
        assert_eq!(served[0], rpt::SERVED);
        assert_eq!(
            served[2] >> 32,
            ntp_proto::PORT as u64,
            "the client did not address UDP 123",
        );
        assert_eq!(
            (served[2] >> 8) & 0xff,
            ntp_proto::VERSION as u64,
            "not an NTPv4 request",
        );
        assert_eq!(
            served[2] & 0xff,
            ntp_proto::mode::CLIENT as u64,
            "not a client-mode request: a server would take this for an unsolicited packet",
        );

        assert_eq!(
            reported[0],
            rpt::SYNCED,
            "the client did not synchronise (report {:?})",
            reported,
        );
        assert_eq!(
            reported[1],
            status::ACCEPTED,
            "the clock service refused a half-second correction",
        );

        let after = clock.page().read();
        assert_eq!(
            after.state,
            state::SYNCED,
            "the clock is {} rather than SYNCED: an accepted proposal is not a SET, and only the \
             service can publish this",
            after.state,
        );
        assert_eq!(
            after.generation,
            before.generation + 1,
            "exactly one publish: the proposal",
        );

        // And the clock really moved, by about what the server claimed. The lower bound is what
        // fails if the proposal was dropped on the floor; the upper bound is what fails if the
        // client believed a server it should have bounded.
        let moved = clock.wall_nanos() - wall_before;
        assert!(
            moved >= step * 4 / 5,
            "the wall clock moved {moved} ns, less than the {step} ns the server claimed",
        );
        assert!(
            moved < step + 3 * NANOS_PER_SEC,
            "the wall clock moved {moved} ns, far past the {step} ns the server claimed",
        );
    }

    /// **A reply that fails validation moves nothing.**
    ///
    /// Three replies a real client meets, and each is a different situation rather than three ways
    /// of saying "bad packet": an origin that is not our nonce is the off-path spoof the check
    /// exists for, a kiss-o'-death is an instruction, and twenty bytes is something that is not NTP
    /// arriving on our socket. `ntp_proto` proves the checks themselves over 2^384 packets; what is
    /// proved here is that the client **honours the verdict**, which is a property of this component.
    ///
    /// The attempt count is the second half of it. A rejected reply is retried, because it may have
    /// been a spoof that beat the real server; a kiss-o'-death is **not**, because retrying into one
    /// is the abusive behaviour the packet exists to stop.
    #[test_case]
    fn a_reply_that_fails_validation_never_becomes_a_proposal() {
        let clock = clock();
        let before = clock.page().read();
        let claimed = clock.wall_nanos() + NANOS_PER_SEC / 2;

        for (variant, code, attempts, what) in [
            (
                srv::BAD_ORIGIN,
                reject::ORIGIN_MISMATCH,
                ATTEMPTS,
                "an origin that is not the nonce we sent",
            ),
            (
                srv::KISS_OF_DEATH,
                reject::KISS_OF_DEATH,
                1,
                "a kiss-o'-death, which must not be retried",
            ),
            (
                srv::SHORT,
                reject::LENGTH,
                ATTEMPTS,
                "twenty bytes, which is not an NTP packet",
            ),
        ] {
            let (_, reported) = exchange(&clock, variant, claimed);
            assert_eq!(
                reported[0],
                rpt::REJECTED,
                "the client accepted {what} (report {reported:?})",
            );
            assert_eq!(reported[1], code, "the wrong check refused {what}");
            assert_eq!(
                reported[2], attempts,
                "the client made {} requests against {what}, not {attempts}",
                reported[2],
            );
            assert_eq!(
                clock.page().read(),
                before,
                "the clock page changed after {what}: a refused reply reached the offset",
            );
        }
    }

    /// **The bounds are the service's, and the client cannot argue with them.**
    ///
    /// The client does everything right here: a valid sample, an honest proposal, a truthful report.
    /// It is the *service* that says no, twice, and asymmetrically: two hours forward is refused for
    /// walking past an expiry, and ten seconds backward is refused because moving backward makes
    /// instants happen twice (DECISIONS §43). A compromised client can lie inside those bounds and
    /// can do nothing else, which is the sentence this test is here to make true rather than
    /// asserted.
    #[test_case]
    fn a_proposal_outside_the_policy_is_refused_by_the_service() {
        let clock = clock();
        let before = clock.page().read();

        for (claimed, want, what) in [
            (
                clock.wall_nanos() + 2 * policy::MAX_STEP_FORWARD_NANOS,
                status::REFUSED_TOO_FAR_FORWARD,
                "two hours forward",
            ),
            (
                clock.wall_nanos() - 10 * NANOS_PER_SEC,
                status::REFUSED_TOO_FAR_BACKWARD,
                "ten seconds backward",
            ),
        ] {
            let (_, reported) = exchange(&clock, srv::GOOD, claimed);
            assert_eq!(
                reported[0],
                rpt::SYNCED,
                "the client should have accepted the sample and proposed it ({what}, report \
                 {reported:?}); the refusal under test is the service's, not the client's",
            );
            assert_eq!(
                reported[1], want,
                "the service answered {} to a server claiming {what}",
                reported[1],
            );
            assert_eq!(
                clock.page().read(),
                before,
                "the clock page changed on a refused proposal ({what})",
            );
        }
    }

    /// **The client holds no writable clock page, and knowing where one would be buys it nothing.**
    ///
    /// The same binary, the same five slots an NTP client is given, plus the exact address at which
    /// a process holding the *set* authority maps the clock page. It reports the address, writes
    /// there, and dies. The boundary is the mapping and not the layout, so there is no address at
    /// which this write succeeds; this one is chosen because it is the address that would matter.
    ///
    /// This is the claim Unix cannot make. `ntpd` runs as root: there is no address in a Unix system
    /// its `settimeofday` cannot reach.
    #[test_case]
    fn an_ntp_client_holds_no_writable_clock_page() {
        let clock = clock();
        let before = clock.page().read();
        // An endpoint nobody serves: the probe never sends a request, and giving it a real server
        // would only add a process to the boot.
        let stack = crate::sched::create_endpoint();

        let faults = USER_FAULTS.load(Ordering::Relaxed);
        let report = ntp_service::start_probe(
            ntp_image(),
            stack,
            clock.propose,
            Some(entropy()),
            clock_service::CLOCK_VA,
        );

        let [tag, va, ..] = crate::sched::ipc_recv(report);
        assert_eq!(tag, rpt::PROBING, "the probe never reached its write");
        assert_eq!(va, clock_service::CLOCK_VA);

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "an NTP client wrote the clock page at {va:#x} and was NOT stopped",
        );
        // The exact address, on both ISAs. This half used to be aarch64-only, because aarch64 had a
        // last-fault record (`FAR_EL1`, stashed for tests) and RISC-V had only a fault *count*.
        // Milestone 19's portable record keeps it on both. The *kind* is deliberately not asserted:
        // the probe holds no mapping of the clock page at all, so the fault is a translation fault,
        // and the claim being made here is about the address the client aimed at.
        assert_eq!(
            crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
            Some(va),
            "something faulted, but not at the clock page's address",
        );
        assert_eq!(
            crate::sched::endpoint_waiting_senders(report),
            0,
            "the probe reported past its write: the write did not fault, so an NTP client set the \
             clock by hand",
        );
        assert_eq!(
            clock.page().read(),
            before,
            "the clock page changed while an NTP client was writing at it",
        );
    }

    /// **The nonce is unpredictable, because it comes from the entropy service.**
    ///
    /// Two exchanges, and the server reports what was actually on the wire each time. Two things
    /// follow, and the second is the one worth having: the two nonces differ, and neither of them is
    /// a **time**. A client using `Query::new` would put its own clock in the transmit field, whose
    /// seconds would be within a moment of now; a 64-bit draw from the device lands in a random one
    /// of 2^32 seconds, so a false failure here is a 7201-in-2^32 event (about one boot in 600,000).
    ///
    /// This is what `Query::with_nonce` is worth, and it is worth nothing if the bits are guessable:
    /// before the entropy service landed, the only source here was splitmix64 seeded off the virtual
    /// counter, which notes/entropy.md calls predictable to anyone who can guess boot-relative time.
    #[test_case]
    fn the_nonce_on_the_wire_is_random_and_is_not_the_clock() {
        let clock = clock();
        let claimed = clock.wall_nanos() + NANOS_PER_SEC / 2;

        // A time in the NTP era the transmit field would carry if it were a clock reading.
        let now_ntp_secs = clock.wall_nanos() / NANOS_PER_SEC + ntp_proto::UNIX_DELTA;

        let mut nonces = [0u64; 2];
        for slot in nonces.iter_mut() {
            let (served, reported) = exchange(&clock, srv::GOOD, claimed);
            assert_eq!(served[0], rpt::SERVED);
            assert_eq!(reported[0], rpt::SYNCED, "report {reported:?}");
            *slot = served[1];
        }

        assert_ne!(
            nonces[0], nonces[1],
            "two exchanges put the same nonce on the wire: the transmit field is not random",
        );
        for (i, &nonce) in nonces.iter().enumerate() {
            let secs = nonce >> 32;
            assert!(
                secs.abs_diff(now_ntp_secs) > 3600,
                "nonce {i} decodes to {secs}, within an hour of the clock ({now_ntp_secs}): the \
                 transmit field is a timestamp, not 64 random bits",
            );
        }
    }

    /// **No entropy capability, no request.**
    ///
    /// DECISIONS §42's no-silent-degradation rule where degrading quietly is worst. The client is
    /// wired with slot 4 empty, which is what "no entropy service" actually is, and it stops before
    /// it touches the network: the server's report endpoint has no waiting sender, so no request was
    /// ever built. The alternative, a quiet fall back to the counter-seeded stream, would hand an
    /// off-path attacker the twelve bits `Query::with_nonce` exists to take away from them, and
    /// nothing in the report would say so. It is the call `SystemRng` makes when it panics.
    #[test_case]
    fn without_entropy_the_client_refuses_rather_than_guessing() {
        let clock = clock();
        let before = clock.page().read();
        let server = ntp_service::start_server(ntp_image(), srv::GOOD, clock.wall_nanos());
        let report = ntp_service::start_client(
            ntp_image(),
            server.stack,
            clock.propose,
            None, // slot 4 empty
            SERVER_IP,
            SERVER_PORT,
        );

        // Bounded, because the failure this test guards against is a client that carries on: it
        // would block in the network it should never have reached, and an unbounded `ipc_recv` here
        // would report that as a watchdog hang rather than as the refusal that did not happen.
        assert!(
            wait_for(|| crate::sched::endpoint_waiting_senders(report) > 0),
            "the client neither refused nor reported: with no nonce it can trust, it went to the \
             network anyway and is blocked there",
        );
        let reported = crate::sched::ipc_recv(report);
        assert_eq!(
            reported[0],
            rpt::NO_ENTROPY,
            "the client proceeded without a source of unguessable bits (report {reported:?})",
        );
        assert_eq!(
            reported[1] as i64,
            abi::Error::NoSuchSlot as i64,
            "an ungranted slot must answer NoSuchSlot (-1), 'there is nothing there', not {}",
            reported[1] as i64,
        );
        assert_eq!(
            crate::sched::endpoint_waiting_senders(server.report),
            0,
            "the server saw a request: the client sent one without a nonce it could trust",
        );
        assert_eq!(clock.page().read(), before, "the clock moved anyway");
    }
}

/// Milestone 11: hand a process an untyped budget and let it spend it.
#[cfg_attr(not(test), allow(dead_code))] // the tour and the userspace tests
pub mod untyped_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    const ROLE_UNTYPED_DEMO: u64 = 7;

    /// Carve `pages` of memory into an untyped region, hand it to a fresh process, and return the
    /// region id, the endpoint the process reports on, and the thread it runs as. The kernel's ONE
    /// allocation is the untyped itself; everything the process maps afterward spends that, not the
    /// allocator.
    ///
    /// **The `Tid` is returned because this process does not exit.** It reports its result and then
    /// spins, deliberately, so that the free-frame count its caller reads is the measurement's
    /// rather than a teardown's. That makes it a thread only the caller can end, and a caller that
    /// drops the name has leaked a spinning thread onto every test that runs after it.
    pub fn start(image: &'static [u8], pages: u64) -> Option<(u64, EpId, crate::thread::Tid)> {
        let region = crate::untyped::create(pages)?;
        let report = crate::sched::create_endpoint();

        let tid = crate::sched::spawn(move || {
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

        Some((region, report, tid))
    }
}

/// **The untyped-backed userspace heap** (milestone 27): spawn the `allocator_exerciser` workload, the
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
    /// (`BTreeMap` nodes, the fmt machinery behind `assert!`) burn more than 4 KiB of stack, so
    /// map three more pages below it. The demo found this the honest way: a data abort at
    /// 0x4ffff8, one word below the mapped page.
    const EXTRA_STACK_PAGES: u64 = 3;

    pub fn start(image: &'static [u8]) -> EpId {
        let budget =
            crate::untyped::create(BUDGET_PAGES).expect("no untyped for allocator_exerciser");
        let report = crate::sched::create_endpoint();

        let mut stack = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; EXTRA_STACK_PAGES as usize];
        for (k, m) in stack.iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for allocator_exerciser stack")
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
        .expect("could not spawn allocator_exerciser");

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
        let image = program("allocator_exerciser")
            .expect("no allocator_exerciser program in the initrd archive");
        let report = alloc_service::start(image);
        let words = crate::sched::ipc_recv(report);
        assert_eq!(
            words[0], 0xA110_C0DE,
            "allocator_exerciser did not complete its heap workout",
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
mod compositor_tests {
    use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use compositor::proto::wlist;
    use compositor::{Rect, SCENE, status};
    use compositor_service::{
        ROLE_CAPTURE, ROLE_INPUT, ROLE_PROBE_INPUT, ROLE_PROBE_NEIGHBOUR, ROLE_PROBE_SCREEN,
        ROLE_SMALL_DAMAGE, ROLE_VICTIM, Wiring,
    };

    use super::*;
    use crate::arch::exceptions::USER_FAULTS;
    use crate::sched;

    /// Spin until `cond`, bounded by wall clock rather than by a yield count: since DECISIONS §28 the
    /// work a test spawns runs on *other* cores, so a yield on an idle core returns at once and a fixed
    /// number of them elapses in no real time. Two seconds is far under the 60 s hang watchdog, so a
    /// genuine lost wakeup still fails loudly.
    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if cond() {
                return true;
            }
            sched::yield_now();
        }
        cond()
    }

    /// What the last flush asked for, and how many there have been. Written by the display stand-in
    /// below; reset by each call to [`kernel_display`]. The compositors left behind by earlier tests are
    /// parked in `RECV` and flush nothing, so this is not shared state in any live sense.
    static LAST_FLUSH: AtomicU64 = AtomicU64::new(u64::MAX);
    static FLUSH_COUNT: AtomicUsize = AtomicUsize::new(0);

    /// **A display the kernel serves itself**: the rung-one contract (`INFO`, `FLUSH`) over frames the
    /// kernel allocated, with no device behind it. Returns `(display endpoint, screen frames)`.
    ///
    /// It exists to make the damage rectangle visible. A real driver honours the rectangle and says
    /// nothing about it; here the flush *is* the observation, so a compositor that quietly repainted the
    /// screen every frame would fail a test rather than merely be slow.
    fn kernel_display() -> (sched::EpId, u64) {
        let frames = gfx_proto::SURFACE_FRAMES as u64;
        let screen = crate::memory::alloc_contiguous(frames as usize)
            .expect("no contiguous screen frames for the compositor")
            .addr();
        // SAFETY: a fresh contiguous run, direct-mapped, owned by nobody else.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(screen) as *mut u8,
                0,
                (frames * FRAME_SIZE) as usize,
            );
        }
        LAST_FLUSH.store(u64::MAX, Ordering::SeqCst);
        FLUSH_COUNT.store(0, Ordering::SeqCst);

        let ep = sched::create_endpoint();
        sched::spawn(move || {
            loop {
                let m = sched::ipc_recv_cap(ep);
                let (w0, slot) = (m[0], m[1]);
                let crate::cap::Object::Reply(caller) = sched::current_cap(slot)
                    .expect("the display stand-in got no reply capability")
                    .object
                else {
                    panic!("the display stand-in was sent something that was not a CALL");
                };
                let (r0, r1) = match gfx_proto::op(w0) {
                    gfx_proto::display::FLUSH => {
                        LAST_FLUSH.store(gfx_proto::operand(w0), Ordering::SeqCst);
                        FLUSH_COUNT.fetch_add(1, Ordering::SeqCst);
                        (0, 0)
                    }
                    gfx_proto::display::INFO => (
                        0,
                        gfx_proto::WIDTH as u64 | ((gfx_proto::HEIGHT as u64) << 32),
                    ),
                    _ => (gfx_proto::EINVAL as u64, 0),
                };
                sched::ipc_reply(caller, [r0, r1]);
                sched::delete_current_cap(slot).expect("consume the one-shot reply");
            }
        })
        .expect("could not spawn the display stand-in");
        (ep, screen)
    }

    /// Wait for the compositor's one status message, and check it.
    fn wait_for_compositor(w: &Wiring) {
        let [tag, windows, focus, ..] = sched::ipc_recv(w.report);
        assert_eq!(
            tag,
            status::COMP_UP,
            "the compositor did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte names \
             the step, see user/src/compositor.rs)",
        );
        assert_eq!(windows, w.n as u64, "the compositor wired the wrong scene");
        assert_eq!(focus, 0, "focus should start on the bottom window");
    }

    /// Take a `CALL` a client parked on its report endpoint: `(the caller, the reply slot, its word)`.
    fn take_call(ep: sched::EpId, want: u64) -> (u64, u64, u64) {
        let m = sched::ipc_recv_cap(ep);
        assert_eq!(
            m[0], want,
            "a client reported {:#x} where {want:#x} was expected (a 0xDEAD_.. word's low byte names \
             the step, see user/src/window.rs)",
            m[0],
        );
        let crate::cap::Object::Reply(caller) = sched::current_cap(m[1])
            .expect("a client's report was not a CALL")
            .object
        else {
            panic!("a client's report carried no reply capability");
        };
        (caller, m[1], m[2])
    }

    fn release(caller: u64, slot: u64) {
        sched::ipc_reply(caller, [0, 0]);
        sched::delete_current_cap(slot).expect("consume the one-shot reply");
    }

    /// Take client `i`'s "painted and committed" report and check the digest against the pattern the
    /// contract says that window holds. Every honest client sends exactly one of these, so a test that
    /// spawns a client owes it a receive: a rendezvous SEND nobody takes leaves the client parked.
    fn expect_painted(w: &Wiring, i: usize) {
        let [tag, digest, id, ..] = sched::ipc_recv(w.client_report[i]);
        assert_eq!(
            tag,
            status::WIN_PAINTED,
            "window {i} reported {tag:#x} instead of painting (a 0xDEAD_.. word's low byte names the \
             step, see user/src/window.rs)",
        );
        assert_eq!(
            digest,
            compositor::expected_window_checksum(i),
            "window {i} did not paint its own window's pattern into its own surface",
        );
        assert_eq!(
            id, i as u64,
            "window {i} was told it was window {id}: the compositor published the wrong control page",
        );
    }

    /// The whole screen equals the picture `compositor` says `committed` windows produce. The kernel's own
    /// witness, computed from the contract and read through the direct map, so no process is grading its
    /// own homework.
    fn assert_screen_is(w: &Wiring, committed: usize) {
        for y in 0..compositor::SCREEN_H {
            for x in 0..compositor::SCREEN_W {
                let got = w.screen_pixel(x, y);
                let want = compositor::expected_screen_pixel(committed, x, y);
                assert_eq!(
                    got, want,
                    "the composed screen is wrong at ({x},{y}): {got:#010x}, expected {want:#010x} \
                     with {committed} windows committed",
                );
            }
        }
    }

    /// **A client cannot reach its neighbour's pixels, and cannot read the screen it draws into.**
    ///
    /// The thesis content of this rung, so it is proved from four directions rather than asserted.
    ///
    /// The attacker is given every advantage short of a capability. It is the *same binary* as the
    /// honest client, with the same grants, and the kernel hands it the **exact virtual address** at
    /// which its neighbour's pixels sit: one page past its own surface, which is where the kernel's
    /// contiguous allocation really did put them (asserted here, so the attack cannot quietly become a
    /// poke at nothing). Every client maps its surface at the same virtual address, so this is also the
    /// address the neighbour uses for its own pixels. It still cannot touch them, because the boundary
    /// is the mapping and not the layout.
    ///
    /// What that proves, in order:
    ///
    /// 1. **The refusal that needs no attack.** The attacker asks the kernel to receive on the input
    ///    slot it was not granted, and gets `NoSuchSlot`: "there is nothing there", not a permission
    ///    error from a server that consulted a list. Slot 2 is empty because the spawn literal left it
    ///    empty, and that emptiness is the whole of the difference between this client and a focusable
    ///    one.
    /// 2. **The write faults**, and on aarch64 the faulting address is exactly the one it was handed.
    /// 3. **Nothing was written**: the victim's witness pattern digests identically before and after,
    ///    from two independent readers (the kernel through the direct map, and the victim itself
    ///    through its own mapping after the attacker is dead). The victim is held in a `CALL` across
    ///    the whole attack so that "after" really is after.
    /// 4. **No ambient display.** A third client, which painted into this very screen, reads the
    ///    address where the screen is mapped in the compositor and in the capture client, and faults.
    ///    It holds no mapping of the screen, so there is nothing to read and no verb to ask with.
    ///
    /// A read fault proves the page is not mapped *at all*, which is the same reason a write cannot
    /// reach it either; the two probes here are a write (integrity) and a read (confidentiality) so
    /// both directions are exercised on real hardware behaviour rather than argued from one.
    #[test_case]
    fn a_client_holds_no_capability_for_its_neighbours_pixels_or_the_screen() {
        const ATTACKER: usize = 0;
        const VICTIM: usize = 1;
        const PEEPER: usize = 2;

        let (display, screen) = kernel_display();
        let w = compositor_service::start(3, 0, display, screen);
        wait_for_compositor(&w);

        // The victim paints and then parks in a CALL, so we can hold it there while it is attacked.
        w.spawn_client(VICTIM, ROLE_VICTIM);
        let (victim, victim_slot, reported) =
            take_call(w.client_report[VICTIM], status::WIN_PAINTED);
        assert_eq!(
            reported,
            compositor::expected_window_checksum(VICTIM),
            "the victim did not paint its own window's pattern",
        );
        let before = w.client_surface_digest(VICTIM);
        assert_eq!(
            before, reported,
            "the kernel and the victim disagree about the victim's surface before any attack",
        );

        // The attacker's address really is the neighbour's pixels. Without this the attack could
        // degenerate into poking an empty hole and still "pass".
        assert_eq!(
            w.neighbour_probe_phys(ATTACKER),
            w.client[VICTIM] + FRAME_SIZE,
            "the probe address is not the victim's first pixel frame: the allocation is not adjacent, \
             so this test would prove nothing about a neighbour",
        );

        let faults = USER_FAULTS.load(Ordering::Relaxed);
        w.spawn_client(ATTACKER, ROLE_PROBE_INPUT | ROLE_PROBE_NEIGHBOUR);

        let [tag, errno, ..] = sched::ipc_recv(w.client_report[ATTACKER]);
        assert_eq!(
            tag,
            status::WIN_REFUSED,
            "the attacker skipped its input probe"
        );
        assert_eq!(
            errno as i64,
            abi::Error::NoSuchSlot as i64,
            "receiving on an ungranted slot must be NoSuchSlot (-1), 'there is nothing there', not \
             {} (a permission error would mean the authority exists and was withheld)",
            errno as i64,
        );

        // It is an honest client up to the moment it is not: it paints its own window and reports, and
        // only then reaches for its neighbour's.
        expect_painted(&w, ATTACKER);

        let [tag, probe_va, ..] = sched::ipc_recv(w.client_report[ATTACKER]);
        assert_eq!(
            tag,
            status::WIN_PROBING,
            "the attacker never reached its probe"
        );
        assert_eq!(probe_va, w.neighbour_probe_va(ATTACKER));

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "a client wrote at {probe_va:#x}, its neighbour's pixels, and was NOT stopped",
        );
        // The exact address, on both ISAs. This half used to be aarch64-only, because aarch64 had a
        // last-fault record (`FAR_EL1`, stashed for tests) and RISC-V had only a fault *count*: it
        // knew `stval` at the instant of the fault and threw it away. Milestone 19's portable
        // record keeps it on both, so "something faulted" is no longer all this ISA can say.
        assert_eq!(
            crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
            Some(probe_va),
            "something faulted, but not at the neighbour's address",
        );
        assert_eq!(
            sched::endpoint_waiting_senders(w.client_report[ATTACKER]),
            0,
            "the attacker reported past its probe: the write did not fault, so it read back what it \
             wrote into a neighbour's surface (WIN_ESCAPED)",
        );

        // The witness pattern, from the kernel and then from the victim itself.
        assert_eq!(
            w.client_surface_digest(VICTIM),
            before,
            "the victim's pixels changed while it was blocked: the attack landed",
        );
        release(victim, victim_slot);
        let [tag, after, was_before, ..] = sched::ipc_recv(w.client_report[VICTIM]);
        assert_eq!(tag, status::WIN_INTACT);
        assert_eq!(was_before, before, "the victim changed its story");
        assert_eq!(
            after, before,
            "the victim's own read-back of its surface changed after the attack",
        );

        // No ambient display: a client that draws into the screen cannot read it.
        let faults = USER_FAULTS.load(Ordering::Relaxed);
        w.spawn_client(PEEPER, ROLE_PROBE_SCREEN);
        expect_painted(&w, PEEPER);
        let [tag, screen_va, ..] = sched::ipc_recv(w.client_report[PEEPER]);
        assert_eq!(tag, status::WIN_PROBING);
        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "a window client read the composed screen at {screen_va:#x} and was NOT stopped: the \
             display is ambient",
        );
        assert_eq!(
            crate::arch::exceptions::last_user_fault().map(|(_, addr)| addr),
            Some(screen_va),
            "something faulted, but not at the composed screen's address",
        );
        assert_eq!(
            sched::endpoint_waiting_senders(w.client_report[PEEPER]),
            0,
            "a client read a pixel of the screen it holds no mapping of (WIN_ESCAPED)",
        );
    }

    /// **A one-window redraw costs one rectangle, not a screen.**
    ///
    /// The damage rectangle is the whole reason rung one put one in the contract, and it is only worth
    /// anything if the compositor honours it end to end. So this test does not measure time (which
    /// under TCG would mean nothing); it observes **what was flushed** and **what was left alone**:
    ///
    /// - the kernel plays the display, so the flush rectangle is a value it can compare against
    ///   `compositor::damage_to_screen`, and the count of flushes says one commit produced one flush;
    /// - between the two frames the kernel **poisons every screen pixel outside** the rectangle the
    ///   coming commit should produce. A compositor that repainted the screen would erase the poison.
    ///   Finding it intact afterwards is the proof, and it is the same technique the crate's host test
    ///   uses in microseconds.
    ///
    /// It also pins the startup behaviour that makes the whole picture predictable: the compositor's
    /// first paint is the background over the whole screen, once, with no window drawn, because no
    /// client has committed yet.
    #[test_case]
    fn a_one_window_redraw_costs_one_rectangle_and_not_the_screen() {
        const POISON: u32 = 0xDEAD_BEEF;

        let (display, screen) = kernel_display();
        let w = compositor_service::start(2, 0, display, screen);
        wait_for_compositor(&w);

        assert_eq!(
            FLUSH_COUNT.load(Ordering::SeqCst),
            1,
            "the compositor's startup should be exactly one flush",
        );
        assert_eq!(
            LAST_FLUSH.load(Ordering::SeqCst),
            gfx_proto::rect(0, 0, compositor::SCREEN_W, compositor::SCREEN_H),
            "the startup flush should be the whole screen",
        );
        assert_screen_is(&w, 0);

        // Window 0 paints, commits, reports, and then parks in a CALL waiting for permission to send
        // its second frame; window 1 paints and commits once.
        w.spawn_client(0, ROLE_SMALL_DAMAGE);
        expect_painted(&w, 0);
        let (client0, slot0, d0) = take_call(w.client_report[0], status::WIN_PAINTED);
        assert_eq!(d0, compositor::expected_window_checksum(0));
        w.spawn_client(1, 0);
        expect_painted(&w, 1);
        assert_screen_is(&w, 2);

        // Poison everything the coming commit must not touch.
        let want = compositor::damage_to_screen(&SCENE[0], compositor::SMALL_DAMAGE);
        assert!(
            !want.is_empty() && want.area() * 20 < Rect::screen().area(),
            "the damage rectangle under test is not small: {want:?}",
        );
        for y in 0..compositor::SCREEN_H {
            for x in 0..compositor::SCREEN_W {
                if !want.contains(x as i32, y as i32) {
                    w.poison_screen_pixel(x, y, POISON);
                }
            }
        }

        let flushes = FLUSH_COUNT.load(Ordering::SeqCst);
        release(client0, slot0);
        let [tag, _, seq, ..] = sched::ipc_recv(w.client_report[0]);
        assert_eq!(tag, status::WIN_PAINTED, "the second commit never happened");
        assert_eq!(
            seq, 2,
            "the second commit should be the client's second sequence"
        );

        assert_eq!(
            FLUSH_COUNT.load(Ordering::SeqCst),
            flushes + 1,
            "one commit must be one flush",
        );
        let flushed = gfx_proto::unrect(LAST_FLUSH.load(Ordering::SeqCst));
        assert_eq!(
            flushed,
            (want.x as u32, want.y as u32, want.w, want.h),
            "the flush was not the client's damage placed on the screen",
        );

        for y in 0..compositor::SCREEN_H {
            for x in 0..compositor::SCREEN_W {
                let got = w.screen_pixel(x, y);
                if want.contains(x as i32, y as i32) {
                    assert_eq!(
                        got,
                        compositor::expected_screen_pixel(2, x, y),
                        "inside the damage, ({x},{y}) was not recomposited",
                    );
                } else {
                    assert_eq!(
                        got, POISON,
                        "outside the damage, ({x},{y}) was overwritten: the compositor repainted more \
                         than it was asked to",
                    );
                }
            }
        }
    }

    /// **Input reaches the focused client because that client holds a capability, and focus is the
    /// compositor's decision.**
    ///
    /// Three things are being separated here, and Unix conflates all three:
    ///
    /// 1. **Who may deliver input.** The keystroke arrives in a ring page shared with the input source
    ///    alone. Any client may ring the doorbell; none of them can write that page, so none of them can
    ///    inject a keystroke into another client. (There is no "grab the keyboard" verb to guard,
    ///    because there is nothing a message could say that would do it.)
    /// 2. **Who may receive it.** The focused client receives because it *holds* an input endpoint. A
    ///    client without one cannot be sent a keystroke by anyone, however the compositor feels about
    ///    it, which is the previous test's `NoSuchSlot` refusal seen from the other side.
    /// 3. **Who decides.** Focus moves on a byte, in userspace, in the compositor. The kernel routes
    ///    the message and knows nothing about focus; this test *witnesses* the decision by reading the
    ///    window-list page the compositor publishes rather than by asking it.
    ///
    /// The negative half is the interesting half: after focus moves, the unfocused client must not
    /// receive the next keystroke, and `endpoint_waiting_senders` is how a test says "and then nothing
    /// happened" without blocking forever on a quiet endpoint.
    #[test_case]
    fn input_reaches_only_the_focused_client_and_focus_is_the_compositors_call() {
        let (display, screen) = kernel_display();
        let mut w = compositor_service::start(2, 2, display, screen);
        wait_for_compositor(&w);

        w.spawn_client(0, ROLE_INPUT);
        w.spawn_client(1, ROLE_INPUT);
        for i in 0..2 {
            expect_painted(&w, i);
        }

        assert_eq!(w.focused(), 0, "focus should start on window 0");
        w.type_bytes(b"a");
        let [tag, byte, count, ..] = sched::ipc_recv(w.client_report[0]);
        assert_eq!(
            tag,
            status::WIN_INPUT,
            "the focused client got no keystroke"
        );
        assert_eq!(byte, b'a' as u64);
        assert_eq!(count, 1);

        // Focus moves, and we read the compositor's decision out of the page it publishes.
        w.type_bytes(&[compositor::proto::FOCUS_NEXT]);
        assert_eq!(
            w.focused(),
            1,
            "the compositor did not move focus, or did not publish that it had",
        );
        let record = mmu::phys_to_virt(w.wlist) + wlist::COUNT;
        // SAFETY: the window-list frame this kernel allocated, through the direct map.
        assert_eq!(unsafe { core::ptr::read_volatile(record as *const u32) }, 2);

        w.type_bytes(b"b");
        let [tag, byte, ..] = sched::ipc_recv(w.client_report[1]);
        assert_eq!(
            tag,
            status::WIN_INPUT,
            "the newly focused client got nothing"
        );
        assert_eq!(byte, b'b' as u64);
        assert_eq!(
            sched::endpoint_waiting_senders(w.client_report[0]),
            0,
            "the unfocused client received a keystroke that was not routed to it",
        );
    }

    /// **Focus routes a keystroke into one terminal's grid and not its neighbour's** (milestone 29's
    /// text increment meeting rung two).
    ///
    /// Two display terminals, side by side, each a compositor client with exactly a window client's
    /// authority. The keystroke routing this rung already proved is now **visible in the picture**,
    /// which is a stronger statement than the endpoint-level one and a different kind of evidence:
    /// an `A` typed while terminal 0 has focus appears in terminal 0's grid, a TAB moves focus, and
    /// a `B` appears in terminal 1's. Neither letter appears in the other window, and the kernel
    /// checks that by comparing every pixel of the composed screen against the two engines it ran
    /// itself.
    ///
    /// # Why this is the capability claim and not a policy claim
    ///
    /// Terminal 1 receives the `B` because it **holds an input endpoint**, not because it asked and
    /// not because the compositor consulted a list (DECISIONS §33). The compositor's whole part in
    /// it is choosing *which* of the capabilities it holds to use. A client granted no input endpoint
    /// has an empty cspace slot and cannot be sent a keystroke by anyone, which the neighbouring test
    /// in this module proves by value (`NoSuchSlot`); this one proves the other half, that a
    /// keystroke routed to one holder does not land in another's memory.
    ///
    /// # And the terminal is a client, unchanged at both seams
    ///
    /// `compositor` cannot tell a display terminal from the `window` client that paints a coordinate
    /// pattern: same grants, same control page, same doorbell, same `COMMIT`. Neither `compositor` nor
    /// `gfx_proto` needed a line changed to carry text. That is the seam claim made twice in one
    /// milestone, once at each rung.
    ///
    /// Uses the kernel's display stand-in rather than the GPU, deliberately: this test is about
    /// routing and composition, the device is proved elsewhere in this file, and a third `display`
    /// against the same physical device would put the scanout's picture in a race with the
    /// host-side check that reads it.
    #[test_case]
    fn focus_routes_a_keystroke_to_one_terminals_grid_and_not_its_neighbours() {
        let (display, screen) = kernel_display();
        let mut w = compositor_service::start(2, 2, display, screen);
        wait_for_compositor(&w);

        // Both terminals up. Each negotiated its geometry out of the control page the compositor
        // published, so a window whose size the client did not choose is the *normal* case here.
        let mut terms = [video_terminal::Vt::new(1, 1), video_terminal::Vt::new(1, 1)];
        let mut clients = [None, None];
        for i in 0..2 {
            let c = w.spawn_terminal(i);
            let [tag, dims, mode, ..] = sched::ipc_recv(w.client_report[i]);
            assert_eq!(
                tag,
                video_terminal::status::TERM_UP,
                "terminal {i} did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte \
                 names the step, see user/src/display_terminal.rs)",
            );
            assert_eq!(mode, video_terminal::status::MODE_WINDOW);
            let (cols, rows) = ((dims & 0xffff_ffff) as u32, (dims >> 32) as u32);
            assert_eq!(
                (cols, rows),
                (SCENE[i].w / bitfont::GLYPH_W, SCENE[i].h / bitfont::GLYPH_H),
                "terminal {i} sized itself to a grid its window cannot hold",
            );
            terms[i] = video_terminal::script::window(i, cols, rows);
            clients[i] = Some(c);
        }

        // Each terminal prints its own banner. Different text per window, so an OP_WRITE delivered
        // to the wrong terminal is a wrong picture rather than a duplicate one.
        for (i, c) in clients.iter().enumerate() {
            c.as_ref()
                .unwrap()
                .print(video_terminal::script::WINDOW_BANNER[i]);
        }

        // Type at the focused terminal, move focus with TAB, type at the next one. `type_bytes`
        // writes the input ring and rings the doorbell, which is exactly what a keyboard driver
        // does; the authority it exercises is the ring mapping, and no client has one.
        assert_eq!(w.focused(), 0, "focus should start on the bottom window");
        w.type_bytes(video_terminal::script::WINDOW_TYPED[0]);
        assert_eq!(w.focused(), 0, "typing must not move focus");
        w.type_bytes(&[compositor::proto::FOCUS_NEXT]);
        assert_eq!(
            w.focused(),
            1,
            "TAB did not move focus: the compositor's own policy decision, published in the window \
             list so it can be witnessed rather than asked for",
        );
        w.type_bytes(video_terminal::script::WINDOW_TYPED[1]);

        // The picture. Every pixel of the composed screen, against the two engines the kernel ran
        // itself: window content from `video_terminal`, placement and stacking from `compositor`.
        for y in 0..compositor::SCREEN_H {
            for x in 0..compositor::SCREEN_W {
                let got = w.screen_pixel(x, y);
                let want = compositor::expected_screen_pixel_with(2, x, y, |i, sx, sy| {
                    terms[i].pixel(sx, sy)
                });
                assert_eq!(
                    got, want,
                    "the composed screen is wrong at ({x},{y}): {got:#010x}, expected {want:#010x}",
                );
            }
        }

        // **And the letters really are distinguishable**, so the comparison above has teeth: a
        // compositor that sent both keystrokes to both terminals, or the wrong one to each, would
        // have to produce a different picture. Asserted rather than assumed, because if the two
        // scripts ever became the same text this test would keep passing while proving nothing.
        let swapped = [
            video_terminal::script::window(1, terms[0].cols(), terms[0].rows()),
            video_terminal::script::window(0, terms[1].cols(), terms[1].rows()),
        ];
        assert!(
            (0..compositor::SCREEN_H).any(|y| (0..compositor::SCREEN_W).any(|x| {
                compositor::expected_screen_pixel_with(2, x, y, |i, sx, sy| {
                    swapped[i].pixel(sx, sy)
                }) != compositor::expected_screen_pixel_with(2, x, y, |i, sx, sy| {
                    terms[i].pixel(sx, sy)
                })
            })),
            "the two terminals show the same thing: this test cannot tell mis-routed input from \
             correct input",
        );
    }

    /// **Three clients' surfaces become one screen, and the host confirms it.**
    ///
    /// The end-to-end picture, with a real virtio-gpu under it (rung one's driver, unchanged: the
    /// compositor takes `painter`'s place at that seam and `display` cannot tell). Four witnesses, which is
    /// the point, because a compositor's output is exactly the thing one digest cannot be trusted about:
    ///
    /// 1. **the driver**, digesting the frames it handed the device after the device said it had them.
    ///    Its one report is the compositor's *startup* frame, which is the background alone, so it is
    ///    also the check that an empty screen is a defined picture rather than whatever was in RAM;
    /// 2. **the kernel**, reading the scanout frames through the direct map and comparing every pixel
    ///    against `compositor::expected_screen_pixel`, a value it computed itself;
    /// 3. **a capture client in its own address space**, which holds a read-only mapping of the screen
    ///    (that mapping being the screenshot capability) and digests what it sees;
    /// 4. **the host**, through QEMU's monitor, comparing `screendump`'s PPM against the same
    ///    definition. This one is not optional: `-display none` means nothing in the guest can see the
    ///    device's own surface, so a wrong pixel format or scanout rectangle would pass all three
    ///    in-guest witnesses and show garbage on a screen. `cargo xtask` runs it beside this suite.
    ///
    /// The capture client also proves the *shape* of the grant twice over: it enumerates the windows
    /// out of the read-only page the compositor publishes (there is no enumerate verb to call), and its
    /// attempt to **write** the screen faults, because a thing that may look at the screen may not draw
    /// on it.
    #[test_case]
    fn three_clients_compose_into_one_scanout_and_the_host_sees_it() {
        let display = program("display").expect("no display program in the initrd archive");
        let (driver_report, display, screen) = display_service::start_driver(display).expect(
            "no virtio-gpu-pci function on the bus: is CRICKER_GPU missing from the test leg, or \
             the -device virtio-gpu-pci line from the runner?",
        );
        assert!(
            crate::iommu::active(),
            "a virtio-gpu is present but the IOMMU is not active: the GPU's pixel reads are \
             unconfined (notes/framebuffer-contract.md)",
        );
        let [tag, geometry, ..] = sched::ipc_recv(driver_report);
        assert_eq!(
            tag,
            gfx_proto::status::UP,
            "the display driver did not come up (it reported {tag:#x})",
        );
        assert_eq!(
            geometry,
            gfx_proto::WIDTH as u64 | ((gfx_proto::HEIGHT as u64) << 32),
        );

        let w = compositor_service::start(3, 0, display, screen);
        wait_for_compositor(&w);

        // The driver's own account of the compositor's first frame. Taken here and not later because
        // this is a rendezvous SEND: the driver is parked in it, and a test that spawned clients first
        // would deadlock the driver against the compositor's next flush.
        let [tag, driver_digest, pixels, ..] = sched::ipc_recv(driver_report);
        assert_eq!(
            tag,
            gfx_proto::status::FLUSHED,
            "the driver served no flush"
        );
        assert_eq!(pixels, gfx_proto::PIXELS as u64);
        assert_eq!(
            driver_digest,
            compositor::expected_screen_checksum(0),
            "the frames the device read for the compositor's first flush are not the background: an \
             empty screen must be a defined picture",
        );

        // In order, so that the capture client below is looking at a finished screen.
        for i in 0..2 {
            w.spawn_client(i, 0);
            expect_painted(&w, i);
        }

        let faults = USER_FAULTS.load(Ordering::Relaxed);
        w.spawn_client(2, ROLE_CAPTURE);
        expect_painted(&w, 2);

        // The screenshot, taken through a read-only mapping by a process that holds one.
        let [tag, shot, listed, ..] = sched::ipc_recv(w.client_report[2]);
        assert_eq!(
            tag,
            status::WIN_CAPTURED,
            "the capture client reported nothing"
        );
        assert_eq!(
            shot,
            compositor::expected_screen_checksum(3),
            "a client holding the screen's read-only mapping digested a different screen than the \
             kernel computed from the contract",
        );
        assert_eq!(
            listed,
            3u64 << 32,
            "the window list it enumerated does not say three windows with focus on the first",
        );

        // The kernel's own witness, pixel for pixel.
        assert_screen_is(&w, 3);

        // The other half of a read-only grant: it cannot deface what it may read.
        let [tag, va, which, ..] = sched::ipc_recv(w.client_report[2]);
        assert_eq!(tag, status::WIN_PROBING);
        assert_eq!(which, 1, "the capture client skipped its write probe");
        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > faults),
            "a client wrote to the screen through a read-only mapping at {va:#x} and was NOT stopped",
        );
        assert_eq!(
            sched::endpoint_waiting_senders(w.client_report[2]),
            0,
            "the capture client survived writing to the screen (WIN_ESCAPED)",
        );
        // And the write did not land, which is the thing that would otherwise corrupt the picture the
        // host is about to check.
        assert_screen_is(&w, 3);

        // **Hold the picture up for the host.** `cargo xtask` polls QEMU's monitor about every 250 ms
        // while this suite runs, and the next test in the file re-registers the device (which destroys
        // the scanout), so a composed screen that vanished immediately could be missed. Three seconds is
        // an order of magnitude more than the poll needs and is nothing against the per-test ceiling.
        // If the host never sees it, the run fails at the scanout check rather than passing quietly.
        let deadline = crate::arch::timer::now() + 3 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            sched::yield_now();
        }
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
    use gfx_proto as gfx;

    use super::*;
    use crate::sched;

    /// **A confined userspace driver puts a known pattern in a scanout framebuffer.**
    ///
    /// # What this proves, precisely
    ///
    /// The pattern is a per-coordinate function ([`gfx_proto::pixel`]), not a fill, and the digest is
    /// position sensitive, so a blank, stale, shifted, transposed, or truncated surface cannot pass
    /// (`crates/gfx_proto`'s host tests assert exactly those properties of the pattern itself). Two
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
        let display = program("display").expect("no display program in the initrd archive");
        let painter = program("painter").expect("no painter program in the initrd archive");
        let threads_before = sched::thread_count();

        // A GPU asked for but not enumerated is a build-order mistake wearing a machine fact's
        // clothes, the hazard the runners were taught to fail loudly on. The test legs always attach
        // one, so absence is a failure, not a skip.
        let (driver_report, client_report) = display_service::start(display, painter).expect(
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
             is the bring-up step that failed, see user/src/display.rs)",
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
        let display = program("display").expect("no display program in the initrd archive");

        // Drain any stale fault first, so what we observe is this test's.
        while crate::iommu::take_fault().is_some() {}

        let (report, victim) = display_service::start_backing_escape(display).expect(
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
             low byte names the bring-up step, see user/src/display.rs)",
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
        // same reason (user/src/display.rs).
        while crate::iommu::take_fault().is_some() {}
    }

    /// **A bitmap font and a VT engine put readable text on the scanout.**
    ///
    /// Milestone 29's remaining increment, end to end: a terminal component that is a *client* of
    /// the framebuffer contract, drawing glyphs into a surface and flushing a damage rectangle.
    ///
    /// # What makes this a proof rather than a screenshot
    ///
    /// Text is the case where "it looked right" is most tempting and least sufficient, so the
    /// picture is a **value three parties compute independently**:
    ///
    /// - the **terminal** runs the `video_terminal` engine over the bytes it was sent and paints what it says;
    /// - the **kernel** runs the same engine over the same script (`video_terminal::script`) and compares the
    ///   scanout frames pixel for pixel through the direct map. It never asks the terminal anything;
    /// - the **host** runs it again and compares QEMU's `screendump` against the same definition
    ///   (`cargo xtask`, beside this suite). That one is not optional: `-display none` means nothing
    ///   in the guest can see the device's own surface, so a wrong pixel format or scanout rectangle
    ///   would satisfy the first two and show garbage on a screen.
    ///
    /// And the host checker has a **negative control**: it must reject the same screen with one
    /// letter changed (`video_terminal::script::GREETING_TYPO`). A checker that only asked "is there ink?" would
    /// pass every run including the ones that drew the wrong thing.
    ///
    /// # What the script exercises, and why each part is there
    ///
    /// Four rows of text (a one-row picture would hide a stride error), three renditions (a terminal
    /// that ignored SGR would draw every glyph correctly and still fail), a `\r\n` pair (what
    /// `line_editor::expand_output` puts on the wire for a Unix `\n`), descenders and an underscore (the
    /// glyph rows a font table truncated to seven would lose), and then **keystrokes**, delivered as
    /// `OP_BYTES`: the terminal contract's driver half, byte for byte what `user/src/input.rs` sends
    /// and what the compositor forwards to a focused client.
    ///
    /// # And the picture the driver reports is the *blank* terminal, on purpose
    ///
    /// `display`'s one status report covers its first flush, which here is the terminal's blank grid
    /// before anything has been written. So it doubles as the check that an empty terminal is a
    /// *defined* picture (spaces on the default background, with the cursor) rather than whatever
    /// those frames held at boot, exactly as rung two used it for the empty compositor screen.
    ///
    /// **Runs after the confinement test and before the pattern test**, which the name arranges: the
    /// confinement test resets the device and would wipe this, and the pattern test's picture is the
    /// one that stays up until QEMU exits. See notes/glyphs.md for the ordering and what breaks it.
    #[test_case]
    fn a_bitmap_font_and_a_vt_engine_put_readable_text_on_the_scanout() {
        let display = program("display").expect("no display program in the initrd archive");
        let display_terminal =
            program("display_terminal").expect("no display_terminal program in the initrd archive");

        let w = display_service::start_terminal(display, display_terminal).expect(
            "no virtio-gpu-pci function on the bus: is CRICKER_GPU missing from the test leg, or \
             the -device virtio-gpu-pci line from the runner?",
        );

        // The driver came up. Taken first: these are rendezvous SENDs, so the driver is parked here.
        let [tag, geometry, ..] = sched::ipc_recv(w.driver_report);
        assert_eq!(
            tag,
            gfx::status::UP,
            "the display driver did not come up (it reported {tag:#x})",
        );
        assert_eq!(geometry, gfx::WIDTH as u64 | ((gfx::HEIGHT as u64) << 32),);

        // The terminal negotiated its geometry from the driver rather than assuming it, and got the
        // grid the script is written for.
        let [tag, dims, mode, ..] = sched::ipc_recv(w.term_report);
        assert_eq!(
            tag,
            video_terminal::status::TERM_UP,
            "the display terminal did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low \
             byte names the step, see user/src/display_terminal.rs)",
        );
        assert_eq!(
            dims,
            video_terminal::script::COLS as u64 | ((video_terminal::script::ROWS as u64) << 32),
            "the terminal sized itself to a different grid than the script predicts",
        );
        assert_eq!(mode, video_terminal::status::MODE_DISPLAY);

        // The driver's account of the terminal's first flush: the blank grid. A second address
        // space's witness, taken after the device reported the transfer complete.
        let blank =
            video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
        let [tag, driver_digest, pixels, ..] = sched::ipc_recv(w.driver_report);
        assert_eq!(tag, gfx::status::FLUSHED, "the driver served no flush");
        assert_eq!(pixels, gfx::PIXELS as u64);
        assert_eq!(
            driver_digest,
            gfx::checksum(|i| {
                let (x, y) = (
                    (i % gfx::WIDTH as usize) as u32,
                    (i / gfx::WIDTH as usize) as u32,
                );
                blank.pixel(x, y)
            }),
            "the frames the device read for the terminal's first flush are not a blank terminal: an \
             empty terminal must be a defined picture, not whatever was in those frames",
        );
        w.assert_screen_is(&blank, "a terminal that has been sent nothing");

        // Play the application, then the input driver. Both replies mean the pixels are on the
        // device's side, so there is nothing to poll and nothing to sleep for.
        w.print(video_terminal::script::GREETING);
        w.type_bytes(video_terminal::script::TYPED);

        // The kernel's own witness: every pixel, against the engine it ran itself.
        let expect = video_terminal::script::full_screen();
        w.assert_screen_is(&expect, "after the greeting and the typing");

        // A wrong screen must not pass this. The typo picture differs from the real one in one
        // letter, so asserting they differ at all is what says the comparison above has teeth: if
        // `assert_screen_is` were somehow vacuous, this would still be true, which is why the check
        // is that the two *pictures* differ rather than that the screen is not the typo.
        let mut typo =
            video_terminal::Vt::new(video_terminal::script::COLS, video_terminal::script::ROWS);
        typo.feed(video_terminal::script::GREETING_TYPO);
        typo.feed(video_terminal::script::TYPED);
        assert!(
            (0..gfx::HEIGHT)
                .any(|y| (0..gfx::WIDTH).any(|x| typo.pixel(x, y) != expect.pixel(x, y))),
            "the one-letter typo produces an identical picture: the negative control is inert",
        );

        // **Hold the picture up for the host.** `cargo xtask` polls QEMU's monitor while this suite
        // runs, and the next test puts rung one's pattern on the same scanout. Three seconds is an
        // order of magnitude more than the poll needs, and if the host never sees it the run fails at
        // the scanout check rather than passing quietly.
        let deadline = crate::arch::timer::now() + 3 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            sched::yield_now();
        }
    }

    /// **A key pressed on a real device becomes the byte a terminal receives** (milestone 29's
    /// input).
    ///
    /// A confined userspace virtio-input driver, behind the same PCIe transport and the same IOMMU
    /// domain as the GPU, brings up the event queue, takes a real key event off it, and publishes
    /// the byte in the compositor's input ring.
    ///
    /// # Why the host has to press the key, and how
    ///
    /// Nothing in the guest can press a key: that is the point of a device test. So the **host**
    /// does it, over the same QEMU monitor connection the scanout check already holds open.
    /// `cargo xtask` sends `sendkey` beside this suite, exactly as it dumps the scanout beside it,
    /// and `video_terminal::script::HOST_KEY` is the one definition of which key so the pressing side and the
    /// asserting side cannot drift. The keys go out from the start of the run and QEMU drops them
    /// until a driver sets `DRIVER_OK`, so there is nothing to synchronize.
    ///
    /// # What this proves, and where it hands off
    ///
    /// The path from **a physical key event to a terminal byte**: the device is enumerated and
    /// checked (`DeviceID` is virtio-input, not whatever the transport felt like saying), the event
    /// queue is programmed through the confined transport with every buffer device-**writable**,
    /// an event arrives by interrupt, `video_terminal::keymap` turns an evdev code into a character, and the
    /// byte lands in the input ring.
    ///
    /// The rest of the path (ring to focused client to pixels) is
    /// `compositor_tests::focus_routes_a_keystroke_to_one_terminals_grid_and_not_its_neighbours`,
    /// and the seam between the two halves is the ring itself, which is exactly where DECISIONS §33
    /// put the boundary: the driver's authority to type is the ring's mapping, and the compositor's
    /// authority to deliver is the client endpoints it holds. Naming the seam is better than one
    /// test that hides it.
    ///
    /// # The authority this driver does not have
    ///
    /// It holds no client's endpoint, so it cannot choose who receives a keystroke; it cannot even
    /// name a client. And it rings the same **content-free** doorbell every client holds, so nothing
    /// it says carries the keystroke: a client that rang that endpoint forever could not type a
    /// character, because typing is a page it does not map.
    #[test_case]
    fn a_keystroke_from_a_virtio_keyboard_becomes_a_terminal_byte() {
        let kbd = program("kbd").expect("no kbd program in the initrd archive");
        let mut w = keyboard_service::start(kbd).expect(
            "no virtio-input function on the bus: is CRICKER_KBD missing from the test leg, or the \
             -device virtio-keyboard-pci line from the runner?",
        );
        assert!(
            crate::iommu::active(),
            "a keyboard is present but the IOMMU is not active: the device's event buffers are \
             unconfined, and a keyboard's buffers are where every keystroke lands",
        );

        let [tag, buffers, ..] = sched::ipc_recv(w.report);
        assert_eq!(
            tag,
            video_terminal::status::KBD_UP,
            "the keyboard driver did not come up (it reported {tag:#x}; a 0xDEAD_.. word's low byte \
             names the step, see user/src/kbd.rs)",
        );
        assert!(
            buffers > 0,
            "the driver posted no event buffers, so a key would have nowhere to land",
        );

        // Wait for the driver to ring, which it only does when it has typed something. Bounded on
        // the clock: if the host's `sendkey` never reaches the device, this fails with a sentence
        // rather than hanging until the harness's ceiling.
        let deadline = crate::arch::timer::now() + 10 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline
            && sched::endpoint_waiting_senders(w.doorbell) == 0
        {
            sched::yield_now();
        }
        assert!(
            sched::endpoint_waiting_senders(w.doorbell) > 0,
            "the keyboard driver came up but never typed anything in ten seconds: the host's \
             `sendkey {}` is not reaching the device (is the monitor socket attached? see \
             cargo xtask's scanout check, which owns that connection)",
            video_terminal::script::HOST_KEY,
        );
        w.answer_doorbell();

        let mut typed = [0u8; 16];
        let n = w.take_typed(&mut typed);
        assert!(n > 0, "the driver rang the doorbell with an empty ring");
        for (i, &b) in typed[..n].iter().enumerate() {
            assert_eq!(
                b,
                video_terminal::script::HOST_KEY_BYTE,
                "byte {i} of {n} from the keyboard is {:?}, not the {:?} the host pressed: the \
                 evdev keycode was mapped wrong, or a key release was counted as a press",
                b as char,
                video_terminal::script::HOST_KEY_BYTE as char,
            );
        }
        // A release must not type. The host sends press *and* release for each `sendkey`, so a
        // driver that ignored the value field would produce two bytes per press; that would show up
        // above as the right character twice, which is why the count is checked against the
        // presses the host could have made rather than merely being non-zero.
        assert!(
            n <= 64,
            "{n} bytes from a handful of key presses: releases or auto-repeats are being counted \
             more than once",
        );
    }
}

/// **Rust `std` on the native ABI** (milestone 27): spawn the `std_exerciser` demo, an ordinary Rust
/// program (no `no_std`, no attributes) built for the `*-unknown-cricker` custom target with std's
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
pub mod std_service {
    use super::*;
    use crate::cap::{Rights, endpoint_cap, frame_cap, untyped_cap};
    use crate::sched::EpId;

    /// Where the loader maps the clock page for a std program. Must match the std PAL's
    /// `rt::CLOCK_PAGE`, and the slot must match its `rt::CLOCK_SLOT`.
    const CLOCK_PAGE_STD: u64 = 0x1200_0000;
    const CLOCK_SLOT: u64 = 5;

    /// The entropy service's request endpoint (milestone 56). Must match the std PAL's
    /// `rt::ENTROPY_SLOT`. **An endpoint, and no mapping**: unlike the clock, whose read authority
    /// IS a page, randomness is obtained by asking, so the whole grant is one endpoint that names
    /// no device.
    const ENTROPY_SLOT: u64 = 6;

    /// The heap high-water for the demo's Vec/String/HashMap workout plus std's own runtime
    /// allocations and the heap's page tables is well under 1 MiB; 256 pages is comfortable, and
    /// the initial region only needs to be contiguous at spawn, when memory is unfragmented.
    pub const BUDGET_PAGES: u64 = 256;

    /// std's startup, formatting machinery, and collection code use far more stack than a
    /// hand-written `no_std` worker. `load` maps one stack page; map 32 more below it (128 KiB
    /// total), generous so a stack-depth surprise is not what a first std bring-up debugs.
    const EXTRA_STACK_PAGES: u64 = 32;

    pub fn start(
        image: &'static [u8],
        clock_image: &'static [u8],
        entropy_image: &'static [u8],
    ) -> EpId {
        let report = crate::sched::create_endpoint();
        start_on(image, clock_image, entropy_image, report);
        report
    }

    /// The same spawn, with **the output sink chosen by the caller** (milestone 50).
    ///
    /// This split is the milestone's finding expressed as a function signature: everything about a
    /// std program's wiring is fixed except one endpoint capability, and putting a different one in
    /// slot 1 is the whole of redirection. The program is not told, cannot ask, and the two callers
    /// of this function hand it an endpoint the kernel receives on and an endpoint a file sink
    /// receives on. See `sink_tests`.
    pub fn start_on(
        image: &'static [u8],
        clock_image: &'static [u8],
        entropy_image: &'static [u8],
        report: EpId,
    ) {
        let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for std_exerciser");

        // The entropy service, wired once per boot and shared with the milestone-56 tests. Its
        // request endpoint is the whole of a std program's randomness authority: `SystemRng` is a
        // `CALL` on it, and nothing about it reaches the device (DECISIONS §44).
        let entropy = entropy_service::ensure(entropy_image, entropy_service::Bus::Mmio)
            .expect("no virtio-rng device for the std program (is CRICKER_RNG set on this leg?)");
        if let Some(ready) = entropy.ready {
            let report = crate::sched::ipc_recv(ready);
            assert_eq!(
                report[0],
                entropy_proto::READY,
                "the entropy service did not come up for the std program (it reported {:#x})",
                report[0],
            );
        }

        // The clock first, and its startup report taken before the program starts, so the offset is
        // published by the time std reads the page. Waiting is not a synchronisation trick, it is
        // the honest order: a std program that started first would see `state::UNKNOWN` and be
        // correct to say so.
        let clock = clock_service::start(clock_image);
        let _ = crate::sched::ipc_recv(clock.report);

        // The clock page read-only, then the deep stack std needs.
        let mut maps = [Mapping {
            va: 0,
            phys: 0,
            flags: Flags::user_data(),
        }; 1 + EXTRA_STACK_PAGES as usize];
        maps[0] = Mapping {
            va: CLOCK_PAGE_STD,
            phys: clock.page_phys,
            flags: Flags::user_rodata(), // a READER, and the mapping is what says so
        };
        for (k, m) in maps[1..].iter_mut().enumerate() {
            let phys = crate::memory::alloc()
                .expect("no frame for std_exerciser stack")
                .addr();
            // SAFETY: fresh frame via the direct map; zero it so the new process starts clean.
            unsafe {
                core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
            }
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = phys;
        }

        crate::sched::spawn(move || {
            // The clock and entropy capabilities go in at their named slots BEFORE `run` grants in
            // order, so `run`'s two grants land at 0 and 1 and slots 2 to 4 stay empty. The clock
            // is `READ` only: the whole point is that a reader cannot write the offset. See
            // `grant_at`.
            crate::sched::grant_at(CLOCK_SLOT, frame_cap(clock.page_phys, Rights::READ))
                .expect("the std clock slot was already occupied");
            crate::sched::grant_at(ENTROPY_SLOT, endpoint_cap(entropy.request, Rights::WRITE))
                .expect("the std entropy slot was already occupied");
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
                    maps: &maps,
                },
            )
        })
        .expect("could not spawn std_exerciser");
    }
}

#[cfg(test)]
mod std_tests {
    use super::*;

    /// Reassemble a std program's stdout off its endpoint until the writer says the stream is over,
    /// and compare byte for byte.
    ///
    /// The framing is the sink contract's (`crates/sink_proto`, milestone 50), which for bytes is
    /// bit-for-bit the framing the PAL used before that contract existed: `w0` is the count, `w1`
    /// and `w2` are the bytes, little-endian. `SEND` blocks until a receiver takes it, so the
    /// program is somewhere between its last `println!` and `SYS_EXIT` when the bytes land.
    ///
    /// **It reads to end of stream rather than to a byte count, and that is a strengthening.**
    /// Stopping at `want.len()` proved the right bytes came out and said nothing about whether more
    /// were coming; draining to `OP_EOF` proves the program printed exactly this and then finished,
    /// and it exercises the end-of-stream announcement std's `cleanup` now makes, which is what a
    /// pipe's reader will depend on.
    ///
    /// Shared by every std test on both ISAs (the arch-gated test modules reach it here), so all of
    /// them assert the same way and a drift in one is a diff in one place.
    pub(super) fn assert_std_transcript(report: crate::sched::EpId, want: &[u8], what: &str) {
        let mut got = [0u8; 512];
        let len = drain_sink(report, &mut got, what);
        assert_eq!(
            &got[..len],
            want,
            "{what}: stdout did not match the transcript"
        );
    }

    /// Reassemble a sink-contract stream into `out` until end of stream; returns its length.
    ///
    /// The one decoder for every sink in the suite, on purpose. The indifference test's whole claim
    /// is that two destinations produce the same bytes, and it would be a much weaker claim if each
    /// arm were decoded by its own code.
    pub(super) fn drain_sink(ep: crate::sched::EpId, out: &mut [u8], what: &str) -> usize {
        let mut len = 0usize;
        loop {
            let words = crate::sched::ipc_recv(ep);
            let mut chunk = [0u8; sink_proto::INLINE_MAX];
            match sink_proto::unpack(words[0], words[1], words[2], &mut chunk) {
                sink_proto::Msg::Bytes(n) => {
                    for &b in &chunk[..n] {
                        assert!(len < out.len(), "{what}: wrote more than the buffer holds");
                        out[len] = b;
                        len += 1;
                    }
                }
                sink_proto::Msg::Eof => return len,
                sink_proto::Msg::Malformed => {
                    panic!(
                        "{what}: a sink message the contract does not define: {:#x}",
                        words[0]
                    )
                }
            }
        }
    }

    /// Consume the FS service's two readiness sentinels, if this caller is the one that wired it.
    ///
    /// One boot has one FS service (the block server owns the device), so the hand-written client's
    /// test and the `std::fs` test share it, and only the first of them to run gets the sentinels.
    /// Asserting on them where they exist is what separates a hang in the mount from one in the
    /// serve path.
    /// **Kill the filesystem mid-transaction on a real device, then mount what is left of it**
    /// (milestone 37, DECISIONS §34 condition 1). Shared by both ISA test modules, so the property
    /// and its wording are asserted identically on each (rule 5, §19).
    ///
    /// Six steps, each of which has to happen for the next one to mean anything:
    ///
    /// 1. a block server brings up the crash test's own disk (nothing else in the boot touches it);
    /// 2. an FS server mounts it, so the image was sound before this test damaged it;
    /// 3. the driver writes payload A, reads it straight back, and reports: **acknowledged**;
    /// 4. the driver's second write walks into the injector, which tears a block at 2048 bytes and
    ///    traps with the transaction's commit unwritten. The `CUT` word is how we know the kill was
    ///    the injector's rather than something else having gone wrong;
    /// 5. a **different FS-server process** mounts the same disk through the same block server. Its
    ///    readiness sentinel is the consistency result: `Server::open` refuses an image it cannot
    ///    make sense of, so arriving at all means the filesystem is intact;
    /// 6. a fresh client reads the file and says which payload is in it.
    ///
    /// **The assertion is the property, not an outcome.** Either payload is a pass; what fails is a
    /// mixture, a length nobody wrote, or the pre-boot contents (which would mean an acknowledged
    /// write had vanished). Pinning the answer to "A" would be pinning a detail of when RedoxFS
    /// happens to write its commit, and the claim is not about that.
    pub(super) fn assert_a_kill_mid_transaction_recovers(
        blk_image: &'static [u8],
        fs_server_image: &'static [u8],
        client_image: &'static [u8],
    ) {
        use fs_proto::fixture::{READY, SUCCESS, crash};
        let Some(run) = fs_service::start_crash(blk_image, fs_server_image, client_image) else {
            crate::println!("    (no crash disk attached; skipping)");
            return;
        };
        assert_eq!(
            crate::sched::ipc_recv(run.blk_ready)[0],
            READY,
            "the block server did not bring the crash-test disk up",
        );
        assert_eq!(
            crate::sched::ipc_recv(run.fs_ready)[0],
            READY,
            "the FS server did not open the crash-test image, so there was nothing to crash",
        );

        let [w0, w1, ..] = crate::sched::ipc_recv(run.driver_report);
        assert_eq!(
            (w0, w1),
            (SUCCESS, crash::SAW_A),
            "the driver's first write was not acknowledged and read back, so nothing that follows \
             is a statement about an acknowledged write",
        );

        assert_eq!(
            crate::sched::ipc_recv(run.fs_ready)[0],
            crash::CUT,
            "the FS server did not die inside the injector: whatever killed it, it was not this \
             test, and the recovery below would be measuring the wrong thing",
        );

        let (ready, report) = fs_service::recover_crash(fs_server_image, client_image);
        assert_eq!(
            crate::sched::ipc_recv(ready)[0],
            READY,
            "the recovery mount FAILED: a fresh FS server could not open the image the killed one \
             left behind, so a torn write mid-transaction cost the whole filesystem",
        );

        let [len, saw, ..] = crate::sched::ipc_recv(report);
        assert!(
            saw == crash::SAW_A || saw == crash::SAW_B,
            "after a kill mid-transaction the file held {len} bytes that were neither payload \
             whole: a write is either wholly present or wholly absent, and this was neither",
        );
        crate::println!(
            "    (crash recovery: the file holds payload {}, {len} bytes, whole)",
            if saw == crash::SAW_A { "A" } else { "B" },
        );
    }

    /// **The extended-attribute witness's verdict** (milestone 57), asserted as an exact set so that
    /// a client which could do nothing and one which could do everything both fail.
    ///
    /// One copy, here beside the other shared FS assertions, because the two ISA legs assert the
    /// same thing and the useful part is naming *which* claim broke rather than printing a number
    /// the reader has to decode. Each missing bit is one sentence about the layer.
    pub(super) fn assert_attrs(attrs: u64) {
        use fs_proto::fixture::attrs as a;
        if attrs == a::EXPECTED {
            return;
        }
        for (bit, what) in [
            (
                a::SET_AND_READ_BACK,
                "an attribute set and read back with its type code",
            ),
            (a::LISTED, "the listing named it and nothing else"),
            (a::SURVIVED_RENAME, "it followed its file across a rename"),
            (a::GONE_AFTER_REMOVE, "removing it made it ENODATA"),
            (
                a::GONE_AFTER_UNLINK,
                "a file remade at the same name inherited nothing",
            ),
            (
                a::OVERSIZE_REFUSED,
                "an over-long value was refused with E2BIG",
            ),
            (
                a::STORE_UNNAMEABLE,
                "the attribute store could not be named",
            ),
            (a::STORE_UNLISTED, "and did not appear in an enumeration"),
        ] {
            if attrs & bit == 0 {
                crate::println!("    MISSING: {what}");
            }
        }
        if attrs & a::ATTRS_FAILED != 0 {
            crate::println!("    the witness reported that something it should do failed");
        }
        panic!(
            "the extended-attribute witness reported {attrs:#x}, expected {:#x}",
            a::EXPECTED,
        );
    }

    pub(super) fn assert_fs_service_ready(
        readiness: Option<(crate::sched::EpId, crate::sched::EpId)>,
    ) {
        // One copy, in `fs_service`, because draining these is **sequencing** and not only an
        // assertion: each server is parked inside its own blocking announcement until somebody
        // receives it, so nothing it serves can be answered first. The caretakers depend on that.
        fs_service::wait_for_service(readiness);
    }

    /// Build the exact bytes `std_exerciser` prints when it is granted a directory capability, into
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
            b"missing not found\n".as_slice(),
            // Milestone 31 phase 2: `create unsupported` became `write create ok`, plus the two
            // refusals that prove CREATE did not widen what a client can reach.
            b"write create ok\ncreate_new refused\n".as_slice(),
            b"create refused absolute\ncreate refused dotdot\n".as_slice(),
            b"write readback ok\nfs ok\n".as_slice(),
        ] {
            buf[n..n + part.len()].copy_from_slice(part);
            n += part.len();
        }
        n
    }

    /// The exact bytes `std_exerciser` prints, in order. `println!` is line-buffered and every line
    /// ends in `\n`, so the whole transcript is flushed by the time the program exits. Pinned here
    /// so a drift in std's behaviour, the PAL, or the demo is a loud diff rather than a mystery.
    /// `os cricker` proves `std::env::consts::OS` resolves through the patched `env_consts`; the
    /// two `unsupported` lines prove `fs`/`net` refuse honestly rather than pretend.
    pub(super) const EXPECTED: &[u8] = b"hello from std on cricker-os\n\
        os cricker\n\
        vec sum 149985000\n\
        string len 800\n\
        map lookup 1369\n\
        fs honestly unsupported\n\
        net honestly unsupported\n\
        instant monotonic ok\n\
        wall clock ok\n\
        entropy ok\n";

    /// A whole Rust `std` program runs on the native ABI and its output is exactly right.
    ///
    /// Granted a heap, a stdout endpoint, and a clock, so both `fs` and `net` refuse: the two
    /// `unsupported` lines in the transcript are "no ambient filesystem" and "no ambient network"
    /// felt from inside std, on a binary that also runs both for real when it is granted them.
    ///
    /// The `wall clock ok` line is milestone 51's half: `SystemTime::now()` reached a real time,
    /// through a read-only page and the ambient counter, and the program asserted it was inside the
    /// same sanity window the clock service applies. Before that milestone the same call returned
    /// 1970 plus uptime and would have passed any test that only checked it did not crash.
    ///
    /// The `entropy ok` line is milestone 56's half, and it is the same shape of correction:
    /// `std::random` reached a virtio-rng device, through one endpoint that names no device, and
    /// the program asserted two draws differ. Before that milestone the same call returned
    /// splitmix64 seeded from boot-relative time, which would also have passed any test that only
    /// checked it did not crash.
    #[test_case]
    fn a_whole_std_program_runs_on_the_native_abi() {
        let image =
            program("std_exerciser").expect("no std_exerciser program in the initrd archive");
        let clock = program("clock").expect("no clock program in the initrd archive");
        let entropy = program("entropy").expect("no entropy program in the initrd archive");
        let report = std_service::start(image, clock, entropy);
        assert_std_transcript(report, EXPECTED, "std_exerciser");
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

// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
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
/// and delegated. See user/src/hello.rs `ep_maker()/ep_user()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
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
/// and a report line; everything else it constructs. See user/src/hello.rs `aspace_builder()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
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
/// user/src/hello.rs `call_server()/call_client()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
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
/// user/src/hello.rs `revoke_demo()`.
// Test scaffolding: the `tests` module below is the only caller, and it runs on both ISAs now
// (milestone 19's user-test port). This wiring was already portable; it was compiled out on riscv64
// only because its consumer was.
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
mod tests {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    // The std-transcript and FS-readiness assertions live with the std tests so both ISAs share one
    // copy; see `std_tests`. Most of them are used only by the device/FS tests below, which have
    // RISC-V twins in `riscv_virtio_tests` and are gated to aarch64 here.
    #[cfg(target_arch = "aarch64")]
    use super::std_tests::{
        assert_a_kill_mid_transaction_recovers, assert_attrs, assert_fs_service_ready,
        assert_std_transcript, std_fs_expected,
    };
    use super::*;
    use crate::arch::exceptions::{SVC_COUNT, USER_FAULTS, last_user_fault};
    use crate::arch::{UserFault, UserFaultAccess, timer};
    use crate::sched;

    /// **The `hello` binary's ELF bytes**, pulled out of the initrd archive by name (milestone 19f).
    /// This is the binary carrying the milestone 7-19 role catalogue: the printing client, the
    /// untyped demo, the granter and receiver, the call server, the aspace builder, the init roles.
    /// A test that loads a real user program wants the program's bytes, not the whole crickerfs
    /// archive; only the `spawn_init` tests pass the archive, because init parses it itself.
    ///
    /// The archive name differs by ISA and that is the one place it shows. aarch64 packs hello as
    /// **`init`**, because on that ISA hello *is* the boot program. RISC-V's `init` is the portable
    /// `builder` demo, so hello is packed under its own name there. Both point at the same source
    /// file compiled for the local target.
    #[cfg(target_arch = "aarch64")]
    const HELLO_ENTRY: &str = "init";
    #[cfg(target_arch = "riscv64")]
    const HELLO_ENTRY: &str = "hello";

    fn init_image() -> &'static [u8] {
        program(HELLO_ENTRY).expect("no hello program in the initrd archive")
    }

    /// The `outlaw` program's ELF bytes: the two privilege-boundary behaviours that used to be
    /// hand-assembled aarch64 machine code in this file. See [`OUTLAW_ROUND_TRIP`].
    fn outlaw_image() -> &'static [u8] {
        program("outlaw").expect("no outlaw program in the initrd archive")
    }

    /// The `spinner` program's ELF bytes: a `_start` that is nothing but a loop. Milestone 24 built
    /// it for the shell's forcible-interrupt tier, and it is exactly the hostile binary DECISIONS §5
    /// describes, so the preemption test uses it rather than a second copy of the same idea.
    fn spinner_image() -> &'static [u8] {
        program("spinner").expect("no spinner program in the initrd archive")
    }

    /// **An address in the kernel's own memory: mapped, readable by the kernel, forbidden to
    /// userspace.** The address of a kernel static, rather than a per-ISA constant for the kernel's
    /// text base, because taking the address of something the kernel is demonstrably using is true
    /// on any ISA and any link layout, and needs no table of magic numbers to keep current.
    fn a_kernel_address() -> u64 {
        static SENTINEL: u64 = 0x00C0_FFEE_D00D;
        &raw const SENTINEL as u64
    }

    /// Run `image` at user mode with `arg0`/`arg1` and no authority at all: no capabilities, no
    /// extra mappings. It can run its own code and touch its own memory and name nothing else.
    fn spawn_bare(image: &'static [u8], arg0: u64, arg1: u64) -> Option<crate::thread::Tid> {
        sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0,
                    arg1,
                    arg2: 0,
                    grants: &[],
                    maps: &[],
                },
            )
        })
    }

    /// **Take back a thread from [`spawn_bare`] that will never exit on its own.**
    ///
    /// A bare thread belongs to no reclaimable region, so nothing tears it down when the test that
    /// started it returns. For a subject whose whole point is that it never exits (a spinner, or a
    /// child holding a measurement still), "the test ended" and "the thread ended" are unrelated
    /// events, and the thread just keeps running.
    ///
    /// That is not merely untidy. Two such threads spinning on a four-hart machine is a load every
    /// later test in the suite then runs under, and it starved
    /// `reclaim_frees_a_started_then_exited_childs_regions` into its watchdog on CI, intermittently,
    /// nowhere near the tests responsible. See `notes/riscv-parity-scope.md`.
    ///
    /// The kill is armed rather than immediate (DECISIONS §16), so this waits for the name to stop
    /// resolving instead of assuming the thread is gone on return. Returns whether it actually went;
    /// callers assert on it, because a silent failure here restores the leak this exists to remove.
    fn reap_bare(tid: crate::thread::Tid) -> bool {
        if !sched::kill_thread(tid) {
            // Already gone. Nothing to wait for, and not a failure.
            return true;
        }
        wait_for(|| !sched::thread_present(tid))
    }

    /// The `worker` program's ELF bytes (milestone 19f.2), a distinct binary in the archive, not a
    /// role of the init/hello binary. `_start(x0, x1, x2)` reads its input in `x1` and needs no
    /// role selector.
    fn worker_image() -> &'static [u8] {
        program("worker").expect("no worker program in the initrd archive")
    }

    /// The `net_stack` program's ELF bytes (milestone 30, piece 3): the smoltcp net server, a distinct
    /// binary loaded by name. Used only by the net tests, whose RISC-V twins live in
    /// `riscv_virtio_tests`.
    #[cfg(target_arch = "aarch64")]
    fn net_stack_image() -> &'static [u8] {
        program("net_stack").expect("no net_stack program in the initrd archive")
    }

    /// The net client's test selectors and its success word, matching `user/src/socket_test_client.rs`. The
    /// client is a nonzero entry role of the `net_stack` binary, so it needs no image of its own.
    #[cfg(target_arch = "aarch64")]
    const NET_TEST_UDP_DNS: u64 = 1;
    #[cfg(target_arch = "aarch64")]
    const NET_TEST_TCP_ECHO: u64 = 2;
    #[cfg(target_arch = "aarch64")]
    const NET_TEST_TCP_REOPEN: u64 = 3;
    #[cfg(target_arch = "aarch64")]
    const NET_TEST_UDP_TFTP: u64 = 4;
    #[cfg(target_arch = "aarch64")]
    const NET_CLIENT_OK: u64 = 1;
    /// The client could not complete for an ENVIRONMENTAL reason (the host resolver never answered),
    /// not because of a defect here. Only the non-gating real-DNS check can report it.
    #[cfg(target_arch = "aarch64")]
    const NET_CLIENT_NO_ANSWER: u64 = 2;

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
    ///
    /// **aarch64-only, and there is no RISC-V analogue to write.** RISC-V does not bank the stack
    /// pointer by privilege level at all: there is one `sp`, and the kernel swaps it with `sscratch`
    /// on the way into a trap. The hazard this test guards (two names for one register, silently)
    /// cannot exist there, so a RISC-V twin would have nothing to assert. See
    /// notes/riscv-parity-scope.md.
    #[cfg(target_arch = "aarch64")]
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

    /// User mode. The boundary.
    ///
    /// Two syscalls, not one, and that is the point: the second can only happen if the return from
    /// the first **put us back at EL0/U-mode**. One proves we left. Two prove we came back.
    #[test_case]
    fn a_user_program_reaches_el0_and_returns_twice() {
        let before = SVC_COUNT.load(Ordering::Relaxed);

        spawn_bare(outlaw_image(), OUTLAW_ROUND_TRIP, 0).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) >= before + 2),
            "saw {} syscalls from user mode, wanted 2",
            SVC_COUNT.load(Ordering::Relaxed) - before,
        );
    }

    /// **The privilege boundary is real, and it is a PERMISSION fault, not a missing page.**
    ///
    /// The address the user reaches for is mapped, and readable, and the kernel reads it all day.
    /// The precondition assertions below say exactly that before the program is even started,
    /// because without them the test is vacuous: "userspace cannot read this" proves nothing about
    /// a page nobody mapped.
    ///
    /// So a **permission** fault rather than a translation fault is the whole assertion. A
    /// translation fault would mean we had merely failed to map something, which would pass a
    /// sloppier test and prove nothing at all. Both ISAs assert it; only aarch64 is *told* it (see
    /// `arch::UserFault`, and the BUGS note on the RISC-V classifier).
    #[test_case]
    fn a_user_program_cannot_read_a_kernel_address() {
        let kernel_addr = a_kernel_address();

        // The precondition, and it is what gives the assertion below its teeth.
        assert!(
            mmu::translate(kernel_addr).is_some(),
            "the kernel's own static is not mapped, so this test proves nothing",
        );

        let before = USER_FAULTS.load(Ordering::Relaxed);

        spawn_bare(outlaw_image(), OUTLAW_READ_KERNEL, kernel_addr).expect("spawn failed");

        assert!(
            wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > before),
            "the user program read a kernel address and was NOT stopped",
        );

        let (kind, addr) = last_user_fault().expect("the kernel recorded no user fault");

        assert_eq!(
            kind,
            UserFault::Permission(UserFaultAccess::Read),
            "not a PERMISSION fault on a read: a translation fault would mean we had merely \
             failed to map something, which proves nothing about the privilege boundary",
        );
        assert_eq!(addr, kernel_addr, "faulted on the wrong address");

        // And the kernel is executing this line, which is the other half of the claim.
    }

    /// DECISIONS §5's arbitrary binary, at user mode, in the flesh.
    ///
    /// A program with no yield, no syscall, and not even a function call: `spinner`'s whole `_start`
    /// is a loop. The **only** thing in the universe that can take the CPU back from it is a timer
    /// interrupt landing between two of its instructions. Milestone 6 proved this for a kernel
    /// thread we compiled. This is the case that actually mattered.
    #[test_case]
    fn a_user_program_that_never_yields_is_preempted_anyway() {
        let preemptions = sched::preemptions();
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        let spinner = spawn_bare(spinner_image(), 0, 0).expect("spawn failed");

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

        // Now take it back for good. `spinner` never yields, never syscalls and never returns, so
        // nothing but an armed kill ends it, and leaving it running would spend a core for the rest
        // of the suite. The assertions above are already done, so the kill cannot weaken them.
        assert!(
            reap_bare(spinner),
            "the spinning user thread outlived its kill"
        );
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
        // This build's own machine, so the forgery gets past the machine check and reaches the
        // property under test rather than being refused for being foreign.
        out[18..20].copy_from_slice(&elf::NATIVE_MACHINE.to_le_bytes());
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
    /// This is the attack. An ELF names its own load address, so a hostile one simply names the
    /// base of the kernel's half and waits to see whether the loader is credulous.
    ///
    /// It is refused **by construction, not by a check we remembered to write**: the user
    /// `Mapper` is built with `Half::Low`, and a high address is not a thing it can express. The
    /// same `WrongHalf` guard has been in `paging` since milestone 4, put there because a *host*
    /// test discovered that bits 63:48 are not translated. It has been waiting for this file.
    ///
    /// The address is `KERNEL_VA_BASE` rather than a constant, which is what makes this the same
    /// attack on both ISAs: aarch64's kernel half starts at `0xffff_0000_0000_0000` and RISC-V's
    /// Sv39 kernel half at `0xffff_ffc0_0000_0000`, and the loader must refuse either.
    #[test_case]
    fn an_elf_that_asks_to_be_loaded_over_the_kernel_is_refused() {
        let image = forged_elf(mmu::KERNEL_VA_BASE, elf::PF_R | elf::PF_X);

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

    /// The initrd is there, and it is the program we built, for **this** machine: `Elf::parse`
    /// refuses a foreign `e_machine`, so parsing at all is half the assertion.
    #[test_case]
    fn the_initrd_holds_a_native_executable() {
        let image = init_image();
        let e = elf::Elf::parse(image).expect("the initrd is not a loadable native ELF");

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

    /// **The whole of 7c.** A separately compiled binary, arriving in the initrd, running at user
    /// mode.
    ///
    /// The program checks its own image and speaks with the only two words it has: a syscall if
    /// every expectation about its own memory holds, a trap instruction if not. **No data crosses
    /// the boundary**, because there is no ABI yet and we are not going to invent one by accident.
    ///
    /// So a syscall and no fault means: `.text` executed, `.rodata` was readable, `.data` was copied
    /// from the file, `.bss` was zeroed (the file does not contain those bytes), and the stack
    /// worked well enough to recurse eight frames. That is hello's `SELF_CHECK` role, which is the
    /// same source compiled for whichever machine this build is.
    #[test_case]
    fn a_real_elf_from_the_initrd_runs_at_el0_and_verifies_itself() {
        let svc = SVC_COUNT.load(Ordering::Relaxed);
        let faults = USER_FAULTS.load(Ordering::Relaxed);

        spawn_bare(init_image(), 0, 0).expect("spawn failed");

        assert!(
            wait_for(|| SVC_COUNT.load(Ordering::Relaxed) > svc),
            "the program never reached its syscall",
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
    ///
    /// **aarch64-only, and deliberately NOT ported, because the property is not true on RISC-V
    /// yet.** `riscv64::mmu::write_satp` issues an unconditional `sfence.vma` on every root switch,
    /// so that ISA still swings the sledgehammer: a twin of this test would read B's byte because
    /// everything was just flushed, not because the tagging works, and it would pass forever
    /// whatever happened to the ASIDs. That is a test converted into something that passes for the
    /// wrong reason, which is worse than no test. RISC-V does allocate and tag ASIDs (`asid_bits`
    /// probes the width, `ttbr0_value` packs it into `satp[59:44]`), so the flush is what makes them
    /// pointless there; dropping it is a kernel change with its own shootdown consequences, not a
    /// test-portability change. Recorded in notes/riscv-parity-scope.md as an open gap.
    #[cfg(target_arch = "aarch64")]
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
    /// non-executable segment as `user_data()`, which is **writable**: quietly granting the
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
    ///
    /// **aarch64-only because it already has a RISC-V twin, not because RISC-V cannot ask.**
    /// `riscv_virtio_tests::the_page_tables_say_u_mode_cannot_read_the_kernels_memory` asserts the
    /// same three things there. They are deliberately separate tests rather than one portable one,
    /// because the *mechanism* is what each is about: this one asks the silicon a question
    /// (`AT S1E0R`), and RISC-V has no such instruction, so its twin walks the tables in software
    /// and reads the `U` bit. Merging them would mean asserting only what both can say, which is
    /// less than either says now.
    #[cfg(target_arch = "aarch64")]
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
                    let dst = (&raw mut BUF).cast::<u8>();
                    for i in 0..len as usize {
                        // SAFETY: both pointers are in range; BUF is 128 bytes and len <= 128.
                        unsafe {
                            core::ptr::write_volatile(
                                dst.add(i),
                                core::ptr::read_volatile(src.add(i)),
                            );
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
        let got = unsafe { core::slice::from_raw_parts((&raw const BUF).cast::<u8>(), len) };
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
        // A real device's MMIO on this machine, whichever machine it is: the virtio-mmio bus base.
        // It was the PL011's `0x0900_0000`, which is an aarch64 `virt` fact; the point of the test
        // is that a device-typed mapping lands where it was asked to, and either address serves it.
        let device_phys = mmu::VIRTIO_MMIO_BASE;

        let mut space = AddressSpace::new(2).expect("no address space");
        let frame = crate::memory::alloc().expect("no frame").addr();

        space
            .map_physical(DATA_VA, frame, Flags::user_data())
            .expect("shared map failed");
        space
            .map_physical(DEV_VA, device_phys, Flags::user_device())
            .expect("device map failed");

        // SAFETY: nothing is at EL0; we are a kernel thread mid-test.
        unsafe { mmu::activate_user(space.ttbr0()) };

        let (data_pa, data_f) = mmu::translate_user(DATA_VA).expect("shared page not mapped");
        assert_eq!(data_pa, frame, "shared page maps the wrong frame");
        assert!(data_f.is_user_accessible() && data_f.is_writable());
        assert!(!data_f.is_user_executable());

        let (dev_pa, dev_f) = mmu::translate_user(DEV_VA).expect("device page not mapped");
        assert_eq!(
            dev_pa, device_phys,
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
    // RISC-V twin: `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_userspace_driver_reads_a_file_from_a_virtio_disk() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let Some(report) = virtio_service::start(init_image()) else {
            // No disk attached to this run. Nothing to test; do not fail.
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::std_fs_reads_a_file_through_a_granted_directory_capability`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn std_fs_reads_a_file_through_a_granted_directory_capability() {
        let Some((readiness, report)) = fs_service::start_std(
            init_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            std_exerciser_image(),
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::the_fs_server_serves_redoxfs_over_a_capability_contract`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn the_fs_server_serves_redoxfs_over_a_capability_contract() {
        let Some((readiness, report)) = fs_service::start(
            init_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
            0, // the end-to-end proof role, not the benchmark loop
        ) else {
            // No RedoxFS disk attached to this run. Nothing to test; do not fail.
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };

        // The two servers' readiness sentinels, if this test is the one that wired them (the
        // `std::fs` test shares the same service, and each sentinel is sent exactly once).
        assert_fs_service_ready(readiness);

        // Then: the client has read motd, round-tripped scratch, exercised extended attributes, and
        // reported. If any of the three processes faults, it never sends and the QEMU-level timeout
        // is the backstop.
        let [head, status, attrs, ..] = sched::ipc_recv(report);
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
        assert_attrs(attrs);
    }

    /// **A read-only per-file grant, attacked** (milestone 31 phase 2, notes/grant-expression.md).
    ///
    /// `run wc report.txt` must hand over one file, not the directory it lives in. This wires
    /// exactly that: an `fs_file_caretaker` holding the directory capability, a confined program
    /// holding only the caretaker's endpoint, and a grant of `motd`, read-only. The program is the
    /// attacker role of `fs_test_client`, and it spends its life trying to make that sentence false.
    ///
    /// **What makes it a witness and not a formality.** Every attempt is against something that
    /// really exists and that the process one hop up the chain can really reach: `scratch` is on
    /// the image, one directory entry away, and the caretaker could open it on any request.
    /// Milestone 33's attacker was handed a real neighbour's address rather than a fictional one
    /// for the same reason. And this test alone would still be weak, because a caretaker that
    /// refused *everything* would pass it; that is what the writable twin below is for, and why the
    /// verdict is a bitmap rather than a boolean.
    // RISC-V twin: `riscv_virtio_tests::a_read_only_per_file_grant_survives_an_attacker`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_read_only_per_file_grant_survives_an_attacker() {
        let Some(verdict) = attack_a_grant(fs_proto::grant::READ, false) else {
            return;
        };
        assert_eq!(
            verdict,
            0,
            "the read-only per-file grant leaked: {}",
            describe_escape(verdict),
        );
    }

    /// **A read-only per-file grant carries its file's attributes, and cannot write them**
    /// (milestone 61).
    ///
    /// It is the same run as the test above, read a second way, which is why it costs nothing: a
    /// clean verdict of zero already says both halves. `GRANTED_ATTRS_FAILED` clear means the
    /// listing and the get went **through** the caretaker to the store, which they could not do
    /// before this milestone (all four attribute verbs answered `EOPNOTSUPP`); `WROTE_ATTR` clear
    /// means the set did not, because a read-only grant must not forward one.
    ///
    /// Stated as its own assertion rather than left inside the zero, because "the verdict was zero"
    /// does not tell a reader which properties were in it, and the whole point of a bitmap is that
    /// each bit is a sentence. The writable twin below is what stops this passing vacuously.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_read_only_per_file_grant_reads_its_files_attributes_and_writes_none() {
        use fs_proto::fixture::escape;
        let Some(verdict) = attack_a_grant(fs_proto::grant::READ, false) else {
            return;
        };
        assert_eq!(
            verdict & escape::GRANTED_ATTRS_FAILED,
            0,
            "the caretaker did not forward the attribute reads, so a program behind a per-file \
             grant still cannot reach its own file's attributes",
        );
        assert_eq!(
            verdict & escape::WROTE_ATTR,
            0,
            "a read-only grant forwarded SETXATTR: an attribute is a way to change a file",
        );
    }

    /// **A writable per-file grant, attacked** (the second witness, and the first one's control).
    ///
    /// Same caretaker, same attacker, same neighbouring file; only the granted direction changes.
    /// Two things fall out of it, and the second is why it exists:
    ///
    /// - A writable file capability really does write, and still reaches **only** its one file. The
    ///   widening is exactly one axis wide.
    /// - The read-only test above is now meaningful. Its refusals are a narrowed capability rather
    ///   than a caretaker that says no to everything, because here the same requests, through the
    ///   same code, succeed. A confinement test with no witness that the thing being confined
    ///   *works* is a test that passes when the feature is missing entirely.
    // RISC-V twin: `riscv_virtio_tests::a_writable_per_file_grant_writes_that_file_and_still_only_that_file`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_writable_per_file_grant_writes_that_file_and_still_only_that_file() {
        use fs_proto::fixture::escape;
        let Some(verdict) = attack_a_grant(fs_proto::grant::READ | fs_proto::grant::WRITE, true)
        else {
            return;
        };
        // `WROTE_ATTR` joined the expected set in milestone 61, and it is the third way to change a
        // file: bytes, length, and what is attached to it. A direction check that covered only the
        // first two would have left one open.
        let expected = escape::WROTE | escape::TRUNCATED | escape::WROTE_ATTR;
        assert_eq!(
            verdict,
            expected,
            "a writable grant must write, truncate and set an attribute on its own file and do \
             nothing else: {}",
            describe_escape(verdict & !expected),
        );
    }

    /// Wire a per-file grant of the given direction, run the attacker against it, and return its
    /// verdict bitmap. `None` when no RedoxFS disk is attached (nothing to test; do not fail).
    #[cfg(target_arch = "aarch64")]
    fn attack_a_grant(rights: u64, writable: bool) -> Option<u64> {
        let Some(report) = fs_service::start_granted(
            init_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_file_caretaker")
                .expect("no fs_file_caretaker program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
            fs_service::Grant {
                // The writable run damages what it is granted, so it is granted the file the fixture
                // discipline already covers; see the attacker's own note.
                name: if writable {
                    fs_proto::fixture::SCRATCH_NAME
                } else {
                    fs_proto::fixture::MOTD_NAME
                },
                rights,
                role: 2, // ROLE_ATTACKER
                arg: writable as u64,
            },
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return None;
        };
        // The two handshakes happened inside `start_granted`, before this attacker existed: they
        // are what makes the caretaker's own staged request safe on a page all three share.
        let [tag, verdict, ..] = sched::ipc_recv(report);
        assert_eq!(
            tag,
            fs_proto::fixture::VERDICT,
            "the attacker's report is not a verdict word",
        );
        Some(verdict)
    }

    /// Name the bits an escape verdict set, so a failure reads as a sentence instead of a bitmap.
    #[cfg(target_arch = "aarch64")]
    fn describe_escape(v: u64) -> &'static str {
        use fs_proto::fixture::escape;
        if v & escape::SECOND_FILE != 0 {
            "it opened a file the grant does not designate"
        } else if v & escape::WROTE != 0 {
            "it wrote through a read-only grant"
        } else if v & escape::TRUNCATED != 0 {
            "it truncated through a read-only grant"
        } else if v & escape::CREATED != 0 {
            "it created a file through a file capability"
        } else if v & escape::FORGED_HANDLE != 0 {
            "it reached a file with a handle it was never given"
        } else if v & escape::GRANTED_READ_FAILED != 0 {
            "the granted read itself failed, so nothing above was actually proven"
        } else if v & escape::WROTE_ATTR != 0 {
            "it set an extended attribute through a read-only grant"
        } else if v & escape::GRANTED_ATTRS_FAILED != 0 {
            "it could not reach its own file's attributes, which milestone 61 says it carries"
        } else {
            "nothing (an empty verdict should not have failed an assertion)"
        }
    }

    /// **The FS server's stack is sized by measurement, and this is the measurement.**
    ///
    /// Runs after both FS clients, so the poison in the server's stack pages has been overwritten to
    /// exactly the depth RedoxFS reached across a mount, reads, writes, a create and two truncates.
    /// It prints the number and fails if less than a quarter of the grant is left.
    ///
    /// This exists because the previous size was a guess, and the guess was **528 bytes short**.
    /// Milestone 31 phase 2's `CREATE` and `TRUNCATE` added one more level of tree recursion, the FS
    /// server ran off the bottom of its stack mid-request, and the kernel killed it, correctly and
    /// legibly. What was not legible was anything downstream: the std client sat blocked on a `CALL`
    /// nobody would ever answer, and since other tests had left processes spinning on other cores,
    /// the no-progress heartbeat saw a healthy system. The only instrument that fired was the
    /// per-test wall-clock ceiling, so a 368-byte overflow presented as "`std_fs` takes 914 seconds".
    /// A number nobody can defend is a number that will be wrong again; this one now has a witness.
    /// **A kill mid-transaction, on the real device** (milestone 37, DECISIONS §34 condition 1).
    /// The host sweep proves the property over every fault point against a reconstructed platter;
    /// this proves it once through the whole stack, with a real virtio write torn in half, a real
    /// FS-server process killed inside its own transaction, and a real second process recovering the
    /// disk it left behind. See `std_tests::assert_a_kill_mid_transaction_recovers`.
    // RISC-V twin: `riscv_virtio_tests::a_kill_mid_transaction_leaves_the_filesystem_consistent`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_kill_mid_transaction_leaves_the_filesystem_consistent() {
        assert_a_kill_mid_transaction_recovers(
            init_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        );
    }

    // RISC-V twin: `riscv_virtio_tests::the_fs_servers_stack_still_has_headroom`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn the_fs_servers_stack_still_has_headroom() {
        let Some((used, total)) = fs_service::fs_stack_used() else {
            crate::println!("    (no FS service wired this boot; skipping)");
            return;
        };
        crate::println!("    (FS server stack high-water: {used} of {total} bytes) ");
        assert!(
            used * 4 <= total * 3,
            "the FS server used {used} of {total} stack bytes: under a quarter left. RedoxFS \
             recurses in 8 KiB frames, so the next verb that deepens a tree walk will overflow and \
             the server will die mid-request. Raise FS_STACK_PAGES.",
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
    // RISC-V twin: `riscv_virtio_tests::a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net() {
        let Some(report) = virtio_service::start_net(init_image()) else {
            // No NIC on this run (a bare boot). The test runners always attach one, so this
            // branch is not the parity gate. See scripts/qemu-runner*.sh (CRICKER_NET).
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net_pci`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net_pci() {
        let Some(report) = virtio_service::start_net_pci(init_image()) else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::the_net_server_acquires_a_dhcp_lease_over_smoltcp`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn the_net_server_acquires_a_dhcp_lease_over_smoltcp() {
        let Some(report) = virtio_service::start_net_server(net_stack_image()) else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::the_net_server_acquires_a_dhcp_lease_over_smoltcp_pci`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn the_net_server_acquires_a_dhcp_lease_over_smoltcp_pci() {
        let Some(report) = virtio_service::start_net_server_pci(net_stack_image()) else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
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
    /// it, opens a UDP socket by id, sends a datagram, and reads the reply back through the same
    /// frame. No ambient network: the client acts only through the capability it was granted, and the
    /// bytes cross in the shared frame, never in a message. Proves the whole path, client to `net_stack` to
    /// smoltcp to the confined NIC, over the mmio transport.
    ///
    /// The peer is **slirp's own TFTP server** (10.0.2.2:69), served inside libslirp, so the exchange
    /// is deterministic and never leaves the emulator. This test used to query 10.0.2.3:53, which
    /// reads like a local resolver but is not one: libslirp NATs that address to the *host's*
    /// nameserver, so the gate depended on the developer's DNS answering at that instant and flaked
    /// (~2.5% per query, measured). The real-resolution case still runs, non-gating, below.
    // RISC-V twin: `riscv_virtio_tests::a_client_completes_a_udp_round_trip_through_the_socket_contract`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_client_completes_a_udp_round_trip_through_the_socket_contract() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_UDP_TFTP, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP round trip against slirp's TFTP server failed (client code {verdict:#x})",
        );
    }

    /// The same UDP round trip over the PCIe transport, behind the IOMMU.
    // RISC-V twin: `riscv_virtio_tests::a_client_completes_a_udp_round_trip_through_the_socket_contract_pci`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_client_completes_a_udp_round_trip_through_the_socket_contract_pci() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_UDP_TFTP, true)
        else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP round trip over PCIe failed (client code {verdict:#x})",
        );
    }

    /// **Real DNS resolution, deliberately non-gating.** The query goes to 10.0.2.3, which libslirp
    /// NATs to the host's configured nameserver (`get_dns_addr_libresolv`), so whether it is answered
    /// is a fact about the developer's machine, not about this kernel. The client retries like any
    /// resolver client and reports `NO_ANSWER` if the host never replied, which we print and skip: a
    /// committed gate must not depend on somebody's router. What still fails loudly is a response
    /// that arrives and is *wrong* (not our transaction id, or not a response), because that would be
    /// our defect. The deterministic UDP coverage is the TFTP pair above. See notes/net.md.
    // RISC-V twin: `riscv_virtio_tests::a_client_resolves_a_real_dns_name_when_the_host_resolver_answers`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_client_resolves_a_real_dns_name_when_the_host_resolver_answers() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_UDP_DNS, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        if verdict == NET_CLIENT_NO_ANSWER {
            crate::println!(
                "    (the host's resolver did not answer; real-DNS check skipped, not a failure)"
            );
            return;
        }
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "a DNS response came back but was not a valid reply to our query (client code \
             {verdict:#x}): a socket-contract defect, not a network problem",
        );
    }

    /// **The socket contract, TCP end to end** (milestone 30, piece 3 phase B). A client opens a TCP
    /// socket by id, connects to slirp's guestfwd echo peer (10.0.2.9:7777, piped to `/bin/cat`),
    /// sends a payload, receives the echo, and closes. The full round trip, handshake through
    /// bidirectional data to teardown, deterministic and zero-host-setup (nothing outlives QEMU),
    /// through the client, `net_stack`, smoltcp, and the confined NIC.
    // RISC-V twin: `riscv_virtio_tests::a_client_echoes_over_tcp_through_the_socket_contract`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_TCP_ECHO, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the TCP echo round trip through the socket contract failed (client code {verdict:#x})",
        );
    }

    /// The same TCP echo round trip over the PCIe transport, behind the IOMMU.
    // RISC-V twin: `riscv_virtio_tests::a_client_echoes_over_tcp_through_the_socket_contract_pci`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract_pci() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_TCP_ECHO, true)
        else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the TCP echo round trip over PCIe failed (client code {verdict:#x})",
        );
    }

    /// **Regression: reusing a socket id is safe** (the ephemeral-port fix). A client opens a TCP
    /// socket on id 0, connects to the echo peer, closes it, then reopens the same id and connects
    /// again. `net_stack` derived the local port from the socket id, so the reopen reused the exact port and
    /// the second connect stalled on a slirp flow that had not cleared; the rotating allocator hands
    /// the reopen a fresh port, so both connects complete. The client reports OK only if they do.
    // RISC-V twin: `riscv_virtio_tests::a_reopened_socket_id_connects_again_over_tcp`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_reopened_socket_id_connects_again_over_tcp() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_TCP_REOPEN, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "reopening a socket id and connecting again failed (client code {verdict:#x}): the \
             ephemeral local port is not independent of the socket id",
        );
    }

    /// The `std_exerciser` std program's ELF bytes. The same binary the offline std test spawns; given
    /// the network here, its `UdpSocket::bind` probe succeeds and it runs the net transcript.
    #[cfg(target_arch = "aarch64")]
    fn std_exerciser_image() -> &'static [u8] {
        program("std_exerciser").expect("no std_exerciser program in the initrd archive")
    }

    /// The exact transcript `std_exerciser` prints when it is granted the network. Pinned so a drift in
    /// the net PAL, the contract, or the demo is a loud diff rather than a mystery.
    #[cfg(target_arch = "aarch64")]
    const STD_NET_EXPECTED: &[u8] = b"std net on cricker-os\nudp ok\ntcp echo ok\n";

    /// **`std::net` end to end over the socket contract** (milestone 27 phase two): the `std_exerciser`
    /// std binary, given the network, does a real UDP DNS query and a TCP echo round trip through
    /// `std::net::{UdpSocket, TcpStream}`, whose PAL binds to `net_stack`'s contract. The program never
    /// sees a capability or a socket id; it writes to a socket and reads from it. This closes the
    /// `net honestly unsupported` gap from phase one: std's networking runs on the native ABI,
    /// reaching the same path the hand-written client does through std's blocking API. Its stdout
    /// is reassembled off the endpoint and compared byte for byte, the `std_exerciser` discipline.
    // RISC-V twin: `riscv_virtio_tests::std_net_runs_over_the_socket_contract`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn std_net_runs_over_the_socket_contract() {
        let Some(report) = virtio_service::start_net_std(net_stack_image(), std_exerciser_image())
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };

        assert_std_transcript(report, STD_NET_EXPECTED, "std net");
    }

    /// **The shell's `run` mechanism: spawn a process, get its answer.** Milestone 10's core.
    ///
    /// A worker process is started at EL0 with an argument, computes `n*n`, reports the result on
    /// an endpoint it was handed, and exits. The whole lifecycle a shell drives when you type
    /// `run n`, minus the interactive loop, which is exercised by the piped demo instead.
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
        let (region, report, demo) = untyped_service::start(init_image(), PAGES)
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

        // Every measurement above is taken. The demo spins to hold the free-frame count still while
        // it is read, which is the reason it does not exit on its own, and also the reason it has to
        // be ended here: past this point it is a spinning thread with nothing left to hold.
        assert!(reap_bare(demo), "the untyped demo outlived its kill");
    }

    /// **The DMA-confinement fix, end to end.** A malicious driver at EL0 holds a real `Virtio`
    /// capability and its own DMA region, and points a descriptor at the kernel's image, asking
    /// the device to write there. Because the device has no IOMMU, this would succeed if the
    /// driver could ring it directly. The kernel validates every descriptor on submit and refuses
    /// this one, so the device is never told to go and never touches the kernel. The driver
    /// reports `1` when it was refused.
    // RISC-V twin: `riscv_virtio_tests::the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region() {
        let Some(report) = virtio_service::start_attacker(init_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::the_kernel_refuses_an_indirect_descriptor_escape`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn the_kernel_refuses_an_indirect_descriptor_escape() {
        let Some(report) = virtio_service::start_attacker_indirect(init_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::a_userspace_driver_reads_a_file_over_the_pcie_transport`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_userspace_driver_reads_a_file_over_the_pcie_transport() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let Some(report) = virtio_service::start_pci(init_image()) else {
            crate::println!("    (no virtio-pci disk on the bus; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::a_userspace_driver_writes_a_block_and_reads_it_back`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_userspace_driver_writes_a_block_and_reads_it_back() {
        let Some(report) = virtio_service::start_writer(init_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::a_userspace_driver_writes_a_block_over_the_pcie_transport`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_userspace_driver_writes_a_block_over_the_pcie_transport() {
        let Some(report) = virtio_service::start_writer_pci(init_image()) else {
            crate::println!("    (no virtio-pci disk on the bus; skipping)");
            return;
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
    // RISC-V twin: `riscv_virtio_tests::a_driver_killed_mid_write_leaves_the_device_and_transport_sane`. Gated here rather than run twice: that
    // module drives the same property through the dedicated `blk`/`net_stack` binaries, and a
    // second copy through hello's roles would double the suite's slowest tests to prove
    // nothing new. See this module's comment on the two kinds of gate.
    #[cfg(target_arch = "aarch64")]
    #[test_case]
    fn a_driver_killed_mid_write_leaves_the_device_and_transport_sane() {
        let faults = USER_FAULTS.load(Ordering::Relaxed);
        let Some(report) = virtio_service::start_write_abandoner(init_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
        let image = outlaw_image();
        let kernel_addr = a_kernel_address();
        let outlaw_here = move || {
            sched::spawn_on(here, move || {
                run(
                    image,
                    Spawn {
                        arg0: OUTLAW_READ_KERNEL,
                        arg1: kernel_addr,
                        arg2: 0,
                        grants: &[],
                        maps: &[],
                    },
                )
            })
        };

        let f0 = USER_FAULTS.load(Ordering::Relaxed);
        outlaw_here().expect("spawn failed");
        assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f0));
        assert!(wait_for(|| sched::thread_count() <= baseline));

        // Sample the baseline only once `used()` has STOPPED MOVING, for the same reason the
        // assertion below waits rather than reading instantly, applied to the other end. The warm-up
        // outlaw's address space is freed by `finish_switch` on whatever core actually ran it, which
        // under §28 placement need not be this one, and that free lands a beat *after*
        // `thread_count` falls. Sampling `before` inside that window captures frames that are about
        // to come back, `used()` then settles BELOW `before`, and the wait for equality can never
        // succeed. The failure says so plainly when it happens: it reported "-18 frames did not come
        // back", a NEGATIVE leak, which no real leak can produce. Found when an unrelated change to
        // the std::fs test shifted this test's timing; the race was already here.
        // Two agreeing samples a yield apart mean nothing is in flight. Bounded by `wait_for`'s own
        // deadline, so a genuinely unstable allocator fails the test rather than spinning here.
        let mut last = used();
        let settled = wait_for(|| {
            sched::yield_now();
            let now = core::mem::replace(&mut last, used());
            now == last
        });
        assert!(
            settled,
            "frame accounting never settled before the baseline"
        );
        let before = last;

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
    ///
    /// **aarch64-only, because RISC-V has no second interrupt to raise.** It has no
    /// software-generated interrupt a test can assert on itself at all (the SBI IPI arrives down the
    /// software-interrupt arm and never reaches `irq_route`), so the only line it can raise by hand
    /// is the console UART's own, which `spawn_init` is already routing for the input driver init
    /// builds. A twin would have to share that one source between init's UART capability and the
    /// test's delegated one, and would then prove delivery through whichever route was bound last
    /// rather than through the delegated capability, which is the entire claim. The *property*
    /// (an interrupt arriving as a message through a delegated Irq cap) is proved on RISC-V by
    /// `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk`, which asserts
    /// `ROUTED_IRQS` rises while a userspace driver waits on its own Irq cap, and by
    /// `sched::tests::an_interrupt_becomes_a_message`. See notes/interrupts.md.
    #[cfg(target_arch = "aarch64")]
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
    ///
    /// **aarch64-only, because the assertion is a PL011 register and RISC-V `virt` has no PL011.**
    /// `0xB105F00D` in the PrimeCell identification registers is what makes this test exact rather
    /// than "the read did not fault"; the NS16550 on the other machine has no equivalent constant to
    /// name, and swapping in a virtio magic number would be a different test wearing this one's
    /// name. Device delegation to a userspace driver *is* proved on RISC-V, by
    /// `riscv_virtio_tests::a_userspace_driver_reads_a_file_from_a_virtio_disk`, which is a stronger
    /// version of the same claim (device MMIO, a DMA region, and an interrupt, all delegated).
    #[cfg(target_arch = "aarch64")]
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
        // The child's program: SEND(slot 0, endpoint::SEND, REPORT_WORD) then EXIT, nine
        // instructions, with the child's first granted cap in slot 0. This file used to carry three
        // separate aarch64 copies of it; `supervision_tests` already keeps one pair (aarch64 and
        // RISC-V) for its own children, so all three now share that pair. Same shape, one definition,
        // and the tests below run on either machine.
        let code = super::supervision_tests::REPORT_STUB;
        let expect_word = super::supervision_tests::REPORT_WORD;

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
        sync_icache(mmu::phys_to_virt(code_phys), size_of_val(code));
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
            got, expect_word,
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
        // SEND(slot 0, endpoint::SEND, REPORT_WORD) then EXIT, the shared stub (see the test above).
        let code = super::supervision_tests::REPORT_STUB;
        let expect_word = super::supervision_tests::REPORT_WORD;

        // The report endpoint is created before the baseline: it lives in the kernel's own pinned
        // endpoint region (never reclaimed here; endpoint revocation is a later piece), so it must
        // not count against the frame accounting.
        let report = crate::sched::create_endpoint();
        let frames_before = crate::memory::free_frames();

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
        sync_icache(mmu::phys_to_virt(code_phys), size_of_val(code));
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
        assert_eq!(got, expect_word, "the child never reported");

        // Let the reaper collect the now-Finished child. A Finished thread is removed when its own
        // core switches away from it, and DECISIONS §28's placement can have put this child on
        // ANOTHER core, so yielding on THIS core cannot make that happen: a hundred cheap yields
        // complete long before the remote core's next timer tick. So wait on the clock, not on a
        // yield count. Still a leak trap rather than a masked failure: a child that is never reaped
        // times out and fails; only cross-core reap lag is tolerated.
        //
        // **And wait on this child, not on a headcount.** This asked whether `thread_count()` had
        // returned to a baseline sampled at the top of the test, which is the size of the WHOLE
        // thread table: the previous test's processes are still tearing down at that instant, so the
        // baseline was a number the system would move on its own, and the wait was really waiting
        // for everything else to hold still. It failed exactly that way once on RISC-V, where the
        // slower machine leaves more teardown in flight. `thread_present` asks the question the test
        // means. The whole history here is a wait that keeps being written against something wider
        // than the property: it was a yield count until §28's scattering broke it, then a
        // clock-bounded headcount until this. A sibling wait below had the same defect.
        assert!(
            wait_for(|| !crate::sched::thread_present(tid)),
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
    /// leak; the real magnitudes wait on the EL0 `lat_proc` benchmark.
    #[test_case]
    fn spawn_to_reap_repeats_without_leaking() {
        const CODE_VA: u64 = 0x40_0000;
        const STACK_VA: u64 = 0x50_0000;
        let code = super::supervision_tests::REPORT_STUB;
        let expect_word = super::supervision_tests::REPORT_WORD;

        let report = crate::sched::create_endpoint();
        let baseline = crate::memory::free_frames();

        for round in 0..6 {
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
            sync_icache(mmu::phys_to_virt(code_phys), size_of_val(code));
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
                expect_word,
                "round {round}: the child never reported"
            );
            // On the clock, not on yields, and on THIS child rather than on a headcount, both for
            // the reasons spelled out in the test above: §28 can place the child on another core and
            // only that core's switch reaps it, and `thread_count()` is the size of the whole table,
            // so waiting for it to return to a baseline is waiting for the rest of the system.
            // A lagging reap here would surface as the reclaim below refusing a live thread.
            assert!(
                wait_for(|| !crate::sched::thread_present(tid)),
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
    /// kernel at spawn. See user/src/hello.rs and `user::delegation_service`.
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
    /// user/src/hello.rs and `user::frame_service`.
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

    /// Pages in `root_supervisor`'s construction budget. It builds two servers out of this, splits the
    /// spawner's budget from it, and then deletes it; the spawner's split is the only memory the tree
    /// spends afterwards.
    const ROOT_BUDGET_PAGES: u64 = 1024;

    /// **Spawn the tree's root the way the kernel spawns init**, and return the report endpoint every
    /// process in the tree holds a WRITE view of.
    ///
    /// Deliberately the same endowment `spawn_init` gives (`INITRD_VA`, an untyped in slot 0, a report
    /// endpoint in slot 1) so what is being tested is `root_supervisor`'s *choices*, not a privileged shortcut.
    fn spawn_tree() -> sched::EpId {
        let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
        let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
        let bytes =
            program("root_supervisor").expect("no root_supervisor program in the initrd archive");
        let elf = Elf::parse(bytes).expect("root_supervisor is not loadable");

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
        let mut space = AddressSpace::new(content).expect("no memory for root_supervisor");
        map_segments(&mut space, &elf).expect("could not lay out root_supervisor");
        for k in 0..INIT_STACK_PAGES {
            space
                .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
                .expect("could not map root_supervisor's stack");
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
        let aspace = readopt_user_aspace(space).expect("register the root_supervisor aspace");

        let report = sched::create_endpoint();
        let budget =
            crate::untyped::create(ROOT_BUDGET_PAGES).expect("no budget for root_supervisor");
        let tcb_region = crate::untyped::create(2).expect("no tcb region");
        let tid = sched::create_tcb(tcb_region).expect("no tcb");
        let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
            .expect("insert budget");
        assert_eq!(s0, 0, "root_supervisor's budget must land in slot 0");
        let s1 = sched::tcb_insert_cap(
            tid,
            crate::cap::endpoint_cap(
                report,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            ),
            None,
        )
        .expect("insert report");
        assert_eq!(
            s1, 1,
            "root_supervisor's report endpoint must land in slot 1"
        );
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
    /// `root_supervisor` builds its two servers, deletes the wiring capabilities and then the untyped budget
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
mod c_seam_tests {
    use super::*;
    use crate::sched;

    /// The report protocol, matching `crates/c_seam`. Userspace owns the definition; the test
    /// mirrors it, the same convention `authority_tests` and the net client's selectors follow.
    const RPT_RAN: u64 = 1;
    const RPT_DEATH: u64 = 2;
    const RPT_SITE: u64 = 3;
    const RPT_VERDICT: u64 = 4;
    const RPT_FAILED: u64 = 9;

    /// The verdict bits, matching `crates/c_seam`'s `checks` module.
    const IN_GRANT_WRITE_LANDED: u64 = 1 << 0;
    const WITNESS_RO_INTACT: u64 = 1 << 1;
    const WITNESS_FAR_INTACT: u64 = 1 << 2;
    const FAULT_ADDR_AS_EXPECTED: u64 = 1 << 3;
    const OUTPUT_CORRECT: u64 = 1 << 4;

    /// What a run of one faulting attempt must report: the component's own store landed, both
    /// witnesses survived, and the fault was where the C code pointed.
    const CONFINED: u64 =
        IN_GRANT_WRITE_LANDED | WITNESS_RO_INTACT | WITNESS_FAR_INTACT | FAULT_ADDR_AS_EXPECTED;
    /// What the honest attempt must report: all of the above, plus a correct answer.
    const CONFINED_AND_CORRECT: u64 = CONFINED | OUTPUT_CORRECT;

    /// The attempts, matching `crates/c_seam`.
    const ATTEMPTS: usize = 3;
    const ATTEMPT_HONEST: u64 = 2;

    /// The confiner's budget. It builds three instances of the shim, one at a time, reaping each
    /// before the next, so this covers one instance region plus its own scratch mappings rather
    /// than three.
    const CONFINER_BUDGET_PAGES: u64 = 512;

    /// Four reports per attempt (ran, death, site, verdict).
    const EXPECTED_REPORTS: usize = 4 * ATTEMPTS;

    /// **Spawn the confiner the way the kernel spawns init**, and return the report endpoint every
    /// process in the run holds a WRITE view of. Deliberately the same endowment `spawn_init` gives
    /// (the archive read-only at `INITRD_VA`, an untyped in slot 0, a report endpoint in slot 1), so
    /// what is under test is the seam rather than a privileged shortcut.
    fn spawn_confiner() -> sched::EpId {
        let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
        let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
        let bytes = program("c_confiner").expect("no c_confiner program in the initrd archive");
        let elf = Elf::parse(bytes).expect("c_confiner is not loadable");

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
        let mut space = AddressSpace::new(content).expect("no memory for c_confiner");
        map_segments(&mut space, &elf).expect("could not lay out c_confiner");
        for k in 0..INIT_STACK_PAGES {
            space
                .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
                .expect("could not map c_confiner's stack");
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
        let aspace = readopt_user_aspace(space).expect("register the c_confiner aspace");

        let report = sched::create_endpoint();
        let budget =
            crate::untyped::create(CONFINER_BUDGET_PAGES).expect("no budget for c_confiner");
        let tcb_region = crate::untyped::create(2).expect("no tcb region");
        let tid = sched::create_tcb(tcb_region).expect("no tcb");
        let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
            .expect("insert budget");
        assert_eq!(s0, 0, "c_confiner's budget must land in slot 0");
        let s1 = sched::tcb_insert_cap(
            tid,
            crate::cap::endpoint_cap(
                report,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            ),
            None,
        )
        .expect("insert report");
        assert_eq!(s1, 1, "c_confiner's report endpoint must land in slot 1");
        sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
        sched::start_tcb(tid, [0, initrd_len, 0]).expect("start");
        report
    }

    /// **Run one confiner from spawn to quiescence**, returning every report it made.
    ///
    /// Run to the end rather than stopping at the first interesting message, for the reason
    /// `authority_tests::run_tree` records: a half-run harness keeps building processes in the
    /// background, and a test that leaves work running is a test that fails somebody else.
    fn run_seam() -> [[u64; 5]; EXPECTED_REPORTS] {
        let report = spawn_confiner();
        let mut msgs = [[0u64; 5]; EXPECTED_REPORTS];
        for slot in msgs.iter_mut() {
            let msg = sched::ipc_recv(report);
            assert_ne!(
                msg[0], RPT_FAILED,
                "the C seam harness could not be built: stage {}. Stages 1-2 are the archive and \
                 the c_shim ELF, 3-6 the shared pages, 7 the supervision endpoint, 10-13 building, \
                 starting, and reaping an instance.",
                msg[1],
            );
            *slot = msg;
        }

        // Let it settle, then prove there is nothing more to say. A parked sender here means a
        // thirteenth report exists, which is how "the supervisor did not restart the instance that
        // finished" is proven without a blocking receive that would hang when the code is right.
        for _ in 0..400 {
            sched::yield_now();
        }
        assert_eq!(
            sched::endpoint_waiting_senders(report),
            0,
            "the run made more than {EXPECTED_REPORTS} reports: the supervisor acted after the \
             honest attempt finished",
        );
        msgs
    }

    /// Every report of one kind, in arrival order.
    fn of_kind(msgs: &[[u64; 5]; EXPECTED_REPORTS], kind: u64) -> impl Iterator<Item = &[u64; 5]> {
        msgs.iter().filter(move |m| m[0] == kind)
    }

    /// **A C out-of-bounds write faults the process and touches nothing outside its grant.** The
    /// milestone's one load-bearing test.
    ///
    /// Two bugs, two fault paths, two witnesses. The off-by-one store lands on a page mapped
    /// read-only into the component and read/write into the confiner: same physical memory, present
    /// in the offender's page tables, and provably unchanged, which is the strongest form the claim
    /// can take. The wild store a page further out lands on a virtual address the component has no
    /// mapping for and the confiner does: a different frame, the same number, and unchanged.
    ///
    /// The verdict bitmap is asserted for **equality**, not for containing the interesting bits,
    /// because a missing bit is exactly what a broken confinement looks like and a superset would
    /// mean the checker started answering a question nobody asked.
    #[test_case]
    fn a_c_out_of_bounds_write_faults_and_changes_nothing_outside_its_grant() {
        let msgs = run_seam();

        let mut verdicts = of_kind(&msgs, RPT_VERDICT);
        for attempt in 0..2u64 {
            let v = verdicts
                .next()
                .unwrap_or_else(|| panic!("no verdict for attempt {attempt}"));
            assert_eq!(v[1], attempt, "verdicts arrived out of order");
            assert_eq!(
                v[2],
                CONFINED,
                "attempt {attempt}: the C component was not confined. Bits that should be set and \
                 are not: in-grant store landed {}, read-only witness intact {}, unmapped witness \
                 intact {}, fault address as expected {}.",
                v[2] & IN_GRANT_WRITE_LANDED != 0,
                v[2] & WITNESS_RO_INTACT != 0,
                v[2] & WITNESS_FAR_INTACT != 0,
                v[2] & FAULT_ADDR_AS_EXPECTED != 0,
            );
        }

        // The deaths themselves: a real fault, not a cooperative exit, and a tid the supervisor can
        // trust because the kernel is the only sender on this path (§26.5).
        let mut deaths = of_kind(&msgs, RPT_DEATH);
        for attempt in 0..2u64 {
            let d = deaths
                .next()
                .unwrap_or_else(|| panic!("attempt {attempt} produced no death message"));
            assert_eq!(
                d[2],
                abi::fault::EVENT_FAULT,
                "attempt {attempt} did not fault: the out-of-bounds write was tolerated, or the \
                 process exited some other way",
            );
            assert_ne!(
                d[1], 0,
                "attempt {attempt}'s fault message carried no tid: the supervisor cannot tell who \
                 died",
            );
        }

        // And the fault sites are real addresses in real code, not zeroed placeholders. The
        // *equality* to the intended address is checked inside the confiner (which knows the
        // layout); this is the sanity check that the kernel filled both words at all.
        let mut sites = of_kind(&msgs, RPT_SITE);
        for attempt in 0..2u64 {
            let s = sites
                .next()
                .unwrap_or_else(|| panic!("attempt {attempt} produced no fault site"));
            assert_ne!(
                s[1], 0,
                "attempt {attempt}: the kernel reported no faulting pc"
            );
            assert_ne!(
                s[2], 0,
                "attempt {attempt}: the kernel reported no faulting address"
            );
        }
    }

    /// **The supervisor restarts the crashed C component, and the restarted component does real
    /// work.**
    ///
    /// The restart half of §26, with a foreign component on the end of it. Three instances run in
    /// sequence: two crash, and the third computes a checksum and a transform in C, writes them
    /// into the shared grant, and exits cleanly. The confiner checks that output against an
    /// independent Rust implementation of the same definition, which is why `OUTPUT_CORRECT` is
    /// worth a bit of its own: "an instance ran" is cheap, "the C produced the right answer after
    /// two crashes" is the claim.
    ///
    /// The clean exit must arrive as `EVENT_EXIT` and must **not** be restarted, which is the other
    /// half of "both events flow" (§26.3) and is what ends the run.
    #[test_case]
    fn the_supervisor_restarts_the_faulted_c_component_and_the_restart_does_real_work() {
        let msgs = run_seam();

        // Every attempt reached its C call, in order, including the two that came after a crash.
        // (No `Vec` here: this is the kernel, and there is no allocator in a test.)
        let mut ran = [u64::MAX; ATTEMPTS];
        let mut count = 0usize;
        for m in of_kind(&msgs, RPT_RAN) {
            if count < ATTEMPTS {
                ran[count] = m[1];
            }
            count += 1;
        }
        assert_eq!(
            count, ATTEMPTS,
            "expected {ATTEMPTS} instances of the C component to run, saw {count}: the supervisor \
             did not restart a crashed child, or restarted one it should have left finished",
        );
        for (i, &attempt) in ran.iter().enumerate() {
            assert_eq!(
                attempt, i as u64,
                "instance {i} reported itself as attempt {attempt}: the supervisor's restart policy \
                 ran with the wrong state",
            );
        }

        // The honest attempt: confined like the others, and correct.
        let honest = of_kind(&msgs, RPT_VERDICT)
            .find(|v| v[1] == ATTEMPT_HONEST)
            .expect("the honest attempt produced no verdict");
        assert_eq!(
            honest[2],
            CONFINED_AND_CORRECT,
            "the restarted C component was reached but did not produce the right answer (output \
             correct: {}). A restart that revives a process which computes nothing is not a restart.",
            honest[2] & OUTPUT_CORRECT != 0,
        );

        // A clean exit reads as EXIT, which is what lets a userspace policy tell "finished" from
        // "crashed" without guessing.
        let last = of_kind(&msgs, RPT_DEATH)
            .last()
            .expect("no death messages at all");
        assert_eq!(
            last[2],
            abi::fault::EVENT_EXIT,
            "the honest attempt exited cleanly, so its supervisor must see EXIT, not FAULT",
        );
    }
}

/// **A running component replaced under a talking client** (milestone 23, DECISIONS §41).
///
/// The flagship the roadmap points at, and the thing to notice about it is what the kernel does not
/// contain. There is no component object, no swap syscall, no naming service, and no
/// livecycle-aware anything: `swapper` is an unprivileged process with a budget, one device
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
mod live_swap_tests {
    use super::*;
    use crate::sched;

    /// The report protocol, matching user/src/swap.rs. Userspace owns the definition; the test
    /// mirrors it, the same convention `authority_tests` and `c_seam_tests` follow.
    const RPT_UP: u64 = 1;
    const RPT_QUIESCED: u64 = 2;
    const RPT_PROBE_SURVIVED: u64 = 3;
    const RPT_STEP: u64 = 4;
    const RPT_LOG: u64 = 5;
    const RPT_CLIENT: u64 = 6;
    const RPT_ATTACK: u64 = 7;
    const RPT_DEATH: u64 = 8;
    const RPT_SITE: u64 = 9;
    const RPT_DRAINED: u64 = 10;
    const RPT_FAILED: u64 = 99;

    /// The operator's steps.
    const STEP_BUILT: u64 = 1;
    const STEP_DRAINED: u64 = 2;
    const STEP_REVOKED: u64 = 3;
    const STEP_STARTED: u64 = 4;

    /// The operator's verdict bits (`swap::log_checks`).
    const LOG_NO_GAP: u64 = 1 << 0;
    const LOG_MONOTONE: u64 = 1 << 1;
    const LOG_BOTH_VERSIONS: u64 = 1 << 2;
    const LOG_REVOKE_ENFORCED: u64 = 1 << 3;

    /// The client's verdict bits (`swap::client_checks`).
    const CL_ALL_REPLIED: u64 = 1 << 0;
    const CL_SEQ_ECHOED: u64 = 1 << 1;
    const CL_DIGEST_CORRECT: u64 = 1 << 2;
    const CL_ONE_TRANSITION: u64 = 1 << 3;
    const CL_SPANNED_SWAP: u64 = 1 << 4;
    const CL_WAS_BUFFERED: u64 = 1 << 5;
    const CL_NONE_REFUSED: u64 = 1 << 6;

    /// The roles, and the two versions.
    const ROLE_DIRECT: u64 = 0;
    const ROLE_QUEUED: u64 = 1;
    const V1: u64 = 1;
    const V2: u64 = 2;
    const REQUESTS: u64 = 64;

    /// The device's virtual address in every component, matching `swap::DEV_VA`. The test asserts
    /// the kernel's reported fault address against this, which is why both sides name one constant.
    const DEV_VA: u64 = 0x0310_0000;

    /// The console UART's physical address, matching `crate::console`. This is the device the
    /// operator lends, takes back, and lends again.
    #[cfg(target_arch = "aarch64")]
    const UART_PHYS: u64 = 0x0900_0000; // PL011
    #[cfg(target_arch = "riscv64")]
    const UART_PHYS: u64 = 0x1000_0000; // NS16550

    /// The operator's budget: five instance regions of forty pages plus its own scratch mappings and
    /// their page tables.
    ///
    /// Kept tight on purpose, and it is not merely tidiness. `untyped::create` takes a **contiguous**
    /// run of frames and the suite runs three of these systems, on top of a dozen earlier tests that
    /// each park an init holding an eight-megabyte region. An over-generous budget here fragments
    /// the frame allocator enough that a *later, unrelated* test cannot get init's region, which is
    /// how both of this milestone's memory failures surfaced: nowhere near their cause.
    const SWAPPER_BUDGET_PAGES: u64 = 224;

    /// How many reports one run can make before the test gives up waiting for the operator's final
    /// verdict. Generous: the loop stops at `RPT_LOG`, and this is only the tripwire for a run that
    /// never gets there.
    const MAX_REPORTS: usize = 24;

    /// **Spawn the operator the way the kernel spawns init**, and return the report endpoint every
    /// process in the run holds a WRITE view of.
    ///
    /// Deliberately the same endowment `spawn_init` gives (the archive read-only at `INITRD_VA`, an
    /// untyped in slot 0, a report endpoint in slot 1), **plus** the one thing this milestone is
    /// about: a device capability in slot 2, `WRITE|GRANT`, exactly as init gets one at boot. So
    /// what is under test is the operator's choices, not a privileged shortcut.
    fn spawn_swapper(role: u64) -> (sched::EpId, u64, u64) {
        let (initrd_start, initrd_len) = memory::initrd_region().expect("no initrd region");
        let initrd_pages = initrd_len.div_ceil(FRAME_SIZE);
        let bytes = program("swapper").expect("no swapper program in the initrd archive");
        let elf = Elf::parse(bytes).expect("swapper is not loadable");

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
        let mut space = AddressSpace::new(content).expect("no memory for swapper");
        map_segments(&mut space, &elf).expect("could not lay out swapper");
        for k in 0..INIT_STACK_PAGES {
            space
                .map_new(USER_STACK_VA - k * FRAME_SIZE, Flags::user_data())
                .expect("could not map swapper's stack");
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
        let aspace = readopt_user_aspace(space).expect("register the swapper aspace");

        let report = sched::create_endpoint();
        let budget = crate::untyped::create(SWAPPER_BUDGET_PAGES).expect("no budget for swapper");
        let tcb_region = crate::untyped::create(2).expect("no tcb region");
        let tid = sched::create_tcb(tcb_region).expect("no tcb");
        let s0 = sched::tcb_insert_cap(tid, crate::cap::untyped_root_cap(budget), None)
            .expect("insert budget");
        assert_eq!(s0, 0, "swapper's budget must land in slot 0");
        let s1 = sched::tcb_insert_cap(
            tid,
            crate::cap::endpoint_cap(
                report,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            ),
            None,
        )
        .expect("insert report");
        assert_eq!(s1, 1, "swapper's report endpoint must land in slot 1");
        let s2 = sched::tcb_insert_cap(
            tid,
            crate::cap::device_frame_cap(
                UART_PHYS,
                crate::cap::Rights::WRITE.union(crate::cap::Rights::GRANT),
            ),
            None,
        )
        .expect("insert device");
        assert_eq!(s2, 2, "swapper's device capability must land in slot 2");
        sched::configure_tcb(tid, elf.entry(), USER_STACK_TOP, aspace).expect("configure");
        sched::start_tcb(tid, [role, initrd_len, 0]).expect("start");
        (report, budget, tcb_region)
    }

    /// **Run one swap system to its own verdict**, returning every report it made.
    ///
    /// Run to the end rather than stopping at the first interesting message, for the reason
    /// `authority_tests::run_tree` records: a half-run system keeps building processes in the
    /// background, and a test that leaves work running is a test that fails somebody else. The
    /// operator's `RPT_LOG` is always its last word, so that is the stop condition.
    fn run_swap(role: u64) -> ([[u64; 5]; MAX_REPORTS], usize) {
        let before = memory::free_frames();
        let (report, budget, tcb_region) = spawn_swapper(role);
        let mut msgs = [[0u64; 5]; MAX_REPORTS];
        let mut n = 0;
        while n < MAX_REPORTS {
            let msg = sched::ipc_recv(report);
            assert_ne!(
                msg[0], RPT_FAILED,
                "the swap system could not be built: stage {}. Stages 1-4 are the archive and the \
                 four program images, 5-10 the endpoints and the witness page, 11-16 the incumbent \
                 and the client, 20-27 the swap itself, 30-33 the attacker, 40-51 the queued rung.",
                msg[1],
            );
            assert_ne!(
                msg[0], RPT_PROBE_SURVIVED,
                "the outgoing instance read the device AFTER the operator revoked it and the read \
                 succeeded: the revoke did not take, so there was a window with two owners of one \
                 device's registers",
            );
            msgs[n] = msg;
            n += 1;
            if msg[0] == RPT_LOG {
                break;
            }
        }
        assert!(
            n < MAX_REPORTS,
            "the operator never reached its final verdict in {MAX_REPORTS} reports",
        );

        // Let the system settle, then prove it has nothing more to say. A parked sender here would
        // mean something acted after the operator had finished, which is how "the run left nothing
        // running" is proven without a blocking receive that would hang when the code is right.
        for _ in 0..400 {
            sched::yield_now();
        }
        assert_eq!(
            sched::endpoint_waiting_senders(report),
            0,
            "the swap system had more to say after the operator's final verdict",
        );

        // **The system reclaims itself**, and this is an assertion rather than housekeeping.
        //
        // The operator supervises every child it starts and collects every corpse through its
        // supervision endpoint (DECISIONS §32), which returns each instance's region to its budget.
        // So a reclaim of the budget can only *succeed* if all five of those splits are gone: §16
        // refuses a region whose children are still carved out of it. Success is the statement that
        // nothing leaked; the frame delta is the statement that the whole run came back.
        //
        // It also has to work, for a reason that has nothing to do with tidiness. `untyped::create`
        // takes a **contiguous** run of frames, these tests run three systems, and the first version
        // of this leaked all three, which fragmented the allocator badly enough that a *later* test
        // could not get init's own eight-megabyte region.
        //
        // `sched::reclaim_region` rather than `untyped::destroy` because these regions are *pinned*:
        // the operator retyped four endpoints and a frame out of its budget. Reclaiming a region
        // with objects in it is the §16 teardown, and it is the entry point the `Untyped::DESTROY`
        // syscall uses, so the test cannot succeed down a path userspace could not have taken.
        let before_reclaim = memory::free_frames();
        sched::reclaim_region(budget).expect(
            "the operator's budget would not reclaim: a child region is still carved out of it, so \
             the swap system leaked one of its components",
        );
        let recovered = memory::free_frames() - before_reclaim;
        assert_eq!(
            recovered, SWAPPER_BUDGET_PAGES as usize,
            "reclaiming the operator's budget returned {recovered} of {SWAPPER_BUDGET_PAGES} pages",
        );

        // The operator's own address space and TCB are **not** in that budget: the kernel built the
        // operator the way it builds init. They come home through the ordinary reaper, which with
        // per-CPU run queues (DECISIONS §28) runs when the core the operator died on next schedules,
        // and the boot thread yielding here cannot force that. So this is hygiene, deliberately not
        // asserted on: what this milestone is responsible for is the swap system's own memory,
        // which is what the assertion above covers. `before` is kept for the same reason it is
        // taken: a reader comparing it against the reclaim above can see the two agree.
        debug_assert!(before >= memory::free_frames());
        let _ = sched::reclaim_region(tcb_region);
        (msgs, n)
    }

    /// Every report of one kind, in arrival order.
    fn of_kind(msgs: &[[u64; 5]], kind: u64) -> impl Iterator<Item = &[u64; 5]> {
        msgs.iter().filter(move |m| m[0] == kind)
    }

    /// Did the operator report this step?
    fn had_step(msgs: &[[u64; 5]], step: u64) -> bool {
        of_kind(msgs, RPT_STEP).any(|m| m[1] == step)
    }

    /// **The flagship: a component is replaced under a client that is talking to it.**
    ///
    /// The four steps all happen, in an order the operator chose, and then two independent
    /// witnesses in two address spaces agree that the conversation was unbroken. The client is not
    /// consulted about the swap and the operator is not consulted about the replies; each says only
    /// what it saw.
    #[test_case]
    fn a_client_keeps_talking_while_the_server_underneath_it_is_replaced() {
        let (msgs, n) = run_swap(ROLE_DIRECT);
        let msgs = &msgs[..n];

        // The four steps, each on machinery that existed before this milestone.
        for (step, what) in [
            (STEP_BUILT, "build the replacement"),
            (STEP_DRAINED, "drain the incumbent"),
            (STEP_REVOKED, "revoke the device"),
            (STEP_STARTED, "start the replacement"),
        ] {
            assert!(
                had_step(msgs, step),
                "the operator never got as far as: {what}",
            );
        }

        // Both instances ran, and both could reach the device they were endowed with. The second
        // half matters as much as the first: an instance that answered every request while the
        // registers went unowned would look like a perfect swap.
        let mut ups = of_kind(msgs, RPT_UP);
        let first = ups.next().expect("the incumbent never started");
        let second = ups.next().expect("the replacement never started");
        assert!(ups.next().is_none(), "a third instance started");
        assert_eq!(first[1], V1, "the incumbent should be version 1");
        assert_eq!(second[1], V2, "the replacement should be version 2");
        assert!(
            first[2] == 1 && second[2] == 1,
            "an instance could not read the device it was endowed with, so the registers were not \
             where the swap thinks they were",
        );

        // The incumbent served a real share of the conversation before it was drained: a swap that
        // happened before anyone was talking would prove nothing.
        let quiesced = of_kind(msgs, RPT_QUIESCED)
            .next()
            .expect("the incumbent never acknowledged the drain");
        assert!(
            quiesced[2] > 0 && quiesced[2] < REQUESTS,
            "the incumbent served {} of {REQUESTS} requests: the swap did not land inside the \
             conversation",
            quiesced[2],
        );

        // Witness one: the client, from its own replies, in its own address space.
        let client = of_kind(msgs, RPT_CLIENT)
            .next()
            .expect("the client never reported a verdict");
        const CLIENT_UNBROKEN: u64 = CL_ALL_REPLIED
            | CL_SEQ_ECHOED
            | CL_DIGEST_CORRECT
            | CL_ONE_TRANSITION
            | CL_SPANNED_SWAP;
        assert_eq!(
            client[1] & CLIENT_UNBROKEN,
            CLIENT_UNBROKEN,
            "the client's stream was broken (verdict {:#x}): missing {:#x}. ALL_REPLIED={}, \
             SEQ_ECHOED={}, DIGEST_CORRECT={}, ONE_TRANSITION={}, SPANNED_SWAP={}",
            client[1],
            CLIENT_UNBROKEN & !client[1],
            client[1] & CL_ALL_REPLIED != 0,
            client[1] & CL_SEQ_ECHOED != 0,
            client[1] & CL_DIGEST_CORRECT != 0,
            client[1] & CL_ONE_TRANSITION != 0,
            client[1] & CL_SPANNED_SWAP != 0,
        );

        // Witness two: the operator, from the shared page, after every writer is dead.
        let log = of_kind(msgs, RPT_LOG)
            .next()
            .expect("the operator never reported its verdict");
        const LOG_CLEAN: u64 = LOG_NO_GAP | LOG_MONOTONE | LOG_BOTH_VERSIONS | LOG_REVOKE_ENFORCED;
        assert_eq!(
            log[1] & LOG_CLEAN,
            LOG_CLEAN,
            "the operator's log says the swap was not clean (verdict {:#x}): NO_GAP={} (a request \
             nobody served), MONOTONE={} (the old instance answered after the new one, so two \
             owners), BOTH_VERSIONS={}, REVOKE_ENFORCED={} (the post-revoke device read did not \
             fault where it should have)",
            log[1],
            log[1] & LOG_NO_GAP != 0,
            log[1] & LOG_MONOTONE != 0,
            log[1] & LOG_BOTH_VERSIONS != 0,
            log[1] & LOG_REVOKE_ENFORCED != 0,
        );
        // The two witnesses agree on *where* the swap happened, which is the cross-check that makes
        // each of them evidence rather than a self-report.
        assert_eq!(
            log[2], client[2],
            "the operator's log and the client's replies disagree about which request the \
             replacement took over at ({} vs {})",
            log[2], client[2],
        );

        // The control: the outgoing instance died faulting on the device it no longer had, at the
        // device's own virtual address. `run_swap` has already refused a run in which that read
        // succeeded.
        let death = of_kind(msgs, RPT_DEATH)
            .next()
            .expect("the outgoing instance never died");
        assert_eq!(
            death[2],
            abi::fault::EVENT_FAULT,
            "the outgoing instance should have faulted on the revoked device, not exited cleanly",
        );
        let site = of_kind(msgs, RPT_SITE)
            .next()
            .expect("no fault site was reported");
        assert_eq!(
            site[1] & !(FRAME_SIZE - 1),
            DEV_VA,
            "the outgoing instance faulted at {:#x}, which is not in the device page {DEV_VA:#x}: \
             it died of something other than the revoke, which would make the rest of this test \
             vacuous",
            site[1],
        );
    }

    /// **The attacker holds a real capability to the stable endpoint and still cannot be the
    /// server.**
    ///
    /// The milestone rests on endpoint-only naming, and the obvious worry about it is that a name
    /// with no peer in it is a name anybody can answer to. It is not: `SEND` and `RECV` are gated by
    /// different rights on the same object, so the same endpoint handed out two ways is a one-way
    /// pipe in whichever direction each holder was trusted with. The attacker is endowed with
    /// *exactly* what the honest client holds, so the refusal is about rights and not about
    /// wiring.
    #[test_case]
    fn a_client_of_the_stable_endpoint_cannot_become_its_server() {
        let (msgs, n) = run_swap(ROLE_DIRECT);
        let attack = of_kind(&msgs[..n], RPT_ATTACK)
            .next()
            .expect("the attacker never reported");
        assert_eq!(
            attack[1],
            (-(abi::Error::NotPermitted as i64)) as u64,
            "a client of the stable endpoint received on it (or was refused for the wrong reason): \
             error {}, wanted NotPermitted. If this succeeded, any holder of a request capability \
             could impersonate the component.",
            attack[1] as i64,
        );
    }

    /// **The opt-in rung: a producer keeps producing while no backend exists at all.**
    ///
    /// The direct rung's down window costs the caller a block: its request is safe (it parks on the
    /// endpoint's sender queue and the next server drains it) but it is stopped until then. For a
    /// channel that cannot afford that, `broker` takes custody. The price is one extra hop on
    /// every request in the steady state, which is why it is chosen per channel and never by
    /// default; `broker_rtt` in bench/baseline.txt is that price.
    ///
    /// What this proves that the direct test does not: there is a window here in which the backend
    /// **does not exist** (it was quiesced, it died, and its corpse was collected before the
    /// replacement was built), the producer kept calling through it, and every item it handed over
    /// turns up in the new backend's log, in order.
    #[test_case]
    fn a_producer_never_blocks_on_an_absent_consumer_and_loses_nothing() {
        let (msgs, n) = run_swap(ROLE_QUEUED);
        let msgs = &msgs[..n];

        let producer = of_kind(msgs, RPT_CLIENT)
            .next()
            .expect("the producer never reported a verdict");
        const PRODUCER_OK: u64 =
            CL_ALL_REPLIED | CL_SEQ_ECHOED | CL_DIGEST_CORRECT | CL_NONE_REFUSED | CL_WAS_BUFFERED;
        assert_eq!(
            producer[1] & PRODUCER_OK,
            PRODUCER_OK,
            "the queued producer's run was not clean (verdict {:#x}): NONE_REFUSED={} (the queue \
             overflowed or a request was rejected), WAS_BUFFERED={} (nothing was ever buffered, so \
             the producer never actually spanned a window with no backend)",
            producer[1],
            producer[1] & CL_NONE_REFUSED != 0,
            producer[1] & CL_WAS_BUFFERED != 0,
        );

        // The broker's own account: it drained everything it ever took custody of.
        let drained = of_kind(msgs, RPT_DRAINED)
            .next()
            .expect("the broker never reported a drain");
        assert!(drained[1] > 0, "the broker drained nothing");
        assert_eq!(
            drained[1], producer[2],
            "the broker drained {} items but the producer said it had handed over {}",
            drained[1], producer[2],
        );

        // And the backend's log, read by the operator after both backends are gone: every item, in
        // order, served by somebody, with the version changing exactly where the swap was.
        let log = of_kind(msgs, RPT_LOG)
            .next()
            .expect("the operator never reported its verdict");
        const LOG_CLEAN: u64 = LOG_NO_GAP | LOG_MONOTONE | LOG_BOTH_VERSIONS;
        assert_eq!(
            log[1] & LOG_CLEAN,
            LOG_CLEAN,
            "the queued channel lost or reordered work (verdict {:#x}): NO_GAP={}, MONOTONE={}, \
             BOTH_VERSIONS={}",
            log[1],
            log[1] & LOG_NO_GAP != 0,
            log[1] & LOG_MONOTONE != 0,
            log[1] & LOG_BOTH_VERSIONS != 0,
        );
    }
}

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
mod measured_boot_tests {
    use super::*;

    /// The boot-program entry both architectures' kernels load themselves. (riscv's shell boot loads
    /// `system_initializer` instead, measured under that name by the same trust root; `init` is the entry both
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
        let mut h = measured_boot::Sha256::new();
        h.update(&[bytes[0] ^ 1]);
        h.update(&bytes[1..]);
        let tampered = h.finalize();

        assert_eq!(
            measured_boot::verify_digest(crate::trust::TRUST_ROOT, BOOT_PROGRAM, &tampered),
            Err(measured_boot::VerifyError::Mismatch),
            "a boot program with one bit flipped still satisfied the trust root",
        );
        // Fail-closed on the other axis: a program the trust root says nothing about is refused, not
        // waved through. This is what makes an empty or stale trust root safe.
        assert_eq!(
            crate::trust::verify("no-such-program", bytes),
            Err(measured_boot::VerifyError::Unmeasured),
            "the kernel vouched for a program it has no measurement for",
        );
    }
}

/// **The fault endpoint: a supervisor watches a child die and reap it** (milestone 22, DECISIONS
/// §26). These are the cross-ISA tests, because the mechanism is portable: a supervised child that
/// faults (or exits) turns into a five-word message on its supervision endpoint, its corpse persists
/// until the supervisor reaps it with §16 revocation, and a fresh child runs in its place. The only
/// per-architecture parts are the two tiny code stubs (a null load that faults, and a `SEND` + exit),
/// and even those are the same shape both ISAs already use elsewhere in this file. The kernel is the
/// only sender on the fault endpoint, so the tid the supervisor reads is trustworthy without a badge.
#[cfg(test)]
mod supervision_tests {
    use abi::fault::{EVENT_EXIT, EVENT_FAULT, FAULT_EP_SLOT};

    use super::*;
    use crate::sched;

    pub(super) const CODE_VA: u64 = 0x40_0000;
    pub(super) const STACK_VA: u64 = 0x50_0000;
    /// The unmapped address the fault stub loads from. Distinctive, so the delivered fault address
    /// proves the message carries real fault-time state and not a zero placeholder.
    const BAD_ADDR: u64 = 0x00A5_0000;
    /// The word the report stub SENDs, so a test can tell "the child ran" from "the child faulted."
    pub(super) const REPORT_WORD: u64 = 0x42;

    /// A child that faults on its very first memory access: load from [`BAD_ADDR`], which nothing
    /// maps. Two instructions; the faulting one is the second, so the reported pc is `CODE_VA + 4`.
    #[cfg(target_arch = "aarch64")]
    pub(super) const FAULT_STUB: &[u32] = &[
        0xD2A0_14A0, // movz x0, #0xA5, lsl #16   (x0 = 0x00A5_0000)
        0xF940_0001, // ldr  x1, [x0]             (data abort: nothing maps BAD_ADDR)
    ];
    #[cfg(target_arch = "riscv64")]
    pub(super) const FAULT_STUB: &[u32] = &[
        0x00A5_0537, // lui a0, 0xA50             (a0 = 0x00A5_0000)
        0x0005_3583, // ld  a1, 0(a0)             (load page fault: nothing maps BAD_ADDR)
    ];

    /// A child that SENDs [`REPORT_WORD`] on the endpoint in slot 0, then exits cleanly. The same
    /// nine-instruction shape the region-reclaim tests use, so "it ran" is the SEND arriving.
    #[cfg(target_arch = "aarch64")]
    pub(super) const REPORT_STUB: &[u32] = &[
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
    pub(super) const REPORT_STUB: &[u32] = &[
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
        (build_child_in(region, stub, report, fault_ep), region)
    }

    /// [`build_child`], but into a region the caller already owns. The reap tests need this: §32's
    /// property is that the reclaimed pages go back to **the builder's** budget, which can only be
    /// observed if the builder's region is one the test still holds and can measure.
    pub(super) fn build_child_in(
        region: u64,
        stub: &[u32],
        report: Option<sched::EpId>,
        fault_ep: Option<sched::EpId>,
    ) -> u64 {
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
        tid
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
mod reap_tests {
    use abi::Error;
    use abi::fault::{EVENT_EXIT, EVENT_FAULT};

    use super::supervision_tests::{FAULT_STUB, REPORT_STUB, REPORT_WORD, build_child_in};
    use crate::arch::exceptions::TrapFrame;
    use crate::cap::{Object, Rights};
    use crate::sched;
    use crate::syscall::invoke;

    /// The builder's whole budget. Room for three instances plus slack, so the LIFO return-of-pages
    /// (§16) can be watched happening more than once.
    const BUILDER_BUDGET_PAGES: u64 = 80;

    /// Pages per instance region: the child's address space (root and tables), its code page, its
    /// stack page, and its TCB. Sixteen is what `build_child` has always carved.
    const INSTANCE_PAGES: u64 = 16;

    /// Pages for the test's own endpoints, one per endpoint (`RETYPE_OBJ`'s one-object-per-page
    /// rule). Two is the most any one test here needs; four is slack.
    const ENDPOINT_PAGES: u64 = 4;

    /// The slot half of a generational name (`crates/slots` packs generation in the high 32 bits,
    /// slot in the low 32). The recycled-tid test needs it to assert it is genuinely replaying a
    /// name that now *aliases a live thread's slot*, which is the only version of that test worth
    /// running.
    const SLOT_MASK: u64 = 0xffff_ffff;

    /// One test's world: a budget the *builder* owns, and a small region the test's endpoints are
    /// retyped from. The endpoints come out of a reclaimable region rather than `create_endpoint`'s
    /// shared kernel one so that `tidy` can give them back; six tests each leaking a couple of
    /// endpoints exhausted the kernel's endpoint budget for every test that ran afterwards, which is
    /// the sort of failure a test should not be able to inflict on its neighbours.
    fn arena() -> (u64, u64) {
        let budget = crate::untyped::create(BUILDER_BUDGET_PAGES).expect("no builder budget");
        let endpoints = crate::untyped::create(ENDPOINT_PAGES).expect("no endpoint region");
        (budget, endpoints)
    }

    /// An endpoint out of the test's own region (one page each).
    fn endpoint(region: u64) -> sched::EpId {
        sched::create_endpoint_from(region).expect("no endpoint")
    }

    /// Hold a supervision endpoint the way a supervisor holds one: `READ` alone, the right to
    /// receive deaths here. No `WRITE` (it is not a sender on its own children's death channel) and
    /// no `GRANT`.
    fn hold_endpoint(ep: sched::EpId) -> u64 {
        sched::grant(crate::cap::endpoint_cap(ep, Rights::READ)).expect("grant the endpoint")
    }

    /// `invoke(cap, REAP, tid, _, _)`, through the real dispatcher.
    fn reap(slot: u64, tid: u64) -> Result<i64, Error> {
        let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
        invoke(&mut frame, slot, abi::endpoint::REAP, tid, 0, 0)
    }

    /// Receive one five-word death message through the ABI, so the tid a test reaps with is the tid
    /// a real supervisor would have read out of its registers.
    fn recv_death(slot: u64) -> [u64; 5] {
        let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
        let w0 = invoke(&mut frame, slot, abi::endpoint::RECV, 0, 0, 0).expect("RECV refused");
        [
            w0 as u64,
            frame.arg(1),
            frame.arg(2),
            frame.arg(3),
            frame.arg(4),
        ]
    }

    /// How many capability slots the supervisor occupies. The "no richer than before" measurement:
    /// a reap that handed authority back would have to put it somewhere, and every path that gives a
    /// process a capability lands it in a slot.
    fn occupied_slots() -> usize {
        (0..abi::CSPACE_SLOTS)
            .filter(|&s| sched::current_cap(s).is_ok())
            .count()
    }

    /// **The supervisor cannot construct anything, audited slot by slot.**
    ///
    /// For every empty slot, the two primitives that build a process (`RETYPE_OBJ` for a TCB and for
    /// an address space) are driven through the real dispatcher and must answer `NoSuchSlot`: there
    /// is nothing there, which is what no-ambient-authority feels like, and it is a different fact
    /// from `NotPermitted` (something there, restricted). For every occupied slot, the capability
    /// must be an `Endpoint`, which cannot build by dispatch: the endpoint arm of `invoke` offers
    /// send, receive, delegate, call, and reap, and no constructor at all. Untyped methods are *not*
    /// invoked on those slots, because method numbers are per-object-type and `RETYPE_OBJ`'s number
    /// is `SEND_CAP`'s on an endpoint.
    fn assert_can_only_supervise(expected: &[u64]) {
        let mut frame = TrapFrame::for_user_entry(0, 0, [0, 0, 0]);
        for slot in 0..abi::CSPACE_SLOTS {
            match sched::current_cap(slot) {
                Err(_) => {
                    for objtype in [abi::objtype::TCB, abi::objtype::ASPACE] {
                        assert_eq!(
                            invoke(&mut frame, slot, abi::untyped::RETYPE_OBJ, objtype, 0, 0),
                            Err(Error::NoSuchSlot),
                            "slot {slot} answered something other than \"you hold no such \
                             capability\" to a construction request",
                        );
                    }
                }
                Ok(cap) => {
                    assert!(
                        expected.contains(&slot),
                        "the supervisor holds an unexpected capability in slot {slot}: this test's \
                         confinement claim is only about a cspace it fully accounts for",
                    );
                    assert!(
                        matches!(cap.object, Object::Endpoint(_)),
                        "the supervisor holds a non-endpoint capability in slot {slot}",
                    );
                }
            }
        }
    }

    /// Give everything back: the supervisor's capability slots, then the builder's budget (which
    /// reclaims any instance region still under it), then the endpoint region. In that order,
    /// because reclaiming the endpoints first would revoke the channels the corpses are still
    /// attached to.
    fn tidy(budget: u64, endpoints: u64, slots: &[u64]) {
        for &s in slots {
            let _ = sched::delete_current_cap(s);
        }
        sched::reclaim_region(budget).expect("the builder's own budget did not come back");
        sched::reclaim_region(endpoints).expect("the test's endpoint region did not come back");
    }

    /// **The headline: a supervisor that cannot build anything collects its dead child.**
    ///
    /// Its entire authority is one supervision endpoint with `READ`. It holds no untyped, no frame,
    /// no TCB, and no address space, and the audit proves that from the inside, on the two
    /// primitives that build a process. Then it reaps, through the endpoint, naming the tid the
    /// kernel stamped on the death message it just received. Before §32 this required `WRITE` on the
    /// child's region, which is the same right that builds an arbitrary process out of it.
    #[test_case]
    fn a_supervisor_holding_only_its_endpoint_reaps_its_dead_child() {
        let (budget, endpoints) = arena();
        let fault_ep = endpoint(endpoints);
        let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
        let child = build_child_in(instance, FAULT_STUB, None, Some(fault_ep));

        let cap = hold_endpoint(fault_ep);
        assert_can_only_supervise(&[cap]);

        let msg = recv_death(cap);
        assert_eq!(msg[0], EVENT_FAULT, "the child should have crashed");
        assert_eq!(msg[1], child, "the death message named the wrong thread");

        assert_eq!(
            reap(cap, msg[1]),
            Ok(0),
            "a supervisor holding only its supervision endpoint could not collect its own corpse",
        );
        assert_eq!(
            sched::corpse_fault_msg(child),
            None,
            "the corpse survived the reap",
        );
        // The reap did not turn into authority: the same audit still holds, and the same tid is now
        // an unknown name rather than a second free reap.
        assert_can_only_supervise(&[cap]);
        assert_eq!(reap(cap, child), Err(Error::NotSupervised));

        tidy(budget, endpoints, &[cap]);
    }

    /// **The reclaimed region returns to the builder, not to the reaper** (§32's first consequence,
    /// and the property that makes the decision worth having).
    ///
    /// The builder splits an instance region off its own budget, which bumps its watermark; the
    /// supervisor reaps; the watermark comes back down (§16's LIFO return-of-pages) and the builder
    /// can spend those pages again. The supervisor, meanwhile, occupies exactly the slots it did
    /// before and still holds nothing but an endpoint. A reap that had quietly credited the reaper
    /// would fail the first pair of assertions; one that had merely freed the corpse without
    /// returning the memory would fail the second.
    #[test_case]
    fn the_reaped_region_returns_to_the_builder_not_the_reaper() {
        let (budget, endpoints) = arena();
        let fault_ep = endpoint(endpoints);
        let (spent_before, _) = crate::untyped::usage(budget).expect("the budget exists");

        let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
        assert_eq!(
            crate::untyped::usage(budget).unwrap().0,
            spent_before + INSTANCE_PAGES,
            "the split did not come out of the builder's budget",
        );
        let child = build_child_in(instance, FAULT_STUB, None, Some(fault_ep));

        let cap = hold_endpoint(fault_ep);
        let msg = recv_death(cap);
        assert_eq!(msg[1], child);
        let slots_before = occupied_slots();

        assert_eq!(reap(cap, msg[1]), Ok(0), "the reap failed");

        assert_eq!(
            crate::untyped::usage(budget).unwrap().0,
            spent_before,
            "the reaped pages did not go back to the builder's budget: the reaper freed the corpse \
             and stranded its memory",
        );
        assert!(
            !crate::untyped::has_children(budget),
            "the instance region outlived its corpse, so the builder's budget is still committed",
        );
        // Genuinely spendable again, not merely un-bumped bookkeeping.
        let again = crate::untyped::split(budget, INSTANCE_PAGES)
            .expect("the builder could not re-spend the pages the reap returned");
        sched::reclaim_region(again).expect("reclaim the re-split region");

        assert_eq!(
            occupied_slots(),
            slots_before,
            "the supervisor gained a capability by reaping",
        );
        assert_can_only_supervise(&[cap]);

        tidy(budget, endpoints, &[cap]);
    }

    /// **A live child is refused, with an error of its own.** Collecting a corpse is not killing.
    /// The child here is blocked in a `SEND` nobody has received, so it is provably alive at the
    /// moment of the attempt, and the refusal is `StillAlive` rather than a generic `NotPermitted`:
    /// a restart policy needs "wait, or escalate to the owner's `DESTROY`" to be distinguishable
    /// from "there is no such child of mine". Then the child is let go, dies, and the *same* tid
    /// through the *same* endpoint succeeds, which is what proves the refusal was about liveness and
    /// not about authority.
    #[test_case]
    fn reap_refuses_a_live_child_with_a_distinct_error() {
        let (budget, endpoints) = arena();
        let report = endpoint(endpoints);
        let fault_ep = endpoint(endpoints);
        let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
        let child = build_child_in(instance, REPORT_STUB, Some(report), Some(fault_ep));

        let cap = hold_endpoint(fault_ep);
        assert_eq!(
            reap(cap, child),
            Err(Error::StillAlive),
            "a supervisor collected a thread that was still running: REAP authorizes collecting a \
             corpse, never killing",
        );

        // Let it finish. It SENDs, we receive, it exits, and its death arrives on the endpoint.
        assert_eq!(
            sched::ipc_recv(report)[0],
            REPORT_WORD,
            "the child never ran"
        );
        let msg = recv_death(cap);
        assert_eq!(msg[0], EVENT_EXIT, "a clean exit must report EXIT");
        assert_eq!(msg[1], child);
        assert_eq!(
            reap(cap, child),
            Ok(0),
            "the same tid through the same endpoint failed once dead: the earlier refusal was not \
             about liveness after all",
        );

        tidy(budget, endpoints, &[cap]);
    }

    /// **Another supervisor's child is refused, even to a holder of both endpoints.**
    ///
    /// The sharpest form of §32's authorization rule: one process holds two supervision endpoints,
    /// and learns a tid legitimately, through the endpoint that supervises it. Naming that tid on
    /// the *other* endpoint is refused, because authorization is the `(tid, endpoint)` relationship
    /// and not "am I a supervisor". So a tid is a name inside a relationship, never a global handle,
    /// which is what lets §26's kernel-stamped tid be reused for this with no new bookkeeping.
    #[test_case]
    fn reap_refuses_another_supervisors_child() {
        let (budget, endpoints) = arena();
        let mine = endpoint(endpoints);
        let theirs = endpoint(endpoints);
        let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
        let child = build_child_in(instance, FAULT_STUB, None, Some(theirs));

        let cap_mine = hold_endpoint(mine);
        let cap_theirs = hold_endpoint(theirs);

        let msg = recv_death(cap_theirs);
        assert_eq!(msg[1], child, "the death arrived on the wrong endpoint");
        assert_eq!(
            reap(cap_mine, child),
            Err(Error::NotSupervised),
            "a corpse was collected through an endpoint that does not supervise it",
        );
        assert!(
            sched::corpse_fault_msg(child).is_some(),
            "the refused reap still tore something down",
        );

        assert_eq!(
            reap(cap_theirs, child),
            Ok(0),
            "the supervising endpoint could not collect its own corpse",
        );

        tidy(budget, endpoints, &[cap_mine, cap_theirs]);
    }

    /// **A recycled tid is refused, not resolved to the thread now in that slot.**
    ///
    /// Generational names (`crates/slots`) exist for exactly this, and this is the test that proves
    /// the reap path actually uses them: reap a child, build a replacement that lands in the freed
    /// thread-table slot with a bumped generation, then replay the old tid. The replay must be
    /// refused, and the replacement must be untouched, which is asserted by letting it run to
    /// completion afterwards rather than by inspecting it. The slot-aliasing assertion is part of
    /// the test: without it, the replay would be of a name nothing has reused and would prove
    /// nothing.
    #[test_case]
    fn reap_refuses_a_recycled_tid_rather_than_the_wrong_thread() {
        let (budget, endpoints) = arena();
        let fault_ep = endpoint(endpoints);
        let report = endpoint(endpoints);
        let cap = hold_endpoint(fault_ep);

        let first_region = crate::untyped::split(budget, INSTANCE_PAGES).expect("no first region");
        let first = build_child_in(first_region, FAULT_STUB, None, Some(fault_ep));
        assert_eq!(recv_death(cap)[1], first);
        assert_eq!(reap(cap, first), Ok(0), "the first reap failed");

        let second_region =
            crate::untyped::split(budget, INSTANCE_PAGES).expect("no second region");
        let second = build_child_in(second_region, REPORT_STUB, Some(report), Some(fault_ep));
        assert_eq!(
            second & SLOT_MASK,
            first & SLOT_MASK,
            "the replacement did not land in the reaped thread's table slot, so replaying the old \
             tid would not be aliasing anything: this test needs the reuse to mean something",
        );
        assert_ne!(
            second, first,
            "a reused slot handed out the same name: the generation did not bump",
        );

        assert_eq!(
            reap(cap, first),
            Err(Error::NotSupervised),
            "a stale tid resolved: the reap path is not going through the generational name",
        );
        // The replacement is untouched, proven by it running to completion.
        assert_eq!(
            sched::ipc_recv(report)[0],
            REPORT_WORD,
            "the replayed tid reaped the thread that had recycled its slot",
        );
        let msg = recv_death(cap);
        assert_eq!(msg[1], second);
        assert_eq!(reap(cap, second), Ok(0));

        tidy(budget, endpoints, &[cap]);
    }

    /// **Reaping a corpse whose death message was never collected leaves the endpoint clean.**
    ///
    /// A supervised thread that dies with nobody in `RECV` parks on its supervision endpoint's
    /// sender queue holding the message (§26 implementation note 2). Nothing requires a supervisor
    /// to collect that message before reaping: it can be told the tid by its builder, or simply
    /// choose not to read. So the reap has to unlink the corpse from that queue before freeing its
    /// TCB, or the supervisor's next `RECV` follows a pointer into a recycled page.
    ///
    /// This was reachable before §32 too, through `Untyped::DESTROY`; every existing caller happened
    /// to receive first, so it never fired. `endpoint::REAP` makes it easy to reach, which is how it
    /// was found. `crates/ipc`'s `remove_sender` is the fix, and the second half of this test (a
    /// fresh child's death arriving normally on the same endpoint) is what proves the queue is
    /// genuinely intact rather than merely counted right.
    #[test_case]
    fn reaping_an_uncollected_corpse_leaves_no_ghost_on_the_endpoint() {
        let (budget, endpoints) = arena();
        let fault_ep = endpoint(endpoints);
        let instance = crate::untyped::split(budget, INSTANCE_PAGES).expect("no instance region");
        let child = build_child_in(instance, FAULT_STUB, None, Some(fault_ep));
        let cap = hold_endpoint(fault_ep);

        // Nobody is receiving, so the corpse must park. Wait for it rather than assume a schedule.
        for _ in 0..4000 {
            if sched::endpoint_waiting_senders(fault_ep) == 1 {
                break;
            }
            sched::yield_now();
        }
        assert_eq!(
            sched::endpoint_waiting_senders(fault_ep),
            1,
            "the corpse never parked on its supervision endpoint",
        );

        assert_eq!(reap(cap, child), Ok(0), "the reap failed");
        assert_eq!(
            sched::endpoint_waiting_senders(fault_ep),
            0,
            "a freed TCB is still linked into the supervision endpoint's sender queue: the next \
             RECV would follow a dangling pointer into a recycled page",
        );

        // The endpoint still works, which is the real assertion: a stale head or tail would show up
        // here and not in the count above.
        let next_region = crate::untyped::split(budget, INSTANCE_PAGES).expect("no second region");
        let next = build_child_in(next_region, FAULT_STUB, None, Some(fault_ep));
        let msg = recv_death(cap);
        assert_eq!(
            msg[1], next,
            "the endpoint delivered something other than the new child's death",
        );
        assert_eq!(reap(cap, next), Ok(0));

        tidy(budget, endpoints, &[cap]);
    }
}

/// **The directory capability, attacked** (milestone 47, notes/dir-capability.md).
///
/// One module for both ISAs rather than an aarch64 test with a riscv twin, which the FS tests above
/// have. Nothing here is architecture-specific: it wires three portable programs and asserts on a
/// bitmap, so the only difference between the legs is which binary carries the block-server role,
/// and that is one `cfg` in [`blk_server_image`] rather than a second copy of every assertion. The
/// parity gate (DECISIONS §19) is met by literally the same test running twice.
#[cfg(test)]
mod dir_capability_tests {
    use fs_proto::dir;
    use fs_proto::fixture::dirscape as esc;

    use super::*;
    use crate::sched;

    /// The binary carrying the block server's role. One `cfg`, in `fs_service`, because the boot
    /// path needs the same answer this module's tests do.
    pub(super) fn blk_server_image() -> &'static [u8] {
        fs_service::blk_server_image()
    }

    /// Wire a `fs_subtree_caretaker` holding a capability to the fixture's `sub` with exactly
    /// `rights`, run the directory attacker against it, and return its verdict bitmap. `run` keeps
    /// the names the attacker creates distinct, because all three runs share one image within a
    /// boot and an `EEXIST` would otherwise read as a refusal.
    ///
    /// `None` when no RedoxFS disk is attached (nothing to test; do not fail).
    fn attack_a_subtree(rights: u64, run: u64) -> Option<u64> {
        let Some(report) = fs_service::start_granted_dir(
            blk_server_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_subtree_caretaker")
                .expect("no fs_subtree_caretaker program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
            fs_service::DirGrant {
                name: fs_proto::fixture::tree::SUB,
                rights,
                role: 5, // ROLE_DIR_ATTACKER
                arg: run,
                arg2: 0,
                stack_pages: 0,
            },
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return None;
        };
        // Both handshakes happened inside `start_granted_dir`, before this attacker existed. That
        // ordering is the fix for the startup clobber `fs_service::wait_for_caretaker` records.
        let [tag, verdict, ..] = sched::ipc_recv(report);
        assert_eq!(
            tag,
            fs_proto::fixture::VERDICT,
            "the attacker's report is not a verdict word",
        );
        Some(verdict)
    }

    /// Compare a verdict against the exact set the configuration is specified to produce, and say
    /// what the difference means in words rather than in hex.
    ///
    /// Both directions are failures and they are different failures, which is why this is not one
    /// equality assertion: a bit set that should not be is an escape, and a bit clear that should be
    /// set means the capability did not work, so every refusal it reported proves nothing.
    fn assert_verdict(got: u64, want: u64, what: &str) {
        let leaked = got & !want;
        let missing = want & !got;
        assert_eq!(
            leaked,
            0,
            "the {what} directory capability leaked: {}",
            describe_dirscape(leaked),
        );
        assert_eq!(
            missing, 0,
            "the {what} directory capability could not do what it was granted ({missing:#x}), so \
             its refusals prove nothing: a capability that reaches nothing is trivially confined",
        );
    }

    /// Name the bits a verdict set, so a failure reads as a sentence instead of a bitmap.
    fn describe_dirscape(v: u64) -> &'static str {
        if v & esc::REACHED_PARENT != 0 {
            "it opened a file that is in the granted directory's PARENT"
        } else if v & esc::REACHED_SIBLING != 0 {
            "it reached the granted directory's SIBLING"
        } else if v & esc::WALKED_UP != 0 {
            "`..` resolved to something"
        } else if v & esc::WIDENED != 0 {
            "a child carried a right its parent did not have"
        } else if v & esc::ENUMERATED_A_STRANGER != 0 {
            "a listing held a name from outside the grant"
        } else if v & esc::FORGED_HANDLE != 0 {
            "it reached something with a handle it was never given"
        } else if v & esc::CREATED != 0 {
            "it created a name through a capability with no create right"
        } else if v & esc::MADE_A_DIR != 0 {
            "it made a directory through a capability that could not"
        } else if v & esc::RENAMED != 0 {
            "it renamed a name through a capability with no remove right"
        } else if v & esc::WROTE != 0 {
            "it wrote through a capability with no write right"
        } else if v & esc::DESCENDED != 0 {
            "it descended through a capability with no descend right"
        } else if v & esc::ENUMERATED != 0 {
            "it enumerated through a capability with no enumerate right"
        } else if v & esc::REACHED_AN_UNMATCHED_NAME != 0 {
            "it reached a name in the granted directory that the grant's set does not carry"
        } else if v & esc::SET_AN_ATTR != 0 {
            "it set an extended attribute through a capability with no write right"
        } else if v & esc::READ_ATTRS != 0 {
            "it read extended attributes through a capability that could not open the file"
        } else if v & esc::GRANTED_ACCESS_FAILED != 0 {
            "the granted access itself failed, so nothing was actually proven"
        } else {
            "nothing (an empty verdict should not have failed an assertion)"
        }
    }

    /// **A read-only explorer: it may descend, read and list, and it reaches nothing above itself.**
    ///
    /// This is milestone 47's keystone under attack. The attacker holds a capability to `sub` and
    /// spends its life trying to name `motd` (which is in the parent), `other` and `other/secret`
    /// (which are in a sibling), and `..`. All three exist on the image and the caretaker one hop
    /// up can reach every one of them on any request it likes, so each refusal is a fact about the
    /// capability rather than about the filesystem.
    ///
    /// It also proves both halves of "a child can never exceed its parent": a child asked for no
    /// rights can do nothing at all, and a child asked for a right this grant does not carry is
    /// refused rather than quietly given something smaller.
    #[test_case]
    fn a_read_only_directory_capability_reaches_its_subtree_and_nothing_above_it() {
        let Some(v) = attack_a_subtree(dir::DESCEND | dir::READ | dir::ENUMERATE, 1) else {
            return;
        };
        assert_verdict(
            v,
            esc::OPENED_ITS_OWN | esc::ENUMERATED | esc::DESCENDED | esc::READ_ATTRS,
            "read-only",
        );
    }

    /// **The same attacker against a capability carrying every right**, which is what makes the run
    /// above mean anything.
    ///
    /// Without it, a `fs_subtree_caretaker` that answered no to everything would pass the read-only
    /// test perfectly, and so would a grant that reached nothing at all. Here the same requests
    /// through the same code succeed: it writes, it creates, it makes a directory. And it *still*
    /// reaches nothing above itself, which is the point: the widening is exactly the axes that were
    /// widened.
    #[test_case]
    fn a_full_directory_capability_does_everything_inside_and_nothing_outside() {
        let Some(v) = attack_a_subtree(dir::ALL, 2) else {
            return;
        };
        assert_verdict(
            v,
            esc::OPENED_ITS_OWN
                | esc::ENUMERATED
                | esc::DESCENDED
                | esc::CREATED
                | esc::WROTE
                | esc::RENAMED
                | esc::MADE_A_DIR
                | esc::READ_ATTRS
                | esc::SET_AN_ATTR,
            "full",
        );
    }

    /// **Milestone 47's motivating sentence, made a test**: a program handed a directory to write
    /// into can add to it and write to what it added, and it cannot walk into a subdirectory or find
    /// out what else is in there.
    ///
    /// Three rungs withheld at once (`DESCEND`, `ENUMERATE`, `REMOVE`), and two of them are the
    /// interesting cases:
    ///
    /// - `DESCEND` withheld while `CREATE` is held: `mkdir` needs both, so this capability can make
    ///   a file and cannot make a directory. A directory it could not have walked into would be a
    ///   way to mint a capability out of a right that was withheld.
    /// - `REMOVE` withheld while `CREATE` is held is **"add to this, destroy nothing"** exactly, and
    ///   it is what `RENAME` exists to make falsifiable: this attacker creates a name and then tries
    ///   to move it, through the same code the full run below moves it with, and cannot. Before that
    ///   verb existed nothing on the wire consulted `REMOVE` at all, so the rung was a claim rather
    ///   than a rule.
    #[test_case]
    fn an_append_only_directory_capability_adds_and_cannot_walk_or_list() {
        let Some(v) = attack_a_subtree(dir::READ | dir::WRITE | dir::CREATE, 3) else {
            return;
        };
        assert_verdict(
            v,
            esc::OPENED_ITS_OWN | esc::CREATED | esc::WROTE | esc::READ_ATTRS | esc::SET_AN_ATTR,
            "append-only",
        );
    }

    /// **A name-set capability carries its files' attributes and still designates only its names**
    /// (milestone 61, the third caretaker).
    ///
    /// The other two caretakers are covered by the runs above and by the per-file attacker.
    /// `fs_nameset_caretaker` is the one that inspects a name on **every** request, so teaching it
    /// four verbs whose operand is an *attribute* name rather than a directory name is where this
    /// milestone could most easily have gone wrong, in either direction:
    ///
    /// - Filter the attribute name against the set, and a program behind `rm *.txt` cannot read what
    ///   is attached to a file the pattern did match. `READ_ATTRS` and `SET_AN_ATTR` catch that.
    /// - Stop filtering the directory names, and the set stops being a set. `REACHED_AN_UNMATCHED`
    ///   `_NAME` catches that, and the witness holds `dir::READ`, which `rm`'s own witness does not,
    ///   so the naming question is asked here through the verbs `rm` never sends.
    ///
    /// The set is one name inside the `sub` fixture, so `deeper` is one directory entry away and the
    /// caretaker one hop up holds a capability that could open it.
    #[test_case]
    fn a_name_set_capability_reads_its_attributes_and_still_names_only_its_set() {
        let Some(report) = fs_service::start_granted_set(
            blk_server_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_nameset_caretaker")
                .expect("no fs_nameset_caretaker program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
            fs_service::SetGrant {
                dir: fs_proto::fixture::tree::SUB,
                // Exactly one name, and `deeper` deliberately left out of it.
                names: &[(fs_proto::fixture::tree::INNER.as_bytes(), false)],
                rights: dir::READ | dir::WRITE | dir::DESCEND,
                role: 6, // ROLE_SET_ATTRS
                arg: 0,
                arg2: 0,
                stack_pages: 0,
            },
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let [tag, v, ..] = sched::ipc_recv(report);
        assert_eq!(
            tag,
            fs_proto::fixture::VERDICT,
            "the name-set witness's report is not a verdict word",
        );
        assert_verdict(
            v,
            esc::OPENED_ITS_OWN | esc::READ_ATTRS | esc::SET_AN_ATTR,
            "name-set",
        );
    }
}

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
mod shell_navigation_tests {
    use fs_proto::dir;
    use fs_proto::fixture::{navscape as nb, tree};

    use super::*;
    use crate::sched;

    /// The `swish` binary's navigating role (`user/src/swish.rs`).
    const ROLE_NAVIGATE: u64 = 1;

    /// The bits every navigating shell must report whatever it was rooted in: `pwd` at its root,
    /// `..` clamped, an absolute path refused, a listing, and the whole `mkdir` / create / `rm`
    /// sequence including the two halves of "unlink is not revoke".
    const ALWAYS: u64 = nb::PWD_IS_ROOT
        | nb::CLAMPED_AT_ROOT
        | nb::ABSOLUTE_REFUSED
        | nb::LISTED
        | nb::CREATED
        | nb::MADE_DIR
        | nb::UNLINKED
        | nb::HOLDER_KEPT_READING
        | nb::NAME_GONE_AFTER_UNLINK
        | nb::UNLINK_REFUSED_A_DIRECTORY
        // The two halves of "no single call takes a subtree away", added with `RMDIR`: a directory
        // with a name in it is refused, and the same call works once the name is out.
        | nb::RMDIR_REFUSED_NON_EMPTY
        | nb::RMDIR_REMOVED_EMPTY;

    /// Wire a `fs_subtree_caretaker` holding a capability to `root` and run the shell's navigation
    /// script inside it. `run` keeps the names it creates distinct across runs sharing one image.
    ///
    /// The run index and the rights ride in one word packed by `fs_proto::grant::spec`, the same
    /// packing the caretaker's own grant uses: the shell is **told** what its capability carries
    /// because nothing on this wire reports what a handle holds, and `OPENDIR` refuses a request
    /// wider than the parent rather than narrowing it (notes/shell-navigation.md).
    ///
    /// `None` when no RedoxFS disk is attached.
    fn navigate(root: &'static str, run: u64) -> Option<u64> {
        let report = fs_service::start_granted_dir(
            dir_capability_tests::blk_server_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_subtree_caretaker")
                .expect("no fs_subtree_caretaker program in the initrd archive"),
            program("swish").expect("no swish program in the initrd archive"),
            fs_service::DirGrant {
                name: root,
                rights: dir::ALL,
                role: ROLE_NAVIGATE,
                arg: fs_proto::grant::spec(run as usize, dir::ALL),
                arg2: 0,
                // Measured, not guessed: see `spawn_fs_client`. A shell carries a path stack, a
                // parsed path and a listing buffer by value, and one page is 192 bytes short.
                stack_pages: 2,
            },
        )?;
        let [tag, verdict, ..] = sched::ipc_recv(report);
        assert_eq!(
            tag,
            fs_proto::fixture::VERDICT,
            "the shell's report is not a verdict word",
        );
        Some(verdict)
    }

    /// Compare a report against the exact set the configuration is specified to produce. Both
    /// directions are failures and they are different ones: a bit set that should not be is a
    /// shell reaching past its root, and a bit clear that should be set is a shell that could not
    /// navigate at all, whose refusals therefore prove nothing.
    fn assert_report(got: u64, want: u64, what: &str) {
        assert_eq!(
            got & !want,
            0,
            "the shell rooted at {what} did something it must not: {}",
            describe(got & !want),
        );
        assert_eq!(
            got & want,
            want,
            "the shell rooted at {what} could not do what its capability allows ({:#x} missing), \
             so every refusal it reported proves nothing",
            want & !got,
        );
    }

    /// Name a bit, so a failure reads as a sentence.
    fn describe(v: u64) -> &'static str {
        if v & nb::WALKED_UP != 0 {
            "`..` climbed out of its root"
        } else if v & (nb::REACHED_SECRET | nb::REACHED_INNER) != 0 {
            "it opened a file that exists only in the OTHER shell's root"
        } else if v & (nb::SAW_SECRET | nb::SAW_INNER) != 0 {
            "its listing held a name from the other shell's root"
        } else if v & nb::NAVIGATION_FAILED != 0 {
            "the navigation itself failed, so nothing was proven"
        } else if v & nb::DESCENDED != 0 {
            "it descended into a directory that is not in its root"
        } else {
            "something no other bit describes"
        }
    }

    /// **A shell navigates the subtree it holds, and cannot climb out of it.**
    ///
    /// `pwd` renders `/` at its root because that is the root of the only namespace it has; `..`
    /// there is refused with nothing sent, because `..` is a pop of the stack of capabilities the
    /// shell descended through and at the root there is nothing to pop; an absolute path is refused
    /// as *unnameable*, since there is no namespace to root one in. Then the verbs that change the
    /// tree, including the one this milestone insists on separating: `rm` removes the **name**, the
    /// handle the shell still holds keeps reading the bytes, and the name really is gone.
    #[test_case]
    fn a_shell_navigates_its_own_subtree_and_clamps_at_its_root() {
        let Some(v) = navigate(tree::SUB, 4) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        assert_report(
            v,
            ALWAYS | nb::REACHED_INNER | nb::SAW_INNER | nb::DESCENDED | nb::RETURNED,
            "sub",
        );
    }

    /// **The headline: every shell has its own root, and neither can name the other's files.**
    ///
    /// Two shells, two subtrees of one image, one script. Each tries to open `sub/inner` and
    /// `other/secret` and reports which it reached, and it is **told nothing about which subtree it
    /// was rooted in**, so the property is read off the pair rather than claimed by either. The
    /// listings are checked the same way, because a listing is a rendering of authority: a name from
    /// the other shell's root appearing in one would be an escape even though nothing was opened.
    ///
    /// Not by policy. The FS server can reach both directories on any request it likes, and the
    /// caretakers one hop up hold the whole image root. What stops each shell is that **no
    /// capability reaching the other subtree exists in its cspace**, which is why the two runs are
    /// sequential and it costs nothing: they are separate processes with separate roots, and being
    /// alive at the same instant would prove no more than this does (they share one page with the
    /// FS server, so the harness runs them in turn).
    #[test_case]
    fn two_shells_with_different_roots_cannot_name_each_others_files() {
        let Some(a) = navigate(tree::SUB, 5) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let b = navigate(tree::OTHER, 6).expect("the service was wired for the first shell");

        assert_report(
            a,
            ALWAYS | nb::REACHED_INNER | nb::SAW_INNER | nb::DESCENDED | nb::RETURNED,
            "sub",
        );
        // The second holds a subtree with no child directory in it, so it cannot descend, and that
        // difference is the point: the same script against a different capability does different
        // things, and neither shell's world contains the other's.
        assert_report(b, ALWAYS | nb::REACHED_SECRET | nb::SAW_SECRET, "other");

        // Stated once more as the crossing, because that is the sentence the milestone makes and an
        // exact-set assertion is easy to read as a list of unrelated facts.
        assert_eq!(
            (a & nb::REACHED_SECRET, b & nb::REACHED_INNER),
            (0, 0),
            "a shell named a file in the other shell's root",
        );
        assert_ne!(a & nb::REACHED_INNER, 0);
        assert_ne!(b & nb::REACHED_SECRET, 0);
    }
}

/// **`rm` as a program, and a recursive removal bounded by the capability it was handed**
/// (milestone 47's `rm -r`; notes/rm-recursion.md).
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
mod rm_program_tests {
    use fs_proto::dir;
    use fs_proto::fixture::{VERDICT, rm as rr, tree};
    use grant_plan::rmopt;

    use super::*;
    use crate::sched;

    /// The most messages one run may send before the harness stops reading. A `rm` that never sent
    /// its verdict would otherwise hang the boot; this turns it into a failed assertion.
    const MAX_MESSAGES: usize = 64;

    /// What one run of the program reported.
    struct Outcome {
        /// Its exit status: 0, or the errno of the first failure (`fs_proto::fixture::rm`).
        status: u64,
        /// How many names it removed.
        removed: u64,
        /// **How many lines of text it printed before the verdict.** Zero is the interesting value:
        /// `rm(1)` is silent on success, and a `SEND` blocks until somebody receives it, so a line
        /// that was emitted cannot be missed by looking after the fact.
        printed: usize,
    }

    /// Run `rm` once inside a `fs_subtree_caretaker` holding [`tree::RMTREE`] with exactly
    /// `rights`, told to remove `name` with `flags`.
    ///
    /// `None` when no RedoxFS disk is attached (nothing to test; do not fail).
    fn run_rm(rights: u64, name: &str, flags: u64) -> Option<Outcome> {
        assert!(
            fs_proto::grant::fits(name.as_bytes()),
            "the operand rides in two argument words",
        );
        let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
        let report = fs_service::start_granted_dir(
            dir_capability_tests::blk_server_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_subtree_caretaker")
                .expect("no fs_subtree_caretaker program in the initrd archive"),
            program("rm").expect("no rm program in the initrd archive"),
            fs_service::DirGrant {
                name: tree::RMTREE,
                rights,
                // `rm` is started with a **grant's** three words rather than a role and a number:
                // the spec (the operand's length, and the options where a caretaker's rights ride),
                // then the two words of name.
                role: fs_proto::grant::spec(name.len(), flags),
                arg: lo,
                arg2: hi,
                // Measured the way `spawn_fs_client` asks for: the recursion is real stack, each
                // level holding a listing buffer by value, and this program has no allocator.
                stack_pages: 6,
            },
        )?;

        // `printed` is the enumeration index rather than a counter, and the two coincide exactly:
        // the verdict arm returns *before* the increment, so on the iteration that sees it, the
        // index is the number of text frames that arrived first. That is the quantity being
        // reported, so this is not merely appeasing the lint.
        for (printed, _) in (0..MAX_MESSAGES).enumerate() {
            let [w0, w1, w2, _, _] = sched::ipc_recv(report);
            if w0 == VERDICT {
                return Some(Outcome {
                    status: w1,
                    removed: w2,
                    printed,
                });
            }
            // A text frame: its first word is a byte count, which cannot collide with the verdict
            // word. That is what makes "it printed nothing" an assertion rather than a hope.
            assert!(w0 <= 16, "neither a verdict nor a text frame: {w0:#x}");
        }
        panic!("rm sent {MAX_MESSAGES} messages and never a verdict");
    }

    /// **The headline: `rm -r` removes the subtree it was granted, and a narrower capability
    /// cannot begin.**
    ///
    /// Three runs against one tree, in order, because they are one argument:
    ///
    /// 1. `rm rm-doomed` with the full grant is **`EISDIR` and nothing removed**. A directory is a
    ///    refusal, never a silent escalation to recursive removal, which is `rm(1)`'s behaviour and
    ///    is what makes `-r` mean something.
    /// 2. `rm -r rm-doomed` through a capability carrying only `REMOVE` **cannot even look**: the
    ///    `OPENDIR` is `ENOENT`, because a naming right withheld answers "in this scope there is no
    ///    such name" (DECISIONS §47). The tree is still there. Nothing in the program decided this;
    ///    the capability to walk was never handed over.
    /// 3. The same command through a capability carrying `ENUMERATE｜DESCEND｜REMOVE` takes the
    ///    whole tree, bottom-up, five names.
    ///
    /// Runs 1 and 2 are what make run 3 more than "a loop deleted some files": the *same binary*,
    /// the *same operand*, and the outcome is decided by what was in the cspace.
    #[test_case]
    fn rm_r_takes_the_subtree_it_was_granted_and_a_narrower_grant_cannot_begin() {
        let Some(refused) = run_rm(dir::REMOVE_TREE, tree::RM_DOOMED, 0) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        assert_eq!(
            refused.status,
            rr::status(dir::EISDIR),
            "`rm` of a directory must refuse, not quietly recurse",
        );
        assert_eq!(refused.removed, 0);
        assert!(
            refused.printed > 0,
            "a failure is a diagnostic AND an exit status; this one printed nothing",
        );

        // The narrow grant: it may take names out of `rmtree` and may not walk into anything.
        let blind = run_rm(dir::REMOVE, tree::RM_DOOMED, rmopt::RECURSIVE)
            .expect("the service was wired by the first run");
        assert_eq!(
            blind.status,
            rr::status(2), // ENOENT
            "a capability that may not descend must not learn that the subtree is there",
        );
        assert_eq!(
            blind.removed, 0,
            "a `rm -r` that could not walk must not have removed anything on the way",
        );

        // And the same line through the grant that carries the walk.
        let done = run_rm(
            dir::REMOVE_TREE,
            tree::RM_DOOMED,
            rmopt::RECURSIVE | rmopt::VERBOSE,
        )
        .expect("the service was wired by the first run");
        assert_eq!(done.status, rr::OK, "the removal reported a failure");
        assert_eq!(
            done.removed, 5,
            "two files, a leaf, the directory holding it, and the top: five names",
        );
        assert_eq!(
            done.printed, 5,
            "`-v` prints one line per name removed, and nothing else",
        );
    }

    /// **`-f` is idempotency, and silence is the default.** Both are `rm(1)`'s, checked against the
    /// man page rather than remembered.
    ///
    /// - `rm rm-nothing` on a name that is not there is a diagnostic and a non-zero status.
    /// - `rm -f rm-nothing` is **neither**: "if the file does not exist, do not display a diagnostic
    ///   message or modify the exit status". That is what makes a script re-runnable, and "absence
    ///   is the desired state" is not a lie about failure.
    /// - `rm rm-solo` on a name that *is* there removes it and **prints nothing at all**, which is
    ///   why `-v` exists. Without this run the `-f` claim above is equally true of a program that
    ///   never prints.
    ///
    /// Its names are its own, so this run and the recursive one above cannot depend on each other's
    /// order.
    #[test_case]
    fn rm_f_ignores_a_name_that_is_not_there_and_success_says_nothing() {
        let Some(loud) = run_rm(dir::REMOVE_TREE, tree::RM_MISSING, 0) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        assert_eq!(loud.status, rr::status(2), "a missing name is ENOENT");
        assert!(loud.printed > 0, "and it says so");

        let quiet = run_rm(dir::REMOVE_TREE, tree::RM_MISSING, rmopt::FORCE)
            .expect("the service was wired by the first run");
        assert_eq!(
            quiet.status,
            rr::OK,
            "`-f` must not modify the exit status for a name that is not there",
        );
        assert_eq!(quiet.printed, 0, "`-f` suppresses the diagnostic too");
        assert_eq!(quiet.removed, 0, "there was nothing to remove");

        let did = run_rm(dir::REMOVE_TREE, tree::RM_SOLO, 0)
            .expect("the service was wired by the first run");
        assert_eq!(did.status, rr::OK);
        assert_eq!(did.removed, 1);
        assert_eq!(
            did.printed, 0,
            "success prints nothing; `-v` is the option that changes that",
        );
    }
}

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
mod glob_grant_tests {
    use fs_proto::dir;
    use fs_proto::fixture::{VERDICT, globscape as gb, rm as rr, tree};

    use super::*;
    use crate::sched;

    /// The `swish` binary's globbing role (`user/src/swish.rs`).
    const ROLE_GLOB: u64 = 2;

    /// The most messages one `rm` run may send before the harness stops reading, as
    /// [`rm_program_tests`] bounds it and for the same reason.
    const MAX_MESSAGES: usize = 64;

    /// The set `gl-*.txt` matches in the fixture, which `fs_proto`'s host test pins against the
    /// matcher. Stated literally here because building a grant is the one thing the kernel can do
    /// without an enumeration, and proved to *be* the expansion over there.
    const MATCHED: [(&[u8], bool); 2] = [
        (tree::GLOB_ONE.as_bytes(), false),
        (tree::GLOB_TWO.as_bytes(), false),
    ];

    /// **The two phases are one test**, and the order is load-bearing rather than incidental: the
    /// shell expands the pattern over the fixture as staged, and the second phase then removes what
    /// it matched. Run the other way round, the expansion would find nothing and the agreement would
    /// be an agreement about the empty set.
    ///
    /// Splitting them into two `#[test_case]`s would make that ordering a property of the harness,
    /// which is exactly the order-coupled fixture DECISIONS §27 spent a day on.
    #[test_case]
    fn what_a_shell_shows_is_what_a_set_grant_takes_away() {
        let Some(shown) = shell_expanded() else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        assert_shell_agreed(shown);
        a_set_capability_cannot_name_a_stranger();
        the_set_grant_removes_exactly_the_match();
    }

    /// Phase one: a shell rooted in `globset` runs `echo gl-*.txt` and plans `rm gl-*.txt`, and
    /// reports whether the two produced the same names.
    ///
    /// The rights it is granted are what expansion actually costs: `ENUMERATE` to list the
    /// directory, `DESCEND` because the shell walks a path with `OPENDIR`, and `READ` so a listing
    /// is not the only thing it could ever do. There is no `REMOVE` in it at all, which is the point
    /// of `echo` being the half that demonstrates this: **showing the authority costs none of it.**
    fn shell_expanded() -> Option<u64> {
        let report = fs_service::start_granted_dir(
            dir_capability_tests::blk_server_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_subtree_caretaker")
                .expect("no fs_subtree_caretaker program in the initrd archive"),
            program("swish").expect("no swish program in the initrd archive"),
            fs_service::DirGrant {
                name: tree::GLOBSET,
                rights: dir::ENUMERATE | dir::DESCEND | dir::READ,
                role: ROLE_GLOB,
                // The shell is told what its capability carries, for notes/shell-navigation.md's
                // reason: nothing on this wire reports what a handle holds.
                arg: fs_proto::grant::spec(0, dir::ENUMERATE | dir::DESCEND | dir::READ),
                arg2: 0,
                // **Four, and the number is a measurement**, as the navigating role's two were.
                // Two overflowed by 256 bytes, presenting as a data abort on the shell's own `sp`
                // and then as the 60 s lost-wakeup watchdog, because the test was still waiting for
                // a report from a process that had died.
                //
                // The cost is a name set travelling **by value** through a chain of frames a debug
                // build does not collapse: the expander holds one, `Expansion` carries one into
                // `plan`, `designate` returns one, and the `Endowment` that comes back carries one
                // more. That measurement is also what set `nameset::MAX_NAMES`: sixteen names did
                // not fit in the four pages this wiring maps, and `CLIENT_EXTRA_STACK`'s note is
                // right that the answer to that is smaller frames rather than a bigger number.
                stack_pages: 6,
            },
        )?;
        let [tag, verdict, ..] = sched::ipc_recv(report);
        assert_eq!(tag, VERDICT, "the shell's report is not a verdict word");
        Some(verdict)
    }

    fn assert_shell_agreed(v: u64) {
        assert_eq!(
            v & gb::GLOB_FAILED,
            0,
            "the shell could not plan the grant at all, so nothing it reported means anything",
        );
        let want = gb::EXPANDED
            | gb::AGREED
            | gb::EXCLUDED_A_STRANGER
            | gb::NO_MATCH_REFUSED
            | gb::PATTERN_IN_PATH_REFUSED
            | gb::TEXT_UNTOUCHED;
        assert_eq!(
            v, want,
            "the globbing witness reported {v:#x}, wanted {want:#x}",
        );
    }

    /// Run `rm` once behind a `fs_nameset_caretaker` holding [`MATCHED`] inside `globset`.
    ///
    /// `name` empty means [`fs_proto::grant::WHOLE_NAMESPACE`]: the operand is the set, and `rm`
    /// learns it by enumerating the capability it was handed.
    fn run_rm(name: &str, flags: u64) -> (u64, u64) {
        let (lo, hi) = fs_proto::grant::pack_name(name.as_bytes());
        let report = fs_service::start_granted_set(
            dir_capability_tests::blk_server_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_nameset_caretaker")
                .expect("no fs_nameset_caretaker program in the initrd archive"),
            program("rm").expect("no rm program in the initrd archive"),
            fs_service::SetGrant {
                dir: tree::GLOBSET,
                names: &MATCHED,
                // **`REMOVE` and nothing else.** `rm *.txt` takes names out of one directory; it may
                // not read them, write them, create beside them or walk under them. Listing the set
                // is not on this ladder at all, because the caretaker answers that from the grant.
                rights: dir::REMOVE,
                role: fs_proto::grant::spec(name.len(), flags),
                arg: lo,
                arg2: hi,
                stack_pages: 6,
            },
        )
        .expect("the FS service was wired by the shell phase");

        for _ in 0..MAX_MESSAGES {
            let [w0, w1, w2, _, _] = sched::ipc_recv(report);
            if w0 == VERDICT {
                return (w1, w2);
            }
            assert!(w0 <= 16, "neither a verdict nor a text frame: {w0:#x}");
        }
        panic!("rm sent {MAX_MESSAGES} messages and never a verdict");
    }

    /// **The attacker, and it is `rm` itself.** Told to remove a name that exists, sits one
    /// directory entry away from the two it was granted, and that the caretaker one hop up could
    /// remove on any request it liked.
    ///
    /// It gets `ENOENT`: in this scope there is no such name. Nothing consulted a permission, and
    /// nothing in `rm` decided not to try, which is what makes this a fact about the capability.
    fn a_set_capability_cannot_name_a_stranger() {
        let (status, removed) = run_rm(tree::GLOB_MISS, 0);
        assert_eq!(
            status,
            rr::status(2), // ENOENT
            "a name outside the set must not be nameable through a set capability",
        );
        assert_eq!(removed, 0);
    }

    /// **And the grant works**, which is what stops the refusal above being equally true of a
    /// capability that reaches nothing. Two names in, two names removed, and the two the pattern did
    /// not match are still on the disk, asserted from the host by
    /// `xtask::redoxfs_glob_grant_took_exactly_the_match`.
    fn the_set_grant_removes_exactly_the_match() {
        let (status, removed) = run_rm("", 0);
        assert_eq!(status, rr::OK, "the set grant reported a failure");
        assert_eq!(
            removed,
            MATCHED.len() as u64,
            "a set grant must remove every name in it, and only those",
        );
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
    use core::sync::atomic::Ordering;

    // Shared with the aarch64 module so both ISAs assert a std transcript the same way.
    use super::std_tests::{
        assert_a_kill_mid_transaction_recovers, assert_attrs, assert_fs_service_ready,
        assert_std_transcript, std_fs_expected,
    };
    use super::*;
    use crate::sched;

    /// The `blk` driver's ELF bytes from the riscv initrd archive. Absent means the initrd was
    /// built without it (or the aarch64 archive was handed to a riscv boot, the mix-up the xtask
    /// riscv test leg exists to prevent); fail loudly rather than skip.
    fn blk_image() -> &'static [u8] {
        program("blk").expect("no blk program in the initrd archive")
    }

    /// The `net_stack` program's ELF bytes (milestone 30, piece 3): the smoltcp net server.
    fn net_stack_image() -> &'static [u8] {
        program("net_stack").expect("no net_stack program in the initrd archive")
    }

    /// The net client's test selectors and success word, matching `user/src/socket_test_client.rs`. The client is
    /// a nonzero entry role of the `net_stack` binary, so it needs no image of its own.
    const NET_TEST_UDP_DNS: u64 = 1;
    const NET_TEST_TCP_ECHO: u64 = 2;
    const NET_TEST_TCP_REOPEN: u64 = 3;
    const NET_TEST_UDP_TFTP: u64 = 4;
    const NET_CLIENT_OK: u64 = 1;
    /// The client could not complete for an ENVIRONMENTAL reason (the host resolver never answered),
    /// not because of a defect here. Only the non-gating real-DNS check can report it.
    const NET_CLIENT_NO_ANSWER: u64 = 2;

    /// Spin the scheduler until `done()`, or give up after a wall-clock deadline. A second copy of
    /// the `tests` module's helper, one of the small duplications this module carries; time-based
    /// for the same reason (DECISIONS §28: a fixed yield count elapses in no real time on an idle
    /// core).
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

    /// **The confused-deputy question, asked on the ISA where the answer is harder** (the RISC-V
    /// twin of `the_hardware_says_el0_cannot_read_the_kernels_memory`, milestone 41).
    ///
    /// aarch64 has `AT S1E0R`, one instruction that makes the silicon answer "could EL0 read
    /// this". RISC-V has no such instruction, so `mmu::user_can_read` walks the installed tables
    /// in software and reads the `U` bit. That software answer is the thing under test, and it
    /// matters more here than on aarch64: RISC-V has one root register, so the **same** page
    /// tables that translate a user address also translate the kernel's, and the `U` bit is the
    /// only thing between U-mode and the kernel's own memory. On aarch64 the split TTBR0/TTBR1
    /// gives a second, structural line of defence. Here there is one line, and this is it.
    ///
    /// The precondition assertion is the same one the aarch64 test carries, for the same reason:
    /// "U-mode cannot read the kernel" proves nothing if the kernel is not mapped here at all.
    /// On RISC-V it also proves the kernel-half share actually happened.
    ///
    /// This test exists because milestone 41 removed the crate-wide `allow(dead_code)` that riscv64
    /// builds carried, and `user_can_read`/`user_can_write` fell out of it as dead on this ISA
    /// only: the aarch64 test module proved them, and nothing on RISC-V ever called them.
    #[test_case]
    fn the_page_tables_say_u_mode_cannot_read_the_kernels_memory() {
        // Inside the direct map, so it is mapped for certain and it is the kernel's own memory.
        let kernel_va = crate::arch::mmu::KERNEL_VA_BASE + 0x8000_0000;

        let image = program("init").expect("no init program in the initrd archive");
        let (space, _) = load(image).expect("the initrd did not load");

        // SAFETY: nothing is at U-mode; we are a kernel thread mid-test, and the root carries the
        // kernel half (else this instruction would not retire).
        unsafe { mmu::activate_user(space.ttbr0()) };

        assert!(
            mmu::translate(kernel_va).is_some(),
            "the kernel's direct map is not mapped, so this test proves nothing",
        );
        assert!(
            !mmu::user_can_read(kernel_va),
            "the page tables say U-mode could read the kernel's own memory",
        );
        assert!(!mmu::user_can_write(kernel_va));

        // And it says yes to the process's own text, or the check is a rubber stamp.
        assert!(
            mmu::user_can_read(0x40_0000),
            "U-mode cannot read its own .text, so the check refuses everything and proves nothing",
        );

        // Not an unmapped address in its own half.
        assert!(!mmu::user_can_read(0x7000_0000));

        mmu::deactivate_user();
        drop(space);
    }

    /// The headline, on the second ISA: an unprivileged process drives a real block device over
    /// DMA and reads a file off it, with the kernel owning only the confinement. Interrupt
    /// delivery is asserted too: the completion reached the driver as a message through its Irq
    /// capability, via the PLIC rather than the GIC.
    #[test_case]
    fn a_userspace_driver_reads_a_file_from_a_virtio_disk() {
        use crate::arch::exceptions::ROUTED_IRQS;

        let Some(report) = virtio_service::start(blk_image()) else {
            // No disk attached to this run. Nothing to test; do not fail.
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
        let Some((readiness, report)) = fs_service::start(
            blk_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
            0, // the end-to-end proof role, not the benchmark loop
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };

        assert_fs_service_ready(readiness);
        let [head, status, attrs, ..] = sched::ipc_recv(report);
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
        // Milestone 57: the same attribute witness, the same exact expected set. The layer is the
        // FS server's own logic rather than anything arch-specific, so what this leg adds is the
        // parity gate's own claim (DECISIONS §19): it ships on both ISAs or it does not ship.
        assert_attrs(attrs);
    }

    /// **`std::fs` over the FS-service contract, on the second ISA** (milestone 27 phase two, the
    /// parity gate). The aarch64 twin's proof, same binary, same contract, same transcript: a std
    /// program granted one directory capability reads the file the RedoxFS image ships and is
    /// refused every path that would leave that directory. See the aarch64 twin for what it proves.
    #[test_case]
    fn std_fs_reads_a_file_through_a_granted_directory_capability() {
        let Some((readiness, report)) = fs_service::start_std(
            blk_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("std_exerciser").expect("no std_exerciser program in the initrd archive"),
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        assert_fs_service_ready(readiness);

        let mut want = [0u8; 512];
        let n = std_fs_expected(&mut want);
        assert_std_transcript(report, &want[..n], "std fs");
    }

    /// **A read-only per-file grant, attacked, on the second ISA** (the parity gate). The aarch64
    /// twin carries the reasoning: same caretaker, same attacker, same verdict.
    ///
    /// Milestone 61's attribute forwarding is inside this zero, in both directions: the attribute
    /// reads reached the store (`GRANTED_ATTRS_FAILED` clear) and the attribute write did not
    /// (`WROTE_ATTR` clear). Spelled out in the aarch64 twin's second assertion; here the exact
    /// verdict carries it, which is what the parity gate wants (§19: the same suite, both ISAs).
    #[test_case]
    fn a_read_only_per_file_grant_survives_an_attacker() {
        let Some(verdict) = attack_a_grant(fs_proto::grant::READ, false) else {
            return;
        };
        assert_eq!(
            verdict, 0,
            "the read-only per-file grant leaked (see the aarch64 twin for what each bit means)",
        );
    }

    /// **A writable per-file grant, attacked, on the second ISA** (the parity gate, and the read-only
    /// test's control here too). See the aarch64 twin.
    #[test_case]
    fn a_writable_per_file_grant_writes_that_file_and_still_only_that_file() {
        use fs_proto::fixture::escape;
        let Some(verdict) = attack_a_grant(fs_proto::grant::READ | fs_proto::grant::WRITE, true)
        else {
            return;
        };
        assert_eq!(
            verdict,
            escape::WROTE | escape::TRUNCATED | escape::WROTE_ATTR,
            "a writable grant must write, truncate and set an attribute on its own file and do \
             nothing else",
        );
    }

    /// The riscv half of the aarch64 twin's helper; the only difference is the block-server binary
    /// (the portable `blk` here, the PL011-tied `hello` there).
    fn attack_a_grant(rights: u64, writable: bool) -> Option<u64> {
        let Some(report) = fs_service::start_granted(
            blk_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_file_caretaker")
                .expect("no fs_file_caretaker program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
            fs_service::Grant {
                name: if writable {
                    fs_proto::fixture::SCRATCH_NAME
                } else {
                    fs_proto::fixture::MOTD_NAME
                },
                rights,
                role: 2, // ROLE_ATTACKER
                arg: writable as u64,
            },
        ) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return None;
        };
        // The two handshakes happened inside `start_granted`, before this attacker existed: they
        // are what makes the caretaker's own staged request safe on a page all three share.
        let [tag, verdict, ..] = sched::ipc_recv(report);
        assert_eq!(
            tag,
            fs_proto::fixture::VERDICT,
            "the attacker's report is not a verdict word",
        );
        Some(verdict)
    }

    /// **The FS server's stack headroom, on the second ISA** (the parity gate). The aarch64 twin
    /// carries the reasoning; the number is worth having on both because the two ISAs do not use the
    /// same amount of stack for the same recursion, and a size proven on one proves nothing on the
    /// other.
    /// **A kill mid-transaction, on the real device, on the second ISA** (milestone 37, the §19
    /// parity twin of the aarch64 test). Same six steps, same property, same words.
    #[test_case]
    fn a_kill_mid_transaction_leaves_the_filesystem_consistent() {
        assert_a_kill_mid_transaction_recovers(
            blk_image(),
            program("fs_server").expect("no fs_server program in the initrd archive"),
            program("fs_test_client").expect("no fs_test_client program in the initrd archive"),
        );
    }

    #[test_case]
    fn the_fs_servers_stack_still_has_headroom() {
        let Some((used, total)) = fs_service::fs_stack_used() else {
            crate::println!("    (no FS service wired this boot; skipping)");
            return;
        };
        crate::println!("    (FS server stack high-water: {used} of {total} bytes) ");
        assert!(
            used * 4 <= total * 3,
            "the FS server used {used} of {total} stack bytes: under a quarter left. RedoxFS \
             recurses in 8 KiB frames, so the next verb that deepens a tree walk will overflow and \
             the server will die mid-request. Raise FS_STACK_PAGES.",
        );
    }

    /// The virtio-net DHCP round trip, on the second ISA (milestone 30): a driver at EL0 brings up
    /// both queues, transmits a DHCP DISCOVER, and receives slirp's OFFER, all behind the multi-queue
    /// confinement, with the completion delivered via the PLIC. Parity with the aarch64 net test.
    #[test_case]
    fn a_userspace_driver_completes_a_dhcp_round_trip_over_virtio_net() {
        let Some(report) = virtio_service::start_net(blk_image()) else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
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
        let Some(report) = virtio_service::start_net_pci(blk_image()) else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
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
        let Some(report) = virtio_service::start_net_server(net_stack_image()) else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
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
        let Some(report) = virtio_service::start_net_server_pci(net_stack_image()) else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
        };
        let addr = sched::ipc_recv(report)[0] as u32;
        assert_eq!(
            addr & 0xffff_ff00,
            0x0A00_0200,
            "smoltcp's DHCP lease {addr:#010x} over PCIe is not in QEMU slirp's 10.0.2.0/24",
        );
    }

    /// The socket contract, UDP end to end on the second ISA (milestone 30, piece 3 phase B): a
    /// client sends a datagram and reads the reply through the granted `Stack` endpoint and shared
    /// frame. The peer is slirp's own TFTP server, so the exchange is deterministic and offline; see
    /// the aarch64 twin for why the old DNS-based version was environment-dependent.
    #[test_case]
    fn a_client_completes_a_udp_round_trip_through_the_socket_contract() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_UDP_TFTP, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP round trip against slirp's TFTP server failed (client code {verdict:#x})",
        );
    }

    /// The riscv UDP round trip over PCIe, behind the RISC-V IOMMU.
    #[test_case]
    fn a_client_completes_a_udp_round_trip_through_the_socket_contract_pci() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_UDP_TFTP, true)
        else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "the UDP round trip over PCIe failed (client code {verdict:#x})",
        );
    }

    /// Real DNS resolution on the second ISA, non-gating for the same reason as the aarch64 twin: the
    /// upstream is the host's resolver, so a non-answer is skipped and only a malformed reply fails.
    #[test_case]
    fn a_client_resolves_a_real_dns_name_when_the_host_resolver_answers() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_UDP_DNS, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        if verdict == NET_CLIENT_NO_ANSWER {
            crate::println!(
                "    (the host's resolver did not answer; real-DNS check skipped, not a failure)"
            );
            return;
        }
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "a DNS response came back but was not a valid reply to our query (client code \
             {verdict:#x}): a socket-contract defect, not a network problem",
        );
    }

    /// The socket contract, TCP end to end on the second ISA: connect to slirp's guestfwd echo peer,
    /// send, receive the echo, close, the full round trip through the confined NIC.
    #[test_case]
    fn a_client_echoes_over_tcp_through_the_socket_contract() {
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_TCP_ECHO, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
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
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_TCP_ECHO, true)
        else {
            crate::println!("    (no virtio-net-pci device attached; skipping)");
            return;
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
        let Some(report) =
            virtio_service::start_net_stack(net_stack_image(), NET_TEST_TCP_REOPEN, false)
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };
        let verdict = sched::ipc_recv(report)[0];
        assert_eq!(
            verdict, NET_CLIENT_OK,
            "reopening a socket id and connecting again failed (client code {verdict:#x})",
        );
    }

    /// The `std_exerciser` std program's ELF bytes from the riscv initrd. Given the network here, its
    /// `UdpSocket::bind` probe succeeds and it runs the net transcript.
    fn std_exerciser_image() -> &'static [u8] {
        program("std_exerciser").expect("no std_exerciser program in the initrd archive")
    }

    /// The exact transcript `std_exerciser` prints when it is granted the network.
    const STD_NET_EXPECTED: &[u8] = b"std net on cricker-os\nudp ok\ntcp echo ok\n";

    /// **`std::net` end to end over the socket contract, on the second ISA** (milestone 27 phase
    /// two): the riscv twin of the aarch64 std-net test. The `std_exerciser` std binary, given the
    /// network, does a real UDP DNS query and a TCP echo round trip through `std::net`, whose PAL
    /// binds to `net_stack`'s contract, proving std's networking runs on the native ABI on both
    /// architectures (the §19 parity gate). Its stdout is reassembled and compared byte for byte.
    #[test_case]
    fn std_net_runs_over_the_socket_contract() {
        let Some(report) = virtio_service::start_net_std(net_stack_image(), std_exerciser_image())
        else {
            crate::println!("    (no virtio-net device attached; skipping)");
            return;
        };

        assert_std_transcript(report, STD_NET_EXPECTED, "std net");
    }

    /// The DMA confinement holds on riscv: a descriptor aimed at kernel memory is refused and
    /// the device is never rung. The attacker reports `1` (refused).
    #[test_case]
    fn the_kernel_refuses_a_dma_descriptor_that_escapes_the_drivers_region() {
        let Some(report) = virtio_service::start_attacker(blk_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
        let Some(report) = virtio_service::start_attacker_indirect(blk_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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

        let Some(report) = virtio_service::start_pci(blk_image()) else {
            crate::println!("    (no virtio-pci disk on the bus; skipping)");
            return;
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
        let Some(report) = virtio_service::start_writer(blk_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
        let Some(report) = virtio_service::start_writer_pci(blk_image()) else {
            crate::println!("    (no virtio-pci disk on the bus; skipping)");
            return;
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
        let Some(report) = virtio_service::start_write_abandoner(blk_image()) else {
            crate::println!("    (no virtio disk attached; skipping)");
            return;
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
pub mod pipeline_service {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::cap::{Rights, endpoint_cap, untyped_cap};
    use crate::sched::EpId;

    /// The `swish` binary's pipeline role (`user/src/swish.rs`).
    const ROLE_PIPELINE: u64 = 3;

    /// The VAs the shell hardcodes for its terminal pages. Must match user/src/swish.rs.
    const OUT_VA: u64 = 0x0000_0000_00c0_0000;
    const LINE_VA: u64 = 0x0000_0000_00b0_0000;

    /// The budget the shell mints its pipes out of. Each pipeline splits a region off this and
    /// gives it back, so one number covers a script of several lines; it matches `system_initializer`'s grant.
    const SH_BUDGET_PAGES: u64 = 128;

    /// Pages of stack **below** the one `run` maps. A shell needs more than a hand-sized program
    /// for `spawn_fs_client`'s measured reason, and milestone 50 added an array of planned
    /// endowments to what it carries by value.
    const SHELL_EXTRA_STACK: usize = 6;

    /// What the kernel holds of a scripted shell.
    pub struct Wiring {
        /// The terminal endpoint the shell `CALL`s. The test serves it.
        pub term: EpId,
        /// Where the shell's printed bytes land, so the test can read them out.
        pub out_phys: u64,
    }

    /// The collected transcript. A static because the terminal service and the assertions are the
    /// same thread and the buffer outlives one call; sized for the whole script with room to spare,
    /// so a transcript that ran long is truncated rather than overrunning.
    static TRANSCRIPT: spin::Mutex<[u8; 4096]> = spin::Mutex::new([0; 4096]);
    static WRITTEN: AtomicUsize = AtomicUsize::new(0);

    /// **Wire a scripted shell and the init service behind it.**
    ///
    /// The endowment is deliberately the interactive one, slot for slot, because a witness with a
    /// wider endowment would be proving something about a shell nobody runs.
    pub fn start() -> Option<Wiring> {
        start_with(ROLE_PIPELINE, 0, None)
    }

    /// **The same shell, one capability wider**: a directory at slot 4 and the page it shares with
    /// the FS server (milestone 50's `>` and `<`).
    ///
    /// This is what makes the pair of witnesses worth having. [`start`] and this call spawn the same
    /// ELF from the same archive with the same four slots, and the only difference between a
    /// `date > report.txt` that is refused and one that writes a file is whether slot 4 is
    /// occupied. Neither behaviour is a branch in the program; both are facts about a cspace.
    ///
    /// `dir` is `(the narrowed directory endpoint, the physical frame it shares with the FS server)`,
    /// which is what `fs_service::narrow_dir` hands back.
    pub fn start_redirecting(dir: (EpId, u64), rights: u64) -> Option<Wiring> {
        start_with(ROLE_REDIRECT, rights, Some(dir))
    }

    /// The shell's redirection role (`user/src/swish.rs`).
    const ROLE_REDIRECT: u64 = 4;

    /// Where an FS client maps the page it shares with the FS server (`fs_service`'s
    /// `FILE_VA_CLIENT`, and `user/src/swish.rs`'s `FS_VA`).
    const FS_VA: u64 = 0x0000_0000_0060_0000;

    fn start_with(role: u64, arg: u64, dir: Option<(EpId, u64)>) -> Option<Wiring> {
        let image = program("swish")?;
        let term = crate::sched::create_endpoint();
        let spawn_ep = crate::sched::create_endpoint();
        let result = crate::sched::create_endpoint();
        let budget = crate::untyped::create(SH_BUDGET_PAGES)?;
        let out_phys = crate::memory::alloc()?.addr();
        let line_phys = crate::memory::alloc()?.addr();
        // SAFETY: two freshly allocated frames, named through the direct map, owned by nobody yet.
        unsafe {
            core::ptr::write_bytes(
                mmu::phys_to_virt(out_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
            core::ptr::write_bytes(
                mmu::phys_to_virt(line_phys) as *mut u8,
                0,
                FRAME_SIZE as usize,
            );
        }
        WRITTEN.store(0, Ordering::SeqCst);

        // init first, so a shell that spawns before the service is listening merely blocks in a
        // rendezvous rather than failing.
        crate::sched::spawn(move || init_service(spawn_ep, result))?;

        crate::sched::spawn(move || {
            // The extra stack goes in as ordinary mappings below the one `run` maps, which is the
            // shape `spawn_fs_client` uses and for the same measured reason: a shell carries a path
            // stack, a parsed line and now an array of planned endowments, all by value.
            let mut maps = [Mapping {
                va: OUT_VA,
                phys: out_phys,
                flags: Flags::user_data(),
            }; 3 + SHELL_EXTRA_STACK];
            maps[1] = Mapping {
                va: LINE_VA,
                phys: line_phys,
                flags: Flags::user_rodata(),
            };
            for (k, m) in maps[2..2 + SHELL_EXTRA_STACK].iter_mut().enumerate() {
                m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
                m.phys = crate::memory::alloc()
                    .expect("no frame for the shell's stack")
                    .addr();
                m.flags = Flags::user_data();
            }
            // The FS page last, so a shell wired without a filesystem maps exactly what it did
            // before and the count is the only difference between the two wirings.
            let n = match dir {
                Some((_, file_shared)) => {
                    maps[2 + SHELL_EXTRA_STACK] = Mapping {
                        va: FS_VA,
                        phys: file_shared,
                        flags: Flags::user_data(),
                    };
                    maps.len()
                }
                None => maps.len() - 1,
            };
            let grants: &[crate::cap::Cap] = &[
                endpoint_cap(term, Rights::WRITE),     // slot 0: the terminal
                endpoint_cap(spawn_ep, Rights::WRITE), // slot 1: direct init
                endpoint_cap(result, Rights::READ),    // slot 2: a child's answer
                untyped_cap(budget),                   // slot 3: what it mints pipes from
            ];
            match dir {
                Some((dir_ep, _)) => run(
                    image,
                    Spawn {
                        arg0: role,
                        arg1: arg,
                        arg2: 0,
                        grants: &[
                            grants[0],
                            grants[1],
                            grants[2],
                            grants[3],
                            // slot 4: the directory it resolves `>` and `<` against
                            endpoint_cap(dir_ep, Rights::WRITE),
                        ],
                        maps: &maps[..n],
                    },
                ),
                None => run(
                    image,
                    Spawn {
                        arg0: role,
                        arg1: arg,
                        arg2: 0,
                        grants,
                        maps: &maps[..n],
                    },
                ),
            }
        })?;

        Some(Wiring { term, out_phys })
    }

    /// **Serve the terminal until the shell says it is finished**, collecting everything it printed.
    ///
    /// One `OP_WRITE` at a time, replied the way the real line discipline replies (the byte count),
    /// because the shell blocks on that reply and a test that answered differently would be testing
    /// a terminal nobody has.
    pub fn transcript(w: &Wiring, sentinel: &[u8], out: &mut [u8]) -> usize {
        loop {
            let m = crate::sched::ipc_recv_cap(w.term);
            let (w0, slot) = (m[0], m[1]);
            let crate::cap::Object::Reply(caller) = crate::sched::current_cap(slot)
                .expect("the shell's terminal write carried no reply capability")
                .object
            else {
                panic!("the shell sent the terminal something that was not a CALL");
            };
            let n = line_editor::proto::len(w0);
            if line_editor::proto::op(w0) == line_editor::proto::OP_WRITE {
                let mut buf = TRANSCRIPT.lock();
                let at = WRITTEN.load(Ordering::SeqCst);
                let n = n.min(buf.len().saturating_sub(at));
                for i in 0..n {
                    // SAFETY: the shell's output page, mapped read/write into it and named here
                    // through the direct map; `n` is bounded by the page and by the buffer.
                    buf[at + i] = unsafe {
                        core::ptr::read_volatile(
                            (mmu::phys_to_virt(w.out_phys) + i as u64) as *const u8,
                        )
                    };
                }
                WRITTEN.store(at + n, Ordering::SeqCst);
            }
            crate::sched::ipc_reply(caller, [n as u64, 0]);
            crate::sched::delete_current_cap(slot).expect("consume the one-shot reply");

            let done = {
                let buf = TRANSCRIPT.lock();
                let len = WRITTEN.load(Ordering::SeqCst);
                len >= sentinel.len() && buf[len - sentinel.len()..len] == *sentinel
            };
            if done {
                let buf = TRANSCRIPT.lock();
                let len = WRITTEN.load(Ordering::SeqCst).min(out.len());
                out[..len].copy_from_slice(&buf[..len]);
                return len;
            }
        }
    }

    /// **The text the shell printed in response to `line`**, up to but not including the next
    /// prompt.
    ///
    /// The transcript is echoed prompts and answers, so slicing it this way is what lets an
    /// assertion name the command it is about instead of counting lines. It lives here rather than
    /// in one test module because both witnesses (the pipeline script and the redirection script)
    /// read their transcripts the same way, and a second copy would be a second thing to keep true.
    pub fn answer<'a>(t: &'a [u8], line: &[u8]) -> &'a [u8] {
        let mut needle = [0u8; 64];
        needle[0] = b'$';
        needle[1] = b' ';
        needle[2..2 + line.len()].copy_from_slice(line);
        needle[2 + line.len()] = b'\n';
        let needle = &needle[..3 + line.len()];
        let start = t
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| {
                panic!(
                    "the shell never ran {:?}. transcript:\n{}",
                    core::str::from_utf8(line).unwrap(),
                    core::str::from_utf8(t).unwrap_or("<not utf-8>"),
                )
            })
            + needle.len();
        let rest = &t[start..];
        let end = rest
            .windows(2)
            .position(|w| w == b"$ ")
            .unwrap_or(rest.len());
        &rest[..end]
    }

    /// Parse `wc`'s three numbers out of what the shell printed for it. `wc` prints
    /// `lines words bytes` and a newline; the caller strips the shell's two-space prefix.
    pub fn counts(said: &[u8]) -> (u64, u64, u64) {
        let text = core::str::from_utf8(said).expect("wc printed non-UTF-8");
        let mut it = text.split_ascii_whitespace().map(|w| {
            w.parse::<u64>()
                .unwrap_or_else(|_| panic!("wc printed {text:?}, which is not three numbers"))
        });
        let (l, w, b) = (
            it.next().expect("no line count"),
            it.next().expect("no word count"),
            it.next().expect("no byte count"),
        );
        assert!(
            it.next().is_none(),
            "wc printed more than three numbers: {text:?}"
        );
        (l, w, b)
    }

    /// **init, as the shell sees it**: the spawn protocol, including milestone 50's two delegated
    /// capabilities.
    ///
    /// The whole of what the operators added is here, and it is small: receive an endpoint, and put
    /// it where the result endpoint would have gone. Nothing decides what is behind it, because
    /// nothing here can find out.
    fn init_service(spawn_ep: EpId, result: EpId) -> ! {
        loop {
            let m = crate::sched::ipc_recv(spawn_ep);
            let (w0, w1, w2) = (m[0], m[1], m[2]);
            let prog = grant_plan::Prog::from_id(grant_plan::spawnproto::prog_id(w0));
            let arg = grant_plan::spawnproto::arg(w1);
            let wiring = grant_plan::spawnproto::wiring(w2);

            // In the protocol's order. A capability received but never expected, or expected and
            // never sent, deadlocks both sides, which is why the order is the contract.
            let sink = if wiring.sink {
                take_endpoint(spawn_ep)
            } else {
                None
            };
            let source = if wiring.source {
                take_endpoint(spawn_ep)
            } else {
                None
            };
            // A `--mem` grant is received and dropped: no stage of the pipeline script asks for
            // one, and receiving it anyway is what keeps the two sides in lockstep if one ever
            // does. `user/src/system_initializer.rs` is where a budget actually reaches a child.
            if grant_plan::spawnproto::mem_pages(w2) > 0 {
                let m = crate::sched::ipc_recv_cap(spawn_ep);
                let _ = crate::sched::delete_current_cap(m[1]);
            }

            let image = prog.and_then(|p| program(p.name()));
            let started = match image {
                Some(image) => {
                    // Slot 0 is the output: the result endpoint, or the sink the shell delegated.
                    // Slot 1 is the input source when there is one. That is the whole difference.
                    let out = sink.unwrap_or(result);
                    crate::sched::spawn(move || match source {
                        Some(src) => run(
                            image,
                            Spawn {
                                arg0: arg,
                                arg1: 0,
                                arg2: 0,
                                grants: &[
                                    endpoint_cap(out, Rights::WRITE),
                                    endpoint_cap(src, Rights::READ),
                                ],
                                maps: &[],
                            },
                        ),
                        None => run(
                            image,
                            Spawn {
                                arg0: arg,
                                arg1: 0,
                                arg2: 0,
                                grants: &[endpoint_cap(out, Rights::WRITE)],
                                maps: &[],
                            },
                        ),
                    })
                    .is_some()
                }
                None => false,
            };

            // A redirected child's answer goes somewhere else, so the shell has nothing to read and
            // init owes it an ack. Unredirected, the child's own message is the shell's single read.
            if wiring.sink {
                crate::sched::ipc_send(
                    result,
                    [
                        if started {
                            grant_plan::spawnproto::SPAWN_OK
                        } else {
                            grant_plan::spawnproto::SPAWN_FAILED
                        },
                        0,
                        0,
                    ],
                );
            } else if !started {
                crate::sched::ipc_send(result, [grant_plan::spawnproto::SPAWN_FAILED, 0, 0]);
            }
        }
    }

    /// Take one delegated capability and read the endpoint out of it. The slot is dropped straight
    /// away: what init needs is the *name* of the endpoint, and holding the capability afterwards
    /// would fill a cspace over a long session for nothing.
    fn take_endpoint(ep: EpId) -> Option<EpId> {
        let m = crate::sched::ipc_recv_cap(ep);
        let slot = m[1];
        let cap = crate::sched::current_cap(slot).ok()?;
        let _ = crate::sched::delete_current_cap(slot);
        match cap.object {
            crate::cap::Object::Endpoint(id) => Some(id),
            _ => None,
        }
    }
}

/// **`>`, `<` and `|` at a real prompt** (milestone 50, notes/pipes.md).
///
/// The claim under test is one sentence: **a program holds an endpoint for its output and cannot
/// tell what is on the other end.** So the assertions are all of the form "the same binary, two
/// destinations, the same bytes", never "the pipeline printed something".
#[cfg(test)]
mod pipeline_tests {
    use super::*;

    /// The last line `user/src/swish.rs`'s pipeline role prints. Must match `PIPELINE_DONE` there.
    const DONE: &[u8] = b"== pipelines done\n";

    /// The script's transcript, run **once** and shared by every assertion below.
    ///
    /// Cached because running it is expensive (a shell, an init service, and several spawned
    /// programs) and because the assertions are about one run of one script: re-running it per test
    /// would not be four independent measurements, it would be the same measurement four times.
    static TRANSCRIPT: spin::Mutex<Option<([u8; 3072], usize)>> = spin::Mutex::new(None);

    /// Run the script if nothing has yet, and hand back everything the shell printed.
    fn transcript(out: &mut [u8; 3072]) -> usize {
        let mut cache = TRANSCRIPT.lock();
        if cache.is_none() {
            let Some(w) = pipeline_service::start() else {
                panic!("no swish program in the initrd archive, or no memory to wire one");
            };
            let mut buf = [0u8; 3072];
            let n = pipeline_service::transcript(&w, DONE, &mut buf);
            *cache = Some((buf, n));
        }
        let (buf, n) = cache.as_ref().expect("just filled");
        out.copy_from_slice(buf);
        *n
    }

    use pipeline_service::{answer, counts};

    /// **The headline: the same `date`, two destinations, the same bytes.**
    ///
    /// `date` is spawned twice from the same ELF with the same argument. The first time its output
    /// slot holds the shell's result endpoint and the shell prints what arrives; the second time it
    /// holds an endpoint into `wc`, and `wc` counts what arrives. Neither `date` was told which, and
    /// there is no message on the sink contract that could have told it.
    ///
    /// The assertion is the byte count, which is the one number that has to agree. It is compared
    /// against the *observed* first arm rather than a constant, so it holds whether or not this
    /// boot has a clock and whatever `date` decides to say.
    ///
    /// The word and line counts are checked too, because a byte count alone would pass for a `wc`
    /// that had simply counted the right number of wrong bytes.
    #[test_case]
    fn one_program_two_destinations_and_the_same_bytes() {
        let mut buf = [0u8; 3072];
        let n = transcript(&mut buf);
        let t = &buf[..n];

        // Arm one: the shell's own result endpoint. It prints two spaces, then date's bytes.
        let direct = answer(t, b"date");
        assert!(
            direct.starts_with(b"  ") && direct.ends_with(b"\n"),
            "date printed {:?}, which is not one line through the shell",
            core::str::from_utf8(direct).unwrap_or("<not utf-8>"),
        );
        let printed = &direct[2..];

        // Arm two: an endpoint into `wc`.
        let (lines, words, bytes) = counts(&answer(t, b"date | wc")[2..]);
        assert_eq!(
            bytes as usize,
            printed.len(),
            "the same date wrote {} bytes to the shell and {bytes} into a pipe: {:?}",
            printed.len(),
            core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
        );
        assert_eq!(lines, 1, "date prints one line either way");
        assert_eq!(
            words as usize,
            core::str::from_utf8(printed)
                .expect("date printed non-UTF-8")
                .split_ascii_whitespace()
                .count(),
            "the words differ between the two destinations",
        );
    }

    /// **A builtin can lead a pipeline, because the shell can be a sink's client too.**
    ///
    /// `echo hello world | wc` is a real two-party pipeline with only one spawned process in it:
    /// the shell writes the bytes into the endpoint itself. That costs no new mechanism, and it is
    /// what the sink contract being register-only buys (notes/sink-protocol.md): being a writer
    /// needs one capability and nothing else.
    ///
    /// Twelve bytes: `hello world` plus the newline the shell adds, which is what `echo` prints.
    #[test_case]
    fn the_shell_can_be_the_producer() {
        let mut buf = [0u8; 3072];
        let n = transcript(&mut buf);
        let t = &buf[..n];
        assert_eq!(
            counts(&answer(t, b"echo hello world | wc")[2..]),
            (1, 2, 12),
            "the shell's own bytes did not arrive at wc intact",
        );
    }

    /// **Three stages, so a middle one is a real middle.**
    ///
    /// The second `wc` counts the first `wc`'s output, which is only possible because `wc` writes
    /// the same contract it reads. `1 2 12\n` is seven bytes, one line, three words.
    #[test_case]
    fn a_pipeline_composes_past_two_stages() {
        let mut buf = [0u8; 3072];
        let n = transcript(&mut buf);
        let t = &buf[..n];
        assert_eq!(
            counts(&answer(t, b"echo hello world | wc | wc")[2..]),
            (1, 3, 7),
            "the middle stage's output did not reach the last one",
        );
    }

    /// **The refusals happen at the prompt, with nothing spawned**, which is the half of this that
    /// Unix cannot do. Each of these is a fact the manifest knows and a shell can therefore act on
    /// before it moves any authority.
    #[test_case]
    fn the_line_is_refused_before_anything_is_spawned() {
        let mut buf = [0u8; 3072];
        let n = transcript(&mut buf);
        let t = &buf[..n];

        // A reader with nothing to read. On Unix this is a shell that appears to hang.
        let said = answer(t, b"wc");
        assert!(
            core::str::from_utf8(said)
                .unwrap_or("")
                .contains("waits forever"),
            "`wc` alone should be refused for having no input, not run: {:?}",
            core::str::from_utf8(said).unwrap_or("<not utf-8>"),
        );

        // A program whose answer is a register, on the left of a pipe.
        let said = answer(t, b"worker 9 | wc");
        assert!(
            core::str::from_utf8(said)
                .unwrap_or("")
                .contains("byte stream"),
            "`worker 9 | wc` should be refused for having no bytes: {:?}",
            core::str::from_utf8(said).unwrap_or("<not utf-8>"),
        );

        // And one that reads nothing, on the right of one.
        let said = answer(t, b"date | date");
        assert!(
            core::str::from_utf8(said)
                .unwrap_or("")
                .contains("no input"),
            "`date | date` should be refused: date reads nothing. got {:?}",
            core::str::from_utf8(said).unwrap_or("<not utf-8>"),
        );
    }

    /// **`>` needs a filesystem this shell was granted none of, and it says so** rather than
    /// running the command and printing the output to the terminal.
    ///
    /// This is the **negative control** for [`redirection_tests`], which runs the same ELF from the
    /// same archive with one more capability in it and writes the file. Neither behaviour is a
    /// branch in the shell: the refusal is a fact about a cspace with nothing in slot 4, and that is
    /// the sentence milestone 31 wrote and this milestone had to keep true. See notes/pipes.md.
    #[test_case]
    fn a_redirection_a_shell_cannot_back_is_refused_rather_than_dropped() {
        let mut buf = [0u8; 3072];
        let n = transcript(&mut buf);
        let t = &buf[..n];
        let said = answer(t, b"date > report.txt");
        assert!(
            core::str::from_utf8(said)
                .unwrap_or("")
                .contains("no such capability"),
            "a redirection this shell cannot back must be refused, not silently dropped: {:?}",
            core::str::from_utf8(said).unwrap_or("<not utf-8>"),
        );
    }
}

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
mod redirection_tests {
    use fs_proto::dir;
    use pipeline_service::{answer, counts};

    use super::*;

    /// The last line `user/src/swish.rs`'s redirection role prints. Must match `REDIRECT_DONE`.
    const DONE: &[u8] = b"== redirections done\n";

    /// The script's transcript, run **once** and shared by every assertion below, for
    /// [`pipeline_tests`]'s reason: the assertions are about one run of one script, and re-running
    /// it per test would be the same measurement four times rather than four measurements.
    static TRANSCRIPT: spin::Mutex<Option<([u8; 3072], usize)>> = spin::Mutex::new(None);

    /// Run the script if nothing has yet, and hand back everything the shell printed. `None` when
    /// this boot has no RedoxFS disk attached, which is the same skip every FS test takes.
    fn transcript(out: &mut [u8; 3072]) -> Option<usize> {
        let mut cache = TRANSCRIPT.lock();
        if cache.is_none() {
            let dir = fs_service::narrow_dir(
                dir_capability_tests::blk_server_image(),
                program("fs_server")?,
                program("fs_subtree_caretaker")
                    .expect("no fs_subtree_caretaker program in the initrd archive"),
                fs_proto::fixture::tree::REDIR,
                dir::ALL,
            )?;
            let Some(w) = pipeline_service::start_redirecting(dir, dir::ALL) else {
                panic!("no swish program in the initrd archive, or no memory to wire one");
            };
            let mut buf = [0u8; 3072];
            let n = pipeline_service::transcript(&w, DONE, &mut buf);
            *cache = Some((buf, n));
        }
        let (buf, n) = cache.as_ref().expect("just filled");
        out.copy_from_slice(buf);
        Some(*n)
    }

    /// What `wc` should report for a listing the shell **printed**, which is the same listing it
    /// **wrote** with two bytes of indent removed from each line.
    ///
    /// The prompt renders an entry as `"  name\n"` and a producer feeding a byte stream renders it
    /// as `"name\n"`, because the indent is the terminal's manners and not part of the listing. So
    /// the expected counts are derived here rather than hardcoded, and the derivation is the whole
    /// assertion: a file that does not match this is a file with different bytes in it.
    fn listing_counts(printed: &[u8]) -> (u64, u64, u64) {
        let mut lines = 0u64;
        let mut words = 0u64;
        let mut bytes = 0u64;
        for line in printed.split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let name = line.strip_prefix(b"  ").unwrap_or(line);
            lines += 1;
            words += core::str::from_utf8(name)
                .expect("a listed name is not UTF-8")
                .split_ascii_whitespace()
                .count() as u64;
            bytes += name.len() as u64 + 1; // the newline the producer writes
        }
        (lines, words, bytes)
    }

    /// **The headline: one builtin, two destinations, the same bytes.**
    ///
    /// `ls > out.txt` writes a listing into a file and prints nothing. The `ls` after it prints that
    /// same listing (nothing between the two lines changes the directory: `>` created `out.txt`
    /// *before* the first listing was read, so both see it). `wc < out.txt` then has to agree with
    /// what was printed, byte for byte, once the prompt's two-space indent is taken off.
    ///
    /// **This line spawns no process at all**, which is the part worth noticing. The shell is the
    /// producer and the shell is the thing behind the file, so `ls > out.txt` is one process doing
    /// two things it already knew how to do. See notes/pipes.md for why the file end is the shell's
    /// and not a sink process's.
    #[test_case]
    fn one_builtin_two_destinations_and_the_same_bytes() {
        let mut buf = [0u8; 3072];
        let Some(n) = transcript(&mut buf) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let t = &buf[..n];

        // `>` prints nothing. A redirection that quietly also printed would mean the bytes went two
        // places, and the whole claim is that they went to exactly one.
        let redirected = answer(t, b"ls > out.txt");
        assert!(
            redirected.iter().all(|b| b.is_ascii_whitespace()),
            "`ls > out.txt` printed {:?} instead of writing it to the file",
            core::str::from_utf8(redirected).unwrap_or("<not utf-8>"),
        );

        let printed = answer(t, b"ls");
        assert!(
            printed.windows(6).any(|w| w == b"alpha\n"),
            "the listing does not hold the fixture's own files, so it proves nothing: {:?}",
            core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
        );
        assert!(
            printed.windows(8).any(|w| w == b"out.txt\n"),
            "`>` did not create the file it redirects into: {:?}",
            core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
        );

        assert_eq!(
            counts(&answer(t, b"wc < out.txt")[2..]),
            listing_counts(printed),
            "the listing that went into the file is not the listing that went to the terminal",
        );
    }

    /// **And the same claim for a program's output**, which is the half `>` shares with `|`.
    ///
    /// `date` is spawned twice from the same ELF. The first time the shell prints what arrives on
    /// its result endpoint; the second time it writes it into a file. `date` is not told, and the
    /// byte count `wc` reports for the file has to be the length of what was printed.
    #[test_case]
    fn one_program_two_destinations_and_the_same_bytes() {
        let mut buf = [0u8; 3072];
        let Some(n) = transcript(&mut buf) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let t = &buf[..n];

        let direct = answer(t, b"date");
        assert!(
            direct.starts_with(b"  ") && direct.ends_with(b"\n"),
            "date printed {:?}, which is not one line through the shell",
            core::str::from_utf8(direct).unwrap_or("<not utf-8>"),
        );
        let printed = &direct[2..];

        let (lines, words, bytes) = counts(&answer(t, b"wc < date.txt")[2..]);
        assert_eq!(
            bytes as usize,
            printed.len(),
            "the same date wrote {} bytes to the terminal and {bytes} into a file: {:?}",
            printed.len(),
            core::str::from_utf8(printed).unwrap_or("<not utf-8>"),
        );
        assert_eq!(lines, 1, "date prints one line either way");
        assert_eq!(
            words as usize,
            core::str::from_utf8(printed)
                .expect("date printed non-UTF-8")
                .split_ascii_whitespace()
                .count(),
            "the words differ between the two destinations",
        );
    }

    /// **`>>` is `>` with the emptying left out, and the two are measured against each other.**
    ///
    /// The script runs the same two commands twice, changing one character:
    ///
    /// ```text
    /// echo one > trunc.txt        echo one > app.txt
    /// echo two > trunc.txt        echo two >> app.txt
    /// ```
    ///
    /// so the append file must hold **exactly twice** what the truncate file holds, in all three
    /// counts. Doubling is the assertion rather than `(2, 2, 8)` because a literal would be a claim
    /// about what `echo` prints; this is a claim about what the operator did, and it holds whatever
    /// the two lines say.
    ///
    /// The truncate arm is load-bearing in its own right: it is what says `>` still empties the
    /// file, which is the property `>>` exists to be the opposite of.
    #[test_case]
    fn append_keeps_what_truncate_throws_away() {
        let mut buf = [0u8; 3072];
        let Some(n) = transcript(&mut buf) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let t = &buf[..n];

        let (tl, tw, tb) = counts(&answer(t, b"wc < trunc.txt")[2..]);
        assert!(
            tb > 0,
            "the truncate arm wrote nothing, so the comparison below proves nothing",
        );
        assert_eq!(
            counts(&answer(t, b"wc < app.txt")[2..]),
            (tl * 2, tw * 2, tb * 2),
            "`>>` did not keep the first line: `>` wrote ({tl}, {tw}, {tb}) for the same two \
             commands, so append must be exactly twice that",
        );

        // **`>>` creates a name that is not there**, so it is not "open, then seek to the end":
        // there is nothing to open. On a fresh fixture that file holds exactly one copy of what
        // `echo one` writes; the assertion is stated as a whole number of copies because
        // `CRICKER_KEEP_REDOXFS=1` deliberately re-runs the suite against the image the last boot
        // left behind, and this line would then append to its own leftovers. Order-independent
        // either way, and it still fails on a `>>` that wrote a partial or a doubled line.
        assert_eq!(
            (tl, tw),
            (1, 1),
            "the truncate arm should be one line of one word; the check below assumes it",
        );
        let (fl, fw, fb) = counts(&answer(t, b"wc < fresh.txt")[2..]);
        assert!(
            fb >= tb && fb % tb == 0 && fl == fb / tb && fw == fl,
            "`>> a-name-that-is-not-there` should have created it and written one line into it, \
             and reported some whole number of those lines; got ({fl}, {fw}, {fb}) against a \
             single line of ({tl}, {tw}, {tb})",
        );
    }

    /// **A redirection that cannot be opened refuses the line rather than running it.**
    ///
    /// `<` does not create, so `wc < nosuch.txt` is the filesystem's own sentence and nothing is
    /// spawned. The alternative, an empty stream, would have `wc` truthfully report zero for a file
    /// that does not exist, which is a number a person would believe.
    ///
    /// And the manifest still wins over the capability: `worker 9 > out.txt` is refused for having
    /// no byte stream even in a shell that could open the file, because what a `>` needs is a
    /// program with bytes and not a shell with a directory.
    #[test_case]
    fn a_redirection_that_cannot_be_backed_is_still_refused() {
        let mut buf = [0u8; 3072];
        let Some(n) = transcript(&mut buf) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let t = &buf[..n];

        let said = answer(t, b"wc < nosuch.txt");
        assert!(
            core::str::from_utf8(said)
                .unwrap_or("")
                .contains("no such name"),
            "`wc < nosuch.txt` must be the filesystem's refusal, not an empty stream: {:?}",
            core::str::from_utf8(said).unwrap_or("<not utf-8>"),
        );

        let said = answer(t, b"worker 9 > out.txt");
        assert!(
            core::str::from_utf8(said)
                .unwrap_or("")
                .contains("byte stream"),
            "`worker 9 > out.txt` should be refused for having no bytes: {:?}",
            core::str::from_utf8(said).unwrap_or("<not utf-8>"),
        );
    }
}

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
mod sink_tests {
    use sink_proto::fixture;

    use super::*;
    use crate::cap::{Rights, endpoint_cap};
    use crate::sched::EpId;

    /// `user/src/sink.rs`'s writer role. Its `arg1` is how many times to write the transcript, with
    /// 0 meaning "until the sink stops taking it".
    const ROLE_WRITER: u64 = 0;

    /// Spawn the writer against `sink`, and return the endpoint it reports its classification on.
    ///
    /// `None` is the case that has no sink, and the wiring says so by **leaving slot 0 empty**
    /// rather than by passing a flag: an empty cspace slot is how this kernel spells "you were
    /// never given one", and it is the refusal an ungranted display client meets too (§29). The
    /// report still goes in at slot 1, placed rather than appended, so the program that holds no
    /// sink is otherwise wired identically to the one that does.
    fn spawn_writer(image: &'static [u8], sink: Option<EpId>, repeat: u64) -> EpId {
        let report = crate::sched::create_endpoint();
        crate::sched::spawn(move || match sink {
            Some(ep) => run(
                image,
                Spawn {
                    arg0: ROLE_WRITER,
                    arg1: repeat,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(ep, Rights::WRITE),     // slot 0: the byte sink
                        endpoint_cap(report, Rights::WRITE), // slot 1: the classification
                    ],
                    maps: &[],
                },
            ),
            None => {
                crate::sched::grant_at(1, endpoint_cap(report, Rights::WRITE))
                    .expect("the writer's report slot was already occupied");
                run(
                    image,
                    Spawn {
                        arg0: ROLE_WRITER,
                        arg1: repeat,
                        arg2: 0,
                        grants: &[],
                        maps: &[],
                    },
                )
            }
        })
        .expect("could not spawn the sink writer");
        report
    }

    /// Spawn `wc` with `source` in its input slot, and return the endpoint its answer arrives on.
    ///
    /// Two capabilities and nothing else, which is what makes the test below mean something: `wc`
    /// holds no file, no directory, no page and nothing that names the FS server. Whatever is
    /// behind its input slot, it is handed the bytes.
    fn spawn_wc(source: EpId) -> EpId {
        let image = program("wc").expect("no wc program in the initrd archive");
        let out = crate::sched::create_endpoint();
        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: 0,
                    arg1: 0,
                    arg2: 0,
                    grants: &[
                        endpoint_cap(out, Rights::WRITE),   // slot 0: where its answer goes
                        endpoint_cap(source, Rights::READ), // slot 1: where its bytes come from
                    ],
                    maps: &[],
                },
            )
        })
        .expect("could not spawn wc");
        out
    }

    /// Drain `wc`'s answer and parse the three numbers out of it.
    fn wc_counts(out: EpId, what: &str) -> (u64, u64, u64) {
        let mut buf = [0u8; 64];
        let n = super::std_tests::drain_sink(out, &mut buf, what);
        let text = core::str::from_utf8(&buf[..n]).expect("wc printed non-UTF-8");
        let mut it = text.split_ascii_whitespace().map(|w| {
            w.parse::<u64>()
                .unwrap_or_else(|_| panic!("{what}: wc printed {text:?}"))
        });
        (
            it.next().expect("no line count"),
            it.next().expect("no word count"),
            it.next().expect("no byte count"),
        )
    }

    /// **`<` proven the way `>` is: the same `wc`, two sources, the same answer.**
    ///
    /// This is the indifference claim on the **input** side, which milestone 50's protocol lane
    /// explicitly left open ("both `< file` and a pipe's read end need an input-slot convention
    /// that does not exist"). The convention chosen is the smallest one available: a source is the
    /// sink contract *received* rather than sent. So this test is what says that choice was real.
    ///
    /// One `wc` ELF, spawned twice with identical grants except for what is behind slot 1:
    ///
    /// - **a pipe**: this test sends the transcript on an endpoint itself, sixteen bytes at a time,
    ///   then `OP_EOF`. That is exactly what a program on the left of a `|` does.
    /// - **a file**: the transcript is written into a real file on the real RedoxFS image by
    ///   `sink`'s file role, and read back out by its source role, which streams it over the same
    ///   contract. That is `wc < report.txt`, minus the shell that would name the file.
    ///
    /// The two arms share nothing but the framing. The second crosses two userspace processes, an
    /// FS server, a block server and a virtio disk; the first does not leave this address space.
    /// `wc` was not told which and holds nothing that could tell it.
    ///
    /// The answers must be equal **and** must equal what the transcript actually is, because two
    /// arms broken the same way would satisfy equality on its own.
    #[test_case]
    fn one_reader_two_sources_and_the_same_answer() {
        let sink_image = program("sink").expect("no sink program in the initrd archive");
        let blk = program("init").expect("no init program in the initrd archive");
        let Some(fs_server) = program("fs_server") else {
            crate::println!("    (no FS server in this archive; skipping)");
            return;
        };

        // Arm one: a pipe. The kernel is the producer, which is the same position the shell is in
        // when a builtin leads a pipeline (`kernel::user::pipeline_tests`).
        let pipe = crate::sched::create_endpoint();
        let out = spawn_wc(pipe);
        let mut off = 0usize;
        while off < fixture::TRANSCRIPT.len() {
            let (w0, w1, w2, n) = sink_proto::pack(&fixture::TRANSCRIPT[off..]);
            crate::sched::ipc_send(pipe, [w0, w1, w2]);
            off += n;
        }
        crate::sched::ipc_send(pipe, [sink_proto::eof(), 0, 0]);
        let piped = wc_counts(out, "wc reading a pipe");

        // The transcript's own numbers, so the pipe arm is anchored to something rather than only
        // to the file arm.
        let text = core::str::from_utf8(fixture::TRANSCRIPT).expect("the fixture is not UTF-8");
        assert_eq!(
            piped,
            (
                text.lines().count() as u64,
                text.split_ascii_whitespace().count() as u64,
                fixture::TRANSCRIPT.len() as u64,
            ),
            "wc miscounted the transcript it was handed down a pipe",
        );

        // Arm two: the same bytes, through a real filesystem. Write them first.
        let Some(file_sink) = fs_service::start_file_sink(blk, fs_server, sink_image) else {
            crate::println!("    (no RedoxFS disk attached; the pipe arm stands alone)");
            return;
        };
        fs_service::wait_for_service(file_sink.readiness);
        assert_eq!(
            crate::sched::ipc_recv(file_sink.report)[0],
            fixture::READY,
            "the file sink could not open its file",
        );
        let wrote = spawn_writer(sink_image, Some(file_sink.sink), 1);
        let [code, total, ..] = crate::sched::ipc_recv(wrote);
        assert_eq!(
            code,
            fixture::code(sink_proto::Sent::Ok),
            "the writer did not deliver the transcript to the file sink",
        );
        let [done, wrote_total, ..] = crate::sched::ipc_recv(file_sink.report);
        assert_eq!(done, fixture::DONE, "the file sink did not finish cleanly");
        assert_eq!(
            wrote_total, total,
            "the file sink recorded a different byte count from the one the writer sent",
        );

        // Then read them back into `wc`, which is `<`.
        let Some((source, verify_report)) =
            fs_service::start_sink_verify(blk, fs_server, sink_image)
        else {
            panic!("the FS service vanished between the file sink and its source");
        };
        let out = spawn_wc(source);
        let filed = wc_counts(out, "wc reading a file");
        let [vdone, size, ..] = crate::sched::ipc_recv(verify_report);
        assert_eq!(
            vdone,
            fixture::DONE,
            "the source adapter could not read the file back ({vdone:#x})",
        );
        assert_eq!(
            size, total,
            "the file is not the size that was written into it"
        );

        assert_eq!(
            piped, filed,
            "the same wc answered differently for a pipe and for a file, so its input slot is not \
             opaque after all",
        );
    }

    /// **The same program, two destinations, the same bytes.**
    ///
    /// `std_exerciser` is spawned twice with **identical grants except for what is behind slot 1**: once
    /// with an endpoint this test receives on, which is the pipe shape (an ordinary receiver, no
    /// page, no reply), and once with an endpoint served by `sink` in its file role, which appends
    /// every message into a file on the real RedoxFS image through the real FS server. The file is
    /// then read back by a **third** process with its own FS session and streamed home over the
    /// sink contract.
    ///
    /// The two arms share nothing but sixteen bytes of message. One ends in the kernel's own
    /// address space; the other crosses two userspace processes, an FS server, a block server and a
    /// virtio disk. The binary is identical, its code never chose either, and the bytes must be
    /// equal.
    ///
    /// **What would make this vacuous, and why it is not.** Comparing each arm against a constant
    /// would pass if both arms were broken the same way, so the second arm is compared against
    /// *what the first arm actually received*, and the first arm against the pinned transcript. An
    /// empty file would satisfy "equal" if the program printed nothing, so the byte count is
    /// asserted on both sides as well.
    #[test_case]
    fn a_program_cannot_tell_what_its_output_slot_holds() {
        let std_exerciser =
            program("std_exerciser").expect("no std_exerciser program in the initrd archive");
        let clock = program("clock").expect("no clock program in the initrd archive");
        let entropy = program("entropy").expect("no entropy program in the initrd archive");
        let sink_image = program("sink").expect("no sink program in the initrd archive");
        let Some(fs_server) = program("fs_server") else {
            crate::println!("    (no FS server in this archive; skipping)");
            return;
        };

        // Arm one: the kernel receives. Everything here is what the existing std test does, which
        // is the point: nothing was special-cased for the sink.
        let direct = crate::sched::create_endpoint();
        std_service::start_on(std_exerciser, clock, entropy, direct);
        let mut first = [0u8; 512];
        let n1 = super::std_tests::drain_sink(direct, &mut first, "std_exerciser, direct endpoint");
        assert_eq!(
            &first[..n1],
            super::std_tests::EXPECTED,
            "the direct arm did not print the pinned transcript, so it is no reference for what \
             the other arm produces",
        );

        // Arm two: a file sink. It opens its file before the writer exists, for the reason
        // `fs_service::wait_for_caretaker` records: it stages a name in the page it shares with the
        // FS server, and a client that already existed could write over it.
        let blk = program("init").expect("no init program in the initrd archive");
        let Some(file_sink) = fs_service::start_file_sink(blk, fs_server, sink_image) else {
            crate::println!("    (no RedoxFS disk attached; skipping)");
            return;
        };
        let (sink_ep, sink_report) = (file_sink.sink, file_sink.report);
        fs_service::wait_for_service(file_sink.readiness);
        assert_eq!(
            crate::sched::ipc_recv(sink_report)[0],
            fixture::READY,
            "the file sink could not open its file, so there was nothing to redirect into",
        );

        std_service::start_on(std_exerciser, clock, entropy, sink_ep);
        let [done, total, ..] = crate::sched::ipc_recv(sink_report);
        assert_eq!(
            done,
            fixture::DONE,
            "the file sink reported {done:#x} rather than finishing: it refused a message, or the \
             FS server refused a write",
        );
        assert_eq!(
            total as usize, n1,
            "the file sink wrote {total} bytes for a program that printed {n1}",
        );

        // The read-back, in a third process with its own FS session, and only now that the sink has
        // closed the file: the two share the FS server's one file page.
        let Some((out, verify_report)) = fs_service::start_sink_verify(blk, fs_server, sink_image)
        else {
            panic!("the FS service vanished between the file sink and its verifier");
        };
        let mut second = [0u8; 512];
        let n2 = super::std_tests::drain_sink(out, &mut second, "std_exerciser, file sink");
        let [vdone, vsize, ..] = crate::sched::ipc_recv(verify_report);
        assert_eq!(
            vdone,
            fixture::DONE,
            "the verifier could not read back the file the sink wrote ({vdone:#x})",
        );
        assert_eq!(
            vsize as usize, n2,
            "the verifier streamed a length it did not find on the platter",
        );

        assert_eq!(
            &second[..n2],
            &first[..n1],
            "the same program printed different bytes into a file than into an endpoint, so its \
             output destination is something it can tell apart",
        );
        crate::println!("    ({n1} bytes, identical through an endpoint and through RedoxFS)");
    }

    /// **A dead reader ends the writer, and a missing one does not.** Milestone 50's one behaviour
    /// change, asserted by value on all three outcomes.
    ///
    /// Unix needs `SIGPIPE` here because an anonymous file descriptor gives the kernel no way to
    /// reach a writer that is not making a syscall. This writer *is* making a syscall, on a
    /// capability the kernel can see is dead, so the fact arrives as a return code.
    ///
    /// 1. **A sink that stays**: the transcript arrives and the writer classifies `Ok`.
    /// 2. **A sink destroyed under it**: this test takes a few messages, reclaims the region the
    ///    endpoint was retyped from (§16), and the writer's blocked `SEND` classifies `Gone`.
    ///    Blocked is the case that matters: a producer in `yes | head` is almost always parked in a
    ///    send when its reader exits.
    /// 3. **No sink at all**: an empty slot classifies `NoSink` and the program **keeps running**.
    ///
    /// Case 3 is what makes case 2 mean anything. Both used to be `NoSuchSlot`, so a writer that
    /// ended on case 2 would have ended on case 3 as well, killing every program that was simply
    /// never given a console.
    #[test_case]
    fn a_destroyed_sink_ends_the_writer_and_an_absent_one_does_not() {
        let image = program("sink").expect("no sink program in the initrd archive");

        // 1. A sink that stays.
        let live = crate::sched::create_endpoint();
        let report = spawn_writer(image, Some(live), 1);
        let mut got = [0u8; 256];
        let n = super::std_tests::drain_sink(live, &mut got, "the writer with a live sink");
        assert_eq!(
            &got[..n],
            fixture::TRANSCRIPT,
            "the writer did not deliver its transcript to a sink that was there",
        );
        let [class, total, ..] = crate::sched::ipc_recv(report);
        assert_eq!(
            (class, total as usize),
            (
                fixture::code(sink_proto::Sent::Ok),
                fixture::TRANSCRIPT.len()
            ),
            "a writer whose sink stayed put must classify Ok",
        );

        // 2. A sink destroyed under a writer blocked in a send. The endpoint is retyped out of a
        // region this test owns, because destroying the region is how an endpoint dies (§16) and
        // the kernel's own endpoints are not in one.
        let region = crate::untyped::create(4).expect("no untyped for the doomed sink");
        let doomed = crate::sched::create_endpoint_from(region).expect("no endpoint in the region");
        let report = spawn_writer(image, Some(doomed), 0);
        // Take a few messages first, so the writer is demonstrably running and parked in the next
        // send rather than having failed before it ever reached one.
        for _ in 0..3 {
            let _ = crate::sched::ipc_recv(doomed);
        }
        crate::sched::reclaim_region(region).expect("the doomed sink's region would not reclaim");
        let [class, total, ..] = crate::sched::ipc_recv(report);
        assert_eq!(
            class,
            fixture::code(sink_proto::Sent::Gone),
            "a writer whose sink was destroyed classified {class} rather than Gone. Before \
             milestone 50 this arrived as NoSuchSlot, indistinguishable from never having had a \
             sink, which is exactly why `yes | head` could not end.",
        );
        assert!(
            total >= 3,
            "the writer reported {total} bytes through, so it never reached a live send and the \
             classification above says nothing about a sink dying under one",
        );

        // 3. No sink at all: the slot is empty, and the program keeps running.
        let report = spawn_writer(image, None, 1);
        let [class, total, ..] = crate::sched::ipc_recv(report);
        assert_eq!(
            (class, total),
            (fixture::code(sink_proto::Sent::NoSink), 0),
            "a program with an empty output slot must keep running and print into the void, which \
             is what every OS does to a process whose stdout is closed",
        );
    }
}

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
mod thread_leak_police {
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
