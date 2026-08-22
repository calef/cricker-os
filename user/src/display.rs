//! **The display driver: virtio-gpu, at EL0, confined** (milestone 29, the display ladder's rung
//! one).
//!
//! The first pixels this system ever puts in a scanout, put there by an unprivileged process whose
//! whole world is a cspace:
//!
//! - slot 0, a **report** endpoint: how it tells its spawner it came up, and what it flushed;
//! - slot 1, an **`Irq`**: the device's completion interrupt, arriving as a message;
//! - slot 2, a **`Virtio`**: the confined transport. The device's registers are NOT mapped here, so
//!   this process cannot program a queue address or ring a doorbell; the kernel does both, and
//!   validates every descriptor first (notes/dma.md);
//! - slot 3, a **display** endpoint it RECVs on: where clients ask for a flush;
//! - slot 4, an **untyped**: the budget the page tables for its own mappings come out of;
//! - slots 5.., its **DMA region**: [`DMA_FRAMES`] `Frame` capabilities, one per page, which it maps
//!   itself (milestone 108). The region's *physical* base still arrives in `x1`, because descriptors
//!   speak physical addresses and a process only knows virtual ones.
//!
//! **Nine capabilities for one contiguous region** is the shape a `Frame` forces, and it is worth
//! seeing: this driver spends nine of the fourteen usable slots in its cspace naming pages that are
//! adjacent in physics, adjacent in its address space, and covered as a single range by the IOMMU
//! domain the kernel programmed. See notes/frames.md's BUGS.
//!
//! # The thing that draws is not the thing that talks to the hardware
//!
//! This process never draws a pixel. It owns the device and the transport, and it serves a
//! contract ([`gfx_proto`], notes/framebuffer-contract.md) to a client that owns the pixels and no
//! device at all. That split is the whole point of the increment: rung two's compositor takes the
//! client's place unchanged, and the security story is that a hostile client can draw nonsense and
//! nothing more.
//!
//! # The memory story, because a framebuffer does not fit in a page
//!
//! Every other driver here gets one 4 KiB DMA page. A framebuffer does not fit in one, and the
//! tempting shortcut, letting the GPU's backing live outside the DMA region, would put the one
//! device that reads bulk memory outside the confinement everything else is inside. So the region is
//! **larger**, not special: page 0 holds the rings and the control request/response buffers (driver
//! private), and pages 1.. hold the surface, which is the run of frames the client also maps. The
//! kernel registers the whole run as this driver's DMA region, so the shadow-ring validator bounds
//! every descriptor to it and the IOMMU domain covers exactly it. See notes/framebuffer-contract.md.
//!
//! # What this driver does NOT do
//!
//! No fonts, no VT state, no scrollback, no input. Those are later increments and they arrive as
//! *clients* of the contract this serves, not as code in here.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46), replacing `gpud`.
//! Refused `gpud` (the `-d` claim).

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use gfx_proto as gfx;
use user_rt::{exit, invoke, recv_cap, send};

/// Capability slots, by convention with `kernel/src/user/display_service.rs`.
const REPORT: u64 = 0;
const IRQ: u64 = 1;
const VIRTIO: u64 = 2;
const DISPLAY: u64 = 3;
/// The untyped this driver spends on the page tables its DMA region needs.
const BUDGET: u64 = 4;
/// The first of [`DMA_FRAMES`] consecutive slots holding the DMA region, a `Frame` per page.
const DMA_FRAME: u64 = 5;

/// The DMA region, in frames: one for the rings and control buffers, then the surface. Must match
/// `display_service::DMA_FRAMES`.
const DMA_FRAMES: u64 = 1 + gfx::SURFACE_FRAMES as u64;

/// Where this driver puts its DMA region. **Its choice, not the kernel's**: it holds the frames and
/// maps them (milestone 108). The *physical* base still arrives in `a1`, and has to: descriptors
/// speak physical, and a process only knows virtual addresses.
const DMA_VA: u64 = 0x0000_0000_0090_0000;

// --- virtio-mmio v2 register offsets. The PCI transport answers this same vocabulary
// (kernel/src/virtio.rs, the transport seam), so this driver reads like every other one here even
// though a virtio-gpu only ever arrives over PCIe on our boards. ---
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

/// `VIRTIO_F_VERSION_1` (feature bit 32) and `VIRTIO_F_ACCESS_PLATFORM` (bit 33), both in the high
/// feature word. `ACCESS_PLATFORM` is offered only when the device sits behind an IOMMU, and is
/// acked only when offered, exactly as the disk and net drivers do it: our DMA domain is an identity
/// map, so accepting it changes no address we compute, it only tells the device we know we are
/// translated.
const F_VERSION_1_HI: u32 = 1;
const F_ACCESS_PLATFORM_HI: u32 = 1 << 1;

/// The virtio device type a GPU reports. The kernel's PCI transport answers `DEVICE_ID` with the
/// type recovered from the PCI id, so this check means the same thing on either bus; before
/// milestone 29 it would have read 2 here and this line is what found that out.
const VIRTIO_ID_GPU: u32 = 16;

/// The control virtqueue. A virtio-gpu also has a cursor queue (1); this driver has no cursor, so it
/// never sets that queue up, which keeps the confinement's two-queue ceiling untouched.
const CONTROL_Q: u64 = 0;

/// Our ring size, matching the kernel's `QSIZE`.
const QSIZE: usize = 8;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

// --- The DMA region's layout. Page 0 is driver-private; the surface starts at page 1. ---
//
// The ring offsets are the kernel's contract (kernel/src/virtio.rs: queue q's rings sit at
// q * 0x200 + {0x000, 0x080, 0x100}); queue 0 is the only one we use. The request and response
// buffers sit above both queues' ring blocks, the same place the net driver puts its packet buffers.
const OFF_DESC: u64 = 0x000;
const OFF_AVAIL: u64 = 0x080;
const OFF_USED: u64 = 0x100;
/// The control request the device reads. The largest command we send is 56 bytes.
const OFF_REQ: u64 = 0x400;
/// The control response the device writes. `GET_DISPLAY_INFO`'s reply is 408 bytes (a header plus 16
/// scanout descriptions), which is what sizes this and why it is the last thing in page 0.
const OFF_RESP: u64 = 0x600;
/// The surface: page 1 onward, `gfx::SURFACE_FRAMES` frames of it, shared with the client.
const OFF_SURFACE: u64 = 0x1000;

// --- virtio-gpu control commands and responses (virtio spec 5.7). ---
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;

/// A control header is 24 bytes: type, flags, fence id, context id, ring index, padding.
const HDR_LEN: u64 = 24;

/// The one resource this driver creates. Rung two will want several (one per client surface); the
/// contract already routes by endpoint, so that is a driver change and not a contract change.
const RESOURCE_ID: u32 = 1;
/// The scanout we drive. QEMU's virtio-gpu offers one output by default.
const SCANOUT_ID: u32 = 0;

/// The entry role that attacks its own confinement instead of serving (see [`run_backing_escape`]).
/// Must match `ROLE_BACKING_ESCAPE` in `kernel/src/user/display_service.rs`.
const ROLE_BACKING_ESCAPE: u64 = 1;

/// Failure codes reported to the spawner, so a bring-up failure names its step instead of hanging.
/// Each is OR'd into a `0xDEAD_...` word the kernel test prints.
const E_NOT_VIRTIO: u64 = 0x01;
const E_NOT_GPU: u64 = 0x02;
const E_FEATURES: u64 = 0x03;
const E_QUEUE: u64 = 0x04;
const E_DISPLAY_INFO: u64 = 0x05;
const E_CREATE_2D: u64 = 0x06;
const E_ATTACH_BACKING: u64 = 0x07;
const E_SET_SCANOUT: u64 = 0x08;
const E_DISPLAY_TOO_SMALL: u64 = 0x09;
const E_DMA_REFUSED: u64 = 0x0A;
const E_NO_COMPLETION: u64 = 0x0B;

fn mr(off: u64) -> u32 {
    // SAFETY: `svc`/`ecall`; the kernel validates the Virtio capability in slot 2. Register reads
    // are DMA-safe, so any offset is permitted.
    unsafe { invoke(VIRTIO, abi::virtio::READ_REG, off, 0, 0) as u32 }
}

fn mw(off: u64, v: u32) {
    // SAFETY: as above. Only DMA-*safe* registers reach the device; the queue-address and notify
    // registers are refused by the kernel and go through SETUP_QUEUE / NOTIFY instead.
    unsafe { invoke(VIRTIO, abi::virtio::WRITE_REG, off, v as u64, 0) };
}

/// Order our stores to the rings and buffers against the device's reads, and against the kernel's
/// notify. `dmb ish` on aarch64, one full `fence` on RISC-V, the same conservative pair the other
/// drivers and `arch::dma_wmb` use.
fn barrier() {
    // SAFETY: a barrier has no operands and cannot be unsound.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("dmb ish", options(nostack, nomem, preserves_flags));
    };
    // SAFETY: as above.
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence", options(nostack, preserves_flags))
    };
}

fn dma_write<T>(off: u64, val: T) {
    // SAFETY: `off` is inside the mapped DMA region and `T` fits; the region is mapped read/write.
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut T, val) };
}

fn dma_read<T: Copy>(off: u64) -> T {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const T) }
}

/// Report a bring-up failure and stop. Distinct from every success word, so the kernel test prints
/// the step that failed rather than timing out on a silent driver.
fn die(code: u64) -> ! {
    send(REPORT, 0xDEAD_0000_0000_0000 | code, 0, 0);
    // Exit so the kernel reaps this thread instead of leaving it on a run queue forever; a leaked
    // spinner starves later tests (notes/supervision.md).
    exit();
}

/// Write one 16-byte descriptor into queue 0's table: `{ u64 addr; u32 len; u16 flags; u16 next }`.
fn write_desc(i: u64, addr: u64, len: u32, flags: u16, next: u16) {
    let b = OFF_DESC + i * 16;
    dma_write::<u64>(b, addr);
    dma_write::<u32>(b + 8, len);
    dma_write::<u16>(b + 12, flags);
    dma_write::<u16>(b + 14, next);
}

/// Write a control header at `OFF_REQ`: the command type, and zeros for everything else. We use no
/// fences (the used ring is our completion) and no 3D contexts, so every other field is zero.
fn write_hdr(cmd: u32) {
    dma_write::<u32>(OFF_REQ, cmd);
    dma_write::<u32>(OFF_REQ + 4, 0); // flags: no VIRTIO_GPU_FLAG_FENCE
    dma_write::<u64>(OFF_REQ + 8, 0); // fence_id
    dma_write::<u32>(OFF_REQ + 16, 0); // ctx_id
    dma_write::<u32>(OFF_REQ + 20, 0); // ring_idx + padding
}

/// Write a `virtio_gpu_rect` at `OFF_REQ + at`.
fn write_rect(at: u64, x: u32, y: u32, w: u32, h: u32) {
    dma_write::<u32>(OFF_REQ + at, x);
    dma_write::<u32>(OFF_REQ + at + 4, y);
    dma_write::<u32>(OFF_REQ + at + 8, w);
    dma_write::<u32>(OFF_REQ + at + 12, h);
}

/// **Submit the control command sitting at `OFF_REQ` and wait for the device's reply.** Returns the
/// response type the device wrote into `OFF_RESP`.
///
/// Two descriptors: the request, which the device reads, and the response buffer, which it writes.
/// Both are inside our DMA region, so the kernel's validator passes them; a bug that aimed either
/// outside would be refused before the device was ever rung, which is why `E_DMA_REFUSED` is a
/// driver bug and not a device error.
fn submit(dma_phys: u64, req_len: u32) -> u32 {
    // Zero the response type first: a stale OK from the previous command must not be readable as
    // this command's answer.
    dma_write::<u32>(OFF_RESP, 0);

    write_desc(0, dma_phys + OFF_REQ, req_len, VIRTQ_DESC_F_NEXT, 1);
    write_desc(1, dma_phys + OFF_RESP, 512, VIRTQ_DESC_F_WRITE, 0);

    let used_before: u16 = dma_read::<u16>(OFF_USED + 2);
    let idx: u16 = dma_read::<u16>(OFF_AVAIL + 2);
    dma_write::<u16>(OFF_AVAIL + 4 + (idx as u64 % QSIZE as u64) * 2, 0); // ring[idx] = head 0
    barrier(); // the descriptors and the ring slot must be visible before idx moves
    dma_write::<u16>(OFF_AVAIL + 2, idx.wrapping_add(1));
    barrier(); // idx must be visible before the kernel rings the device

    // SAFETY: `svc`/`ecall`. The kernel validates both descriptors against our DMA region, copies
    // them into the shadow ring the device actually reads, and only then rings the doorbell.
    if unsafe { invoke(VIRTIO, abi::virtio::NOTIFY, CONTROL_Q, 0, 0) } < 0 {
        die(E_DMA_REFUSED);
    }

    // **The completion is the used ring advancing, not the wakeup.** The device's interrupt line is
    // shared across every driver that ever holds this device, and a wakeup can be spurious,
    // coalesced, or a previous operator's leftover, so the used index is what decides (notes/dma.md,
    // the kill-mid-write section). The bound turns "no completion ever" into a legible failure
    // instead of a hang the watchdog has to catch.
    for _ in 0..64 {
        // SAFETY: `svc`/`ecall`; blocks until the device raises its line.
        unsafe { invoke(IRQ, abi::irq::WAIT, 0, 0, 0) };
        let istatus = mr(INTERRUPT_STATUS);
        mw(INTERRUPT_ACK, istatus);
        // SAFETY: `svc`/`ecall`; re-enable the line the kernel masked when it fired.
        unsafe { invoke(IRQ, abi::irq::ACK, 0, 0, 0) };
        barrier();
        if dma_read::<u16>(OFF_USED + 2) != used_before {
            return dma_read::<u32>(OFF_RESP);
        }
    }
    die(E_NO_COMPLETION)
}

/// The virtio handshake, then queue 0. The queue is set up **through the kernel**, which programs
/// the ring addresses to fixed offsets in our region; we never choose them, which is what keeps the
/// device inside our grant.
fn init(dma_phys: u64) -> (u32, u32) {
    if mr(MAGIC) != 0x7472_6976 {
        die(E_NOT_VIRTIO); // not "virt": we are not talking to a virtio device at all
    }
    if mr(DEVICE_ID) != VIRTIO_ID_GPU {
        die(E_NOT_GPU); // the kernel handed us something that is not a GPU
    }

    mw(STATUS, 0);
    mw(STATUS, S_ACKNOWLEDGE);
    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER);

    // No low-word gpu features: not VIRGL (no 3D, that is milestone 34), not EDID, not
    // RESOURCE_BLOB. A 2D scanout out of guest pages is the whole of rung one.
    mw(DRIVER_FEATURES_SEL, 0);
    mw(DRIVER_FEATURES, 0);

    mw(DEVICE_FEATURES_SEL, 1);
    let dev_hi = mr(DEVICE_FEATURES);
    let mut ack_hi = F_VERSION_1_HI;
    if dev_hi & F_ACCESS_PLATFORM_HI != 0 {
        ack_hi |= F_ACCESS_PLATFORM_HI; // required when the device is behind the IOMMU
    }
    mw(DRIVER_FEATURES_SEL, 1);
    mw(DRIVER_FEATURES, ack_hi);

    mw(STATUS, S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK);
    if mr(STATUS) & S_FEATURES_OK == 0 {
        die(E_FEATURES);
    }

    // SAFETY: `svc`/`ecall`; the kernel places queue 0's rings at the contract's offsets.
    if unsafe { invoke(VIRTIO, abi::virtio::SETUP_QUEUE, QSIZE as u64, CONTROL_Q, 0) } != 0 {
        die(E_QUEUE);
    }

    mw(
        STATUS,
        S_ACKNOWLEDGE | S_DRIVER | S_FEATURES_OK | S_DRIVER_OK,
    );

    // Ask the device how big the display is. We do not have to obey it (SET_SCANOUT is bounded by
    // the resource, not the display), but a surface larger than the panel means the scanout would be
    // clipped somewhere we cannot see, so refuse loudly rather than draw into the dark.
    write_hdr(CMD_GET_DISPLAY_INFO);
    if submit(dma_phys, HDR_LEN as u32) != RESP_OK_DISPLAY_INFO {
        die(E_DISPLAY_INFO);
    }
    // The reply is a header then 16 `virtio_gpu_display_one`, each a rect plus enabled and flags.
    // Scanout 0's rect starts right after the header.
    let dw: u32 = dma_read::<u32>(OFF_RESP + HDR_LEN + 8);
    let dh: u32 = dma_read::<u32>(OFF_RESP + HDR_LEN + 12);
    if dw < gfx::WIDTH || dh < gfx::HEIGHT {
        die(E_DISPLAY_TOO_SMALL);
    }
    (dw, dh)
}

/// Create the 2D resource, point it at the surface frames, and put it on the scanout. After this the
/// device holds a mapping of exactly the frames the client writes, and nothing else.
fn bring_up_surface(dma_phys: u64) {
    // RESOURCE_CREATE_2D: a host-side image of our geometry and format.
    write_hdr(CMD_RESOURCE_CREATE_2D);
    dma_write::<u32>(OFF_REQ + HDR_LEN, RESOURCE_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 4, gfx::FORMAT);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 8, gfx::WIDTH);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 12, gfx::HEIGHT);
    if submit(dma_phys, (HDR_LEN + 16) as u32) != RESP_OK_NODATA {
        die(E_CREATE_2D);
    }

    // RESOURCE_ATTACH_BACKING: the guest memory the device reads pixels out of. **One entry**,
    // because the surface is a contiguous run of frames inside our DMA region, which is what makes
    // the IOMMU domain cover it: the domain is exactly this region plus the kernel's shadow page, so
    // an entry pointing anywhere else would fail to translate and the device would refuse the
    // command. See notes/framebuffer-contract.md.
    write_hdr(CMD_RESOURCE_ATTACH_BACKING);
    dma_write::<u32>(OFF_REQ + HDR_LEN, RESOURCE_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 4, 1); // nr_entries
    dma_write::<u64>(OFF_REQ + HDR_LEN + 8, dma_phys + OFF_SURFACE); // entry addr
    dma_write::<u32>(OFF_REQ + HDR_LEN + 16, gfx::SURFACE_BYTES); // entry length
    dma_write::<u32>(OFF_REQ + HDR_LEN + 20, 0); // entry padding
    if submit(dma_phys, (HDR_LEN + 24) as u32) != RESP_OK_NODATA {
        die(E_ATTACH_BACKING);
    }

    // SET_SCANOUT: this resource, whole, is what the display shows.
    write_hdr(CMD_SET_SCANOUT);
    write_rect(HDR_LEN, 0, 0, gfx::WIDTH, gfx::HEIGHT);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 16, SCANOUT_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 20, RESOURCE_ID);
    if submit(dma_phys, (HDR_LEN + 24) as u32) != RESP_OK_NODATA {
        die(E_SET_SCANOUT);
    }
}

/// Serve one flush: copy the damaged rectangle from the surface into the host resource, then put
/// that resource on the screen. Returns 0, or the contract's `EINVAL` for a rectangle that is not
/// wholly inside the surface.
///
/// Two device commands, in this order and for this reason: `TRANSFER_TO_HOST_2D` is what actually
/// reads our pixels (the device DMAs them out of the backing), and `RESOURCE_FLUSH` is what makes
/// the host show the result. A driver that only flushed would put stale pixels on the screen.
fn flush(dma_phys: u64, x: u32, y: u32, w: u32, h: u32) -> i64 {
    if !gfx::rect_in_surface(x, y, w, h) {
        return gfx::EINVAL;
    }

    write_hdr(CMD_TRANSFER_TO_HOST_2D);
    write_rect(HDR_LEN, x, y, w, h);
    // The offset of the rectangle's first pixel in the backing. The device walks rows of `stride`
    // from here, so this is the one place the surface's stride reaches the device.
    dma_write::<u64>(OFF_REQ + HDR_LEN + 16, gfx::offset_of(x, y) as u64);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 24, RESOURCE_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 28, 0); // padding
    if submit(dma_phys, (HDR_LEN + 32) as u32) != RESP_OK_NODATA {
        return gfx::EINVAL;
    }

    write_hdr(CMD_RESOURCE_FLUSH);
    write_rect(HDR_LEN, x, y, w, h);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 16, RESOURCE_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 20, 0); // padding
    if submit(dma_phys, (HDR_LEN + 24) as u32) != RESP_OK_NODATA {
        return gfx::EINVAL;
    }
    0
}

/// Read pixel `i` of the surface, through our own mapping of the shared frames.
fn surface_pixel(i: usize) -> u32 {
    dma_read::<u32>(OFF_SURFACE + (i * 4) as u64)
}

/// **The escape attempt.** Bring the device up honestly, then ask it to read pixels out of memory
/// this driver was never granted: a `RESOURCE_ATTACH_BACKING` naming `victim`, a frame the kernel
/// allocated and deliberately left out of this device's IOMMU domain. Then transfer from it, so the
/// device really tries to move those bytes. Reports the device's response code and stops.
///
/// This is the GPU's own confinement question, and it is not the disk's. Everywhere else, the
/// addresses a device will touch arrive in virtqueue descriptors, which the kernel validates and
/// copies into a shadow ring the driver cannot reach (notes/dma.md). A virtio-gpu's backing addresses
/// arrive in a **device-level command payload** instead. The kernel bounds the descriptor carrying
/// that command, but the addresses inside it are bytes it does not parse, and it should not start
/// parsing them: that would put device knowledge in the transport, which is the line §18 draws.
///
/// So the IOMMU is the barrier here, and the kernel-side test is what checks it held, by draining the
/// IOMMU's fault queue. **The device's response code is not the evidence**, and finding that out was
/// the surprise: QEMU's DMA layer answers a translation failure with a bounce buffer rather than a
/// failed mapping, so the command comes back OK while the bytes the device gets are not the victim's.
/// The fault the IOMMU recorded is the fact; the response code is reported only so the test can say
/// what happened. See notes/framebuffer-contract.md.
///
/// `victim` comes from the kernel rather than being computed here, for the same reason the milestone
/// 16b confinement test picks its own escape frame: the test must know the exact address to look for
/// in the fault queue. An earlier version guessed at "the frame just past my region" and that was a
/// bad guess, because the allocator's next frame is not reliably out of the domain (the kernel's
/// shadow page is allocated right after the region).
fn run_backing_escape(dma_phys: u64, victim: u64) -> ! {
    init(dma_phys);

    write_hdr(CMD_RESOURCE_CREATE_2D);
    dma_write::<u32>(OFF_REQ + HDR_LEN, RESOURCE_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 4, gfx::FORMAT);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 8, gfx::WIDTH);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 12, gfx::HEIGHT);
    if submit(dma_phys, (HDR_LEN + 16) as u32) != RESP_OK_NODATA {
        die(E_CREATE_2D);
    }

    // Point the resource's backing at the victim frame: memory outside our grant, which a hostile
    // display driver would love to read (another process's pixels, a key, a filesystem block) by
    // having the GPU copy it into a scanout. Attaching is itself the access that must be refused: the
    // device maps the backing here, and mapping an untranslatable address is what the IOMMU faults on.
    //
    // **One pixel's worth, not a whole frame, and the reason is a real limit worth knowing.** The
    // RISC-V IOMMU's fault queue holds 128 records and the driver does not clear the queue's overflow
    // bit, so a flood of faults latches the overflow and **silently stops fault reporting for
    // everything after it**. A 4096-byte backing produced exactly that flood and broke the *next*
    // test's fault assertion (milestone 16b's `the_iommu_faults_a_dma_that_escapes_the_domain`, which
    // then saw no faults at all and reported the IOMMU as absent). Four bytes is one translation, one
    // fault, and the same security question: can this device reach an address outside its grant?
    // Recorded in notes/framebuffer-contract.md, because the overflow behaviour will matter more to
    // whoever routes faults to a production handler than it does here.
    write_hdr(CMD_RESOURCE_ATTACH_BACKING);
    dma_write::<u32>(OFF_REQ + HDR_LEN, RESOURCE_ID);
    dma_write::<u32>(OFF_REQ + HDR_LEN + 4, 1); // nr_entries
    dma_write::<u64>(OFF_REQ + HDR_LEN + 8, victim); // OUTSIDE the grant
    dma_write::<u32>(OFF_REQ + HDR_LEN + 16, 4); // one pixel: see above
    dma_write::<u32>(OFF_REQ + HDR_LEN + 20, 0);
    let resp = submit(dma_phys, (HDR_LEN + 24) as u32);

    // Report what the device said about the attach. The kernel test decides whether the barrier held,
    // from the IOMMU's fault queue; this role does not get to grade its own attack.
    send(REPORT, gfx::status::BACKING, resp as u64, 0);
    exit();
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, dma_phys: u64, arg2: u64) -> ! {
    // **The DMA region is ours to place** (milestone 108): DMA_FRAMES capabilities, one per page,
    // mapped read/write out of our own budget. Before either role, because the rings live in the
    // first page of it and the escape attempt writes a descriptor too.
    //
    // One `MAP` per page is what a `Frame` costs: the object names a page, and this region is a
    // contiguous run of nine. The kernel had to reserve slots 5..13 of a sixteen-slot cspace to
    // hand it over. See notes/frames.md's BUGS.
    for k in 0..DMA_FRAMES {
        if !user_rt::map_frame(DMA_FRAME + k, DMA_VA + k * 4096, true, BUDGET) {
            // Nothing to report: `gfx::status` has no code for "I never reached my own rings",
            // and inventing one would be a protocol change to say what the missing `UP` already
            // says. A spawner that never sees `UP` knows bring-up failed.
            exit();
        }
    }

    // Two roles in one binary, for the reason the blk attackers ride with the blk driver: the attack
    // differs from the honest driver by one field, and sharing the bring-up is what makes it a fair
    // test rather than a different program that fails for its own reasons.
    if role == ROLE_BACKING_ESCAPE {
        run_backing_escape(dma_phys, arg2);
    }

    let (dw, dh) = init(dma_phys);
    bring_up_surface(dma_phys);

    // The display is up: resource created, backed, and on the scanout. A spawner that never sees
    // this knows bring-up failed rather than drawing.
    send(
        REPORT,
        gfx::status::UP,
        gfx::WIDTH as u64 | ((gfx::HEIGHT as u64) << 32),
        dw as u64 | ((dh as u64) << 32),
    );

    // Serve the contract. One client today; the loop answers whoever holds the WRITE half of the
    // display endpoint, through the kernel-minted one-shot Reply, so it never names its caller.
    let mut reported_flush = false;
    loop {
        // The opcode AND the damage rectangle both ride in the first word (`gfx::req` packs the
        // rectangle into its low 56 bits), so a flush is one message with no second word to keep in
        // step. `recv_cap`'s third value is the caller's second word, which this contract does not
        // use; reading the rectangle out of it instead of out of `w0` was the first bug this test
        // caught, and it presented as a flush refused for an empty rectangle.
        let (w0, reply_slot, _) = recv_cap(DISPLAY);
        let r0: i64 = match gfx::op(w0) {
            gfx::display::INFO => {
                // The runtime half of the geometry contract. The reply's second word carries it, so
                // a client need not have been compiled against our constants.
                0
            }
            gfx::display::FLUSH => {
                let (x, y, w, h) = gfx::unrect(gfx::operand(w0));
                flush(dma_phys, x, y, w, h)
            }
            _ => gfx::EINVAL,
        };
        let r1 = match gfx::op(w0) {
            gfx::display::INFO => gfx::WIDTH as u64 | ((gfx::HEIGHT as u64) << 32),
            _ => 0,
        };
        // SAFETY: `svc`/`ecall`; the kernel validated the Reply capability and consumes it here.
        unsafe { invoke(reply_slot, abi::reply::REPLY, r0 as u64, r1, 0) };

        // **The driver-side witness**, once, after the first successful flush. The digest is taken
        // in this address space, from our own mapping of the surface, after the device reported the
        // transfer complete: a second independent account of what reached the hardware, from a
        // different process than the one that wrote the pixels. Status, not contract; rung two can
        // ignore it (notes/framebuffer-contract.md).
        if !reported_flush && gfx::op(w0) == gfx::display::FLUSH && r0 == 0 {
            reported_flush = true;
            let digest = gfx::checksum(surface_pixel);
            send(REPORT, gfx::status::FLUSHED, digest, gfx::PIXELS as u64);
        }
    }
}

user_rt::panic_handler!();
