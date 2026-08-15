//! Bringing the other cores online (SMP steps 2 and 3b, DECISIONS.md §11).
//!
//! Core 0 starts each secondary with a PSCI `CPU_ON`, handing it a physical entry point
//! (`secondary_boot` in boot.s) and its own stack. The secondary replays the MMU-enable and lands
//! in [`secondary_main`].
//!
//! **Step 2** brought a core up "alive and idle" (bring-up path only). **Step 3b-ii** makes it a
//! full scheduler participant: it adopts the kernel's fine map, becomes its own idle thread with
//! its own run queue, brings up its GIC CPU interface and timer, and from then on schedules and is
//! preempted like the boot core.
//!
//! **Step 3c** adds cross-core placement. A core's run queue is single-owner, so to give it a
//! thread another core hands it through this core's **inbox** (the one cross-core scheduler
//! structure) and fires the reschedule SGI; the target drains its inbox into its own queue in the
//! handler. `sched::spawn_on(core, f)` is the primitive.
//!
//! **Load balancing** is the last piece, and since DECISIONS §28 it lives in `sched::spawn` itself:
//! two random choices at spawn, then work stealing by idle cores. There used to be an explicit
//! `spawn_balanced` that round-robined placement, and milestone 41 deleted it, because §28 left it
//! with no caller and a doc comment claiming a test used it that had already moved to plain `spawn`.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use isa::cpu_list::CpuList;

use crate::cpu::{self, MAX_CPUS};
use crate::{arch, print, println};

/// 64 KiB per secondary, matching core 0's boot stack (link-aarch64.ld).
const SECONDARY_STACK_SIZE: usize = 64 * 1024;

/// The unmapped page below each secondary stack (milestone 90).
///
/// One page of address space per core and **zero physical frames**: `map_everything` simply skips
/// it, so the MMU faults on the first byte written past the bottom of the stack. Same mechanism as
/// the boot stack's `__stack_guard` (link-aarch64.ld, link-riscv64.ld) and as a kernel thread's
/// guard (`thread::KernelStack`); this was the one kernel stack that had no such page.
const SECONDARY_STACK_GUARD: usize = 4096;

/// A core's slot: its guard page, then the stack that grows down into it. A multiple of the page
/// size, so slot `n`'s guard is page-aligned given the region is.
const SECONDARY_STACK_SLOT: usize = SECONDARY_STACK_GUARD + SECONDARY_STACK_SIZE;

/// One boot stack per core, each over its own guard page. Slot 0 is unused (core 0 has its own
/// stack from link-aarch64.ld); slots `1..MAX_CPUS` belong to the secondaries. On RISC-V the boot
/// hart is whichever one OpenSBI picked, so the unused slot is not always 0.
///
/// **The `UnsafeCell` is load-bearing, and not for interior mutability the usual way.** An
/// immutable `static` of plain arrays lands in **`.rodata`**, which the fine kernel map makes
/// **read-only** (W^X), and a stack you cannot write is not a stack. That bug is invisible on the
/// coarse boot map (where `.rodata` is writable) and fires the instant a secondary adopts the fine
/// map. See DECISIONS.md §11.
///
/// **The `link_section` is what makes the guard pages possible** (milestone 90). These used to be
/// ordinary `.bss`, and `map_everything` maps `.data`..`__bss_end` as one range, so there was
/// nowhere to put a hole. In their own region the mapper maps each stack separately and skips each
/// guard. The region is `(NOLOAD)`, so nothing zeroes it, which `.bss` was getting for free from
/// boot.s: a stack does not need it, and the high-water instrument paints every usable byte of one
/// in a test build anyway.
///
/// The core count stays here rather than being written again in two linker scripts: the scripts
/// anchor `__secondary_stacks_start`..`__secondary_stacks_end` around whatever this emits, and
/// `the_secondary_stack_region_is_the_size_the_linker_reserved` holds the two together.
#[repr(C, align(4096))]
struct Stacks(core::cell::UnsafeCell<[[u8; SECONDARY_STACK_SLOT]; MAX_CPUS]>);

// SAFETY: each core writes only its OWN slot, as its stack via SP, so no two cores mutate the same
// bytes. The cell exists to place the stacks in writable memory, not to share them.
unsafe impl Sync for Stacks {}

#[unsafe(link_section = ".secondary_stacks")]
static SECONDARY_STACKS: Stacks = Stacks(core::cell::UnsafeCell::new(
    [[0; SECONDARY_STACK_SLOT]; MAX_CPUS],
));

/// How many secondaries have reached [`secondary_main`] and are idling.
static ONLINE: AtomicUsize = AtomicUsize::new(0);

/// A bitmask of the online cores, by id. The boot core sets its bit before bring-up; each secondary
/// sets its own as it comes online. RISC-V uses it as the hart mask for SBI RFENCE TLB shootdown
/// (aarch64's `tlbi ..., is` broadcasts in hardware and needs no mask). Kept here, portable, because
/// which cores are online is a scheduler fact, not an arch one.
static ONLINE_MASK: AtomicUsize = AtomicUsize::new(0);

/// How many cores are online: the boot core plus the secondaries that came up. Used to spread work
/// (SMP step 3c) without baking in a core count.
pub fn online_count() -> usize {
    ONLINE.load(Ordering::Acquire) + 1
}

/// The bitmask of online cores (by id). RISC-V's TLB shootdown and icache push target every online
/// core but the one doing the flush (aarch64's `tlbi ..., is` broadcasts in hardware and needs no
/// mask there); [`online_cpus`] is this mask as an iterator on both ISAs.
pub fn online_harts_mask() -> usize {
    ONLINE_MASK.load(Ordering::Acquire)
}

/// The online cpus, by id, in ascending order: the mask above as an iterator. The online set is
/// NOT contiguous from zero on real boards (the VisionFive 2's slot 0 is the excluded S7, so its
/// set is {1,2,3}), and `0..online_count()` as an index range is the bug the first-silicon bench
/// found in `pick_spawn_target` (2026-08-14): modulo-count produced index 0, a placement into a
/// parked core's inbox that nothing will ever drain. Iterate the set, never the count.
///
/// The mask arithmetic lives in `crates/cpu_set`, where the {1,2,3} shape QEMU cannot boot is
/// host-tested; this adds only the live mask.
pub fn online_cpus() -> cpu_set::Cpus {
    cpu_set::cpus_in(online_harts_mask())
}

/// The k-th online cpu, wrapping: the sampler's safe form of "a random online cpu". Falls back to
/// the calling cpu only if the mask is somehow empty (before the boot core registers itself).
pub fn nth_online(k: usize) -> usize {
    cpu_set::nth_in(online_harts_mask(), k).unwrap_or(crate::cpu::id())
}

/// Set by each secondary's probe thread, indexed by the core it actually ran on. The proof that a
/// secondary schedules real work from its own queue, not just idles. See `secondary_main` step 5.
#[cfg(test)]
static RAN_ON: [core::sync::atomic::AtomicBool; MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; MAX_CPUS];

/// Set by a probe placed on a specific core via `spawn_on`: indexed by the core it was **placed
/// on**, holding the core it actually **ran on**, plus one (so zero means "has not run yet").
///
/// Both halves matter, and they used to be conflated: the index was the core it ran on, which made
/// the array a record of which cores had been busy rather than of which placements arrived. Since
/// DECISIONS §28.3 an idle core may steal a placed thread before its target runs it, so the two are
/// genuinely different questions. See `work_can_be_placed_on_every_core`.
#[cfg(test)]
static SPREAD: [core::sync::atomic::AtomicU32; MAX_CPUS] =
    [const { core::sync::atomic::AtomicU32::new(0) }; MAX_CPUS];

/// The low end of core `id`'s slot, which is its **guard page**: one page of address space the
/// kernel map deliberately leaves unmapped. `arch::mmu::map_everything` skips exactly this page per
/// core, and `verify` refuses to install a map in which it translates.
pub fn secondary_stack_guard(id: usize) -> u64 {
    SECONDARY_STACKS.0.get() as u64 + (id as u64) * SECONDARY_STACK_SLOT as u64
}

/// Core `id`'s usable stack as `(bottom, top)`: the slot above its guard page. Used by the mapper
/// (what to map) and by the high-water report (what to paint and scan, milestone 84). The boot
/// core's slot exists but is never painted and never used; the report skips it.
pub fn secondary_stack_span(id: usize) -> (u64, u64) {
    let bottom = secondary_stack_guard(id) + SECONDARY_STACK_GUARD as u64;
    (bottom, bottom + SECONDARY_STACK_SIZE as u64)
}

/// The whole region, `(start, end)`, for the check against what the linker reserved.
#[cfg(test)]
fn secondary_stack_region() -> (u64, u64) {
    let start = SECONDARY_STACKS.0.get() as u64;
    (start, start + (MAX_CPUS * SECONDARY_STACK_SLOT) as u64)
}

/// The high-VA top of core `id`'s boot stack. Stacks grow down, so the top is the high address, and
/// the 16-byte alignment `sp` requires comes from the region's page alignment and every size here
/// being a multiple of 16. See notes/stack.md.
fn stack_top(id: usize) -> u64 {
    secondary_stack_span(id).1
}

/// The hardware id of each core the machine described, indexed by logical id. `u64::MAX` in a slot
/// the tree did not fill. Written once by [`read_cpu_list`], before any secondary exists.
///
/// An "id" here is whatever the bring-up call names a core by: an `MPIDR_EL1[39:0]` affinity value
/// on aarch64, a hart id on RISC-V. Nothing portable interprets it; it is read out of `/cpus` and
/// handed back to `arch::psci_cpu_on`.
///
/// `u64::MAX` is safe as the empty marker rather than merely convenient: an aarch64 affinity value
/// is 40 bits by architecture, so it cannot reach it, and a RISC-V hart id that could would fail the
/// hardware-id-equals-index check in [`bring_up_secondaries`] long before anything indexed by it.
static HWID: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];

/// Per slot: did the tree describe a core this kernel could actually start? False for a core
/// firmware marked `status = "disabled"`, for one whose own `riscv,isa` disclaims supervisor mode
/// (the VisionFive 2's S7, whose vendor node lies about everything else; bench, 2026-08-14), and
/// for one whose `enable-method` is a mechanism this kernel does not speak (`spin-table`, or a
/// per-SoC method).
static STARTABLE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// How many `cpu@` nodes the tree had, which may be more than the slots that got seated. Kept so
/// the boot line can say "this machine has more cores than this kernel was built for" instead of
/// quietly using fewer.
static DESCRIBED: AtomicUsize = AtomicUsize::new(0);

/// **Read the machine's core list out of its device tree** (milestone 100).
///
/// Called once, on the boot core, before [`bring_up_secondaries`] and while the device tree is still
/// mapped (both boots call it in the same breath as `arch::isa::init`, ahead of `mmu::init`
/// replacing the coarse boot map). It records the roster and prints it; it starts nothing.
///
/// Until this existed the core list was `0..MAX_CPUS`, a constant, and on QEMU `virt` that is the
/// right answer for the wrong reason: `virt` numbers its cores densely from zero, so the constant
/// and the machine agreed. On a board that numbers them any other way the kernel would have called
/// `CPU_ON` on ids that are not there and never called it on the ids that are, and the second half
/// of that produces no error at all, because nothing asks about a core it never named.
///
/// # Panics
///
/// If the blob is unreadable. Both callers run after `memory::init` has already parsed the same
/// pointer, so a failure here means something un-mapped the tree between the two, which is a kernel
/// bug rather than a machine we should tolerate.
pub fn read_cpu_list(dtb_ptr: usize) {
    // SAFETY: the pointer firmware handed us, whose magic `memory::init` has already checked, named
    // through the boot map's direct region because we are running virtual. Same call, same window.
    let dt = unsafe { dtb::Dtb::from_ptr(arch::mmu::phys_to_virt(dtb_ptr as u64) as *const u8) }
        .expect("device tree is unreadable");
    let list = CpuList::from_device_tree(&dt).expect("cannot read /cpus");

    // Seat each described core at the slot its own hardware id names. A slot IS a hart id:
    // everywhere the kernel indexes hardware (the per-CPU blocks, the secondary stacks, the RISC-V
    // trap stashes, GIC and PLIC targets, IPI hart masks) it uses the logical id, so seating by
    // hwid makes logical-id-equals-hardware-id hold by construction rather than by a positional
    // assignment checked afterwards at bring-up. It is also what keeps an excluded core from
    // costing anyone else a seat: the old `take(MAX_CPUS)` truncated the *described* list before
    // startability was known, so on the VisionFive 2 the unusable S7 consumed a slot and U74
    // hart 4 fell off the end, parked with nothing to report (bench, 2026-08-14). Seated by id,
    // the S7 occupies only slot 0, its own, and hart 4 sits at slot 4.
    //
    // The startability predicate is `isa::cpu_list::Cpu::startable`, host-tested against both
    // JH7110 fixtures (the mainline tree's `status` lie and the vendor tree's ISA-string truth);
    // see it for why `Unstated` passes and why supervisor mode is required.
    let mut startable = 0;
    let mut unseated = 0; // startable cores whose hart id names no slot
    for cpu in list.cpus().iter() {
        let ok = cpu.startable();
        startable += usize::from(ok);
        let Some(slot) = usize::try_from(cpu.hwid).ok().filter(|&s| s < MAX_CPUS) else {
            // A core this build cannot seat: its id is past the per-CPU statics (or it is a
            // clustered aarch64 machine's Aff1-bearing affinity value, same refusal as ever, made
            // earlier). Only counted against us when we could otherwise have run it.
            unseated += usize::from(ok);
            continue;
        };
        if HWID[slot].load(Ordering::Relaxed) != u64::MAX {
            // Two nodes claiming one id is a malformed tree (or `cpu_list`'s missing-`reg`
            // fallback of zero colliding with the real hart 0). First claim wins; say so.
            println!("  smp: two cpu@ nodes claim hart id {slot}; keeping the first");
            continue;
        }
        HWID[slot].store(cpu.hwid, Ordering::Relaxed);
        STARTABLE[slot].store(ok, Ordering::Relaxed);
    }
    DESCRIBED.store(list.described, Ordering::Release);

    // The RISC-V timer's counter rate is a `/cpus` property and is read separately, earlier in that
    // boot, because `timer::init` runs several steps ahead of this. See arch/riscv64/timer.rs.
    print!("  smp: {} core(s) in the device tree", list.described);
    if startable != list.described {
        print!(", {startable} startable");
    }
    if unseated > 0 {
        print!(
            " ({unseated} of them with hart ids past cpu::MAX_CPUS = {MAX_CPUS}, which sizes the \
             per-CPU statics, so they stay parked)"
        );
    }
    println!();

    // One honest line per hart excluded for missing supervisor mode, in the machine's own terms.
    // On the VisionFive 2's vendor tree this is the only place the S7's exclusion is visible at
    // all: status and mmu-type both claim a schedulable core, and trusting them handed the
    // S-incapable hart to sbi hart_start and crashed vendor OpenSBI (bench, 2026-08-14).
    for cpu in list.cpus().iter() {
        if !cpu.supervisor {
            println!(
                "  smp: cpu {}'s riscv,isa names user mode and not supervisor: it cannot run \
                 this kernel, whatever status and mmu-type claim; not started",
                cpu.hwid,
            );
        }
    }

    // The core we are executing on has to be in the roster at its own index, or the roster describes
    // a machine other than the one running this code. Nothing downstream can recover from that, but
    // saying it beats letting the bring-up report a plausible number.
    let boot = arch::boot_cpu_id();
    if hwid(boot) != Some(boot as u64) {
        println!(
            "  smp: the boot core is logical {boot}, and the device tree does not put a core with \
             that id there"
        );
    }
}

/// The hardware id of logical core `id`, or `None` for a slot no described core's hart id names.
///
/// Since seating went by-id (2026-08-14) a filled slot holds its own index by construction, so a
/// `Some` answer is always `Some(id)`; the value of asking is the `None`, which distinguishes a
/// seat the machine filled from one it did not.
#[cfg_attr(not(test), allow(dead_code))]
pub fn hwid(id: usize) -> Option<u64> {
    match HWID.get(id)?.load(Ordering::Acquire) {
        u64::MAX => None,
        h => Some(h),
    }
}

/// How many cores the device tree described, which is not how many this kernel can use. Greater than
/// [`MAX_CPUS`] on a bigger machine than this build is sized for.
#[cfg_attr(not(test), allow(dead_code))]
pub fn described_count() -> usize {
    DESCRIBED.load(Ordering::Acquire)
}

unsafe extern "C" {
    /// The physical-mode entry every secondary starts at. Defined in boot.s.
    fn secondary_boot();
}

/// Start every secondary core the machine described, and wait until each has come online.
///
/// Called once, on core 0, after the heap and scheduler exist (a secondary's stack is static, so
/// bring-up itself needs no allocator, but step 3 will) and after [`read_cpu_list`] has recorded
/// which cores there are. Core 0 keeps doing all real scheduling work; the secondaries only idle
/// until then.
///
/// # BUGS
///
/// - **A core whose hardware id names no slot is refused, loudly, rather than remapped**
///   (milestone 100, reshaped by the by-id seating, 2026-08-14). The kernel indexes hardware by
///   logical id everywhere: `cpu::PERCPU`, the secondary stacks and the RISC-V trap stashes are
///   arrays indexed by it; the GICv2 targets an SPI by CPU-interface number and `send_sgi` by that
///   same number; the PLIC's S-mode context table (`arch::irq`) is indexed by hart id and the SBI
///   IPI mask is a bitmap of hart ids. Giving a core a logical id other than its hardware id would
///   need every one of those to translate, so [`read_cpu_list`] seats each core at the slot its
///   hardware id names and the equation holds by construction. The refusal that used to live here
///   (a positional slot whose recorded hwid was not its index) is now the seating's: a core whose
///   id is at or past `MAX_CPUS`, which includes every id on a clustered aarch64 board
///   (`MPIDR_EL1.Aff1` in use), gets a named line at roster time and stays parked. A machine whose
///   ids are sparse but small (say harts {0, 2}) now simply works, seats {0, 2}, where the
///   positional roster refused it; nothing between the ids is started or counted.
///
///   What that entanglement no longer includes, since the first-silicon sweep (2026-08-14): the
///   assumption that the online *logical ids* are contiguous from zero. Every per-cpu loop, both
///   IRQ-affinity round-robins and the spawn/wake placement now go through [`online_cpus`] /
///   [`nth_online`] (or mask with [`online_harts_mask`], as the RISC-V RFENCE calls do), so a
///   machine whose online set is {1,2,3,4}, the VisionFive 2 with all four U74s seated, is
///   handled (notes/visionfive2.md).
/// - **The conduit half is proved by parsing and not by calling.** `crates/isa`'s host tests read a
///   real QEMU dump that states `smc`, so the decode is exercised; no machine here boots that
///   configuration, so the `smc` instruction path has never executed. See `arch::psci_cpu_on`.
pub fn bring_up_secondaries() {
    // Record the boot core as online before starting anyone, so a TLB shootdown from here on names
    // it. (Secondaries add their own bits as they come up.)
    ONLINE_MASK.fetch_or(1 << cpu::id(), Ordering::Release);

    // Paint every secondary's stack before any CPU_ON, while the slots are still untouched `.bss`,
    // so no live frame can be painted over (milestone 84). Whole slots: nothing has run on them.
    #[cfg(test)]
    for id in 0..MAX_CPUS {
        if id != cpu::id() {
            let (b, t) = secondary_stack_span(id);
            // SAFETY: a secondary's stack slot is `.bss`, mapped by the kernel's own image, and no
            // `CPU_ON` has been issued yet, so nothing has ever run on it. The `id != cpu::id()`
            // skips the boot core, which is on the linker-script stack and is live right now.
            unsafe { crate::stack::paint(b, t) };
        }
    }

    // The entry point PSCI needs is PHYSICAL: the core starts with its MMU off. Cast the
    // function item through a pointer (not straight to an integer, which the compiler warns on).
    let entry = arch::mmu::virt_to_phys(secondary_boot as *const () as u64);

    // What starts a core here, and whether this machine can be asked at all. The arch prints the
    // mechanism it found (the PSCI conduit and function id on aarch64, read out of `/psci`); a
    // machine that states none is a real machine and this is where we say so, once, rather than
    // firing calls into an exception level that may not exist.
    arch::print_bring_up_mechanism();
    if !arch::can_start_secondaries() {
        println!("  smp: 1 core(s) online");
        return;
    }

    let mut started = 0;
    for id in 0..MAX_CPUS {
        // Start every seated core but the one we booted on. On aarch64 that is always core 0; on
        // RISC-V OpenSBI's boot hart is not guaranteed to be 0, so skip whichever this is.
        if id == cpu::id() {
            continue;
        }
        let hwid = HWID[id].load(Ordering::Relaxed);
        if hwid == u64::MAX {
            // No described core's hart id names this slot: an empty seat, not a refusal.
            // `read_cpu_list` already reported anything it could not seat.
            continue;
        }
        if !STARTABLE[id].load(Ordering::Relaxed) {
            println!("  smp: cpu {id} is not startable by this kernel (disabled, no supervisor");
            println!("       mode, or an enable-method we do not speak); leaving it alone.");
            continue;
        }
        // Seating placed this core at the slot its hardware id names (see read_cpu_list), so the
        // equation the rest of the kernel indexes by holds by construction; this cannot fire.
        debug_assert_eq!(hwid, id as u64, "a seated core is not at its own hart id");
        let ret = arch::psci_cpu_on(hwid, entry, stack_top(id));
        if ret == 0 {
            started += 1;
        } else {
            // A core the firmware declines to start returns an error rather than hanging. Degrade:
            // note it and carry on with the cores we have. Before milestone 100 this line's "not
            // present?" was the only thing standing between us and a wrong core list, because
            // `0..MAX_CPUS` on a machine with fewer cores lands here on every absent one.
            println!("  smp: cpu {id} did not start (firmware returned {ret})");
        }
    }

    // Wait for every core we started to check in. Acquire pairs with the Release in
    // secondary_main: once the count is visible, so is everything that core did before it.
    while ONLINE.load(Ordering::Acquire) < started {
        core::hint::spin_loop();
    }

    println!("  smp: {} core(s) online", started + 1);
}

/// A secondary core's first Rust, called from `secondary_boot` once its MMU is on and it has a
/// stack. `cpu_id` came from `MPIDR_EL1`. It turns the core into a full scheduler participant and
/// then becomes that core's idle thread. (Step 3b-ii.)
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(cpu_id: usize) -> ! {
    // 1. Per-CPU pointer first, ahead of anything that could take a lock (the lock path reads this
    //    core's held-rank out of its PerCpu block). See cpu.rs.
    cpu::init_this_cpu(cpu_id);

    // 2. This core's exception vectors. VBAR_EL1 is per-core, so a secondary that never sets it
    //    jumps to a garbage vector on its first fault or timer tick and dies silently (and, if it
    //    died holding a lock, hangs the others). Set it before anything can trap. The vector table
    //    lives in kernel .text, mapped in both the coarse boot map and the fine map, so this is
    //    valid on either side of the switch below.
    arch::init();

    // 3. Adopt the kernel's fine map. The coarse boot map reaches only the first 2 GiB of the high
    //    half, not the thread-stack area 64 GiB up, so without this the core could not run a spawned
    //    thread. See arch::mmu::init_secondary.
    arch::mmu::init_secondary();

    // 4. Become a scheduler participant: adopt the context we are on as this core's idle thread,
    //    and reserve this core's run queue.
    crate::sched::adopt_secondary_idle();

    // 5. This core's interrupt hardware: its GIC CPU interface, then its timer (the PPI is banked,
    //    so this arms THIS core's tick, which is this core's source of preemption).
    crate::arch::irq::init_this_cpu();
    arch::timer::init();

    // 6. (tests only) Prove this core schedules from its OWN queue by spawning a probe onto it.
    //
    //    `spawn_on(cpu::id(), ..)`, NOT `spawn`, and that is the whole point of the probe. Plain
    //    `spawn` places by DECISIONS §28's power of two choices: the thread lands on the lighter of
    //    two randomly sampled cores, deliberately not the spawner's. So each secondary's probe went
    //    to a random core, several probes piled onto the same one, and the cores nobody happened to
    //    sample never set their `RAN_ON` slot. The test then waited for a condition that could not
    //    become true, and only passed when the random placement happened to cover every core.
    //
    //    It read as host slowness for a long time, because a random placement moves between runs
    //    exactly the way a loaded runner does. Widening the wait from 10 s to 60 s changed nothing,
    //    which is what finally separated the two: a deadline cannot fix an unreachable condition.
    //    Second time §28 has invalidated a test's placement assumption; see the reap wait in
    //    notes/riscv-parity-scope.md, which was a yield count until §28 broke it the same way.
    //
    //    The comment here used to say "there is no migration yet, so it runs here". That was true
    //    when it was written and stopped being true at §28, with nothing to catch it.
    #[cfg(test)]
    crate::sched::spawn_on(cpu::id(), || {
        RAN_ON[cpu::id()].store(true, Ordering::Release);
    })
    .expect("a secondary could not spawn its probe");

    // 7. Online (Release, so a core seeing the count also sees everything set up above), and from
    //    the next line on, preemptible. Add this core to the online mask first, so a TLB shootdown
    //    that runs the instant we are counted also targets us.
    ONLINE_MASK.fetch_or(1 << cpu::id(), Ordering::Release);
    ONLINE.fetch_add(1, Ordering::Release);
    arch::interrupts::enable();

    // 8. This core's idle loop, which is now its idle thread's body: the shared idle body that also
    //    steals work from a loaded core (DECISIONS §28). Yield first so a thread already queued (the
    //    probe) runs at once, then hand off to `run_idle`, which steals, parks in wfi, and yields.
    crate::sched::yield_now();
    crate::sched::run_idle()
}

// The SMP tests need more than one core online (the runners default to `-smp 4`; NIFE_SMP moves
// it, up to MAX_CPUS). They run on both
// architectures now that RISC-V brings up secondary harts via SBI HSM (parity workstream A): the
// bring-up, cross-core work placement (inbox + reschedule IPI), and load balancing are all portable
// and exercised identically on the GIC/SGI and PLIC/SBI-IPI paths.
#[cfg(test)]
mod tests {
    use super::*;

    /// Wait for `cond`, bounded by the CLOCK rather than by a yield count.
    ///
    /// Every wait in this module used to be `spins < 5_000_000` yields, and a yield count is not a
    /// duration. On the machine the number was calibrated on it was generous; on a four-vCPU CI runner
    /// emulating four cores under TCG, this core can burn five million cheap yields long before the
    /// secondaries it is waiting for have been scheduled at all. That is not a hang, it is an
    /// impatient observer, and it failed `main` on 2026-07-30 in two different places for the same
    /// reason. Exactly the defect fixed the same day in the reap waits (kernel/src/user.rs): wait on
    /// the clock, not on a spin count.
    ///
    /// Ten seconds is far beyond any honest completion here (these are sub-second locally) and well
    /// inside the harness's 90 s per-test ceiling, which remains the real backstop for a genuine hang.
    /// Still a leak trap rather than a masked failure: work that never runs times out and fails.
    /// Wait for `cond`, yielding, up to a budget chosen to sit **inside** the suite's per-test
    /// watchdog.
    ///
    /// **Sixty seconds, not ten, and the reason is host contention rather than guest slowness.**
    /// These tests assert that work placed on a secondary actually ran there. On a shared CI runner
    /// (or a laptop running several QEMU instances) the guest's vCPU threads are starved of *host*
    /// CPU, so guest wall clock advances while the secondaries get no chance to run at all. Ten
    /// seconds was short enough that ordinary CI load produced a red run on unmodified `main`:
    /// measured at 1 failure in 6 runs under synthetic load, and on 2026-08-02 it failed three
    /// pull requests in a row, each time on a different RISC-V CPU model. **A failure that moves
    /// between models is a statement about the host, not about the ISA.**
    ///
    /// Widening a timeout usually hides a bug, and this project has said so in this very file's
    /// neighbourhood. It does not here, and the distinction is what makes the change honest:
    /// `all_ran` is *exactly* the property under test, not a proxy for it. Widening a wait that
    /// stands in for the property (a yield count, a whole-machine thread headcount) hides the
    /// property breaking; widening a wait on the property itself only delays noticing, and the thing
    /// that notices is the layer below.
    ///
    /// **That layer is why this is 60 and not unbounded.** `DEFAULT_BUDGET_SECS` fails any test at
    /// 90 s, so a genuinely dead secondary is caught either way. Staying under it means this test
    /// reports `secondary cores did not run scheduled work in time`, which names the subsystem,
    /// rather than the watchdog's generic livelock message. Two deadlines for one property is what
    /// produced the false reds; one of them is now clearly subordinate to the other.
    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = crate::arch::timer::now() + 60 * crate::arch::timer::frequency();
        while crate::arch::timer::now() < deadline {
            if cond() {
                return true;
            }
            crate::sched::yield_now();
        }
        cond()
    }

    /// **Every secondary stack sits on an unmapped page** (milestone 90).
    ///
    /// Proven by walking the live kernel page tables, not by overflowing a stack: `translate` reads
    /// the root out of the hardware register (`TTBR1_EL1` / `satp`), so this is the map the CPU is
    /// actually using. A test that faulted on purpose would be a test the suite could not survive,
    /// and this catches the thing that would actually go wrong, someone mapping the region as one
    /// range again and closing the holes.
    ///
    /// The pages either side of each hole must translate, or the hole is in the wrong place and
    /// protects nothing. That is the failure the boot stack's own guard test guards against
    /// (`the_guard_page_is_a_hole` in each arch's mmu.rs), and it is worth repeating per core here
    /// because these holes come from a loop over slots rather than from one linker symbol.
    ///
    /// This says nothing about the **coarse boot map**, on which the guards are inside a 2 MiB
    /// block and are therefore mapped. A secondary is on that map only between `secondary_boot` and
    /// `mmu::init_secondary`, a few instructions of Rust; the boot stack's guard has exactly the
    /// same window. See notes/stack-high-water.md.
    #[test_case]
    fn every_secondary_stack_sits_on_a_guard_page() {
        for id in 0..MAX_CPUS {
            let guard = super::secondary_stack_guard(id);
            let (bottom, top) = secondary_stack_span(id);

            assert_eq!(
                guard % 4096,
                0,
                "core {id}'s guard page is not page-aligned"
            );
            assert_eq!(
                guard + 4096,
                bottom,
                "core {id}'s guard is not under its stack"
            );

            assert!(
                crate::arch::mmu::translate(guard).is_none(),
                "core {id}'s guard page at {guard:#x} IS mapped: a deep secondary would scribble \
                 instead of faulting",
            );
            assert!(
                crate::arch::mmu::translate(bottom).is_some(),
                "core {id}'s stack is not mapped at its bottom {bottom:#x}",
            );
            assert!(
                crate::arch::mmu::translate(top - 16).is_some(),
                "core {id}'s stack is not mapped at its top {top:#x}",
            );
        }
    }

    /// **The linker reserved exactly the region the Rust side emits** (milestone 90).
    ///
    /// `MAX_CPUS` lives in Rust and the two linker scripts only anchor whatever `.secondary_stacks`
    /// contains, so there is no second copy of the core count to drift. This holds that arrangement
    /// honest from the other side: if the static ever stops landing in the region (a lost
    /// `link_section`, a section renamed in one script), the stacks would still work and the guard
    /// pages would silently be somewhere else.
    #[test_case]
    fn the_secondary_stack_region_is_the_size_the_linker_reserved() {
        unsafe extern "C" {
            static __secondary_stacks_start: core::ffi::c_void;
            static __secondary_stacks_end: core::ffi::c_void;
        }
        let lo = (&raw const __secondary_stacks_start) as u64;
        let hi = (&raw const __secondary_stacks_end) as u64;
        let (start, end) = super::secondary_stack_region();

        assert_eq!(lo, start, "the stacks are not at the start of their region");
        assert!(
            end <= hi,
            "the stacks ({start:#x}..{end:#x}) overflow the region the linker reserved \
             ({lo:#x}..{hi:#x})",
        );
        assert_eq!(lo % 4096, 0, "the region is not page-aligned");
    }

    /// **The cores we started are the cores the machine described** (milestone 100).
    ///
    /// The list used to be `0..MAX_CPUS`, a constant that happens to be right on QEMU `virt`, so the
    /// assertion that matters is not the count but where it came from: the tree is re-read here,
    /// independently of the boot path, and its `cpu@` nodes must be the roster the bring-up used.
    ///
    /// It also pins the invariant the rest of the kernel still rests on. Per-CPU blocks, secondary
    /// stacks, RISC-V trap stashes, GIC targets, PLIC contexts and SBI IPI masks are all indexed by
    /// logical id, and `read_cpu_list` seats each core at the slot its hardware id names, so the
    /// two are equal by construction. This proves the seating put every described core where its
    /// id says, on the machine every merge boots.
    #[test_case]
    fn the_roster_is_the_machines_own_core_list() {
        let ptr = crate::DTB.load(Ordering::Relaxed);
        // SAFETY: the pointer firmware handed us, already parsed twice on this boot.
        let dt = unsafe { dtb::Dtb::from_ptr(arch::mmu::phys_to_virt(ptr as u64) as *const u8) }
            .expect("device tree is unreadable");
        let list = CpuList::from_device_tree(&dt).expect("cannot read /cpus");

        assert_eq!(
            super::described_count(),
            list.described,
            "the roster and a fresh read of the tree disagree about how many cores exist",
        );
        // The tree's count is the runner's `-smp`, which is no longer required to equal MAX_CPUS:
        // the constant is a ceiling (it sizes the per-CPU statics), not the machine. What the suite
        // does require is at least two cores (or the SMP tests prove nothing) and no more than the
        // statics can seat (a bigger `-smp` would park real cores, and
        // `every_core_the_tree_described_is_running` below would fail on the honest count anyway;
        // this names the cause first).
        assert!(
            list.described >= 2,
            "the SMP suite needs -smp of at least 2; the tree describes {}",
            list.described,
        );
        assert!(
            list.described <= MAX_CPUS,
            "the runner passed -smp {} but this kernel seats only {MAX_CPUS}: cores would stay \
             parked, so run the suite at or below cpu::MAX_CPUS",
            list.described,
        );

        for cpu in list.cpus().iter() {
            let slot = usize::try_from(cpu.hwid).expect("a virt hart id fits in usize");
            assert!(
                slot < MAX_CPUS,
                "hart {slot} has no seat: the runner's -smp exceeds cpu::MAX_CPUS",
            );
            // Seating is by hart id (read_cpu_list), so the slot a core occupies IS its hardware
            // id; this holds the two walks together the way the old positional roster check did.
            assert_eq!(
                super::hwid(slot),
                Some(cpu.hwid),
                "hart {} is not seated at the slot its own id names",
                cpu.hwid,
            );
            // The supervisor rule (bench, 2026-08-14) must change nothing on QEMU: every virt cpu
            // is an S-mode hart, whichever generation of ISA-string spelling its tree uses, so
            // the started set here is exactly what it was before the rule existed. A false
            // `supervisor` on this machine means the parser read a modern string's silence, or a
            // multi-letter extension, as a denial.
            assert!(
                cpu.startable(),
                "hart {} would be excluded from bring-up on the suite's own machine",
                cpu.hwid,
            );
        }
    }

    /// **Every core the machine described came online.**
    ///
    /// The pair of the test above: that one says the roster is the machine's, this one says we used
    /// all of it. Before milestone 100 there was no way to state this, because the count the
    /// bring-up worked from was a constant and could only ever agree with itself.
    #[test_case]
    fn every_core_the_tree_described_is_running() {
        assert_eq!(
            online_count(),
            super::described_count(),
            "the machine describes cores that never came online",
        );
    }

    /// **The online iterator is the live mask, exactly, and the sampler never leaves it** (the
    /// first-silicon sweep's mechanism, 2026-08-14). Every broadcast-style loop in the kernel now
    /// either iterates `online_cpus()` or masks with `online_harts_mask()` (the RISC-V RFENCE
    /// calls), so proving the iterator equals the mask is proving those loops visit exactly the
    /// online set: no parked slot, no skipped online cpu. The non-contiguous {1,2,3} shape this
    /// machine cannot boot is host-proven in `crates/cpu_set`; this pins the delegation to the
    /// live mask, on the machine every merge boots.
    #[test_case]
    fn the_online_set_is_the_mask_and_the_sampler_stays_inside_it() {
        let mask = online_harts_mask();
        let mut seen = 0usize;
        let mut last = None;
        for c in online_cpus() {
            assert!(
                mask & (1 << c) != 0,
                "online_cpus yielded cpu {c}, which the mask {mask:#b} does not name",
            );
            assert!(last.is_none_or(|p| c > p), "online_cpus is not ascending");
            seen |= 1 << c;
            last = Some(c);
        }
        assert_eq!(seen, mask, "online_cpus missed a cpu the mask names");
        assert_eq!(
            online_cpus().count(),
            online_count(),
            "iterator and count disagree"
        );

        let n = online_count();
        for k in 0..2 * n {
            let c = nth_online(k);
            assert!(mask & (1 << c) != 0, "nth_online({k}) = {c} is not online");
            assert_eq!(
                c,
                nth_online(k + n),
                "nth_online does not wrap with period online_count",
            );
        }
    }

    /// Every secondary reached `secondary_main` and set up its per-CPU pointer.
    ///
    /// `bring_up_secondaries` already waited for the count before `test_main` ran, so the other
    /// cores are online by now. The expectation is the machine's own core count (whatever `-smp`
    /// the runner passed), not `MAX_CPUS`: the constant is capacity, and a leg run at `-smp 5`
    /// on an eight-slot kernel has four secondaries, honestly.
    #[test_case]
    fn all_secondaries_came_online() {
        assert_eq!(
            ONLINE.load(Ordering::Acquire),
            super::described_count() - 1,
            "not every core the tree describes came online (described {}, online {})",
            super::described_count(),
            online_count(),
        );
    }

    /// **Every secondary actually runs scheduled work on its own core.** Each spawned a probe onto
    /// its own run queue as it came online; the probe records the core it ran on. This is the proof
    /// that a secondary is a real scheduler participant, not just idling, and it also exercises the
    /// reaper on a secondary (the probe exits, and its successor reaps it).
    ///
    /// The probes run concurrently on their own cores; we wait for them, bounded, yielding so this
    /// core does other work meanwhile rather than pure-spinning.
    #[test_case]
    fn every_secondary_runs_scheduled_work() {
        // A secondary is any core that is not the boot core. On aarch64 the boot core is 0, so this
        // is cores 1..N; on RISC-V QEMU's boot hart is not fixed, so it is every core but that one.
        let boot = crate::arch::boot_cpu_id();
        let is_secondary = |c: usize| c != boot;
        // The online set, not `0..MAX_CPUS`: only a core that came online spawned a probe, so on a
        // machine whose online set is not the full roster the fixed range would wait forever on
        // cores that never existed here (first-silicon sweep, 2026-08-14).
        let all_ran = || {
            online_cpus()
                .filter(|&c| is_secondary(c))
                .all(|c| RAN_ON[c].load(Ordering::Acquire))
        };

        assert!(
            wait_for(all_ran),
            "secondary cores did not run scheduled work in time"
        );

        for c in online_cpus().filter(|&c| is_secondary(c)) {
            assert!(
                RAN_ON[c].load(Ordering::Acquire),
                "secondary core {c} never ran scheduled work",
            );
        }
    }

    /// **Work can be placed on any core from another core** (SMP step 3c, the delivery mechanism):
    /// `spawn_on(target, ..)` pushes the thread into `target`'s inbox and pokes it with a reschedule
    /// SGI, and `target` itself takes it out onto its own run queue.
    ///
    /// One placement is in flight at a time and each is followed all the way to a reap, so the
    /// adoption this counts on the target can only be the thread this test just placed there.
    ///
    /// # What this asserts, and what it deliberately does not (milestone 78)
    ///
    /// It asserts **arrival at the named core**, read from that core's own adoption counter
    /// (`PerCpu::adopted`). It does **not** assert that the target then ran the thread, and the
    /// difference is the whole fix.
    ///
    /// The old version placed one persistent spinning probe per core and waited, up to 60 s, for
    /// every core to have run one. That is a load-balancing outcome rather than a delivery one, and
    /// the scheduler does not promise it: stealing is **pull-based from an idle core** (§28.3), and
    /// a core that is not idle neither steals nor pushes. So this sequence, which a contended host
    /// makes ordinary, wedges permanently:
    ///
    ///   1. the probe for core A is placed on A's run queue while the test thread runs on A;
    ///   2. an idle core B, which has no probe yet, steals it (a queued thread, so it is fair game);
    ///   3. the remaining probes are placed and every core is now busy, so nothing is idle;
    ///   4. A holds only the test thread, which never yields into idleness (`schedule()`: a thread
    ///      yielding into an empty run queue simply carries on), so A never steals a probe back.
    ///
    /// Nothing rebalances after that, and `SPREAD[A]` stays zero forever. The failure was therefore
    /// **not** "not yet, give it longer", which is what the old 60 s budget assumed and what
    /// milestone 78's first pass argued when it left this test alone; it was a condition that could
    /// not become true, reported as a timeout. It blocked merges on `rva23s64` and `thead-c906` on
    /// 2026-08-04, on pull requests that changed no kernel code.
    ///
    /// This is the **third** time §28 has invalidated a placement assumption in a test in this file;
    /// the comment in `secondary_main` step 6 is the second, and it is the one that named the rule a
    /// deadline cannot fix an unreachable condition.
    ///
    /// The claim that was lost is covered where it belongs: `every_secondary_runs_scheduled_work`
    /// proves each core runs what is on its own queue, and `a_batch_of_cpu_bound_work_reaches_every_core`
    /// proves placement plus stealing fills the whole machine. Delivery here, execution there.
    #[test_case]
    fn work_can_be_placed_on_every_core() {
        for s in &SPREAD {
            s.store(0, Ordering::Relaxed);
        }

        let n = online_count();
        let here = cpu::id();
        assert!(
            n >= 2,
            "a cross-core placement test needs at least two online cores; this machine has {n}",
        );

        // Every ONLINE core but this one, placed from here: the remote path, inbox plus SGI. The
        // set, not `0..n` (first-silicon sweep, 2026-08-14): the range would place probes into
        // parked cores' inboxes and wait forever for an adoption no one performs.
        for target in online_cpus().filter(|&c| c != here) {
            place_one_probe_on(target);
        }

        // **And this core as a target**, which needs the placement to come from somewhere else, or
        // it is not the cross-core path at all (`place_on` puts a local target straight onto our own
        // queue, with no inbox and no SGI). The placer cannot itself end up here: a thread changes
        // core only by a steal, a steal is initiated only by an idle core, and this core is running
        // the test thread, which never goes idle.
        let other = online_cpus()
            .find(|&c| c != here)
            .expect("n >= 2 checked above");
        let placer = crate::sched::spawn_on(other, move || place_one_probe_on(here))
            .expect("spawn_on of the placer failed");
        assert!(
            wait_for(|| SPREAD[here].load(Ordering::Relaxed) != 0),
            "core {here} never received a placement made from core {other}",
        );
        assert!(
            wait_for(|| !crate::sched::thread_present(placer)),
            "the placer thread on core {other} was never reaped",
        );
    }

    /// Place one probe on `target` and follow it to a reap: the target must take it out of its own
    /// inbox (or, for a local target, off its own queue), the probe must run, and it must be reaped.
    ///
    /// Where it ran is recorded but not asserted, for the reason in the caller: a steal may move it
    /// first, and `spawn_on` is a placement hint rather than a pin (an explicit pin is a deferred
    /// §28 trigger).
    fn place_one_probe_on(target: usize) {
        let local = target == cpu::id();
        let adopted_before = cpu::of(target).adopted();

        let tid = crate::sched::spawn_on(target, move || {
            SPREAD[target].store(cpu::id() as u32 + 1, Ordering::Relaxed);
        })
        .expect("spawn_on failed");

        if !local {
            assert!(
                wait_for(|| cpu::of(target).adopted() > adopted_before),
                "core {target} never took the thread placed on it out of its inbox: the \
                 reschedule IPI is not reaching that core, or its drain is not running",
            );
        }

        assert!(
            wait_for(|| SPREAD[target].load(Ordering::Relaxed) != 0),
            "the probe placed on core {target} was delivered but never ran anywhere",
        );
        // Leave nothing behind for a later test's thread or frame accounting to find in flight.
        assert!(
            wait_for(|| !crate::sched::thread_present(tid)),
            "the probe placed on core {target} was never reaped",
        );
    }

    /// **A batch of cpu-bound work fills the whole machine** (SMP load balancing; DECISIONS §28).
    /// Offer the scheduler more runnable threads than cores and every core must end up running one.
    ///
    /// The measure is deliberately "which core is a thread running on *now*", marked every spin, not
    /// "which core did a thread first land on". Since §28 the two differ: `spawn` places work, but
    /// idle cores then *steal* it, so a thread migrates. A first-landing count would
    /// race (a core's placed thread can be stolen before that core runs it); the running-now mark
    /// captures placement and stealing together, which is the property that matters, the whole machine
    /// is busy. The threads spin until released so the work persists long enough for stealing to reach
    /// every idle core, then they exit and are reaped (the no-leak proxy would catch it otherwise).
    #[test_case]
    fn a_batch_of_cpu_bound_work_reaches_every_core() {
        static ON: [core::sync::atomic::AtomicU32; MAX_CPUS] =
            [const { core::sync::atomic::AtomicU32::new(0) }; MAX_CPUS];
        static RELEASE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        for r in &ON {
            r.store(0, Ordering::Relaxed);
        }
        RELEASE.store(false, Ordering::Relaxed);

        let n = online_count();
        for _ in 0..(n * 4) {
            crate::sched::spawn(|| {
                // Mark whatever core we are running on right now, every pass, until released. A
                // thread stolen onto an idle core marks that core here, which is exactly what proves
                // the idle core got fed.
                while !RELEASE.load(Ordering::Relaxed) {
                    ON[cpu::id()].store(1, Ordering::Relaxed);
                    core::hint::spin_loop();
                }
            })
            .expect("spawn failed");
        }

        // Every ONLINE core must get fed; `(0..n)` would demand a mark from a parked slot that can
        // never make one and ignore online cores past the count (first-silicon sweep, 2026-08-14).
        let done = || online_cpus().all(|c| ON[c].load(Ordering::Relaxed) != 0);
        assert!(
            wait_for(done),
            "cpu-bound work never reached every core: placement + stealing did not fill the machine",
        );
        // Let them all finish, so they are reaped rather than left spinning.
        RELEASE.store(true, Ordering::Relaxed);
    }

    /// **A kernel thread that migrates between harts keeps a per-CPU pointer that names the hart it
    /// is actually on** (DECISIONS §28; the RISC-V `trap.s` S-mode `tp` fix).
    ///
    /// RISC-V keeps the kernel per-CPU pointer in `tp`, a general register the trap frame saves and
    /// restores. A kernel thread preempted on one hart carries `tp = &percpu[origin]` in its frame;
    /// when an idle hart steals it, it used to resume through `trap_return` with that stale `tp` and
    /// then read, and corrupt, the origin hart's scheduler state (its idle thread was marked
    /// `Finished`, and that hart's next `schedule()` panicked "the last thread exited"). aarch64 never
    /// had the bug: its per-CPU pointer is `TPIDR_EL1`, a system register the frame never carries, so
    /// `percpu_matches_hart` there is a constant `true` and this test is a no-op.
    ///
    /// This drives exactly the corrupting motion: waves of cpu-bound workers, each alive long enough
    /// (several 100 Hz ticks) to be preempted and so to acquire a saved frame, and more of them than
    /// cores so that as the early finishers drain their harts to idle, those harts steal the still-
    /// running, already-preempted workers, migrating a framed kernel thread across harts. Every spin,
    /// each worker re-checks that `tp` still names the hart it runs on; one false reading is the bug.
    /// Pre-fix it trips within the first wave on RISC-V.
    #[test_case]
    fn a_migrated_kernel_thread_keeps_its_hart_pointer() {
        use core::sync::atomic::AtomicUsize;
        static BAD: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        static DONE: AtomicUsize = AtomicUsize::new(0);
        BAD.store(false, Ordering::Relaxed);
        DONE.store(0, Ordering::Relaxed);

        // ~40 ms per worker: at 100 Hz that is several preemption ticks, so a worker is certain to be
        // preempted (and thus stealable with a live frame) before it finishes.
        let life = crate::arch::timer::frequency() / 25;
        let n = online_count();
        let mut total = 0usize;

        for _wave in 0..6 {
            for _ in 0..(n * 3) {
                let spawned = crate::sched::spawn(move || {
                    let end = crate::arch::timer::now() + life;
                    while crate::arch::timer::now() < end {
                        if !crate::arch::percpu_matches_hart() {
                            BAD.store(true, Ordering::Relaxed);
                        }
                        core::hint::spin_loop();
                    }
                    DONE.fetch_add(1, Ordering::Relaxed);
                });
                if spawned.is_some() {
                    total += 1;
                } else {
                    // Thread table momentarily full (a prior wave still draining); stop adding and let
                    // it drain below. Not a failure: we still have plenty of migration in flight.
                    break;
                }
            }

            // Drain this wave, checking the invariant throughout. Time-based: an idle hart's yields
            // return at once under §28, so a fixed spin count would elapse in no time.
            let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
            while DONE.load(Ordering::Relaxed) < total {
                assert!(
                    !BAD.load(Ordering::Relaxed),
                    "a migrated kernel thread read a per-CPU pointer for the WRONG hart: a stale tp \
                     rode a hart migration (DECISIONS §28; trap.s S-mode tp handling)",
                );
                assert!(
                    crate::arch::timer::now() < deadline,
                    "migration workers never drained ({}/{} done)",
                    DONE.load(Ordering::Relaxed),
                    total,
                );
                crate::sched::yield_now();
            }
        }

        assert!(
            !BAD.load(Ordering::Relaxed),
            "a migrated kernel thread read a per-CPU pointer for the WRONG hart (stale tp across a \
             hart migration)",
        );
    }
}
