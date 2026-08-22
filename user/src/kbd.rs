//! **The keyboard driver** (milestone 29's input): a confined userspace virtio-input driver that
//! turns key events into the bytes a terminal understands.
//!
//! ```text
//!   virtio-input ──virtio (PCIe, behind the IOMMU)──► kbd ──the input ring──► compositor
//!    (a keyboard)                                      │    (shared memory)
//!                                                      └──doorbell COMMIT──► "look at the surfaces"
//! ```
//!
//! Its whole authority, and the shape of it is the point:
//!
//! - slot 0, a **report** endpoint: how it tells its spawner it came up;
//! - slot 1, an **`Irq`**: the device's event interrupt;
//! - slot 2, a **`Virtio`**: the confined transport, and the only way it can reach the device;
//! - slot 3, the **doorbell** (WRITE): the same content-free endpoint every compositor client rings;
//! - mapped: its own DMA page, and the **input ring** it shares with the compositor and nobody else.
//!
//! # Why the ring is the authority, and the doorbell is not
//!
//! DECISIONS §33's central idea, from the producing side. This driver's power to inject a keystroke
//! is the **mapping of the input ring**, which no client has. It is not the doorbell: every client
//! holds that, and everything sent on it is content-free, so a client that rang it a thousand times
//! could not type a single character. A keystroke carried in a message word would have been
//! forgeable by any client; a keystroke in a page nobody else maps cannot be forged at all.
//!
//! So this driver is *not* trusted with the compositor's policy and holds no client's endpoint. It
//! cannot choose who receives what it types: focus is the compositor's decision, expressed as which
//! of the input endpoints *it* holds it uses. This program cannot name a client at all.
//!
//! # What it does not do
//!
//! No key repeat of its own (the device sends repeats and [`video_terminal::keymap`] honours them), no LEDs, no
//! layout switching, no mouse or tablet (a `virtio-tablet-pci` presents the same PCI id, which is
//! recorded in `crates/pci` rather than guessed at), and no configuration-space query: it drives the
//! event queue and nothing else. The honest limits are in notes/glyphs.md.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39), among the names recorded there as always
//! right: an abbreviation that is the ordinary name of the thing. It is also why `script/lint`'s
//! `-d` check starts at four characters, since a three-letter abbreviation is not a claim about a
//! process model.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use abi::{irq, virtio};
use user_rt::{call, invoke, send};

/// Capability slots, by convention with `kernel/src/user/keyboard_service.rs`.
const REPORT: u64 = 0;
const IRQ: u64 = 1;
const VIRTIO: u64 = 2;
const DOORBELL: u64 = 3;

/// Where the kernel maps this driver's DMA page and the compositor's input ring.
const DMA_VA: u64 = 0x0000_0000_0090_0000;
const RING_VA: u64 = 0x0000_0000_0082_0000;

// virtio-mmio register offsets. The §18 transport seam speaks this vocabulary on both buses, so a
// driver written against it runs over PCIe and over mmio without knowing which it got.
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

/// The virtio device type this driver will talk to. Checked, not assumed: rung one found the PCI
/// transport synthesizing a device id nobody had verified, and the fix was for a driver to look.
const VIRTIO_ID_INPUT: u32 = 18;

/// The event queue. virtio-input has a second (status) queue for LEDs, which this driver never sets
/// up, so it stays inside the two-queue confinement ceiling DECISIONS §23 records.
const EVENT_Q: u64 = 0;
const QSIZE: u16 = 8;

/// The DMA page's layout: the event queue's rings at the kernel's per-queue offsets, then the event
/// buffers. All inside the one 4 KiB page the kernel granted, so the shadow-ring validator confines
/// every address the device is handed.
const EQ_DESC: u64 = 0x000;
const EQ_AVAIL: u64 = 0x080;
const EQ_USED: u64 = 0x100;
const EVENT_BASE: u64 = 0x400;

/// `struct virtio_input_event { le16 type; le16 code; le32 value; }`. Eight bytes, and the device
/// writes exactly one per buffer.
const EVENT_LEN: u64 = 8;
/// How many events can be in flight. Eight is the queue size; a keyboard produces two or three
/// events per keystroke and this drains on every interrupt, so the ring never has to be deep.
const EVENTS: usize = 8;

/// Bring-up failures, in a `0xDEAD_...` word so a failure names its step instead of hanging.
const E_MAGIC: u64 = 0x01;
const E_DEVICE_ID: u64 = 0x02;
const E_FEATURES: u64 = 0x03;
const E_QUEUE: u64 = 0x04;

fn event_buf(i: usize) -> u64 {
    EVENT_BASE + i as u64 * EVENT_LEN
}

fn r16(off: u64) -> u16 {
    // SAFETY: inside the DMA page the kernel mapped read/write at DMA_VA.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u16) }
}

fn r32(off: u64) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u32) }
}

fn w16(off: u64, v: u16) {
    // SAFETY: as above.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u16, v) }
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

fn write_desc(i: u64, addr: u64, len: u32, flags: u16) {
    let b = EQ_DESC + i * 16;
    // SAFETY: inside the DMA page, at the descriptor table the kernel programmed the queue with.
    unsafe {
        core::ptr::write_volatile((DMA_VA + b) as *mut u64, addr);
        core::ptr::write_volatile((DMA_VA + b + 8) as *mut u32, len);
        core::ptr::write_volatile((DMA_VA + b + 12) as *mut u16, flags);
        core::ptr::write_volatile((DMA_VA + b + 14) as *mut u16, 0);
    }
}

fn die(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    user_rt::exit();
}

/// **Put a byte in the compositor's input ring.**
///
/// The ring is `compositor::proto::ring`: a byte buffer with a head the compositor advances and a tail
/// this driver advances. Writing the bytes *before* the tail is the whole synchronization, and the
/// fence between them is what makes it true on a weakly ordered machine (DECISIONS rule 4).
fn ring_push(tail: &mut u32, byte: u8) {
    use compositor::proto::ring;
    let at = RING_VA + ring::BYTES + (*tail % ring::CAPACITY) as u64;
    // SAFETY: inside the ring frame the kernel mapped read/write into this process and the
    // compositor, and nowhere else.
    unsafe { core::ptr::write_volatile(at as *mut u8, byte) };
    *tail = tail.wrapping_add(1);
}

fn ring_publish(tail: u32) {
    use compositor::proto::ring;
    // The bytes must be visible before the tail that advertises them.
    //
    // PAIR: two readers of one contract. `take_typed` in kernel/src/user/keyboard_service.rs has the
    // matching `fence(SeqCst)`; `drain_input` in user/src/compositor.rs is the half milestone 43's
    // audit found missing (finding 7). The `call(DOORBELL, ...)` this program makes immediately
    // after `ring_publish` orders it against the compositor anyway, because the compositor is
    // blocked in `recv_cap` on that doorbell. See notes/memory-ordering.md.
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    // SAFETY: inside the ring frame.
    unsafe { core::ptr::write_volatile((RING_VA + ring::TAIL) as *mut u32, tail) };
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_arg0: u64, dma_phys: u64, _arg2: u64) -> ! {
    if mr(MAGIC) != 0x7472_6976 {
        die(E_MAGIC);
    }
    // **Check what we are talking to.** Rung one found the PCI transport answering every driver's
    // `DeviceID` read with a hardcoded 2, and it found it because the GPU driver was the first that
    // looked. A keyboard driver that skipped this would happily program a disk's queues.
    if mr(DEVICE_ID) != VIRTIO_ID_INPUT {
        die(E_DEVICE_ID);
    }

    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    mw(DRIVER_FEATURES_SEL, 0);
    mw(DRIVER_FEATURES, 0); // virtio-input has no low-word features
    mw(DEVICE_FEATURES_SEL, 1);
    let dev_hi = mr(DEVICE_FEATURES);
    let mut ack_hi = F_VERSION_1_HI;
    if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
        ack_hi |= F_ACCESS_PLATFORM_HI; // behind an IOMMU, which on this bus it is
    }
    mw(DRIVER_FEATURES_SEL, 1);
    mw(DRIVER_FEATURES, ack_hi);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
    if mr(STATUS) & S_FEATURES_OK == 0 {
        die(E_FEATURES);
    }

    // The kernel programs the queue's ring addresses; this driver never writes a queue address
    // register, which is the §18 seam and the reason the shadow ring can be trusted.
    // SAFETY: `svc`/`ecall`; the kernel validates the capability and the queue index.
    if unsafe { invoke(VIRTIO, virtio::SETUP_QUEUE, QSIZE as u64, EVENT_Q, 0) } != 0 {
        die(E_QUEUE);
    }
    mw(
        STATUS,
        S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
    );

    // **Every buffer is device-writable, and posted before the first key can arrive.** This is the
    // receive direction: the device writes into our memory. It is the direction the DMA validator
    // grew for virtio-net (DECISIONS §23) and the direction where confinement matters most, because
    // an unbounded address here would let a device overwrite whatever it liked.
    let mut avail: u16 = 0;
    for i in 0..EVENTS {
        write_desc(
            i as u64,
            dma_phys + event_buf(i),
            EVENT_LEN as u32,
            VIRTQ_DESC_F_WRITE,
        );
        let slot = (avail % QSIZE) as u64;
        w16(EQ_AVAIL + 4 + slot * 2, i as u16);
        avail = avail.wrapping_add(1);
    }
    barrier();
    w16(EQ_AVAIL + 2, avail);
    barrier();
    // SAFETY: `svc`/`ecall`; the kernel validates the newly published descriptors and copies them
    // into the shadow ring the device actually reads.
    unsafe {
        invoke(VIRTIO, virtio::NOTIFY, EVENT_Q, 0, 0);
    }

    send(REPORT, video_terminal::status::KBD_UP, EVENTS as u64, 0);

    let mut keys = video_terminal::keymap::Keyboard::new();
    let mut seen: u16 = 0; // used-ring index already drained
    let mut tail: u32 = 0; // our end of the compositor's input ring
    loop {
        // SAFETY: `svc`/`ecall`; the kernel validates the `Irq` capability in slot 1.
        unsafe { invoke(IRQ, irq::WAIT, 0, 0, 0) };

        let mut typed = false;
        loop {
            let used_idx = r16(EQ_USED + 2);
            if used_idx == seen {
                break;
            }
            let slot = (seen % QSIZE) as u64;
            // used-ring element: { u32 id; u32 len }.
            let id = r32(EQ_USED + 4 + slot * 8) as usize;
            // **`id` is a 32-bit value the DEVICE wrote** (notes/shared-page-audit.md, finding 6).
            // The IOMMU and `crates/dma_validator` confine where the device may *touch*, not what
            // it may *say*, and the used ring is inside this driver's own DMA page, which the
            // device is entitled to write. Unchecked, `event_buf(id) = 0x400 + id * 8` leaves the
            // one-page region at `id = 462` and reads this process's own memory as a keystroke.
            //
            // Consume and drop a completion naming a buffer we never posted, without re-posting
            // it: a bogus `id` does not say which buffer it was, and a device that lies about its
            // own ring has stopped being a keyboard.
            if id >= EVENTS {
                seen = seen.wrapping_add(1);
                continue;
            }
            let at = event_buf(id);
            let (kind, code, value) = (r16(at), r16(at + 2), r32(at + 4));
            seen = seen.wrapping_add(1);

            if let Some(b) = keys.event(kind, code, value) {
                ring_push(&mut tail, b);
                typed = true;
            }

            // Re-post the buffer. Its descriptor is permanent, so this is one index write.
            let aslot = (avail % QSIZE) as u64;
            w16(EQ_AVAIL + 4 + aslot * 2, id as u16);
            barrier();
            avail = avail.wrapping_add(1);
            w16(EQ_AVAIL + 2, avail);
            barrier();
            // SAFETY: `svc`/`ecall`; as above.
            unsafe {
                invoke(VIRTIO, virtio::NOTIFY, EVENT_Q, 0, 0);
            }
        }

        // Quiet the device, then re-enable the line at the controller. In that order: the disk
        // driver's discipline, and the reason an interrupt does not immediately re-fire.
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: `svc`/`ecall`; re-enable the interrupt the kernel masked when it fired.
        unsafe { invoke(IRQ, irq::ACK, 0, 0, 0) };

        if typed {
            // Publish the tail, then ring. **The doorbell carries nothing**, and that is the design:
            // what was typed is in a page the compositor maps and no client does. This is also the
            // frame that will show the keystroke, because the compositor drains the ring and then
            // rescans every client's control page before it composites (see user/src/display_terminal.rs).
            ring_publish(tail);
            let _ = call(
                DOORBELL,
                compositor::proto::req(compositor::proto::COMMIT, 0),
                0,
            );
        }
    }
}

user_rt::panic_handler!();
