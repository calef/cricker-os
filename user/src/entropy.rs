//! **The entropy service** (milestone 56; DECISIONS §44, notes/entropy.md).
//!
//! The one process that owns a virtio-rng device, and the only thing in the system that can read
//! it. Everything else holds an endpoint that means *"you may obtain randomness"*, which is a
//! strictly smaller authority than *"you may reach the device"*: a client cannot program the
//! queue, cannot see the DMA region the device writes into, and cannot ask the device for anything
//! this service did not ask on its behalf.
//!
//! ```text
//!   virtio-rng ──virtio (mmio or PCIe, §18)──► entropy ──the request endpoint──► clients
//!    (a device)                                  │        (CALL, bytes in the reply)
//!                                                └── its DMA page: nobody else maps it
//! ```
//!
//! Its whole authority is four things placed before it ran:
//!
//! - slot 0, the **request** endpoint (RECV): clients `CALL` here and nothing else;
//! - slot 1, an **`Irq`**: the device's completion interrupt;
//! - slot 2, a **`Virtio`**: the confined transport, and the only way it can reach the device;
//! - slot 3, a **readiness** endpoint (WRITE): one message once the first bytes are in hand;
//! - mapped: one DMA page, which nobody else maps.
//!
//! No initrd, no budget, no filesystem, no network. A compromised entropy service is a machine
//! whose random numbers an attacker chooses, which is exactly as much damage as owning the
//! entropy source should be worth, and it is why the device does not sit inside every program.
//!
//! # It passes the device's bytes through, and computes nothing
//!
//! No pool, no whitening, no mixing, no DRBG. There is no cryptography in this tree yet (that is
//! milestone 56's other half), and without a one-way function every transformation available here
//! is a reversible permutation: it would change the bytes without adding an unpredictability an
//! attacker could not undo, while making the security claim harder to state. So the claim stays
//! one sentence long: **these are the device's bytes**. See DECISIONS §44.
//!
//! What it does keep is a **buffer**, which is a different thing from a pool. One virtio request
//! fetches [`POOL_LEN`] bytes and the service hands them out in order, refilling when they run
//! out. Byte *i* out is byte *i* in, unmodified, handed to exactly one client, and zeroed behind
//! the cursor. That is a cache for round trips, not an entropy transformation, and it turns 32
//! device round trips into one.
//!
//! # When the device gives less than it was asked for
//!
//! virtio-rng is allowed to return fewer bytes than the buffer holds, and the used ring's `len` is
//! where it says so. **QEMU's really does**, which is worth stating as a measurement rather than a
//! spec allowance: the first version of this file passed a short buffer straight through to the
//! client, and the milestone-56 test caught a five-byte reply to an eight-byte request thirty draws
//! in. So the service takes what arrived and asks again for the rest, gathering across the boundary
//! (see [`Pool::take`]). It never pads, never repeats a byte it has already served, and never
//! substitutes a pseudo-random stand-in. If the device produces nothing at all across
//! [`REFILL_TRIES`] attempts, the reply is [`entropy_proto::NO_ENTROPY`] and the caller finds out,
//! because a caller who cannot be given randomness must not be told otherwise (DECISIONS §42).
//!
//! Name: unrecorded. Introduced 2026-07-30 with milestone 56, on the resource-name pattern `clock`
//! also follows.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use abi::{endpoint, irq, virtio};
use entropy_proto as proto;
use user_rt::{exit, invoke, recv_cap, reply, send};

/// Capability slots, by convention with `kernel/src/user/entropy_service.rs`.
const REQ: u64 = 0;
const IRQ: u64 = 1;
const VIRTIO: u64 = 2;
const READY: u64 = 3;

/// Where the kernel maps this service's DMA page. Must match `kernel/src/user/entropy_service.rs`.
const DMA_VA: u64 = 0x0000_0000_0090_0000;

// virtio-mmio register offsets. The §18 transport seam speaks this vocabulary on both buses, so
// this driver runs over PCIe and over mmio without knowing which one it got.
const MAGIC: u64 = 0x000;
const DEVICE_ID: u64 = 0x008;
const DEVICE_FEATURES: u64 = 0x010;
const DEVICE_FEATURES_SEL: u64 = 0x014;
const DRIVER_FEATURES: u64 = 0x020;
const DRIVER_FEATURES_SEL: u64 = 0x024;
const INTERRUPT_STATUS: u64 = 0x060;
const INTERRUPT_ACK: u64 = 0x064;
const STATUS: u64 = 0x070;

const S_ACKNOWLEDGE: u32 = 1;
const S_DRIVER: u32 = 2;
const S_DRIVER_OK: u32 = 4;
const S_FEATURES_OK: u32 = 8;
const F_VERSION_1_HI: u32 = 1; // feature bit 32
const F_ACCESS_PLATFORM_HI: u32 = 1 << 1; // feature bit 33 (behind an IOMMU)

const VIRTQ_DESC_F_WRITE: u16 = 2;

/// The virtio device type this driver will talk to. Checked, not assumed: the display driver found
/// the PCI transport answering every driver's `DeviceID` read with a hardcoded 2, and it found it
/// because it was the first driver that looked.
const VIRTIO_ID_ENTROPY: u32 = 4;

/// virtio-rng has exactly one virtqueue, the request queue. Nothing rides the second slot of the
/// two-queue confinement ceiling here.
const RNG_Q: u64 = 0;
const QSIZE: u16 = 8;

/// The DMA page's layout: the queue's rings at the kernel's per-queue offsets, then the buffer the
/// device fills. All inside the one 4 KiB page the kernel granted, so the shadow-ring validator
/// confines every address the device is handed.
const Q_DESC: u64 = 0x000;
const Q_AVAIL: u64 = 0x080;
const Q_USED: u64 = 0x100;
const POOL_OFF: u64 = 0x400;

/// How many bytes one device request fetches. 256 is 32 clients' worth of [`proto::MAX_BYTES`] per
/// round trip, and it leaves the rest of the page unused rather than crowding the rings.
const POOL_LEN: u64 = 256;

/// How many times to re-ask a device that returned nothing before telling the caller so. A device
/// that has momentarily run dry is worth one more request; a device that never answers is a fact
/// the caller needs, not something to spin on.
const REFILL_TRIES: usize = 4;

/// How many interrupt wakeups to absorb while waiting for one completion. A wakeup can be stale,
/// coalesced, or a previous operator's, so the **used ring**, not the wakeup, is the completion;
/// the bound is what keeps a device that stopped answering from becoming a hang.
const WAIT_WAKEUPS: usize = 64;

/// Bring-up failures, in a `0xDEAD_...` word so a failure names its step instead of hanging.
const E_MAGIC: u64 = 0x01;
const E_DEVICE_ID: u64 = 0x02;
const E_FEATURES: u64 = 0x03;
const E_QUEUE: u64 = 0x04;

fn r8(off: u64) -> u8 {
    // SAFETY: inside the DMA page the kernel mapped read/write at DMA_VA.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u8) }
}

fn w8(off: u64, v: u8) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u8, v) }
}

fn r16(off: u64) -> u16 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u16) }
}

fn w16(off: u64, v: u16) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u16, v) }
}

fn r32(off: u64) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u32) }
}

fn mr(off: u64) -> u32 {
    // SAFETY: `svc`/`ecall`; the kernel validates the `Virtio` capability and the register offset.
    unsafe { invoke(VIRTIO, virtio::READ_REG, off, 0, 0) as u32 }
}

fn mw(off: u64, v: u32) {
    // SAFETY: as above.
    unsafe {
        invoke(VIRTIO, virtio::WRITE_REG, off, v as u64, 0);
    }
}

fn barrier() {
    // The device reads what we wrote, so a store the compiler or the machine reordered past the
    // index that advertises it is a descriptor the device may act on before it is finished.
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a barrier; no memory is accessed.
    unsafe {
        core::arch::asm!("dmb ish", options(nostack, nomem, preserves_flags));
    };
    #[cfg(target_arch = "riscv64")]
    // SAFETY: as above.
    unsafe {
        core::arch::asm!("fence", options(nostack, preserves_flags))
    };
}

fn die(code: u64) -> ! {
    send(READY, 0xDEAD_0000_0000_0000 | code, 0, 0);
    exit();
}

/// Write descriptor 0: the whole buffer, device-**writable**, which is the direction that matters
/// here. This is a receive-only device; nothing this process holds is ever read by it.
fn write_desc(addr: u64, len: u32) {
    // SAFETY: inside the DMA page, at the descriptor table the kernel programmed the queue with.
    unsafe {
        core::ptr::write_volatile((DMA_VA + Q_DESC) as *mut u64, addr);
        core::ptr::write_volatile((DMA_VA + Q_DESC + 8) as *mut u32, len);
        core::ptr::write_volatile((DMA_VA + Q_DESC + 12) as *mut u16, VIRTQ_DESC_F_WRITE);
        core::ptr::write_volatile((DMA_VA + Q_DESC + 14) as *mut u16, 0);
    }
}

/// The service's running state: where the device's bytes are and how many of them are still ours
/// to give. Not a pool: `cursor` only ever moves forward, so no byte is served twice.
struct Pool {
    /// The DMA region's physical base. Descriptors speak physical addresses; a process knows
    /// virtual ones, so the spawner passes this in (rule 2: the driver is told, never told to look).
    dma_phys: u64,
    /// Available-ring index we have published up to.
    avail: u16,
    /// Used-ring index we have drained up to.
    seen: u16,
    /// Next unserved byte's offset within the buffer.
    cursor: u64,
    /// One past the last byte the device wrote. `cursor == filled` means empty.
    filled: u64,
}

impl Pool {
    /// Ask the device for a bufferful and wait for it. Returns how many bytes arrived, which the
    /// spec allows to be fewer than asked for and, on a dry device, zero.
    fn request(&mut self) -> u64 {
        write_desc(self.dma_phys + POOL_OFF, POOL_LEN as u32);
        w16(Q_AVAIL + 4 + (self.avail % QSIZE) as u64 * 2, 0); // ring[idx] = descriptor head 0
        barrier(); // the descriptor must be visible before the index that advertises it
        self.avail = self.avail.wrapping_add(1);
        w16(Q_AVAIL + 2, self.avail);
        barrier(); // and the index before the doorbell

        // SAFETY: `svc`/`ecall`. The kernel walks the descriptor we just published, refuses it if
        // it leaves our region, and only then rings the device.
        if unsafe { invoke(VIRTIO, virtio::NOTIFY, RNG_Q, 0, 0) } < 0 {
            return 0; // the kernel refused our own in-region descriptor: a bug here, not a dry device
        }

        // **The completion is the used ring advancing, not the wakeup**, and this driver looks
        // before it blocks. Two reasons, and the second one is a fact about the board rather than
        // an optimisation:
        //
        //  1. QEMU completes a virtio request synchronously inside `NOTIFY`, so on the machine this
        //     runs on the ring has already moved by the time we get here.
        //  2. **PCI INTx lines are shared four ways on both `virt` boards** (`pci::intx_irq`
        //     swizzles device number modulo 4) while the kernel routes an intid to exactly one
        //     endpoint (`sched::bind_irq`). With five PCI functions attached there is no
        //     unshared line left, so a driver that blocked before looking would be betting on
        //     owning its line. Looking first costs two loads and removes the bet.
        //
        // The wait is still here, and is what a genuinely asynchronous device gets: a wakeup can be
        // stale, coalesced, or a previous operator's, so the loop re-checks the ring each time.
        // Bounded, so a device that stopped answering becomes an honest "no entropy" rather than a
        // hang.
        let mut len = 0;
        for _ in 0..WAIT_WAKEUPS {
            barrier();
            if r16(Q_USED + 2) != self.seen {
                // used-ring element: { u32 id; u32 len }. `len` is the device saying how many
                // bytes it actually wrote, and it is allowed to be short.
                let slot = (self.seen % QSIZE) as u64;
                len = (r32(Q_USED + 4 + slot * 8 + 4) as u64).min(POOL_LEN);
                self.seen = self.seen.wrapping_add(1);
                break;
            }
            // SAFETY: `svc`/`ecall`; the kernel validates the `Irq` capability in slot 1 and blocks
            // us until the device raises its line.
            unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };
            self.quiet();
        }
        // Quiet the device and re-enable the line even on the path that never waited: the device
        // raised its line when it completed and the kernel masked it, and a line left masked is one
        // the *next* holder of a shared intid never hears from. `irq::ACK` is
        // `arch::irq::enable`, which is idempotent, so this is safe when nothing fired.
        self.quiet();
        len
    }

    /// Acknowledge the device's interrupt and re-enable the line at the controller, in that order:
    /// the disk driver's discipline, and the reason an interrupt does not immediately re-fire.
    fn quiet(&self) {
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: `svc`/`ecall`; re-enable the line the kernel masked when it fired.
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };
    }

    /// Refill the buffer, retrying a device that returned nothing. `false` when the device gave us
    /// nothing at all, which is the one case a client is told about.
    fn refill(&mut self) -> bool {
        for _ in 0..REFILL_TRIES {
            let n = self.request();
            if n > 0 {
                self.cursor = 0;
                self.filled = n;
                return true;
            }
        }
        false
    }

    /// Take `n` bytes, as a little-endian word plus a count.
    ///
    /// **The loop is here because the buffer boundary is not the client's problem.** A refill can
    /// land short (QEMU's virtio-rng really does return fewer than the 256 bytes asked for, which
    /// is what turned this from a spec allowance into a fixed bug), so the bytes a client asked for
    /// can straddle two device requests. Gathering across the boundary keeps a short *device* read
    /// invisible, which is the difference between "the device paced us" and "the caller cannot have
    /// what it asked for".
    ///
    /// A count below `n` therefore means one thing only: the device went dry part-way through. It
    /// is never padding. Zero means it was dry from the start.
    fn take(&mut self, n: u64) -> (u64, u64) {
        let mut word = 0u64;
        let mut got = 0;
        while got < n {
            if self.cursor == self.filled && !self.refill() {
                break;
            }
            let run = (n - got).min(self.filled - self.cursor);
            for i in 0..run {
                let at = POOL_OFF + self.cursor + i;
                word |= (r8(at) as u64) << (8 * (got + i));
                // Zero behind the cursor. The cursor alone already guarantees no byte is served
                // twice; this is hygiene, so a byte a client now holds is not also still sitting in
                // a page this long-lived process keeps mapped for the rest of the boot.
                w8(at, 0);
            }
            self.cursor += run;
            got += run;
        }
        (got, word)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_arg0: u64, dma_phys: u64, _arg2: u64) -> ! {
    if mr(MAGIC) != 0x7472_6976 {
        die(E_MAGIC);
    }
    // **Check what we are talking to**, for the reason the keyboard driver records: a transport
    // that answered every `DeviceID` read with a hardcoded number went unnoticed until a driver
    // looked. An entropy service that programmed a disk's queue would be worse than most.
    if mr(DEVICE_ID) != VIRTIO_ID_ENTROPY {
        die(E_DEVICE_ID);
    }

    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    mw(DRIVER_FEATURES_SEL, 0);
    mw(DRIVER_FEATURES, 0); // virtio-rng defines no low-word features at all
    mw(DEVICE_FEATURES_SEL, 1);
    let dev_hi = mr(DEVICE_FEATURES);
    let mut ack_hi = F_VERSION_1_HI;
    if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
        ack_hi |= F_ACCESS_PLATFORM_HI; // behind an IOMMU, which on the PCIe wiring it is
    }
    mw(DRIVER_FEATURES_SEL, 1);
    mw(DRIVER_FEATURES, ack_hi);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
    if mr(STATUS) & S_FEATURES_OK == 0 {
        die(E_FEATURES);
    }

    // The kernel programs the queue's ring addresses; this service never writes a queue-address
    // register, which is the §18 seam and the reason the shadow ring can be trusted.
    // SAFETY: `svc`/`ecall`; the kernel validates the capability and the queue index.
    if unsafe { invoke(VIRTIO, virtio::SETUP_QUEUE, QSIZE as u64, RNG_Q, 0) } != 0 {
        die(E_QUEUE);
    }
    mw(
        STATUS,
        S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
    );

    let mut pool = Pool {
        dma_phys,
        avail: 0,
        seen: 0,
        cursor: 0,
        filled: 0,
    };

    // Fetch the first bufferful before reporting ready, so "the service is up" means "a client
    // that asks will be answered" rather than "the handshake completed". A device that produces
    // nothing is a fact worth learning at boot rather than at the first `CALL`.
    let first = pool.refill();
    send(READY, proto::READY, u64::from(first), pool.filled);

    serve(pool)
}

/// The serve loop: one endpoint, one wait point, forever.
fn serve(mut pool: Pool) -> ! {
    loop {
        let (w0, cap, _) = recv_cap(REQ);
        if cap == endpoint::NO_CAP {
            // A plain SEND on a CALL-only contract. Nobody is waiting for an answer, so there is
            // nothing to do and nothing to report; drop it rather than replying into a slot we do
            // not hold. (The clock service answers the same way for the same reason.)
            continue;
        }
        let (count, word) = match proto::op(w0) {
            proto::GET => pool.take(proto::want(w0)),
            // There is exactly one operation, and the reply's first word is a byte count with no
            // room in it for an error code. So an unknown opcode is answered "you got nothing",
            // which is true. A second operation would be the moment to widen the reply, and that
            // is a contract change rather than a branch added here.
            _ => (proto::NO_ENTROPY, 0),
        };
        reply(cap, count, word);
    }
}

user_rt::panic_handler!();
