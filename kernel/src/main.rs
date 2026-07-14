//! cricker-os
//!
//! # Why the attributes at the top
//!
//! `no_std`  — there is no operating system beneath us, because we *are* the
//!             operating system. `std`'s `File::open` would make a syscall, and
//!             there is nobody to answer it. We link only `core`.
//!
//! `no_main` — in a normal program `main` is not the first thing to run. The C
//!             runtime (`crt0`) sets up the stack, initializes libc, builds `argv`,
//!             and *then* calls `main`. There is no libc here and nobody has set up
//!             a stack, so there can be no `main`. Our entry point is `_start`, in
//!             assembly, and it sets up the stack itself.
//!
//! See notes/no-std.md.

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

mod arch;
mod cap;
mod console;
mod drivers;
mod heap;
mod memory;
mod panic;
mod sched;
mod stack;
mod sync;
mod syscall;
mod thread;
mod user;

#[cfg(test)]
mod testing;

/// The physical address of the Device Tree Blob, as handed to us in `x0`.
///
/// Stashed here so the tests can assert that the boot protocol actually delivered one,
/// which is the whole point of shipping a flat arm64 Image instead of an ELF.
/// See notes/boot-protocol.md.
pub static DTB: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The kernel's Rust entry point, called from `_start` once we have a stack and a
/// zeroed `.bss`.
///
/// `extern "C"` matters: it tells Rust to follow the aarch64 calling convention
/// (AAPCS64), because assembly is about to call this and the two need to agree on
/// where arguments live. `dtb` arrives in `x0`. See notes/registers.md.
///
/// `-> !` means this never returns, which is true: there is nowhere to return *to*.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    DTB.store(dtb, core::sync::atomic::Ordering::Relaxed);

    // Console first, exceptions second, and the order is not arbitrary: the fault
    // handler's entire job is to print, so it is useless until the UART works. The
    // window between these two lines is the last place in the kernel where a fault
    // still kills us silently.
    console::init();
    arch::init();
    stack::init();

    // Now that faults are reportable, go find out how much RAM we actually have. A bug
    // in here is a fault, and a fault is now legible rather than fatal-and-silent.
    memory::init(dtb);

    // And now the sketchiest moment in the kernel. The instant SCTLR_EL1.M is set, the very
    // next instruction is fetched through the MMU. See arch/aarch64/mmu.rs.
    arch::mmu::init();

    // The heap must come AFTER the MMU: it hands out addresses, and with paging on an
    // address is only usable if something has mapped it. From here, `Vec` works.
    heap::init();

    // And now interrupts, which is where every lock in the kernel stops being a formality.
    //
    // The scheduler comes up FIRST, so that the very first timer tick already has somewhere to
    // send a reschedule. Adopting the boot context as thread 0 costs one allocation.
    sched::init();
    interrupts_init(dtb);

    #[cfg(test)]
    test_main();

    #[cfg(not(test))]
    {
        use aarch64_cpu::registers::CurrentEL;
        use tock_registers::interfaces::Readable;

        println!();
        println!("cricker-os");
        println!("  exception level : EL{}", CurrentEL.read(CurrentEL::EL));
        println!("  stack top       : {:#018x}", stack_top());
        println!("  device tree     : {dtb:#018x}");
        memory::print_summary();
        arch::mmu::print_summary();
        heap::print_summary();
        {
            use crate::arch::{interrupts, timer};
            println!(
                "  timer           : {} Hz tick, counter at {} MHz, interrupts {}",
                timer::TICK_HZ,
                timer::frequency() / 1_000_000,
                if interrupts::enabled() { "ON" } else { "off" },
            );
            println!(
                "  scheduler       : {} thread(s), round robin, preemptive",
                sched::thread_count(),
            );
        }

        println!();
        println!("milestone 1: we are running our own code on a CPU with nothing underneath it.");
        println!("milestone 2: and when it goes wrong, we get told.");
        println!("           : and the machine now tells us what it is, instead of us guessing.");
        println!("milestone 3: and we know which parts of it are ours to give away.");
        println!("milestone 4: and nothing writable is executable, and Vec works again.");
        println!("milestone 5: and the machine can now interrupt us. we are preemptible.");
        println!("milestone 6: and a thread that refuses to yield gets preempted anyway.");
        println!("milestone 7: and now it runs a binary it did not compile, unprivileged.");
        println!("           : and that binary can talk to a server it can only name, not reach.");
        println!();

        // The whole argument, executable.
        {
            use crate::arch::timer;
            use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

            static HOSTILE: AtomicU64 = AtomicU64::new(0);
            static POLITE: AtomicU64 = AtomicU64::new(0);
            static STOP: AtomicBool = AtomicBool::new(false);

            // A thread whose entire body is a tight loop. No yield. No syscall. Not even a
            // function call. Under ANY cooperative scheduler this owns the CPU forever and the
            // machine is gone. This is the arbitrary ELF binary, in miniature.
            sched::spawn(|| {
                while !STOP.load(Ordering::Relaxed) {
                    HOSTILE.fetch_add(1, Ordering::Relaxed);
                }
            });

            // A thread that also never yields, but would like a turn.
            sched::spawn(|| {
                while !STOP.load(Ordering::Relaxed) {
                    POLITE.fetch_add(1, Ordering::Relaxed);
                }
            });

            let p0 = sched::preemptions();
            timer::spin_for(timer::frequency() / 2); // half a second, doing nothing
            STOP.store(true, Ordering::Relaxed);

            println!("  half a second later, having spawned two threads that NEVER yield:");
            println!();
            println!(
                "    thread 1 (hostile) : {:>10} iterations",
                HOSTILE.load(Ordering::Relaxed)
            );
            println!(
                "    thread 2 (polite)  : {:>10} iterations",
                POLITE.load(Ordering::Relaxed)
            );
            println!("    preemptions        : {:>10}", sched::preemptions() - p0);
            println!();
            println!("  neither asked to be interrupted. both were.");
        }

        // 7a. EL0.
        {
            use crate::arch::exceptions::{SVC_COUNT, USER_FAULTS};
            use crate::arch::timer;
            use core::sync::atomic::Ordering;

            println!();
            println!("  and now the other side of the boundary:");
            println!();

            // The privilege boundary, still real: a program that reaches for a kernel address is
            // killed, and the kernel is not.
            let faults0 = USER_FAULTS.load(Ordering::Relaxed);
            sched::spawn(|| unsafe { user::exec(user::outlaw()) });
            timer::spin_for(timer::frequency() / 20);
            println!(
                "    outlaw : reached {:#018x}, was killed, kernel survived ({} fault)",
                0xffff_0000_4008_0000u64,
                USER_FAULTS.load(Ordering::Relaxed) - faults0,
            );

            // Milestone 8. The console driver is no longer in the kernel.
            match user::initrd() {
                None => println!("    initrd : none (no -initrd passed to QEMU)"),
                Some(image) => {
                    println!(
                        "    initrd : {} bytes at {:#x}, from the device tree",
                        image.len(),
                        memory::initrd_region().unwrap().0,
                    );
                    println!();
                    println!("  the console driver now runs at EL0. what follows is printed by it:");
                    println!();

                    // Start the console SERVER as a user process that owns the UART, then a
                    // CLIENT wired to it. The lines the client prints travel through a page it
                    // shares with the server; the kernel never touches the bytes.
                    let console = user::console_service::start(image);
                    user::console_service::spawn_client(image, console);
                    timer::spin_for(timer::frequency() / 10);

                    println!();
                    println!("  ...and control is back in the kernel, which never saw those bytes.");
                }
            }

            println!();
            println!("  a userspace program printed to the screen, and the kernel does not");
            println!("  contain a line of code that puts a user's bytes on the wire.");
            let _ = SVC_COUNT.load(Ordering::Relaxed);
        }
    }

    arch::halt()
}

/// Bring up the interrupt controller and the timer, then **unmask interrupts**.
///
/// This is the line the whole locking discipline was written for. From here, a timer interrupt
/// can land between any two instructions in the kernel, and every `IrqSafeMutex` starts
/// actually masking something. See DECISIONS.md §9 and notes/locking.md.
fn interrupts_init(_dtb: usize) {
    use crate::arch::{interrupts, mmu, timer};
    use crate::drivers::gic;

    let ((gicd, _), (gicc, _)) = memory::gic_regions().expect("no interrupt controller in the DTB");

    // The GIC lives in the direct map, like every other physical address the kernel names.
    // SAFETY: the addresses came from the device tree, and `mmu::init` mapped both as DEVICE
    // memory. Mapping them as normal memory would let the CPU cache and reorder writes to an
    // interrupt controller, which is exactly as bad as it sounds.
    unsafe { gic::init(mmu::phys_to_virt(gicd), mmu::phys_to_virt(gicc)) };

    timer::init();

    // The point of no return, in a much friendlier sense than the MMU's. After this, we are
    // preemptible.
    interrupts::enable();
}

/// Read `__stack_top` back out of the linker script, just to prove we can.
///
/// The linker invents this symbol and writes its address into the ELF; we declare
/// it here so Rust can see it. Note that we want the *address of* the symbol, not
/// its contents. There is no value there. See notes/linker-scripts.md.
#[cfg(not(test))]
fn stack_top() -> usize {
    unsafe extern "C" {
        static __stack_top: core::ffi::c_void;
    }
    (&raw const __stack_top) as usize
}

#[cfg(test)]
mod tests {
    //! Tests for the boot path itself: the things `boot.s` and the boot protocol had to get
    //! right before any other code could run at all.
    //!
    //! Everything else lives beside the code it tests. `cargo test -p kernel` still collects
    //! all of them: `custom_test_frameworks` gathers every `#[test_case]` in the crate,
    //! wherever it is.

    /// Proves the harness itself works. If this fails, nothing else is meaningful.
    #[test_case]
    fn harness_runs() {
        assert_eq!(1 + 1, 2);
    }

    /// Proves `boot.s` zeroed `.bss`.
    ///
    /// A zero-initialized static lands in `.bss`, which occupies no bytes in the
    /// ELF file. Nobody loaded it. If our zeroing loop were wrong, this would hold
    /// whatever garbage was in RAM at power-on. See notes/elf.md.
    #[test_case]
    fn bss_was_zeroed() {
        use core::sync::atomic::{AtomicU64, Ordering};
        static CANARY: AtomicU64 = AtomicU64::new(0);
        assert_eq!(CANARY.load(Ordering::Relaxed), 0);
    }

    /// Proves `boot.s` gave us a usable, correctly aligned stack.
    ///
    /// aarch64 faults on a misaligned `sp` when it's used, so a bug here would show
    /// up as a mysterious early crash rather than as anything legible.
    /// See notes/stack.md.
    #[test_case]
    fn stack_pointer_is_16_byte_aligned() {
        let sp: usize;
        // SAFETY: reads a register. No side effects.
        unsafe { core::arch::asm!("mov {}, sp", out(reg) sp) };
        assert_eq!(sp % 16, 0, "sp = {sp:#x}");
    }

    /// Proves we are where we think we are.
    ///
    /// QEMU's `virt` machine drops us at EL1 by default, which is exactly where a
    /// kernel belongs. If this ever reads EL2, we've been handed the hypervisor
    /// level and will need to drop down ourselves. See notes/aarch64.md.
    #[test_case]
    fn running_at_el1() {
        use aarch64_cpu::registers::CurrentEL;
        use tock_registers::interfaces::Readable;
        assert_eq!(CurrentEL.read(CurrentEL::EL), 1);
    }

    /// Proves the boot protocol actually delivered a device tree.
    ///
    /// This is the test that closes the correction from milestone 1. Back then we
    /// shipped an ELF, QEMU took its bare-metal path, and `x0` arrived as zero. Now
    /// we ship a flat binary carrying an arm64 Image header, QEMU recognizes it as a
    /// kernel, follows the Linux boot protocol, and hands us a real pointer.
    ///
    /// A zero here means we have silently regressed to the ELF path, which would be
    /// easy to do by editing the runner script and hard to notice any other way.
    #[test_case]
    fn device_tree_pointer_was_provided() {
        use core::sync::atomic::Ordering;
        assert_ne!(
            crate::DTB.load(Ordering::Relaxed),
            0,
            "no DTB pointer in x0: did we fall back to booting as an ELF?"
        );
    }

    /// Proves the pointer points at an actual device tree, not just at something.
    ///
    /// A nonzero pointer is necessary but not sufficient. Every flattened device tree
    /// begins with the magic `0xd00dfeed`, stored **big-endian** (the format predates
    /// the little-endian consensus and never changed), so we have to byte-swap on the
    /// way in. If this passes, the machine is genuinely describing itself to us.
    #[test_case]
    fn device_tree_has_the_right_magic() {
        use core::sync::atomic::Ordering;

        // DTB holds the PHYSICAL address QEMU gave us in x0. Since the kernel moved to the
        // high half, TTBR0 is disabled and a low address does not exist: dereferencing it
        // directly faults, which is exactly what we want and exactly what this line used to
        // do. Name it through the direct map instead.
        let pa = crate::DTB.load(Ordering::Relaxed) as u64;
        let ptr = crate::arch::mmu::phys_to_virt(pa) as *const u32;

        // SAFETY: QEMU put a device tree at that physical address, and the direct map makes
        // it readable.
        let magic = unsafe { core::ptr::read_volatile(ptr) };

        assert_eq!(
            u32::from_be(magic),
            0xd00d_feed,
            "no device tree magic at {ptr:p}"
        );
    }
}
