use entropy_service::Bus;

use super::*;

/// Reach the service on `bus`, wiring it if this is the first test to ask, and check its
/// startup report when this call is the one that wired it.
fn start(bus: Bus) -> entropy_service::Wiring {
    let image = program("entropy").expect("no entropy program in the initrd archive");
    let w = entropy_service::ensure(image, bus).unwrap_or_else(|| {
        panic!(
            "no virtio-rng device on the {bus:?} bus: is NIFE_RNG missing from the test leg, \
             or the -device virtio-rng line from the runner?"
        )
    });
    if let Some(report) = w.wait_for_ready() {
        assert_eq!(
            report[0],
            entropy_proto::READY,
            "the entropy service did not come up on {bus:?} (it reported {:#x}; a 0xDEAD_.. \
             word's low byte names the step, see user/src/entropy.rs)",
            report[0],
        );
        assert_eq!(
            report[1], 1,
            "the entropy service came up on {bus:?} but the device gave it no bytes at all",
        );
    }
    w
}

/// Draw `WORDS` eight-byte words through the request endpoint, asserting every draw is full.
/// Deliberately more than one bufferful (the service fetches 256 bytes per device request), so
/// this crosses the refill boundary and a cursor that wrapped instead of refilling shows up as
/// a repeat below.
const WORDS: usize = 64;

fn draw(w: &entropy_service::Wiring) -> [u64; WORDS] {
    let mut words = [0u64; WORDS];
    for (i, slot) in words.iter_mut().enumerate() {
        let mut buf = [0u8; 8];
        let n = w.get(8, &mut buf);
        assert_eq!(
            n, 8,
            "draw {i} of {WORDS} on {:?} returned {n} bytes, not 8: the device ran dry, or the \
             service failed to refill",
            w.bus,
        );
        *slot = u64::from_le_bytes(buf);
    }
    words
}

/// Every word distinct, and none of them zero. With a real source a collision among 64 draws is
/// a 2^-58 event, so a failure here is a bug rather than bad luck: a stuck device, a buffer
/// served twice, or a used ring the driver never re-read all present exactly this way.
fn assert_unpredictable(words: &[u64; WORDS], what: &str) {
    for (i, &a) in words.iter().enumerate() {
        assert_ne!(
            a, 0,
            "{what}: draw {i} is all zeros, which is the DMA page unwritten"
        );
        for (j, &b) in words.iter().enumerate().skip(i + 1) {
            assert_ne!(a, b, "{what}: draws {i} and {j} are identical ({a:#018x})");
        }
    }
}

/// **The headline, over virtio-mmio.** A client that holds one endpoint and no device gets
/// bytes off a real random-number generator, 512 of them, across a refill, all different.
#[test_case]
fn a_client_obtains_unpredictable_bytes_from_a_virtio_rng_over_mmio() {
    let w = start(Bus::Mmio);
    let words = draw(&w);
    assert_unpredictable(&words, "mmio");
}

/// **The same service, the same binary, over PCIe** (DECISIONS §18), and behind the IOMMU while
/// it is there. An entropy source's buffer is the one page in memory whose contents must not be
/// guessable, so an unconfined device writing it is worth asserting against rather than hoping.
#[test_case]
fn a_client_obtains_unpredictable_bytes_from_a_virtio_rng_over_pcie() {
    let w = start(Bus::Pci);
    assert!(
        w.confined_by_iommu,
        "the PCIe RNG is present but not behind the IOMMU: the buffer the device writes the \
         system's key material into is unconfined (is iommu_platform=on missing from the \
         runner's virtio-rng-pci line?)",
    );
    let words = draw(&w);
    assert_unpredictable(&words, "pcie");
}

/// **Two independent sources do not agree**, which is what says the bytes came from the devices
/// rather than from anything shared underneath them (a fixed seed, a counter, the DMA page's
/// previous contents). Also the cheapest proof that two services can hold two devices at once.
#[test_case]
fn two_entropy_services_on_two_devices_do_not_produce_the_same_bytes() {
    let mmio = start(Bus::Mmio);
    let pci = start(Bus::Pci);
    let a = draw(&mmio);
    let b = draw(&pci);
    for (i, (&x, &y)) in a.iter().zip(b.iter()).enumerate() {
        assert_ne!(
            x, y,
            "draw {i} is identical on both devices ({x:#018x}): these are not two sources",
        );
    }
}

/// **The count in a reply is the truth about the reply.** A short request gets exactly that
/// many bytes and leaves the rest of the caller's buffer alone, so a caller cannot be handed
/// padding it mistakes for entropy; an oversized one is clamped and answered rather than
/// refused; and an opcode the service does not implement is answered with nothing rather than
/// killing the service, which the draw afterwards proves.
#[test_case]
fn a_reply_never_delivers_more_bytes_than_it_says() {
    let w = start(Bus::Mmio);

    let mut buf = [0xAAu8; 8];
    assert_eq!(w.get(3, &mut buf), 3, "asked for three bytes");
    assert_eq!(
        &buf[3..],
        &[0xAA; 5],
        "the service wrote past the count it reported",
    );

    let mut big = [0u8; 8];
    assert_eq!(
        w.get(200, &mut big),
        entropy_proto::MAX_BYTES as usize,
        "an oversized request should be clamped and answered, not refused",
    );

    let r = crate::sched::ipc_call(w.request, [entropy_proto::req(0xff, 8), 0]);
    assert_eq!(
        r[0],
        entropy_proto::NO_ENTROPY,
        "an unknown opcode should be answered with no bytes",
    );

    let mut after = [0u8; 8];
    assert_eq!(
        w.get(8, &mut after),
        8,
        "the service stopped serving after an unknown opcode",
    );
}
