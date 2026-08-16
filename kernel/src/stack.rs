//! Stack overflow detection.
//!
//! # Why this exists
//!
//! Milestone 3 hung the machine. A test put a 16 KiB array on the 64 KiB boot stack,
//! `sp` walked below `__stack_bottom`, and the frame wrote straight through `.bss`,
//! `.data`, and into `.text`. The kernel then executed its own corrupted code.
//!
//! There was no crash, no fault, no message. It hung *while printing*, and the print
//! had nothing to do with it. The bug was thousands of instructions upstream, in a
//! function prologue that had already returned.
//!
//! That is the worst failure mode this codebase can produce, and it was completely
//! invisible. So now it isn't.
//!
//! # What the real fix is
//!
//! A **guard page**: leave the page below the stack unmapped, and the MMU faults the
//! instant anything touches it. Precise, free at runtime, and impossible to miss.
//!
//! We can't do that yet, because we have no MMU. That's milestone 4, and the TODO is
//! recorded in link-aarch64.ld.
//!
//! Until then, a canary. It is strictly worse than a guard page: it detects the damage
//! *after* it has happened rather than preventing it, and only if the overflow actually
//! wrote over the canary words. But it turns "the machine went insane for no reason"
//! into "you blew the stack," which is the difference between an afternoon and five
//! seconds.

use core::ffi::c_void;

/// Written at the very bottom of the stack. Nothing legitimate should ever touch it.
///
/// Four words rather than one: a big stack frame decrements `sp` past the bottom and
/// then writes throughout the frame, so a wider target is more likely to be hit.
// Arbitrary, but deliberately not zero (fresh RAM and `.bss` are full of zeroes) and not
// a plausible pointer or small integer, so a stray write is unlikely to reproduce one by
// accident.
const CANARY: [u64; 4] = [
    0x57ac_c0de_57ac_c0de,
    0xc0ff_ee00_1eaf_babe,
    0xdead_c0de_5111_c0de,
    0xfeed_face_cafe_f00d,
];

/// Paint the canary. Call this before anything can use much stack.
pub fn init() {
    // SAFETY: `__stack_bottom` is inside our own image, and by definition nothing has
    // pushed a frame that deep, or we would already be dead.
    unsafe {
        core::ptr::write_volatile(bottom() as *mut [u64; 4], CANARY);
    }
}

/// Has anything scribbled below the stack?
pub fn intact() -> bool {
    // SAFETY: reading our own image.
    unsafe { core::ptr::read_volatile(bottom() as *const [u64; 4]) == CANARY }
}

/// How many bytes are left between `sp` and the bottom of the stack.
///
/// Negative means we are *already* below it and are actively corrupting the kernel.
pub fn headroom() -> i64 {
    let sp = crate::arch::current_sp();
    sp.wrapping_sub(bottom()) as i64
}

/// Which kernel stack a faulting address belongs to, when it lands in that stack's guard page.
///
/// There are three kinds and they are allocated three different ways, which is exactly why nothing
/// used to name them: the boot stack's guard is a linker symbol, a secondary's is an offset into a
/// `.bss` array (milestone 90), and a thread's is a slot in the virtual stack area 64 GiB up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardPage {
    /// The boot stack's guard page (`__stack_guard`). Core 0 runs the whole test suite on it.
    Boot,
    /// Secondary core `id`'s guard page.
    Secondary(usize),
    /// A thread kernel stack's guard page: the slot index within `thread::STACK_AREA`, counting
    /// from zero. Not a `Tid`, deliberately, because turning a slot back into a thread needs the
    /// scheduler lock and this runs in a handler that may not take one.
    Thread(u64),
}

/// **Is `addr` inside a kernel stack guard page, and whose?**
///
/// The one question a fatal kernel fault should answer before anything else, because the answer
/// turns "unexpected trap at some address" into "this stack overflowed", and those two send a
/// reader to completely different places. Nothing did it before milestone 78: a thread-stack
/// overflow reached CI as `unexpected RISC-V trap: scause=0xf ... from_user=false`, which reads as
/// a fault in the memory system rather than the guard page doing its job.
///
/// Takes no locks and touches no allocator, because every caller is already in a fault handler.
///
/// # BUGS
///
/// **A `None` here does not mean the stack is fine.** A guard page is one page, and the kernel has
/// stack frames bigger than that (`sched::reap_region_objects` is 6832 bytes on riscv64), so a
/// function entered near the bottom of a 24 KiB thread stack can put `sp` clean below the guard
/// page without ever touching it, and then write into the *neighbouring slot's* top stack page,
/// which is mapped and in use. That produces silent corruption rather than a fault, and this
/// function cannot see it. See notes/load-sensitive-assertions.md.
pub fn guard_page_at(addr: u64) -> Option<GuardPage> {
    const PAGE: u64 = 4096;

    let boot = crate::arch::mmu::stack_guard();
    if (boot..boot + PAGE).contains(&addr) {
        return Some(GuardPage::Boot);
    }

    for id in 0..crate::cpu::MAX_CPUS {
        let g = crate::smp::secondary_stack_guard(id);
        if (g..g + PAGE).contains(&addr) {
            return Some(GuardPage::Secondary(id));
        }
    }

    let (area, watermark) = crate::thread::stack_area_span();
    if (area..watermark).contains(&addr) {
        let off = addr - area;
        // A slot is [guard page][STACK_PAGES of stack], so an offset inside the first page of a
        // slot is a guard-page hit and anything above it is ordinary stack.
        if off % crate::thread::STACK_SLOT_SPAN < PAGE {
            return Some(GuardPage::Thread(off / crate::thread::STACK_SLOT_SPAN));
        }
    }

    None
}

/// Shout if `addr` is a kernel stack guard page, naming which stack and how far past its bottom the
/// access went. Called from both ISAs' fatal-fault paths with the faulting address.
///
/// The distance is the number that matters: a few bytes past the bottom is an ordinary overflow,
/// and *thousands* of bytes past it means a single frame jumped most of the way across the guard,
/// which is the case where the next one jumps clean over it. Printing it is what let milestone 78
/// tell those apart.
pub fn warn_if_guard_page(addr: u64) {
    let Some(kind) = guard_page_at(addr) else {
        return;
    };

    crate::println!();
    crate::println!("  *** KERNEL STACK OVERFLOW ***");
    match kind {
        GuardPage::Boot => {
            crate::println!("  {addr:#018x} is in the BOOT stack's guard page.");
            crate::println!(
                "  bottom {:#018x}, so sp went {} bytes past it.",
                crate::arch::mmu::stack_bottom(),
                crate::arch::mmu::stack_bottom().saturating_sub(addr),
            );
        }
        GuardPage::Secondary(id) => {
            let (bottom, _) = crate::smp::secondary_stack_span(id);
            crate::println!("  {addr:#018x} is in core {id}'s boot-stack guard page.");
            crate::println!(
                "  bottom {bottom:#018x}, so sp went {} bytes past it.",
                bottom.saturating_sub(addr),
            );
        }
        GuardPage::Thread(slot) => {
            let (area, _) = crate::thread::stack_area_span();
            let bottom = area + slot * crate::thread::STACK_SLOT_SPAN + 4096;
            crate::println!(
                "  {addr:#018x} is in THREAD stack slot {slot}'s guard page (thread.rs)."
            );
            crate::println!(
                "  bottom {bottom:#018x}, so sp went {} bytes past it, on a {}-byte stack.",
                bottom.saturating_sub(addr),
                crate::thread::STACK_PAGES * 4096,
            );
            print_text_words(bottom, bottom + (crate::thread::STACK_PAGES * 4096) as u64);
        }
    }
    crate::println!("  The guard page is ONE page. A frame larger than that can step over it");
    crate::println!("  into the slot below without faulting; see notes/stack.md.");
}

/// Print every word on the overflowed stack that points into the kernel's text section, deepest
/// first: a conservative backtrace, and the only kind this kernel can produce, because it does
/// not maintain frame pointers (the 2026-08-15 CI overflow was symbolized by rebuilding CI's
/// exact binary in a container to learn what one register meant; this exists so the next report
/// carries its own call chain). Most hits are genuine return addresses; some are spilled
/// function pointers or stale words from a previous tenant of a reused stack slot, so read it as
/// candidates for `llvm-addr2line`, not as a walked chain. Capped so a full stack cannot flood
/// the serial log the dump itself needs.
///
/// Thread stacks only, deliberately: their pages are all mapped (thread.rs maps every slot
/// whole), so the scan cannot itself fault. The boot and secondary arms above could take the
/// same scan but have never overflowed outside milestone 3's era; add them when one does.
fn print_text_words(bottom: u64, top: u64) {
    const CAP: usize = 40;
    let text = crate::arch::mmu::text_start()..crate::arch::mmu::text_end();
    crate::println!(
        "  Words on the dead stack that point into .text ({:#x}..{:#x}), deepest first,"
        , text.start, text.end
    );
    crate::println!("  as `bottom+offset: word` (candidate return addresses; no frame pointers):");
    let mut printed = 0usize;
    let mut p = bottom;
    while p < top && printed < CAP {
        // SAFETY: `[bottom, top)` is a thread stack slot's mapped span (the caller derived it
        // from the slot geometry), 8-byte aligned; volatile because the stack's owner is dead
        // mid-store and nothing about this memory is ordinary.
        let w = unsafe { core::ptr::read_volatile(p as *const u64) };
        if text.contains(&w) {
            crate::println!("    +{:#07x}: {w:#018x}", p - bottom);
            printed += 1;
        }
        p += 8;
    }
    if printed == CAP {
        crate::println!("    ... capped at {CAP} words; the shallower stack is not shown.");
    }
}

/// Shout if the canary is dead. Called from the panic handler and the fault handler,
/// because a corrupted stack makes every *other* diagnostic a potential lie.
pub fn warn_if_smashed() {
    if !intact() {
        crate::println!();
        crate::println!("  *** STACK OVERFLOW ***");
        crate::println!("  The canary below __stack_bottom is dead, so we have written");
        crate::println!("  through our own .bss/.data/.text. Nothing printed above this");
        crate::println!("  line can be trusted. See notes/stack.md.");
        crate::println!("  headroom: {} bytes", headroom());
    }
}

fn bottom() -> u64 {
    unsafe extern "C" {
        static __stack_bottom: c_void;
    }
    (&raw const __stack_bottom) as u64
}

#[cfg(test)]
fn top() -> u64 {
    unsafe extern "C" {
        static __stack_top: c_void;
    }
    (&raw const __stack_top) as u64
}

// --- Stack high-water measurement (milestone 84) ---
//
// The canary above answers "did an overflow happen"; nothing answered "how close are we". The
// FS-server stack bug (notes/nifefs.md) was found the expensive way, and until this landed the
// claim "the stacks are big enough" was an argument, not a measurement. So: paint every
// kernel-owned stack with a pattern before use, and at the end of the test suite scan each for the
// deepest overwritten word. Test builds only, deliberately: painting 16 KiB per thread spawn would
// perturb the spawn benchmark, and the report goes through the test channel anyway.
//
// A watermark sees only exercised paths. An unexercised deep path stays invisible, the same limit
// coverage has. See notes/stack-high-water.md.

/// The paint word. Not zero (fresh `.bss` is zeroes, and a stack full of zeroes would read as
/// untouched), not a plausible pointer or length, and not one of the canary words.
#[cfg(test)]
const PAINT: u64 = 0x5AFE_57AC_5AFE_57AC;

/// Paint `[bottom, top)` with [`PAINT`].
///
/// # Safety
/// `[bottom, top)` must be a mapped, writable, 8-byte-aligned range that **nothing has used yet**.
/// This writes every word in it, so a live frame anywhere inside is corrupted: a call site painting
/// a stack that is in use has to skip the live portion itself (see [`paint_boot_stack`], which stops
/// a margin below the running `sp`).
///
/// It was a safe fn until milestone 112, with that requirement written as a `// SAFETY:` comment
/// naming "the caller". Three call sites, and any other safe code in the kernel could have written
/// this pattern over an arbitrary address range.
#[cfg(test)]
pub unsafe fn paint(bottom: u64, top: u64) {
    let mut p = bottom as *mut u64;
    while (p as u64) < top {
        // SAFETY: this function's own `# Safety` contract puts `[bottom, top)` in a mapped, unused,
        // aligned region, and the loop stays inside it. Volatile so the writes are not elided or
        // coalesced into something that assumes the region is ordinary memory.
        unsafe { core::ptr::write_volatile(p, PAINT) };
        p = p.wrapping_add(1);
    }
}

/// **The most bytes this stack ever used**, for a painted stack `[bottom, top)`: scan upward from
/// the bottom for the first word that is no longer [`PAINT`], and return the distance from there to
/// the top. Not the depth now and not the space free; the maximum ever reached, which is what
/// painting buys (the deepest frame destroys the paint and the damage outlives it).
///
/// The reading inverts twice, so: the **painted** bytes are the ones nothing ever used, and the
/// number counts *up* from a deepest point that is at a *low* address. Paint left is room to
/// spare. See notes/stack-high-water.md. Iterative, no locals of size, so the scan
/// itself needs no meaningful depth on whatever stack it runs on.
///
/// A frame whose deepest word happened to store the paint value exactly reads one word shallow.
/// Classic limitation of the method; a 64-bit pattern makes it vanishingly unlikely.
///
/// # Safety
/// `[bottom, top)` must be a mapped, readable, 8-byte-aligned range, and it must be a range
/// [`paint`] ran over, or the answer is a reading of somebody else's bytes rather than a
/// measurement.
///
/// **This one is milestone 112's correction to milestone 82's survey**, which found four sites by
/// grepping SAFETY comments for the word "caller" and so missed this one: it had the identical
/// defect (a safe fn dereferencing an address range built from its own arguments) written in the
/// passive voice, "a mapped stack region", which names the obligation without naming anybody who
/// owes it. Passive voice hides the defect from the pattern that found the rest of it.
#[cfg(test)]
pub unsafe fn high_water(bottom: u64, top: u64) -> u64 {
    let mut p = bottom as *const u64;
    while (p as u64) < top {
        // SAFETY: this function's own `# Safety` contract puts `[bottom, top)` in a mapped, aligned
        // region, and the loop stays inside it. Volatile because another core may own this stack and
        // be running on it right now; we only ever compare a snapshot word against the pattern, and
        // a racing write can only make the stack look deeper, never shallower than it truly was.
        if unsafe { core::ptr::read_volatile(p) } != PAINT {
            return top - p as u64;
        }
        p = p.wrapping_add(1);
    }
    0
}

/// How far below the paint-time `sp` the boot-stack paint stopped, recorded so the report can say
/// what its floor is: a measured high-water equal to the floor means "nothing after the paint went
/// deeper than the paint itself", not "this is the true maximum".
#[cfg(test)]
static BOOT_PAINT_CEILING: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Paint the unused part of the boot stack. Called from `kernel_main` right after [`init`], so the
/// live portion (boot.s frames plus `kernel_main`'s own) is honestly skipped rather than painted
/// over: everything from the canary up to a margin below the current `sp` gets the pattern.
///
/// Two honest limits. Depth used *before* this runs and never reached again is invisible; the boot
/// path to here is a handful of shallow frames, so the floor is low, and the report prints it. And
/// the margin below `sp` exists because the paint loop's own callees (`write_volatile` is a real
/// call in a debug build) push frames below our `sp` while the loop runs; painting inside a live
/// callee frame would corrupt it.
#[cfg(test)]
pub fn paint_boot_stack() {
    let ceiling = crate::arch::current_sp() - 512;
    BOOT_PAINT_CEILING.store(ceiling, core::sync::atomic::Ordering::Relaxed);
    // Start above the canary words: painting them would kill the canary check.
    //
    // SAFETY: the boot stack is mapped by the linker script and is live from `_start`, and `ceiling`
    // is 512 bytes below the running `sp`, so the range is the unused part. The 512 is the margin
    // for the paint loop's own callees, which push frames below our `sp` while it runs.
    unsafe { paint(bottom() + core::mem::size_of_val(&CANARY) as u64, ceiling) };
}

/// Deepest thread-stack use seen so far, in bytes, over every reaped [`crate::thread::KernelStack`]
/// (scanned in its `Drop`) and, at report time, every live one. One number, because every kernel
/// thread stack is the same size.
#[cfg(test)]
static THREAD_STACK_MAX: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// How many thread stacks fed [`THREAD_STACK_MAX`], so the report says how much evidence the
/// number rests on.
#[cfg(test)]
static THREAD_STACK_SCANS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record one thread stack's measured use (called from `KernelStack`'s `Drop`, and from the live
/// scan at report time).
#[cfg(test)]
pub fn note_thread_stack_use(used: u64) {
    use core::sync::atomic::Ordering;
    THREAD_STACK_MAX.fetch_max(used, Ordering::Relaxed);
    THREAD_STACK_SCANS.fetch_add(1, Ordering::Relaxed);
}

/// Per-stack high-water report, printed by the test runner after the last test. The numbers are a
/// property of the code and the suite, not of the host: depth is determined by what ran, so a
/// loaded runner moves nothing here (the one caveat is interrupt-arrival timing, which decides
/// where on a stack a trap frame lands; see notes/stack-high-water.md for the measured spread).
#[cfg(test)]
pub fn report_high_water() {
    use core::sync::atomic::Ordering;

    let boot_bottom = bottom() + core::mem::size_of_val(&CANARY) as u64;
    let size = top() - boot_bottom;
    // SAFETY: the same range `paint_boot_stack` painted, on a stack the linker script mapped.
    let used = unsafe { high_water(boot_bottom, top()) };
    let floor = top() - BOOT_PAINT_CEILING.load(Ordering::Relaxed);
    crate::println!(
        "stack high-water: boot   {used}/{size} bytes ({}%), paint floor {floor}",
        used * 100 / size,
    );

    let boot_core = crate::arch::boot_cpu_id();
    let mut max_secondary = 0u64;
    for id in 0..crate::cpu::MAX_CPUS {
        if id == boot_core || crate::smp::online_harts_mask() & (1 << id) == 0 {
            continue; // the boot core runs on the linker-script stack; its slot was never painted
        }
        let (b, t) = crate::smp::secondary_stack_span(id);
        // SAFETY: the span of an online core's `.bss` stack slot, which `bring_up_secondaries`
        // painted whole before any `CPU_ON`. The `continue` above skipped every slot that was not.
        let core_used = unsafe { high_water(b, t) };
        max_secondary = max_secondary.max(core_used);
        crate::println!(
            "stack high-water: core{id}  {core_used}/{} bytes ({}%)",
            t - b,
            core_used * 100 / (t - b),
        );
    }

    // Live thread stacks last, so long-lived service threads (the FS server, the shape of the
    // motivating incident) are counted even though nothing ever reaps them.
    crate::sched::scan_live_thread_stacks();
    let tmax = THREAD_STACK_MAX.load(Ordering::Relaxed);
    let tsize = (crate::thread::STACK_PAGES * 4096) as u64;
    crate::println!(
        "stack high-water: thread {tmax}/{tsize} bytes ({}%, deepest of {} stacks)",
        tmax * 100 / tsize,
        THREAD_STACK_SCANS.load(Ordering::Relaxed),
    );

    // The gate, landed after the measurement per the milestone's measure-then-gate sequence.
    // Checked after the printing, so a trip always comes with the numbers that explain it.
    //
    // The margins are justified by the measured spread, which is unusually small: two aarch64 runs
    // agreed byte for byte on every stack under host loads of 33 and 9, and the two ISAs agree to
    // within ~410 bytes (boot 53808/54216, secondaries 8504/8448, thread 11352/11672; see
    // notes/stack-high-water.md). So each limit sits far above anything observed, and what it buys
    // against that stability is an alarm that still fires BEFORE the stack actually runs out:
    //
    //   - boot 61440: +7.2 KiB over the observed max, and a trip still leaves a full page before
    //     the guard. Growth lands here first (test_main runs every test body on this stack).
    //   - secondary 16384: ~2x observed. Milestone 90 put a guard page under each of these stacks
    //     (smp.rs), so this is no longer the only thing standing between a deep secondary and
    //     silent .bss corruption; it is now what a guard page cannot be, an alarm that fires ~48
    //     KiB BEFORE the fault, in the run that drifts rather than the run that dies.
    //   - thread 18432: sized against the measured worst-case STACKING, not just the observed
    //     high-water. The 2026-08-15 CI overflows (thread.rs, `STACK_PAGES`) showed the honest
    //     worst case is observed-deepest (~11.7 KiB) plus a blocked thread's resident residue
    //     (~1.4 KiB) plus a preemption landing at the deepest instant (~2.3 KiB), about 15.5 KiB,
    //     which is why the old 16 KiB stacks overflowed under load and why the old 14336 limit
    //     could pass a green run whose true worst case was already past the stack. 18432 sits
    //     ~3 KiB above that sum, and trips 6 KiB before the 24 KiB stack's guard, so a load-heavy
    //     but healthy run passes while real growth still alarms long before the fault.
    assert!(
        used <= 61440,
        "boot stack high-water {used} exceeded 61440: the suite's deepest path has grown ~7 KiB \
         past its measured depth and is within a page of the guard (notes/stack-high-water.md)",
    );
    assert!(
        max_secondary <= 16384,
        "a secondary stack's high-water {max_secondary} exceeded 16384, ~2x anything measured; \
         the guard page below it (smp.rs) would still catch a real overflow, but something is \
         running much deeper on an idle-and-traps stack than the suite has ever measured",
    );
    assert!(
        tmax <= 18432,
        "thread stack high-water {tmax} exceeded 18432: some kernel thread is within 6 KiB of its \
         guard page, ~3 KiB past the measured worst-case stacking (notes/stack-high-water.md)",
    );
}

#[cfg(test)]
mod tests {
    //! Tests for stack overflow detection.

    /// Proves the stack canary works, without actually smashing the stack.
    ///
    /// The runner checks this after every test (see testing.rs), so a test that blows the
    /// stack is now caught immediately and by name, rather than corrupting the kernel and
    /// hanging somewhere unrelated. That is exactly how milestone 3 went wrong.
    #[test_case]
    fn stack_canary_is_intact_and_we_have_headroom() {
        assert!(crate::stack::intact(), "stack canary is already dead");
        assert!(
            crate::stack::headroom() > 4096,
            "less than 4 KiB of stack left: {}",
            crate::stack::headroom()
        );
    }
}
