//! nife
//!
//! # Why the attributes at the top
//!
//! `no_std` : there is no operating system beneath us, because we *are* the
//!             operating system. `std`'s `File::open` would make a syscall, and
//!             there is nobody to answer it. We link only `core`.
//!
//! `no_main`: in a normal program `main` is not the first thing to run. The C
//!             runtime (`crt0`) sets up the stack, initializes libc, builds `argv`,
//!             and *then* calls `main`. There is no libc here and nobody has set up
//!             a stack, so there can be no `main`. Our entry point is `_start`, in
//!             assembly, and it sets up the stack itself.
//!
//! See notes/no-std.md.

#![no_std]
#![no_main]
// Not the crates/ library surface milestone 68's ratchet tracks (DECISIONS §107): the kernel binary
// is one crate root behind an ABI boundary, not a documented API.
#![allow(missing_docs)]
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]
// The RISC-V boot runs a self-contained tour (in `kernel_main` below) that ends in `arch::halt()`,
// before the shared, still-aarch64-shaped full boot (userspace init as the boot process, the shell,
// the virtio service). That full-boot code and its helpers are therefore unreferenced from a riscv64
// build and look like dead code, even though the `arch` layer itself is fully implemented. Allow it
// *only* on riscv64, so the aarch64 build keeps full dead-code checking; it goes away if the RISC-V
// boot is ever wired into the shared path instead of halting. See notes/riscv-port.md.

mod arch;
#[cfg(feature = "bench")]
mod bench;
mod cap;
mod console;
mod cpu;
mod drivers;
#[cfg(feature = "icount")]
mod icount;
mod interrupt_stack;
mod iommu;
mod kmem;
mod memory;
mod panic;
// PCIe enumeration + virtio-pci bring-up (the PCIe transport, DECISIONS §18). Portable: the
// decode logic is crates/pci, and each arch supplies its window/irq constants. See
// kernel/src/pci.rs.
mod pci;
// The NVMe block driver (milestone 53's storage half): the volatile half of crates/nvme, brought
// up over the PCIe transport above and confined behind the machine's IOMMU. See kernel/src/nvme.rs.
// Only the test boot drives it today (nothing production-wired rides NVMe until the block-server
// question in notes/nvme.md's BUGS is decided), so the non-test build allows it dead rather than
// cfg-gating a module whose next caller is already known.
#[cfg_attr(not(test), allow(dead_code))]
mod nvme;
mod revoke;
mod sched;
mod smp;
mod stack;
mod sync;
mod syscall;
mod thread;
// The measured-boot trust root: the digest of the boot program this kernel image was built
// against (milestone 22 phase B.1, DECISIONS §22). Generated into the image by build.rs.
mod trust;
mod untyped;
mod user;
mod virtio;

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
// On riscv64, the boot tour below ends in `arch::halt()`, so the rest of `kernel_main` (the shared
// full boot) is deliberately unreachable there. Scoped to riscv so aarch64 keeps the lint.
// The smb_serve boot parks before the tour the same way the riscv tour does: the code after
// either stays compiled and is deliberately unreachable.
// And the icount boot parks before the bench boot it implies, so `bench::run()` and everything after
// it is deliberately unreachable in that one configuration (milestone 78).
#[cfg_attr(
    any(target_arch = "riscv64", feature = "smb_serve", feature = "icount"),
    allow(unreachable_code)
)]
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb: usize) -> ! {
    DTB.store(dtb, core::sync::atomic::Ordering::Relaxed);

    // Per-CPU pointer FIRST, before anything takes a lock. The lock path reads this core's
    // held-rank out of its per-CPU block, so `TPIDR_EL1` must point at that block before
    // `console::init` (the first lock) runs. On one core this is pure setup with no visible
    // effect; it is the foundation SMP is built on. See cpu.rs and DECISIONS.md §11.
    cpu::init_this_cpu(arch::boot_cpu_id());

    // Console first, exceptions second, and the order is not arbitrary: the fault
    // handler's entire job is to print, so it is useless until the UART works. The
    // window between these two lines is the last place in the kernel where a fault
    // still kills us silently.
    console::init();

    // The console's register-block *shape* comes from the device tree, and it must be adopted
    // before the first println: on the JH7110 the DW-8250's registers are four bytes apart, so a
    // byte-strided LSR poll spins on garbage forever and the banner never appears
    // (notes/visionfive2.md; console::configure_from_dtb). On QEMU the tree restates the defaults
    // and this is a no-op in effect.
    #[cfg(target_arch = "riscv64")]
    console::configure_from_dtb(dtb);

    // The RISC-V boot is a self-contained tour, right here in this block, and it halts at the end
    // rather than falling through to the shared boot path below. From OpenSBI's S-mode handoff it
    // brings up its own arch (traps, the Sv39 MMU, the SBI timer) and then demonstrates the whole
    // capability core on RISC-V: the scheduler and preemption, U-mode programs and syscalls,
    // capability invocation, userspace init building a child out of the initrd, and a userspace
    // driver servicing a device interrupt through the PLIC. It stops before the shared full boot
    // (userspace init as the boot process, the shell, the virtio service), which is still
    // aarch64-shaped; making those portable is what would let RISC-V join the shared path instead of
    // halting here. See notes/riscv-port.md.
    #[cfg(target_arch = "riscv64")]
    {
        // A live code address proves we jumped to the high-half alias in boot.s and are executing
        // there, not merely reaching the UART through the identity map (both are mapped by the boot
        // table). If this reads 0xffffffc0_8020_xxxx, Sv39 is on and the kernel is in the high half.
        let pc = kernel_main as *const () as usize;
        println!();
        println!("nife on RISC-V (rv64, S-mode, Sv39)");
        println!("  hart 0 booted: high-half kernel, .bss, and the NS16550 console are up.");
        println!("  running at  : {pc:#018x}  (high half: Sv39 paging is on)");
        println!("  device tree : {dtb:#018x}");

        // Traps: install stvec and prove the round-trip by taking a breakpoint and returning.
        arch::exceptions::init();
        let caught = arch::exceptions::self_test();
        println!("  traps       : stvec set; a breakpoint was caught and stepped over ({caught})");

        // Timer + interrupts: arm the SBI timer, unmask interrupts, and let the S-mode timer
        // interrupt fire for ~0.2 s. A nonzero tick count proves the whole interrupt path (SBI
        // set_timer, sie.STIE, sstatus.SIE, the trap vector routing scause=timer to timer::tick).
        //
        // The counter's rate comes out of the device tree first (milestone 100). It used to be a
        // 10 MHz constant, which is QEMU's; RISC-V has no CNTFRQ_EL0 to read, so the tree is the
        // architected source and this is the earliest point anything needs the number.
        arch::timer::init_frequency(dtb);
        arch::timer::init();
        arch::interrupts::enable();
        let start = arch::timer::now();
        while arch::timer::now().wrapping_sub(start) < arch::timer::frequency() / 5 {
            arch::wait_for_interrupt();
        }
        println!(
            "  timer       : {} ticks in ~0.2s (SBI timer + S-mode interrupt at {} Hz)",
            arch::timer::ticks(),
            arch::timer::TICK_HZ,
        );

        // Memory: parse the device tree OpenSBI handed us (via its high-half alias) for RAM, and
        // bring up the frame allocator. Portable code, reached through the real phys_to_virt now.
        stack::init();
        // Paint the boot stack's unused region for the high-water report (milestone 84), before
        // memory::init and everything after it can push frames into it.
        #[cfg(test)]
        stack::paint_boot_stack();
        memory::init(dtb);
        let f = memory::alloc().expect("no frame from the allocator");
        println!(
            "  memory      : device tree parsed, frame allocator up (first frame {:#x})",
            f.addr(),
        );

        // What machine is this (milestone 60). Before the MMU, because this is where a machine that
        // cannot run us says so and stops, and building fine-grained tables the hardware will not
        // walk is a worse place to find out. It reads the same device tree `memory::init` just
        // parsed, then asks OpenSBI what it implements. The summary prints after paging, because
        // one of the numbers in it is measured by `mmu::init`.
        arch::isa::init(dtb);

        // Which harts does this machine have (milestone 100)? Here, while the device tree is still
        // reachable through the boot map, rather than at bring-up time after `mmu::init` has
        // replaced it. The list used to be `0..cpu::MAX_CPUS`, a constant.
        smp::read_cpu_list(dtb);

        // And how does the PLIC number their S-mode contexts? Read here, in the same window and
        // before anything touches a context: the `2*hart + 1` formula this replaces is QEMU's
        // layout, and the JH7110's disabled S7 shifts every context down one (arch::irq,
        // notes/visionfive2.md). On QEMU the tree reproduces the formula exactly.
        arch::irq::init_contexts(dtb);

        // Replace the coarse RWX boot table with fine-grained W^X Sv39 kernel tables. We keep
        // running (and printing) across the satp switch, which proves the fine map covers this code.
        arch::mmu::init();
        println!(
            "  paging      : fine-grained W^X Sv39 tables installed, satp switched (paging on: {})",
            arch::mmu::is_enabled(),
        );

        // The per-CPU interrupt stacks go live here, for the reason the aarch64 boot arms them
        // right after its own `mmu::init`: their guard pages are holes in the map that was just
        // installed. This hart has had interrupts on since the timer step above, so every trap
        // before this one ran on the stack it interrupted, exactly as it used to.
        interrupt_stack::init();

        // And say what the machine is. Here rather than in the tour below, so the test, shell and
        // bench boots report it too: it is the line whoever brings up a board reads first.
        arch::isa::print_summary();

        // The scheduler comes up before the tour (and, in a test build, before the tests): both
        // need threads to switch between, and preemption needs somewhere to go. aarch64 brings it
        // up in its main boot flow for the same reason.
        //
        // Interrupts have been on since the timer step above, so mask them across `sched::init`: a
        // timer tick landing mid-init would run the deferred `schedule()` before the idle thread is
        // registered, and hit "nothing runnable and no idle thread" (sched.rs). aarch64 gets this
        // ordering for free by bringing the scheduler up before it ever enables interrupts.
        let irqs = arch::interrupts::disable();
        sched::init();
        arch::interrupts::restore(irqs);

        // SMP: bring the other harts online (parity workstream A). First arm this (boot) hart's own
        // software-interrupt source, so a secondary can hand work back to it; then start the
        // secondaries. Each starts via SBI HSM at secondary_boot, adopts the fine kernel map, sets
        // its own trap vector and per-CPU state (its own per-hart trap stash, A1), arms its timer and
        // its IPI source, and becomes a scheduler participant. Before the tests (and the tour), so the
        // SMP tests find the cores online, mirroring aarch64's `bring_up_secondaries` then `test_main`.
        arch::irq::init_this_cpu();
        smp::bring_up_secondaries();

        // The RISC-V IOMMU (milestone 16b), if QEMU presents one (`-device riscv-iommu-pci`). It is
        // a PCI function, so the kernel enumerates it, places its BAR, and brings it up here, before
        // any path (test, shell, tour) confines a virtio-pci device. Absent, this is a no-op and the
        // kernel runs exactly as before. See kernel/src/iommu.rs, notes/iommu.md.
        pci::init_iommu();

        // The instruction-count boot (milestone 78, `script/icount`) diverges here, on the ISA whose
        // claim it was written for: SBI's `set_timer` is write-only, so this is the only place in
        // the tree that proves the firmware was armed with the deadline the kernel recorded. Before
        // the bench boot, whose feature it implies; see the aarch64 site for why.
        #[cfg(feature = "icount")]
        icount::run();

        // A bench build runs the primitive suite here and parks, instead of the tour. It needs the
        // `os_primitives_benchmarker` and `coremark` programs in the initrd (cargo xtask initrd-riscv packs them). The
        // RISC-V equivalent of the aarch64 boot's `#[cfg(feature = "bench")] bench::run()`.
        #[cfg(feature = "bench")]
        bench::run();

        // A `shell` build hands the machine to userspace init here and parks, instead of the tour.
        // The kernel loads `system_initializer` from the initrd, grants it the NS16550 and the UART interrupt,
        // and it builds the console server, input driver, and shell out of its own budget; the shell
        // is interactive over the serial. This is the RISC-V equivalent of the aarch64 `initboot`
        // path. Needs the shell programs in the initrd (cargo xtask initrd-riscv packs them).
        #[cfg(feature = "shell")]
        {
            if let Some((plic_phys, _)) = memory::plic_region() {
                // SAFETY: the PLIC is device-mapped in the direct map; the context is the boot
                // hart's S context (derived, not hardcoded: OpenSBI's hart lottery).
                unsafe {
                    drivers::plic::init(
                        arch::mmu::phys_to_virt(plic_phys) as usize,
                        arch::irq::boot_s_context(),
                    );
                };
            }
            match user::initrd() {
                Some(initrd) => {
                    // The UART's PLIC source, from the machine's own tree: 10 on QEMU virt, 32 on
                    // the JH7110. It was a constant, and the constant was QEMU's; see the tour's
                    // driver step below and notes/visionfive2.md (BUGS). The line names which
                    // source won, so a bench transcript is diagnosable.
                    let (uart_irq, uart_irq_source) = user::uart_irq_and_source();
                    println!("  uart irq: source {uart_irq} ({uart_irq_source})");
                    if let Err(e) = user::riscv_shell_boot(initrd, uart_irq) {
                        println!("  shell boot failed: {e:?}");
                    } else {
                        println!(
                            "  shell: userspace init is building the console, input, and shell."
                        );
                        println!();
                    }
                }
                None => println!("  shell: no initrd (run `cargo xtask initrd-riscv`)"),
            }
            // The boot thread parks; system_initializer and its children (console/input/shell) run on the
            // scheduler. `halt` is a preemptible wfi loop, so they get scheduled from here on.
            arch::halt();
        }

        // A test build runs the kernel suite right here and exits via semihosting, instead of the
        // demonstration tour below. Everything the tests need is now up: memory and the frame
        // allocator, the Sv39 paging, the scheduler and its idle thread, the timer, interrupts, and
        // the other harts. The RISC-V equivalent of the `#[cfg(test)] test_main()` on the aarch64 boot.
        #[cfg(test)]
        {
            // The PLIC too: the parity-C disk tests route a device interrupt, and `plic::enable`
            // through a never-initialized PLIC is a store through a zero base (a fault this test
            // path found the hard way; the shell and tour paths already did this). SEIE as well:
            // enabling a source at the PLIC delivers nothing while supervisor external interrupts
            // are masked in `sie`, and that bit is otherwise only set by the UART driver paths.
            if let Some((plic_phys, _)) = memory::plic_region() {
                // SAFETY: the PLIC is device-mapped in the direct map; the context is the boot
                // hart's S context (derived, not hardcoded: OpenSBI's hart lottery).
                unsafe {
                    drivers::plic::init(
                        arch::mmu::phys_to_virt(plic_phys) as usize,
                        arch::irq::boot_s_context(),
                    );
                };
                arch::exceptions::enable_external();
            }
            test_main();
            arch::halt();
        }

        // Dynamic kernel mapping self-test: map a fresh frame at an unused high-half VA, read it
        // back through the tables, write and read through the mapping, then unmap it. Proves
        // map_page / translate / unmap_page / flush_tlb, which the kernel stack allocator needs.
        {
            use paging::Flags;
            // Above the direct map of whatever RAM this machine actually has, computed rather than
            // assumed: a constant here was the third QEMU-shaped address the VisionFive 2 caught in
            // one bench session (2026-08-14). The direct map ends at KERNEL_VA_BASE + top-of-RAM;
            // one gigapage of headroom clears any alignment the mapper rounds to.
            let ram_top = memory::ram_regions()
                .map(|(start, size)| start + size)
                .max()
                .expect("the device tree described no RAM");
            let test_va = arch::mmu::KERNEL_VA_BASE + ram_top + (1 << 30);
            let frame = memory::alloc()
                .expect("no frame for the mmu self-test")
                .addr();
            arch::mmu::map_page(test_va, frame, Flags::kernel_data()).expect("map_page failed");
            let (pa, flags) = arch::mmu::translate(test_va).expect("translate found nothing");
            // SAFETY: we just mapped this VA read/write.
            unsafe { (test_va as *mut u64).write_volatile(0xc0ffee) };
            // SAFETY: the same VA the line above just wrote through, still mapped read/write; this reads back what it stored.
            let readback = unsafe { (test_va as *const u64).read_volatile() };
            let freed = arch::mmu::unmap_page(test_va).expect("unmap_page failed");
            println!(
                "  kmap test   : mapped {test_va:#x}->{pa:#x} (w:{}), rw={:#x}, unmapped->{freed:#x}",
                flags.is_writable(),
                readback,
            );
        }

        // The scheduler and the context switch: adopt the boot thread, spawn two kernel threads,
        // and wait for both. Each spawned thread runs its closure (via switch_to ->
        // thread_trampoline -> thread_entry -> the closure) and exits, cascading back to us. Both
        // having run proves the RISC-V context switch (context.s: switch_to and the trampolines)
        // works end to end.
        {
            use core::sync::atomic::{AtomicU32, Ordering};
            static RAN: AtomicU32 = AtomicU32::new(0);
            // sched::init() ran above (it is needed by both the tour and the test build).
            sched::spawn(|| {
                RAN.fetch_add(1, Ordering::SeqCst);
            });
            sched::spawn(|| {
                RAN.fetch_add(1, Ordering::SeqCst);
            });
            // Clock-bounded, not yield-bounded, the same shape as the sched test module's
            // `wait_for` and for its reason: the secondaries are online, so placement (§28) can
            // put both threads on other harts, and a fixed count of yields on this hart elapses
            // in microseconds, long before another hart has scheduled them. This used to be four
            // yields, which was always enough on QEMU and missed on the VisionFive 2 (boot 12
            // printed "1 of 2", boot 13 "0 of 2", while the preemption step below ran millions
            // of iterations; see notes/visionfive2.md). Two seconds is far beyond any honest
            // completion, and the failure line says what did not happen instead of claiming the
            // switch works regardless.
            let deadline = arch::timer::now() + 2 * arch::timer::frequency();
            while RAN.load(Ordering::SeqCst) < 2 && arch::timer::now() < deadline {
                sched::yield_now();
            }
            match RAN.load(Ordering::SeqCst) {
                2 => println!(
                    "  scheduler   : 2 of 2 kernel threads ran (RISC-V context switch works)"
                ),
                n => println!(
                    "  scheduler   : FAILED: {n} of 2 kernel threads ran within 2s (context switch not proven)"
                ),
            }
        }

        // User address spaces (the single-satp model): build a process address space, switch satp
        // to it, and keep running. That only survives if share_kernel_half copied the kernel high
        // half into the process root (otherwise this kernel code, at a high VA, unmaps itself on the
        // csrw satp). Then map and translate a user page in the live process space.
        {
            use paging::Flags;
            let aspace = user::AddressSpace::new(1).expect("no process address space");
            let user_va = 0x40_0000u64;
            // SAFETY: aspace.ttbr0() is a well-formed Sv39 satp whose root carries the kernel half.
            unsafe { arch::mmu::activate_user(aspace.ttbr0()) };
            let frame = memory::alloc().expect("no user frame").addr();
            arch::mmu::map_current_user_frame(user_va, frame, Flags::user_data(), || {
                memory::alloc().map(|f| f.addr())
            })
            .expect("user map failed");
            let mapped = arch::mmu::translate_user(user_va);
            arch::mmu::deactivate_user(); // back to the kernel-only root before dropping the aspace
            println!(
                "  user aspace : process satp activated (kernel half shared), user {user_va:#x} -> {mapped:x?}",
            );
        }

        // The capstone: run a real program at U-mode. The loader builds an address space from the
        // `outlaw` ELF's segments and drops to U-mode via enter_user's sret. The program yields
        // (round-tripping U-mode -> trap -> dispatch -> yield -> sret -> U-mode) twice, then exits.
        // The syscall count proves the whole userspace path: enter_user, the sscratch U-mode trap
        // entry, the ecall dispatch through the ABI accessors, and the return to U-mode.
        //
        // This step used to run a hand-assembled RISC-V blob through a one-page raw loader. It runs
        // a compiled ELF out of the initrd now (milestone 19's user-test port), so it needs one, and
        // says so rather than quietly proving nothing when there is none.
        {
            use core::sync::atomic::Ordering;
            match user::program("outlaw") {
                Some(image) => {
                    let before = arch::exceptions::SVC_COUNT.load(Ordering::Relaxed);
                    sched::spawn(move || {
                        user::run(
                            image,
                            user::Spawn {
                                arg0: user::OUTLAW_ROUND_TRIP,
                                arg1: 0,
                                arg2: 0,
                                grants: &[],
                                maps: &[],
                            },
                        )
                    });
                    for _ in 0..4 {
                        sched::yield_now();
                    }
                    let served = arch::exceptions::SVC_COUNT.load(Ordering::Relaxed) - before;
                    println!(
                        "  userspace   : a program ran at U-mode and made {served} syscalls (yield/yield/exit via ecall)",
                    );
                }
                None => println!("  userspace   : skipped (no 'outlaw' program in the initrd)"),
            }
        }

        // The tour-stage breadcrumb (first-silicon diagnostics, 2026-08-15): every dump_threads
        // header prints the last stage reached, so a bench log whose serial lines went missing
        // (boots 7 through 9, notes/visionfive2.md fifth stop) still says how far the boot
        // thread got, re-stated every dump. The table, so a stage number reads without the code:
        //
        //   3 = the outlaw step finished        7 = the UART-driver step finished
        //   4 = the initrd demo was entered     8 = the virtio probe finished
        //   5 = the initrd demo returned        9 = the PCIe probe finished
        //   6 = the preemption step finished   10 = the final banner printed; halting
        sched::note_boot_stage(3);

        // Running a real compiled ELF at U-mode, two ways, depending on the initrd.
        if let Some(initrd) = user::initrd() {
            // A nifefs archive with an "init" entry means the richer path: the kernel loads only
            // "init" (the portable `builder`), maps the whole archive into it, grants it a budget and
            // a report endpoint, and init loads "worker" from the archive and builds it as a child.
            // Anything else is treated as a single bare ELF and run directly (the simpler path).
            let is_archive = nifefs::Fs::parse(initrd)
                .map(|fs| fs.read("init").is_some())
                .unwrap_or(false);
            if is_archive {
                sched::note_boot_stage(4);
                match user::riscv_initrd_demo(initrd) {
                    Ok(sq) => println!(
                        "  init/build  : userspace init loaded 'worker' from a {}-byte archive and built it as a child; the child sent {sq} (expected 81)",
                        initrd.len(),
                    ),
                    Err(e) => println!("  init/build  : init failed: {e:?}"),
                }
            } else {
                const N: u64 = 7;
                match user::riscv_worker_demo(initrd, N) {
                    Ok(sq) => println!(
                        "  user ELF    : loaded a {}-byte riscv ELF, ran worker({N}) at U-mode, it sent {sq} (expected {})",
                        initrd.len(),
                        N * N,
                    ),
                    Err(e) => println!("  user ELF    : load failed: {e:?}"),
                }
            }
        } else {
            println!("  user ELF    : skipped (no -initrd passed to QEMU)");
        }
        sched::note_boot_stage(5);

        // Preemption: the property that separates a kernel from a runtime. Spawn two threads whose
        // entire body is a tight loop, no yield, no syscall, not even a function call. Under any
        // cooperative scheduler the first one owns the CPU forever. The S-mode timer fires every
        // 10ms, riscv_trap_dispatch records the tick and defers a schedule() to the trap tail, and
        // both threads make progress anyway. This is the RISC-V half of DECISIONS §5: an arbitrary
        // binary that refuses to yield is preempted regardless. `STOP` lets them exit cleanly so
        // nothing lingers past the demo. Interrupts have been unmasked since the timer step above.
        {
            use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
            static A: AtomicU64 = AtomicU64::new(0);
            static B: AtomicU64 = AtomicU64::new(0);
            static STOP: AtomicBool = AtomicBool::new(false);

            sched::spawn(|| {
                while !STOP.load(Ordering::Relaxed) {
                    A.fetch_add(1, Ordering::Relaxed);
                }
            });
            sched::spawn(|| {
                while !STOP.load(Ordering::Relaxed) {
                    B.fetch_add(1, Ordering::Relaxed);
                }
            });

            let p0 = sched::preemptions();
            arch::timer::spin_for(arch::timer::frequency() / 5); // ~0.2s, doing nothing but be preemptible
            STOP.store(true, Ordering::Relaxed);
            // Let the two spinners observe STOP and exit, so they are gone before we halt.
            for _ in 0..8 {
                sched::yield_now();
            }
            // The claim only prints when the numbers back it: both spinners made progress and at
            // least one preemption happened. The scheduler smoke line above earned the same
            // honesty the hard way (an unconditional success claim over a raced count).
            let (a, b, p) = (
                A.load(Ordering::Relaxed),
                B.load(Ordering::Relaxed),
                sched::preemptions() - p0,
            );
            if a > 0 && b > 0 && p > 0 {
                println!(
                    "  preemption  : two never-yield threads ran {a} and {b} iterations, {p} preemptions (a thread that refuses to yield is preempted anyway)",
                );
            } else {
                println!(
                    "  preemption  : FAILED: {a} and {b} iterations, {p} preemptions (a never-yield thread did not run, or nothing was preempted)",
                );
            }
        }
        sched::note_boot_stage(6);

        // Device interrupts, serviced by an unprivileged userspace driver: the last piece of the
        // interrupt story, in its real form. The kernel loads `driver` from the initrd, maps the
        // NS16550 into it device-typed, and grants it an Irq capability and a report endpoint. A
        // keystroke raises a line into the PLIC; the PLIC delivers a supervisor external interrupt;
        // riscv_trap_dispatch claims it, masks the source, and notifies the endpoint the Irq cap
        // waits on; the driver (a U-mode process owning no privilege) wakes, reads the byte through
        // its own device mapping, SENDs it, and ACKs, which re-arms the source through arch::irq. The
        // kernel is never in the data path. The `cargo run` harness is non-interactive, so pipe a
        // byte to QEMU's serial *after boot*: `( sleep 4; printf A ) | ...` (the console clears the
        // RX FIFO during init, so a byte sent at t=0 is dropped).
        if let Some((plic_phys, _)) = memory::plic_region() {
            // The UART's PLIC source, from the machine's own tree: 10 on QEMU virt, 32 on the
            // JH7110. This step used to arm a QEMU constant, and on the board that enabled an
            // unrelated source, so a real keystroke could never reach the driver; boot 13 proved
            // it on silicon (notes/visionfive2.md, BUGS). The fallback when the tree does not say
            // is that same constant (user::UART_RX_INTID), and the line below names which source
            // won, so a bench transcript is diagnosable.
            let (uart_irq, uart_irq_source) = user::uart_irq_and_source();
            // SAFETY: the PLIC is device-mapped in the direct map (mmu::map_everything); this is
            // its VA. The context is the boot hart's S context (2*hart + 1), derived rather than
            // hardcoded to 1 because OpenSBI elects the boot hart by lottery; see irq::boot_s_context.
            unsafe {
                drivers::plic::init(
                    arch::mmu::phys_to_virt(plic_phys) as usize,
                    arch::irq::boot_s_context(),
                );
            };

            println!("  uart irq    : source {uart_irq} ({uart_irq_source})");

            let started = user::initrd()
                .filter(|a| {
                    nifefs::Fs::parse(a)
                        .map(|fs| fs.read("driver").is_some())
                        .unwrap_or(false)
                })
                .and_then(|a| user::riscv_uart_driver_demo(a, uart_irq).ok());
            match started {
                Some(report) => {
                    // A receiver for the driver's reports, so the boot tour does not block on input.
                    sched::spawn(move || {
                        let byte = sched::ipc_recv(report)[0] as u8;
                        println!(
                            "  device IRQ  : an unprivileged userspace driver serviced the UART via its Irq cap and sent {byte:#04x} ({:?})",
                            byte as char,
                        );
                    });
                    println!(
                        "  device IRQ  : userspace driver started (holds an Irq cap + a UART mapping); pipe a byte to see it"
                    );
                    // Give a byte already piped a turn to flow through the driver before the banner.
                    for _ in 0..8 {
                        sched::yield_now();
                    }
                }
                None => println!(
                    "  device IRQ  : skipped (no 'driver' in the initrd; run `cargo xtask initrd-riscv`)"
                ),
            }
        }
        sched::note_boot_stage(7);

        // virtio block device discovery (parity C). Probe the virtio-mmio slots the `virt` machine
        // lays out (0x1000_1000..); a block device shows up when a disk is attached (NIFE_DISK).
        // The kernel owns the transport and will hand a userspace driver the device's registers, an
        // Irq capability, and a DMA region; here we just prove the discovery works on RISC-V.
        match virtio::find_block_device() {
            Some(dev) => println!(
                "  virtio      : block device found at {:#x}, PLIC IRQ {} (kernel owns the transport)",
                dev.mmio_phys, dev.intid,
            ),
            None => {
                println!("  virtio      : no block device attached (pass NIFE_DISK to attach one)");
            }
        }
        sched::note_boot_stage(8);

        // PCIe enumeration + virtio-pci bring-up (the PCIe transport, P1/P2). The disk QEMU
        // attaches as `virtio-blk-pci` arrives over the transport real hardware uses: found by
        // walking ECAM config space, its BARs placed by the kernel (OpenSBI does no PCI setup),
        // its register blocks resolved through the virtio vendor capabilities, its INTx line
        // swizzled to a PLIC input.
        match pci::find_block_device() {
            Some(d) => println!(
                "  pcie        : virtio-blk at {:02x}:{:02x}.{}, common {:#x}, notify {:#x} (mult {}), isr {:#x}, PLIC IRQ {}",
                d.bdf.bus,
                d.bdf.dev,
                d.bdf.func,
                d.common,
                d.notify_base,
                d.notify_mult,
                d.isr,
                d.intid,
            ),
            None => {
                println!("  pcie        : no virtio-blk on the bus (pass NIFE_DISK to attach one)");
            }
        }
        sched::note_boot_stage(9);

        println!("nife: the capability core runs on RISC-V.");
        sched::note_boot_stage(10);
        arch::halt();
    }

    arch::init();
    stack::init();
    // Paint the boot stack's unused region for the high-water report (milestone 84), before
    // memory::init and everything after it can push frames into it.
    #[cfg(test)]
    stack::paint_boot_stack();

    // Now that faults are reportable, go find out how much RAM we actually have. A bug
    // in here is a fault, and a fault is now legible rather than fatal-and-silent.
    memory::init(dtb);

    // What machine is this (milestone 60). Three `mrs` reads, and the reason they happen HERE is
    // the line below: `mmu::init` takes `TCR_EL1.IPS` from this record and builds tables on a 4 KiB
    // granule this checks the part actually has. A machine that cannot run us says so and stops,
    // rather than turning the MMU on and faulting on the next instruction fetch with no console
    // line to explain it.
    arch::isa::init(dtb);

    // Which cores does this machine have, and how are they started (milestone 100)? Both come out
    // of the device tree, and both have to be read HERE: `mmu::init` on the next line replaces the
    // coarse boot map, and the blob is only reachable through that one. The list used to be
    // `0..cpu::MAX_CPUS` and the PSCI conduit used to be `hvc`, compiled in; `isa::init` above took
    // the `/psci` half.
    smp::read_cpu_list(dtb);

    // And now the sketchiest moment in the kernel. The instant SCTLR_EL1.M is set, the very
    // next instruction is fetched through the MMU. See arch/aarch64/mmu.rs.
    arch::mmu::init();

    // The per-CPU interrupt stacks go live here, and not one line earlier: their guard pages are
    // holes in the map `mmu::init` just installed, and the coarse boot map has no holes at all. A
    // trap before this point runs its handler on whatever stack it interrupted, which is what every
    // trap did before milestone 124. See kernel/src/interrupt_stack.rs.
    interrupt_stack::init();

    // The SMMUv3 (milestone 16b), if the machine was started with `iommu=smmuv3`. Its register
    // block is a device-tree node (memory::smmu_region), mapped by mmu::init just above; absent, the
    // kernel runs exactly as before. Bringing it up here installs an all-invalid stream table and
    // sets default-deny, so every PCIe stream aborts until virtio::register confines its device.
    // The CPU's own ECAM and BAR reads are not DMA, so PCI enumeration below is unaffected. See
    // kernel/src/iommu.rs, notes/iommu.md.
    if let Some((smmu_base, _)) = memory::smmu_region() {
        arch::iommu::init(smmu_base);
    }

    // The heap must come AFTER the MMU: it hands out addresses, and with paging on an
    // address is only usable if something has mapped it. From here, `Vec` works.

    // And now interrupts, which is where every lock in the kernel stops being a formality.
    //
    // The scheduler comes up FIRST, so that the very first timer tick already has somewhere to
    // send a reschedule. Adopting the boot context as thread 0 costs one allocation.
    sched::init();
    interrupts_init(dtb);

    // Bring the other cores online. They come up idle: step 2 proves the bring-up path works
    // (PSCI, per-core stacks, the MMU replay), and leaves real multi-core scheduling to step 3.
    // Core 0 has IRQs on by now, so it keeps ticking while it waits for the others to check in.
    // See smp.rs and DECISIONS.md §11.
    smp::bring_up_secondaries();

    #[cfg(test)]
    test_main();

    // The instruction-count boot (milestone 78, `script/icount`): assert the two timing claims on
    // the deterministic clock and park. **Before the bench boot, and its feature implies that one**,
    // because the two park at the same point and therefore leave the same functions unreferenced;
    // riding on `bench`'s existing conditions is what keeps this from duplicating a dozen `cfg`s
    // across five files. The `bench::run` below is then unreachable, which is what this function's
    // `allow(unreachable_code)` names.
    #[cfg(feature = "icount")]
    icount::run();

    // The benchmark boot (milestone 21, `script/bench`): run the microbenchmarks and halt,
    // instead of the tour or the shell. Diverges, so everything below is untouched by it.
    #[cfg(feature = "bench")]
    bench::run();

    #[cfg(not(any(test, feature = "bench")))]
    {
        use aarch64_cpu::registers::CurrentEL;
        use tock_registers::interfaces::Readable;

        println!();
        println!("nife");
        println!("  exception level : EL{}", CurrentEL.read(CurrentEL::EL));
        arch::isa::print_summary();
        println!("  stack top       : {:#018x}", stack_top());
        println!("  device tree     : {dtb:#018x}");
        memory::print_summary();
        arch::mmu::print_summary();
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

        // The SMB serve boot (milestone 54): wire the net server and the SMB adapter serving
        // forever, print how to mount it, and hand the machine to them. `cargo xtask smb-serve`;
        // see notes/smb.md. It parks here instead of compiling the tour and the init handoff
        // out, so this feature manufactures no dead code for the lint to chase; the tour below
        // is simply never reached.
        #[cfg(feature = "smb_serve")]
        {
            user::smb_serve_boot();
            arch::halt();
        }

        // The milestone tour. Compiled out by `cargo xtask shell` and `cargo xtask initboot`,
        // which boot straight to the system instead of scrolling all of this first.
        #[cfg(not(any(feature = "shell", feature = "initboot")))]
        {
            println!();
            println!(
                "milestone 1: we are running our own code on a CPU with nothing underneath it."
            );
            println!("milestone 2: and when it goes wrong, we get told.");
            println!(
                "           : and the machine now tells us what it is, instead of us guessing."
            );
            println!("milestone 3: and we know which parts of it are ours to give away.");
            println!("milestone 4: and nothing writable is executable, and Vec works again.");
            println!("milestone 5: and the machine can now interrupt us. we are preemptible.");
            println!("milestone 6: and a thread that refuses to yield gets preempted anyway.");
            println!("milestone 7: and now it runs a binary it did not compile, unprivileged.");
            println!(
                "           : and that binary can talk to a server it can only name, not reach."
            );
            println!();

            // The whole argument, executable.
            {
                use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

                use crate::arch::timer;

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

                let (hostile, polite, preempted) = (
                    HOSTILE.load(Ordering::Relaxed),
                    POLITE.load(Ordering::Relaxed),
                    sched::preemptions() - p0,
                );
                println!("  half a second later, having spawned two threads that NEVER yield:");
                println!();
                println!("    thread 1 (hostile) : {hostile:>10} iterations");
                println!("    thread 2 (polite)  : {polite:>10} iterations");
                println!("    preemptions        : {preempted:>10}");
                println!();
                // The closing claim only prints when the numbers above back it. The RISC-V tour's
                // scheduler smoke line earned this the hard way: an unconditional success claim
                // over a raced count read "0 of 2 ... works" on the VisionFive 2
                // (notes/visionfive2.md, boots 12 and 13). The window here is wall-clock, not a
                // yield count, so timing is sound; only the wording was unconditional.
                if hostile > 0 && polite > 0 && preempted > 0 {
                    println!("  neither asked to be interrupted. both were.");
                } else {
                    println!("  FAILED: a spinner did not run, or nothing was preempted.");
                }
            }

            // 7a. EL0.
            {
                use core::sync::atomic::Ordering;

                use crate::arch::exceptions::SVC_COUNT;
                use crate::arch::timer;

                println!();
                println!("  and now the other side of the boundary:");
                println!();

                // Milestone 9: a virtio block device, driven from userspace.
                match crate::virtio::find_block_device() {
                    None => println!("    virtio : no block device attached"),
                    Some(d) => {
                        println!(
                            "    virtio : block device at {:#x}, INTID {}, handing it to a driver at EL0",
                            d.mmio_phys, d.intid,
                        );
                        if let Some(report) = user::virtio_service::start(image_for_virtio()) {
                            // The driver reads block 0 and sends us its first 8 bytes. We check they
                            // are the nifefs magic, which proves real disk bytes crossed DMA and
                            // the EL0 boundary. This RECV blocks until the driver has done the read.
                            let word = sched::ipc_recv(report)[0];
                            let head = word.to_le_bytes();
                            println!();
                            if &head == b"nife: re" {
                                println!(
                                    "      a driver at EL0 read the file 'motd' off a virtio disk,"
                                );
                                println!("      through a nifefs superblock it parsed itself,");
                                println!(
                                    "      woken by the device's interrupt delivered as a message."
                                );
                                println!(
                                    "      the kernel issued no virtio command and touched no DMA."
                                );
                            } else {
                                println!(
                                    "      the driver reported {head:?}, not the motd contents"
                                );
                            }
                        }
                    }
                }

                // The same disk again, over PCIe (DECISIONS §18): found by ECAM enumeration,
                // BARs placed by the kernel, INTx through the GIC, the identical driver and
                // confinement behind the transport seam.
                match crate::pci::find_block_device() {
                    None => println!("    pcie   : no virtio-blk on the bus"),
                    Some(d) => {
                        println!(
                            "    pcie   : virtio-blk at {:02x}:{:02x}.{}, INTID {}, same driver, same confinement",
                            d.bdf.bus, d.bdf.dev, d.bdf.func, d.intid,
                        );
                        if let Some(report) = user::virtio_service::start_pci(image_for_virtio()) {
                            let word = sched::ipc_recv(report)[0];
                            if &word.to_le_bytes() == b"nife: re" {
                                println!(
                                    "      the same file, over the transport real hardware uses."
                                );
                            } else {
                                println!("      the pcie driver reported the wrong bytes");
                            }
                        }
                    }
                }

                // The privilege boundary, still real: a program that reaches for a kernel address is
                // killed, and the kernel is not. The address is the kernel's own fault counter, so
                // it is certainly mapped and certainly not the process's; the demo hands it to the
                // program rather than baking a constant in, which is what makes it the same demo on
                // either ISA. It used to run `user::outlaw()`, a hand-written aarch64 blob.
                if let Some(image) = user::program("outlaw") {
                    use crate::arch::exceptions::USER_FAULTS;
                    let kernel_addr = &raw const USER_FAULTS as u64;
                    let faults0 = USER_FAULTS.load(Ordering::Relaxed);
                    sched::spawn(move || {
                        user::run(
                            image,
                            user::Spawn {
                                arg0: user::OUTLAW_READ_KERNEL,
                                arg1: kernel_addr,
                                arg2: 0,
                                grants: &[],
                                maps: &[],
                            },
                        )
                    });
                    timer::spin_for(timer::frequency() / 20);
                    println!(
                        "    outlaw : reached {kernel_addr:#018x}, was killed, kernel survived ({} fault)",
                        USER_FAULTS.load(Ordering::Relaxed) - faults0,
                    );
                }

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
                        println!(
                            "  the console driver now runs at EL0. what follows is printed by it:"
                        );
                        println!();

                        // Start the console SERVER as a user process that owns the UART, then a
                        // CLIENT wired to it. The lines the client prints travel through a page it
                        // shares with the server; the kernel never touches the bytes. The server is
                        // its own binary now ("console", 19f.3); the demo client is still a role of
                        // hello, so it takes the "init" entry of the archive.
                        let prog = user::program("init").expect("no init program in the initrd");
                        let console = user::console_service::start();
                        user::console_service::spawn_client(prog, console);
                        timer::spin_for(timer::frequency() / 10);

                        println!();
                        println!(
                            "  ...and control is back in the kernel, which never saw those bytes."
                        );
                    }
                }

                println!();
                println!("  a userspace program printed to the screen, and the kernel does not");
                println!("  contain a line of code that puts a user's bytes on the wire.");
                let _ = SVC_COUNT.load(Ordering::Relaxed);

                // Milestone 11: a process spends its own memory; the kernel allocates nothing.
                if let Some(image) = user::program("init")
                    && let Some((_region, report, _demo)) = user::untyped_service::start(image, 24)
                {
                    sched::ipc_recv(report); // the process signals it is loaded and ready
                    let before = memory::stats().unwrap().used;
                    let mapped = sched::ipc_recv(report)[0]; // it maps until its untyped is spent
                    let after = memory::stats().unwrap().used;
                    println!();
                    println!(
                        "  milestone 11: a process mapped {mapped} pages out of an untyped it was handed,"
                    );
                    println!(
                        "  and the kernel's used-frame count went {before} -> {after} (it did not move)."
                    );
                    println!(
                        "  a process cannot make the kernel allocate, so it cannot exhaust it."
                    );
                }
            }
        } // end of the milestone tour (#[cfg(not(feature = "shell"))])

        // **Milestone 19d.2c, completed at 28: userspace init is the boot path.** The kernel stops
        // wiring services itself. It hands off to init, which brings up the console server, the line
        // discipline (`line_editor`, milestone 28), the input driver, and the shell out of its own budget
        // through the granular verbs. This is the line that retires the kernel as the system's
        // builder. Every aarch64 interactive build reaches it: `--features shell` and the milestone
        // tour hand off straight away (the tour after running its demos), the same way `initboot`
        // always has, and the same way RISC-V's `--features shell` hands off to `system_initializer`. The
        // legacy kernel-wired `user::shell_service` is retired as a boot path (it cannot host the
        // milestone-28 shell, which speaks the terminal contract, not the raw console protocol) and
        // is kept only as dead code for reference.
        if let Some(image) = user::initrd() {
            println!();
            println!("nife: handing the system to userspace init.");
            user::boot_via_init(image);
            // The boot thread's work is done; init and the services it builds run until halt.
        }
    }

    #[cfg(not(feature = "bench"))] // bench::run diverged above; this is everyone else's parking
    arch::halt()
}

/// Bring up the interrupt controller and the timer, then **unmask interrupts**.
///
/// This is the line the whole locking discipline was written for. From here, a timer interrupt
/// can land between any two instructions in the kernel, and every `IrqSafeMutex` starts
/// actually masking something. See DECISIONS.md §9 and notes/locking.md.
/// The initrd image, for the virtio service. Panics if absent (the demo checked `initrd()` above).
#[cfg(not(test))]
// Tour-only: the shell, initboot, and bench boots all skip the milestone tour where it is used.
#[cfg_attr(any(feature = "shell", feature = "initboot"), allow(dead_code))]
#[cfg(not(feature = "bench"))]
fn image_for_virtio() -> &'static [u8] {
    user::program("init").expect("no init program in the initrd")
}

fn interrupts_init(_dtb: usize) {
    use crate::arch::{interrupts, irq, timer};

    // Bring up the interrupt controller (the GIC on aarch64, the PLIC on RISC-V), reading its
    // location from the device tree. Portable code names `arch::irq`, never a specific controller,
    // which is what lets a second architecture be a new `arch/` directory rather than a diff here.
    irq::init();

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
#[cfg(not(feature = "bench"))] // tour-only, and the bench boot skips the tour
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
        // black_box so this is a real runtime check, not a constant clippy folds to `2 == 2`.
        let two = core::hint::black_box(1) + core::hint::black_box(1);
        assert_eq!(two, 2);
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
    /// Both aarch64 and RISC-V require a 16-byte-aligned `sp`, and both fault or corrupt
    /// silently on a misaligned one, so a bug here would show up as a mysterious early crash
    /// rather than as anything legible. Reads `sp` through `arch::current_sp`, so it is portable.
    /// See notes/stack.md.
    #[test_case]
    fn stack_pointer_is_16_byte_aligned() {
        let sp = crate::arch::current_sp();
        assert_eq!(sp % 16, 0, "sp = {sp:#x}");
    }

    /// Proves we are where we think we are.
    ///
    /// QEMU's `virt` machine drops us at EL1 by default, which is exactly where a
    /// kernel belongs. If this ever reads EL2, we've been handed the hypervisor
    /// level and will need to drop down ourselves. See notes/aarch64.md.
    ///
    /// EL is an aarch64 concept, so this is gated. **RISC-V needs no twin, and the older
    /// comment here promising one "with the RISC-V boot path" outlived the boot path it
    /// was waiting for.** That ISA deliberately gives S-mode no way to read its own
    /// privilege level, and it does not need one: `arch::riscv64::exceptions`'s
    /// `breakpoint_is_caught_and_execution_resumes` proves the same thing sideways, because
    /// the breakpoint arm it counts is guarded on the trap having come from S-mode, and an
    /// M-mode `ebreak` would have gone to OpenSBI's `mtvec` and never reached us at all.
    /// See notes/riscv-arch-tests.md.
    #[cfg(target_arch = "aarch64")]
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
