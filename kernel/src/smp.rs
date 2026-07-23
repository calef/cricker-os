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
//! handler. `sched::spawn_on(core, f)` is the primitive. (`spawn` itself still runs work on the
//! calling core; wiring it to round-robin over `spawn_on` is the remaining load-balancing policy.)

use crate::cpu::{self, MAX_CPUS};
use crate::{arch, println};
use core::sync::atomic::{AtomicUsize, Ordering};

/// 64 KiB per secondary, matching core 0's boot stack (link.ld).
///
/// No guard page yet: in step 2 a secondary only runs [`secondary_main`] and idles, so it
/// barely touches its stack. A guard page arrives with step 3, when secondaries run real work
/// and a stack overflow would otherwise walk into whatever `.bss` sits below. See
/// notes/stack.md.
const SECONDARY_STACK_SIZE: usize = 64 * 1024;

/// One boot stack per core. Slot 0 is unused (core 0 has its own from link.ld); slots
/// `1..MAX_CPUS` belong to the secondaries.
///
/// **The `UnsafeCell` is load-bearing, and not for interior mutability the usual way.** An
/// immutable `static` of plain arrays lands in **`.rodata`**, which the fine kernel map makes
/// **read-only** (W^X) — and a stack you cannot write is not a stack. That bug is invisible on the
/// coarse boot map (where `.rodata` is writable) and fires the instant a secondary adopts the fine
/// map. The `UnsafeCell` forces the stacks into writable `.bss` instead. See DECISIONS.md §11.
#[repr(C, align(16))]
struct Stacks(core::cell::UnsafeCell<[[u8; SECONDARY_STACK_SIZE]; MAX_CPUS]>);

// SAFETY: each core writes only its OWN slot, as its stack via SP, so no two cores mutate the same
// bytes. The cell exists to place the stacks in writable memory, not to share them.
unsafe impl Sync for Stacks {}

static SECONDARY_STACKS: Stacks = Stacks(core::cell::UnsafeCell::new(
    [[0; SECONDARY_STACK_SIZE]; MAX_CPUS],
));

/// How many secondaries have reached [`secondary_main`] and are idling.
static ONLINE: AtomicUsize = AtomicUsize::new(0);

/// How many cores are online: the boot core plus the secondaries that came up. Used to spread work
/// (SMP step 3c) without baking in a core count.
#[allow(dead_code)] // used by the SMP tests now, and by spawn's placement policy when it distributes
pub fn online_count() -> usize {
    ONLINE.load(Ordering::Acquire) + 1
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

/// The high-VA top of core `id`'s boot stack. Stacks grow down, so the top is base + size, and
/// the 16-byte alignment `sp` requires comes from `Stack`'s `align(16)` and the size being a
/// multiple of 16. See notes/stack.md.
fn stack_top(id: usize) -> u64 {
    // Slot `id` occupies [base + id*SIZE, base + (id+1)*SIZE); stacks grow down from the top.
    let base = SECONDARY_STACKS.0.get() as u64;
    base + (id as u64 + 1) * SECONDARY_STACK_SIZE as u64
}

unsafe extern "C" {
    /// The physical-mode entry every secondary starts at. Defined in boot.s.
    fn secondary_boot();
}

/// Start every secondary core, and wait until each has come online.
///
/// Called once, on core 0, after the heap and scheduler exist (a secondary's stack is static, so
/// bring-up itself needs no allocator, but step 3 will). Core 0 keeps doing all real scheduling
/// work; the secondaries only idle until then.
pub fn bring_up_secondaries() {
    // The entry point PSCI needs is PHYSICAL: the core starts with its MMU off. Cast the
    // function item through a pointer (not straight to an integer, which the compiler warns on).
    let entry = arch::mmu::virt_to_phys(secondary_boot as *const () as u64);

    let mut started = 0;
    for id in 1..MAX_CPUS {
        // QEMU virt numbers cores 0..N in MPIDR affinity 0, so the target MPIDR is the id.
        // TODO(portability): read the CPU list and their MPIDRs from `/cpus` in the device tree.
        let ret = arch::psci_cpu_on(id as u64, entry, stack_top(id));
        if ret == 0 {
            started += 1;
        } else {
            // A core that isn't present (QEMU started with fewer than MAX_CPUS) returns an error
            // rather than hanging. Degrade: note it and carry on with the cores we have.
            println!("  smp: cpu {id} did not start (PSCI {ret}); not present?");
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
    crate::drivers::gic::init_this_cpu();
    arch::timer::init();

    // 6. (tests only) Prove this core schedules from its OWN queue by spawning a probe onto it.
    //    There is no migration yet (step 3c), so it runs here; it records the core it ran on.
    #[cfg(test)]
    crate::sched::spawn(|| {
        RAN_ON[cpu::id()].store(true, Ordering::Release);
    })
    .expect("a secondary could not spawn its probe");

    // 7. Online (Release, so a core seeing the count also sees everything set up above), and from
    //    the next line on, preemptible.
    ONLINE.fetch_add(1, Ordering::Release);
    arch::interrupts::enable();

    // 8. This core's idle loop, which is now its idle thread's body. Yield first, so a freshly
    //    queued thread (the probe, or later any migrated work) runs at once rather than waiting a
    //    tick; then wfi parks the core until an interrupt makes something runnable.
    loop {
        crate::sched::yield_now();
        arch::wait_for_interrupt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let all_ran = || (1..MAX_CPUS).all(|c| RAN_ON[c].load(Ordering::Acquire));

        let mut spins = 0u64;
        while !all_ran() {
            crate::sched::yield_now();
            spins += 1;
            assert!(
                spins < 5_000_000,
                "secondary cores did not run scheduled work in time"
            );
        }

        for c in 1..MAX_CPUS {
            assert!(
                RAN_ON[c].load(Ordering::Acquire),
                "secondary core {c} never ran scheduled work",
            );
        }
    }

    /// **Work can be placed on any core from another core** (SMP step 3c). Core 0 uses `spawn_on`
    /// to put a probe on each online core; a remote target is reached through its inbox and a
    /// reschedule SGI. Each probe records the core it actually ran on, proving the migration path
    /// delivers a thread to the chosen core.
    #[test_case]
    fn work_can_be_placed_on_every_core() {
        let n = online_count();
        for target in 0..n {
            crate::sched::spawn_on(target, move || {
                SPREAD[cpu::id()].fetch_add(1, Ordering::Release);
            })
            .expect("spawn_on failed");
        }

        let done = || (0..n).all(|c| SPREAD[c].load(Ordering::Acquire) > 0);
        let mut spins = 0u64;
        while !done() {
            crate::sched::yield_now();
            spins += 1;
            assert!(spins < 5_000_000, "work placed on a core never ran there");
        }

        for c in 0..n {
            assert!(
                SPREAD[c].load(Ordering::Acquire) > 0,
                "core {c} never ran work placed on it",
            );
        }
    }
}
