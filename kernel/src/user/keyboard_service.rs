use super::*;
use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
use crate::sched::EpId;

/// Where the driver maps its DMA page and the input ring. Must match user/src/kbd.rs.
const DMA_VA: u64 = 0x0000_0000_0090_0000;
const RING_VA: u64 = 0x0000_0000_0082_0000;

/// One page, like every other driver here except the display's. A keyboard's event queue is
/// eight eight-byte records; there is nothing bulk about it, so the standing rule holds in the
/// other direction too: **a device gets the grant it needs and no more.**
const DMA_FRAMES: u64 = 1;

pub struct Wiring {
    /// The driver's status endpoint.
    pub report: EpId,
    /// The doorbell the driver rings. The kernel holds READ here, playing the compositor.
    pub doorbell: EpId,
    /// The input ring's frame, so the kernel can read what was typed.
    pub ring: u64,
    head: u32,
}

/// **Wire and spawn the keyboard driver.** `None` if no virtio-input function is on the bus.
///
/// The kernel keeps the doorbell's receiving half and the ring, so it can stand in for the
/// compositor; a real system hands both to `compositor` instead and nothing about this driver
/// changes, which is the same swap rung two made at the display seam.
pub fn start(image: &'static [u8]) -> Option<Wiring> {
    let d = crate::pci::find_input_device()?;

    let dma = crate::memory::alloc_contiguous(DMA_FRAMES as usize)
        .expect("no DMA region for the keyboard driver")
        .addr();
    // SAFETY: a fresh frame, direct-mapped, owned by nobody else. Zeroed so no stale descriptor
    // and no stale event is ever visible to the device or to us.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(dma) as *mut u8,
            0,
            (DMA_FRAMES * FRAME_SIZE) as usize,
        );
    }
    let ring = crate::memory::alloc()
        .expect("no frame for the input ring")
        .addr();
    // SAFETY: as above.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(ring) as *mut u8, 0, FRAME_SIZE as usize);
    };

    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(d.intid, irq_ep);
    crate::arch::irq::enable(d.intid);

    let vid = crate::virtio::register(
        crate::virtio::Transport::pci(&d),
        dma,
        DMA_FRAMES * FRAME_SIZE,
        Some(d.rid),
    );

    let report = crate::sched::create_endpoint();
    let doorbell = crate::sched::create_endpoint();

    let maps = [
        Mapping {
            va: DMA_VA,
            phys: dma,
            flags: Flags::user_data(),
        },
        Mapping {
            va: RING_VA,
            phys: ring,
            flags: Flags::user_data(),
        },
    ];
    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 0,
                arg1: dma, // the DMA region's PHYSICAL base: descriptors speak physical
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE),   // slot 0: status
                    irq_cap(d.intid),                      // slot 1: the event interrupt
                    virtio_cap(vid),                       // slot 2: the confined transport
                    endpoint_cap(doorbell, Rights::WRITE), // slot 3: ring the compositor
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the keyboard driver");

    Some(Wiring {
        report,
        doorbell,
        ring,
        head: 0,
    })
}

impl Wiring {
    /// **Take what the driver has typed into the ring**, advancing the head the way a compositor
    /// does. Returns how many bytes landed in `out`.
    pub fn take_typed(&mut self, out: &mut [u8]) -> usize {
        use compositor::proto::ring;
        let base = mmu::phys_to_virt(self.ring);
        // SAFETY: inside the ring frame this kernel allocated and shares with the driver.
        let tail = unsafe { core::ptr::read_volatile((base + ring::TAIL) as *const u32) };
        // The tail is published after the bytes it advertises; read it before them.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let mut n = 0;
        while self.head != tail && n < out.len() {
            let at = base + ring::BYTES + (self.head % ring::CAPACITY) as u64;
            // SAFETY: inside the ring frame.
            out[n] = unsafe { core::ptr::read_volatile(at as *const u8) };
            self.head = self.head.wrapping_add(1);
            n += 1;
        }
        // SAFETY: inside the ring frame; the head is ours to advance.
        unsafe { core::ptr::write_volatile((base + ring::HEAD) as *mut u32, self.head) };
        n
    }

    /// Answer the driver's `COMMIT`, the way a compositor would after compositing.
    pub fn answer_doorbell(&self) {
        let m = crate::sched::ipc_recv_cap(self.doorbell);
        let crate::cap::Object::Reply(caller) = crate::sched::current_cap(m[1])
            .expect("the keyboard driver's ring was not a CALL")
            .object
        else {
            panic!("the keyboard driver rang without a reply capability");
        };
        assert_eq!(
            compositor::proto::op(m[0]),
            compositor::proto::COMMIT,
            "the keyboard driver rang with something other than COMMIT",
        );
        crate::sched::ipc_reply(caller, [0, 0]);
        crate::sched::delete_current_cap(m[1]).expect("consume the one-shot reply");
    }
}
