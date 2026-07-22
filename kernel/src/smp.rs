//! Bringing the other cores online (SMP step 2, DECISIONS.md §11).
//!
//! Core 0 starts each secondary with a PSCI `CPU_ON`, handing it a physical entry point
//! (`secondary_boot` in boot.s) and its own stack. The secondary replays the MMU-enable, lands
//! in [`secondary_main`], sets up its per-CPU pointer, records itself online, and idles.
//!
//! **Step 2 deliberately stops at "alive and idle."** The secondaries do no scheduling: that
//! would need the per-CPU run queues of step 3, and a timer tick into today's single global
//! scheduler would be a bug. So a secondary comes up with interrupts masked and waits. What we
//! prove here is only that the bring-up path works: the cores execute our code, on their own
//! stacks, through the MMU, and check in.

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

#[repr(C, align(16))]
struct Stack([u8; SECONDARY_STACK_SIZE]);

/// One boot stack per core. Slot 0 is unused (core 0 has its own from link.ld); slots
/// `1..MAX_CPUS` belong to the secondaries. Static, so the stacks exist before any allocator and
/// live at kernel-image (high) virtual addresses the coarse boot map already covers.
static SECONDARY_STACKS: [Stack; MAX_CPUS] =
    [const { Stack([0; SECONDARY_STACK_SIZE]) }; MAX_CPUS];

/// How many secondaries have reached [`secondary_main`] and are idling.
static ONLINE: AtomicUsize = AtomicUsize::new(0);

/// The high-VA top of core `id`'s boot stack. Stacks grow down, so the top is base + size, and
/// the 16-byte alignment `sp` requires comes from `Stack`'s `align(16)` and the size being a
/// multiple of 16. See notes/stack.md.
fn stack_top(id: usize) -> u64 {
    let base = &SECONDARY_STACKS[id] as *const Stack as u64;
    base + SECONDARY_STACK_SIZE as u64
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
/// stack. `cpu_id` came from `MPIDR_EL1`.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(cpu_id: usize) -> ! {
    // FIRST, ahead of anything that could take a lock: point TPIDR_EL1 at this core's block, so
    // the lock path can read this core's held-rank. See cpu.rs.
    cpu::init_this_cpu(cpu_id);

    // Announce presence. Release so a core observing the count (with Acquire) also sees this
    // core's setup. This `fetch_add` is the whole of step 2's observable behaviour.
    ONLINE.fetch_add(1, Ordering::Release);

    // Idle with interrupts masked. We deliberately do NOT enable this core's timer: a tick would
    // call into the scheduler, which is still a single global (step 3 makes it per-CPU). So the
    // core waits here until step 3 gives it a run queue and sends it work.
    arch::halt();
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
}
