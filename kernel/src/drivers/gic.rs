//! GICv2: the ARM Generic Interrupt Controller.
//!
//! The thing that decides *which* interrupt you are taking, and whether you are allowed to
//! take it at all. The CPU only has one IRQ input line; the GIC is the multiplexer in front
//! of it, and everything a kernel wants from interrupts (priorities, masking individual
//! sources, routing to a particular core) lives here rather than in the CPU.
//!
//! # Two halves, and the split is the whole design
//!
//! | | Where | Shared? | What it does |
//! |---|---|---|---|
//! | **Distributor** (GICD) | `0x0800_0000` | one for the whole machine | decides *which core* gets an interrupt, and whether a given source is enabled at all |
//! | **CPU interface** (GICC) | `0x0801_0000` | one **per core** (banked) | the core's own view: acknowledge, set a priority mask, signal end-of-interrupt |
//!
//! One distributor, N CPU interfaces at the *same address* (the hardware banks the registers
//! per core). Which is what makes "route this interrupt to core 3" a thing the hardware can do
//! without the software knowing.
//!
//! # Three kinds of interrupt, and the numbering is not arbitrary
//!
//! | INTID | Kind | |
//! |---|---|---|
//! | 0-15 | **SGI** — Software Generated | one core kicking another. How SMP bringup and TLB shootdown work. |
//! | 16-31 | **PPI** — Private Peripheral | *per-core*. The timer is one of these, and it has to be: every core needs its own. |
//! | 32+ | **SPI** — Shared Peripheral | the UART, the disk. Any core may service them. |
//!
//! **The timer is a PPI (INTID 30) and that is not an accident.** A timer that fired on only
//! one core could not preempt threads on the others. Each core has its own timer, its own
//! countdown, and its own interrupt, all wearing the same number.
//!
//! # Priorities are backwards
//!
//! **Lower value = higher priority.** And `GICC_PMR` is a *mask*: an interrupt is delivered
//! only if its priority is **strictly less than** PMR. So `PMR = 0xff` means "let everything
//! through" and `PMR = 0` means "let nothing through".
//!
//! Get the comparison backwards and you get a machine that takes no interrupts and gives you
//! no clue why.

// is_initialized has no non-test caller yet.
#![allow(dead_code)]

use crate::sync::{IrqSafeMutex, rank};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::registers::{ReadOnly, ReadWrite, WriteOnly};
use tock_registers::{register_bitfields, register_structs};

register_bitfields! {
    u32,

    /// Distributor control.
    GICD_CTLR [
        ENABLE OFFSET(0) NUMBITS(1) [],
    ],

    /// CPU interface control.
    GICC_CTLR [
        ENABLE OFFSET(0) NUMBITS(1) [],
    ],

    /// Interrupt acknowledge: reading this *takes* the interrupt.
    GICC_IAR [
        /// The interrupt we are now servicing. **1023 means spurious**: the GIC changed its
        /// mind between raising the line and us getting here (another core took it, or it was
        /// masked). Read it, do nothing, do NOT signal end-of-interrupt.
        INTID OFFSET(0) NUMBITS(10) [],
    ],
}

register_structs! {
    /// The distributor. One per machine.
    #[allow(non_snake_case)]
    pub Distributor {
        (0x000 => CTLR: ReadWrite<u32, GICD_CTLR::Register>),
        (0x004 => TYPER: ReadOnly<u32>),
        (0x008 => _reserved0),
        /// Set-enable. **One BIT per interrupt**, 32 per register.
        (0x100 => ISENABLER: [ReadWrite<u32>; 32]),
        (0x180 => ICENABLER: [ReadWrite<u32>; 32]),
        (0x200 => _reserved1),
        /// Priority. **One BYTE per interrupt**, 4 per register. Note the different stride
        /// from ISENABLER: a bitmap vs an array of bytes, in the same device. Getting these
        /// two confused is a classic way to enable interrupt 7 when you meant interrupt 224.
        (0x400 => IPRIORITYR: [ReadWrite<u8>; 1024]),
        (0x800 => ITARGETSR: [ReadWrite<u8>; 1024]),
        (0xc00 => _reserved2),
        /// Software Generated Interrupt Register. Writing here makes the GIC raise one of the
        /// 16 SGIs (INTID 0-15) on a chosen set of cores. It is how one core pokes another, and
        /// here it is how a test raises a controllable interrupt with no device attached.
        (0xf00 => SGIR: WriteOnly<u32>),
        (0xf04 => _reserved3),
        (0x1000 => @END),
    }
}

register_structs! {
    /// The per-core CPU interface. Banked: every core sees its own at the same address.
    #[allow(non_snake_case)]
    pub CpuInterface {
        (0x000 => CTLR: ReadWrite<u32, GICC_CTLR::Register>),
        /// Priority mask. An interrupt is delivered only if its priority is **strictly less
        /// than** this. Backwards from what you'd guess: `0xff` lets everything through.
        (0x004 => PMR: ReadWrite<u32>),
        (0x008 => _reserved0),
        /// Reading this ACKNOWLEDGES an interrupt and tells you which one.
        (0x00c => IAR: ReadOnly<u32, GICC_IAR::Register>),
        /// Writing the same INTID back says "I'm done". Until you do, the GIC will not deliver
        /// another interrupt of equal or lower priority.
        (0x010 => EOIR: WriteOnly<u32>),
        (0x014 => @END),
    }
}

/// The lowest priority (highest value) that still gets through. `0xff` = everything.
const PRIORITY_MASK: u32 = 0xff;

/// The priority we give the timer. Anything below PMR is delivered; 0 is the most urgent.
const TIMER_PRIORITY: u8 = 0x80;

/// The GIC reported a spurious interrupt: do nothing, and do NOT signal EOI.
pub const SPURIOUS: u32 = 1023;

pub struct Gic {
    gicd: *mut Distributor,
    gicc: *mut CpuInterface,
}

// SAFETY: MMIO pointers, not Rust-managed memory. The lock provides exclusion.
unsafe impl Send for Gic {}

static GIC: IrqSafeMutex<Option<Gic>> = IrqSafeMutex::new(rank::GIC, None);

impl Gic {
    fn gicd(&self) -> &Distributor {
        // SAFETY: the address came from the device tree and was mapped as device memory.
        unsafe { &*self.gicd }
    }

    fn gicc(&self) -> &CpuInterface {
        // SAFETY: as above.
        unsafe { &*self.gicc }
    }
}

/// Bring the GIC up.
///
/// `gicd` and `gicc` are **virtual** addresses: the caller has mapped both as device memory.
///
/// # Safety
/// The two addresses must name a real GICv2, mapped and exclusively ours.
pub unsafe fn init(gicd: u64, gicc: u64) {
    let gic = Gic {
        gicd: gicd as *mut Distributor,
        gicc: gicc as *mut CpuInterface,
    };

    // Distributor: on. This is machine-wide, so **only the boot core does it** (this function).
    gic.gicd().CTLR.write(GICD_CTLR::ENABLE::SET);

    *GIC.lock() = Some(gic);

    // The boot core's own CPU interface. Every other core brings up its own via `init_this_cpu`.
    init_this_cpu();
}

/// Bring up **this core's** GIC CPU interface. The distributor ([`init`]) is machine-wide and done
/// once; the CPU interface is banked per core (same MMIO address, different hardware behind it), so
/// each core must enable its own. Called by the boot core inside [`init`] and by every secondary as
/// it comes online (DECISIONS.md §11).
pub fn init_this_cpu() {
    let guard = GIC.lock();
    let gic = guard.as_ref().expect("gic::init_this_cpu before gic::init");

    // Let everything through, then turn it on.
    //
    // ORDER: the mask before the enable. The other way round leaves a window where the interface is
    // live with whatever PMR the firmware left behind, which on a cold boot is often 0 — "deliver
    // nothing" — and you spend an afternoon wondering why your timer is silent.
    gic.gicc().PMR.set(PRIORITY_MASK);
    gic.gicc().CTLR.write(GICC_CTLR::ENABLE::SET);
}

/// Enable one interrupt source, and give it a priority.
pub fn enable(intid: u32) {
    let guard = GIC.lock();
    let gic = guard.as_ref().expect("gic::enable before gic::init");

    // ISENABLER is a BITMAP: 32 interrupts per 32-bit register.
    let reg = (intid / 32) as usize;
    let bit = intid % 32;
    gic.gicd().ISENABLER[reg].set(1 << bit);

    // IPRIORITYR is an array of BYTES: one per interrupt. Different stride, same device.
    gic.gicd().IPRIORITYR[intid as usize].set(TIMER_PRIORITY);

    // ITARGETSR: which cores may service it. Bit 0 = core 0.
    //
    // **Only meaningful for SPIs (32+).** PPIs and SGIs are per-core by definition, and the
    // hardware ignores writes here for them. Writing anyway is harmless and would be actively
    // wrong to omit once we have SMP and real devices.
    if intid >= 32 {
        gic.gicd().ITARGETSR[intid as usize].set(1);
    }
}

/// Disable one interrupt source, at the distributor.
///
/// **This is how a userspace driver's interrupt gets masked the moment it fires**, so a
/// level-triggered device that holds its line asserted cannot re-fire in a storm before the
/// driver has had a chance to service it. The driver re-enables it (via its `Irq` capability's
/// `ACK`) once it has quieted the device. See notes/interrupts.md.
pub fn disable(intid: u32) {
    let guard = GIC.lock();
    let gic = guard.as_ref().expect("gic::disable before gic::init");

    // ICENABLER is the mirror of ISENABLER: writing a 1-bit CLEARS the enable. Writing zeros
    // does nothing, which is why enable and disable are two registers instead of one.
    let reg = (intid / 32) as usize;
    let bit = intid % 32;
    gic.gicd().ICENABLER[reg].set(1 << bit);
}

/// Raise a Software Generated Interrupt (INTID 0-15) on core 0.
///
/// A controllable interrupt with no hardware behind it. The whole "an interrupt becomes a
/// message" path can be tested with this: raise the SGI, watch a blocked thread wake.
pub fn send_sgi(intid: u32, target_cpu: usize) {
    debug_assert!(intid < 16, "SGIs are INTID 0-15");
    debug_assert!(target_cpu < 8, "GICv2 SGIR targets at most 8 cores");
    let guard = GIC.lock();
    let gic = guard.as_ref().expect("gic::send_sgi before gic::init");

    // SGIR: bits[3:0] the SGI id, bits[23:16] the target CPU list (bit N = core N). This is how
    // one core pokes another (SMP step 3c): a targeted SGI, delivered to that core's banked CPU
    // interface, where its handler runs.
    gic.gicd().SGIR.set((1 << (16 + target_cpu)) | intid);
}

/// Take the interrupt. **Reading `IAR` is what acknowledges it**, so this has a side effect
/// and must be called exactly once per IRQ.
///
/// Returns [`SPURIOUS`] (1023) if the GIC changed its mind. Do nothing with that, and in
/// particular do **not** call [`end_of_interrupt`]: signalling completion for an interrupt you
/// never took corrupts the GIC's priority stack.
pub fn acknowledge() -> u32 {
    let guard = GIC.lock();
    let gic = guard.as_ref().expect("gic::acknowledge before gic::init");
    gic.gicc().IAR.read(GICC_IAR::INTID)
}

/// "I'm finished with this one."
///
/// Until this is written, the GIC will not deliver another interrupt of **equal or lower**
/// priority. Forget it and the timer fires exactly once and then never again, which looks
/// nothing like "you forgot to write a register".
pub fn end_of_interrupt(intid: u32) {
    let guard = GIC.lock();
    let gic = guard
        .as_ref()
        .expect("gic::end_of_interrupt before gic::init");
    gic.gicc().EOIR.set(intid);
}

pub fn is_initialized() -> bool {
    GIC.lock().is_some()
}
