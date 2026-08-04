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

use isa::cpu_list::{CpuList, EnableMethod};

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

/// The bitmask of online cores (by id). RISC-V's TLB shootdown targets every online core but the one
/// doing the flush; see `arch::mmu::flush_tlb`.
#[cfg_attr(target_arch = "aarch64", allow(dead_code))] // aarch64's TLB broadcast needs no mask
pub fn online_harts_mask() -> usize {
    ONLINE_MASK.load(Ordering::Acquire)
}

/// Set by each secondary's probe thread, indexed by the core it actually ran on. The proof that a
/// secondary schedules real work from its own queue, not just idles. See `secondary_main` step 5.
#[cfg(test)]
static RAN_ON: [core::sync::atomic::AtomicBool; MAX_CPUS] =
    [const { core::sync::atomic::AtomicBool::new(false) }; MAX_CPUS];

/// Set by a probe placed on a specific core via `spawn_on`, indexed by the core it ran on. Proof
/// that cross-core placement (the inbox + reschedule SGI) actually delivers work to a chosen core.
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
/// firmware marked `status = "disabled"`, and for one whose `enable-method` is a mechanism this
/// kernel does not speak (`spin-table`, or a per-SoC method).
static STARTABLE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// How many slots of [`HWID`] are filled: `min(what the tree described, MAX_CPUS)`.
static ROSTER: AtomicUsize = AtomicUsize::new(0);

/// How many `cpu@` nodes the tree had, which may be more than [`ROSTER`]. Kept so the boot line can
/// say "this machine has more cores than this kernel was built for" instead of quietly using four.
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

    let n = list.len.min(MAX_CPUS);
    let mut startable = 0;
    for (i, cpu) in list.cpus().iter().take(n).enumerate() {
        HWID[i].store(cpu.hwid, Ordering::Relaxed);
        // `Unstated` counts as startable on purpose. The RISC-V CPU binding has no `enable-method`
        // at all (SBI HSM is the only mechanism), so demanding the property would refuse every
        // RISC-V machine including the one this kernel already runs on.
        let ok = cpu.usable
            && matches!(
                cpu.enable_method,
                EnableMethod::Psci | EnableMethod::Unstated
            );
        STARTABLE[i].store(ok, Ordering::Relaxed);
        startable += usize::from(ok);
    }
    ROSTER.store(n, Ordering::Release);
    DESCRIBED.store(list.described, Ordering::Release);

    // The RISC-V timer's counter rate is a `/cpus` property and is read separately, earlier in that
    // boot, because `timer::init` runs several steps ahead of this. See arch/riscv64/timer.rs.
    print!("  smp: {} core(s) in the device tree", list.described);
    if startable != list.described {
        print!(", {startable} startable");
    }
    if list.described > MAX_CPUS {
        print!(
            " (this kernel is built for {MAX_CPUS}: cpu::MAX_CPUS sizes the per-CPU statics, so the \
             rest stay parked)"
        );
    }
    println!();

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

/// The hardware id of logical core `id`, or `None` for a slot the tree did not describe.
///
/// The reverse of the assumption this milestone retired. Nothing outside the bring-up path calls it
/// yet, and the reason is the limitation in [`bring_up_secondaries`]'s BUGS: the rest of the kernel
/// indexes hardware by logical id, so it is only correct while the two are equal.
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
/// - **A core whose hardware id is not its logical id is refused, loudly, rather than started**
///   (milestone 100). Reading `/cpus` tells us the real ids; *using* an id that differs from the
///   core's index would need three other things to change with it, and none of them are in this
///   milestone. `cpu::PERCPU`, the secondary stacks and the RISC-V trap stashes are arrays indexed
///   by logical id; the GICv2 targets an SPI by CPU-interface number and `send_sgi` by that same
///   number; the PLIC's S-mode context is `2 * hart + 1` and the SBI IPI mask is a bitmap of hart
///   ids. Every one of those reads a logical id today and would need the hardware id instead. So
///   the honest state is: the list is read, the ids are checked, and a machine that numbers its
///   cores any other way gets a named refusal and a smaller machine instead of a silent no-op.
///   A clustered aarch64 board (`MPIDR_EL1.Aff1` in use) is exactly that machine.
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
            crate::stack::paint(b, t);
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
    for id in 0..ROSTER.load(Ordering::Acquire) {
        // Start every core the tree described but the one we booted on. On aarch64 that is always
        // core 0; on RISC-V OpenSBI's boot hart is not guaranteed to be 0, so skip whichever this
        // is. Both are the roster slot whose hardware id equals `boot_cpu_id()`, checked below.
        if id == cpu::id() {
            continue;
        }
        if !STARTABLE[id].load(Ordering::Relaxed) {
            println!("  smp: cpu {id} is not startable by this kernel (the tree disabled it, or");
            println!("       named an enable-method we do not speak); leaving it alone.");
            continue;
        }
        // The hardware id the machine gave this core, and the check the rest of the kernel needs it
        // to pass. See this function's BUGS: per-CPU state, GIC and PLIC targets and IPI masks are
        // all indexed by logical id, so an id that is not its index is a machine we can read and
        // cannot yet run on. Refusing it is the point of the milestone: the alternative is starting
        // nothing and reporting success.
        let hwid = HWID[id].load(Ordering::Relaxed);
        if hwid != id as u64 {
            println!("  smp: cpu {id} has hardware id {hwid:#x}, which is not its index.");
            println!(
                "       This kernel indexes per-CPU state, interrupt targets and IPI masks by"
            );
            println!("       logical id, so it cannot use that core yet. Not started.");
            continue;
        }
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

// The SMP tests need more than one core online (the runner passes `-smp 4`). They run on both
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
    /// logical id, so a hardware id that is not its index is a machine this kernel reads correctly
    /// and cannot yet run on. `bring_up_secondaries` refuses such a core by name; this proves the
    /// machine under the tests is not one, which is what makes that refusal path unreachable here.
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
        assert_eq!(
            list.described, MAX_CPUS,
            "the runner passes -smp {MAX_CPUS}, and the tree should say so",
        );

        for (id, cpu) in list.cpus().iter().enumerate() {
            assert_eq!(
                super::hwid(id),
                Some(cpu.hwid),
                "roster slot {id} does not hold the hardware id the tree gave that core",
            );
            assert_eq!(
                cpu.hwid, id as u64,
                "core {id} has a hardware id that is not its index: this kernel indexes hardware \
                 by logical id, so bring_up_secondaries would have refused it (see its BUGS)",
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

    /// Every secondary reached `secondary_main` and set up its per-CPU pointer.
    ///
    /// `bring_up_secondaries` already waited for the count before `test_main` ran, so the other
    /// cores are online by now. The runner passes `-smp 4`, so we expect `MAX_CPUS - 1`.
    #[test_case]
    fn all_secondaries_came_online() {
        assert_eq!(
            ONLINE.load(Ordering::Acquire),
            MAX_CPUS - 1,
            "not all secondary cores came online (is the runner passing -smp {MAX_CPUS}?)",
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
        let all_ran = || {
            (0..MAX_CPUS)
                .filter(|&c| is_secondary(c))
                .all(|c| RAN_ON[c].load(Ordering::Acquire))
        };

        assert!(
            wait_for(all_ran),
            "secondary cores did not run scheduled work in time"
        );

        for (c, ran) in RAN_ON.iter().enumerate() {
            if is_secondary(c) {
                assert!(
                    ran.load(Ordering::Acquire),
                    "secondary core {c} never ran scheduled work",
                );
            }
        }
    }

    /// **Work can be placed on any core from another core** (SMP step 3c, delivery mechanism). Core 0
    /// uses `spawn_on` to put one probe on each online core; a remote target is reached through its
    /// inbox and a reschedule SGI. Each probe marks the core it runs on, and every core must be hit.
    ///
    /// The probes are **persistent** (they spin until released) and there is exactly **one per core**,
    /// on purpose: DECISIONS §28 added work stealing, so a short probe placed on a core can be stolen
    /// away before that core runs it, and `spawn_on` is a placement hint, not a pin (an explicit pin
    /// is a deferred §28 trigger). One persistent probe per core keeps every core busy with its own,
    /// so no core goes idle and nothing is stolen, which is what lets this stay a clean test of the
    /// delivery path reaching each target. The batch-spread test above covers the stealing case.
    #[test_case]
    fn work_can_be_placed_on_every_core() {
        static RELEASE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        for s in &SPREAD {
            s.store(0, Ordering::Relaxed);
        }
        RELEASE.store(false, Ordering::Relaxed);

        let n = online_count();
        for target in 0..n {
            crate::sched::spawn_on(target, move || {
                while !RELEASE.load(Ordering::Relaxed) {
                    SPREAD[cpu::id()].store(1, Ordering::Relaxed);
                    core::hint::spin_loop();
                }
            })
            .expect("spawn_on failed");
        }

        let done = || (0..n).all(|c| SPREAD[c].load(Ordering::Relaxed) != 0);
        assert!(wait_for(done), "work placed on a core never ran there");
        RELEASE.store(true, Ordering::Relaxed); // let the probes finish and be reaped
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

        let done = || (0..n).all(|c| ON[c].load(Ordering::Relaxed) != 0);
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
