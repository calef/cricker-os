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

use crate::arch::exceptions::TrapFrame;
use crate::arch::mmu::{self, phys_to_ptr};
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

/// `SPSR_EL1` for "return to EL0, AArch64, interrupts on."
///
/// - `M[4] = 0`: AArch64, not AArch32.
/// - `M[3:0] = 0b0000`: **EL0t**. The `t` means SP_EL0, which is the only stack pointer EL0
///   has. There is no EL0h.
/// - `DAIF = 0`: Debug, SError, IRQ and FIQ all **unmasked**.
///
/// So the value is zero, which looks like a bug and is not. It is worth spelling out because
/// the DAIF bits are the interesting part: **IRQs are on the moment we land in EL0.** If they
/// were masked, a user program in a tight loop could never be preempted and the machine would
/// be gone, which is the exact failure DECISIONS §5 spent a milestone refusing to accept.
const SPSR_EL0T: u64 = 0;

unsafe extern "C" {
    /// `mov sp, x0` then fall into `exception_restore`. Two instructions. See vectors.s.
    fn enter_userspace(frame: *mut TrapFrame) -> !;
}

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
    region: usize,
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
            Mapper::new(
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
pub fn user_aspace_create(region: usize) -> Option<u64> {
    let root = crate::untyped::retype_object_page(region)?;

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
    crate::drivers::gic::enable(INIT_TEST_SGI);
    // And the UART receive interrupt (19d.2c): the input driver init builds waits on it. Route and
    // enable it here, so init can delegate the Irq cap to that driver.
    crate::sched::bind_irq(UART_RX_INTID, crate::sched::create_endpoint());
    crate::drivers::gic::enable(UART_RX_INTID);

    crate::sched::spawn(move || {
        // The initrd is a crickerfs archive (milestone 19f), not a bare ELF: it carries init plus
        // the programs init will load. The kernel reads only the one entry it must, "init". This is
        // the same "honest residue" as before (something has to load the first program), now naming
        // that program through a fixed archive index instead of assuming it sits at offset 0. Every
        // other program is init's to parse. See notes/init-and-loading.md.
        let init_bytes = match crickerfs::Fs::parse(image) {
            Ok(fs) => match fs.read("init") {
                Some(bytes) => bytes,
                None => {
                    crate::println!("  boot archive has no 'init' program");
                    crate::sched::exit();
                }
            },
            Err(e) => {
                crate::println!("  boot archive is not a crickerfs image: {e:?}");
                crate::sched::exit();
            }
        };
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
        crate::sched::grant(crate::cap::untyped_cap(build_region)).expect("grant untyped");
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
#[cfg(feature = "initboot")]
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

    let frame = (top - size_of::<TrapFrame>() as u64) as *mut TrapFrame;

    // And prove it, rather than trusting the reasoning above. This is one check, once per
    // exec, against a bug whose symptom is a nested fault storm that eats the kernel image.
    assert!(
        mmu::translate(frame as u64).is_some_and(|(_, f)| f.is_writable()),
        "the user's TrapFrame at {frame:p} is not in writable memory",
    );

    // SAFETY: `frame` is 16-byte-aligned writable kernel stack (a KernelStack top is page
    // aligned and TrapFrame is 272, a multiple of 16), EL0's code and stack are mapped, and
    // TTBR0 is installed.
    unsafe {
        let mut x = [0u64; 31];
        x[0] = arg0; // _start's first argument
        x[1] = arg1; // ...and its second
        x[2] = arg2; // ...and its third
        frame.write(TrapFrame {
            x,
            elr: entry,      // ...where `eret` jumps
            spsr: SPSR_EL0T, // ...and the exception level it jumps to
            sp_el0: user_sp, // ...on the stack it will jump onto (caller's choice, 19c.3)
        });

        enter_userspace(frame)
    }
}

/// Make the instruction fetcher aware of code we just wrote as data.
///
/// The D-cache and the I-cache are **not coherent** on aarch64. This is not a QEMU quirk, it is
/// the architecture: the assumption is that writing code is rare and paying for coherence on
/// every store is not worth it. So the loader has to say so explicitly, and every loader on
/// every ARM machine does exactly this.
///
/// `dc cvau` cleans the data cache to the point of unification, `ic ivau` invalidates the
/// instruction cache, and the barriers make the two agree. Get it wrong and the CPU executes
/// whatever was in that frame *before* the program landed there, which is an extremely
/// entertaining bug.
fn sync_icache(va: u64, len: usize) {
    const LINE: u64 = 64; // conservative: the real size is in CTR_EL0

    let mut p = va & !(LINE - 1);
    let end = va + len as u64;

    // SAFETY: cache maintenance on a mapped, readable range is always sound.
    unsafe {
        while p < end {
            core::arch::asm!("dc cvau, {p}", p = in(reg) p, options(nostack));
            p += LINE;
        }
        core::arch::asm!("dsb ish", options(nostack));

        let mut p = va & !(LINE - 1);
        while p < end {
            core::arch::asm!("ic ivau, {p}", p = in(reg) p, options(nostack));
            p += LINE;
        }
        core::arch::asm!("dsb ish", "isb", options(nostack));
    }
}

// --- the programs ---
//
// Hand-written aarch64, assembled into `.rodata` and copied into a user page at load time.
// There is no ELF loader yet (that is 7c) and no filesystem to load from (that is milestone 9),
// so the "binary" rides along inside the kernel image. Honest scaffolding, and it goes away.

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

user_program!(hello, USER_HELLO_START, USER_HELLO_END);
user_program!(spin, USER_SPIN_START, USER_SPIN_END);
user_program!(outlaw, USER_OUTLAW_START, USER_OUTLAW_END);

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

    /// Start the driver. Returns the endpoint it will report its result on, or `None` if there is
    /// no disk attached to enumerate.
    pub fn start(image: &'static [u8]) -> Option<EpId> {
        let dev = crate::virtio::find_block_device()?;

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
        crate::sched::bind_irq(dev.intid, irq_ep);
        crate::drivers::gic::enable(dev.intid);

        // Where the driver reports the bytes it read.
        let report = crate::sched::create_endpoint();

        // Register the device's transport with the kernel: the kernel owns the MMIO and the
        // DMA-critical operations, and confines the device to this DMA region. The driver gets a
        // `Virtio` capability, not the registers.
        let vid = crate::virtio::register(dev.mmio_phys, dma, FRAME_SIZE);

        crate::sched::spawn(move || {
            run(
                image,
                Spawn {
                    arg0: ROLE_VIRTIO_BLK,
                    arg1: dma, // the DMA region's PHYSICAL address (still needed to build requests)
                    arg2: 0,
                    grants: &[
                        endpoint_cap(report, Rights::WRITE), // slot 0: SEND the result
                        irq_cap(dev.intid),                  // slot 1: WAIT / ACK the interrupt
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

        Some(report)
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
        let vid = crate::virtio::register(dev.mmio_phys, dma, FRAME_SIZE);
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
        let vid = crate::virtio::register(dev.mmio_phys, dma, FRAME_SIZE);
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
        crate::drivers::gic::enable(UART_INTID);

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
    pub fn start(image: &'static [u8], pages: u64) -> Option<(usize, EpId)> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::exceptions::{
        LAST_USER_FAULT_ESR, LAST_USER_FAULT_FAR, SVC_COUNT, USER_FAULTS,
    };
    use crate::arch::timer;
    use crate::sched;
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

    /// Spin the scheduler until `done()`, or give up. Returns whether it happened.
    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..2000 {
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
        let f0 = USER_FAULTS.load(Ordering::Relaxed);
        sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");
        assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f0));
        assert!(wait_for(|| sched::thread_count() <= baseline));

        let before = used();

        for _ in 0..4 {
            let f = USER_FAULTS.load(Ordering::Relaxed);
            sched::spawn(|| unsafe { exec(outlaw()) }).expect("spawn failed");
            assert!(wait_for(|| USER_FAULTS.load(Ordering::Relaxed) > f));
            assert!(wait_for(|| sched::thread_count() <= baseline));
        }

        assert_eq!(
            used(),
            before,
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

        let [crc, ticks, freq] = crate::sched::ipc_recv(report);
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
        let slot = crate::sched::tcb_insert_cap(tid, report_cap).expect("cap insert");
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
