use super::*;
use crate::cap::{Rights, endpoint_cap, irq_cap, virtio_cap};
use crate::sched::EpId;

/// Where the driver expects its DMA page. Must match user/src/virtio.rs. The device registers
/// are NOT mapped to the driver any more: it drives the device through a `Virtio` capability,
/// so it cannot point the device outside this DMA region.
const DMA_VA: u64 = 0x0000_0000_0090_0000;

const ROLE_VIRTIO_BLK: u64 = 3;
/// The write-path roles (milestone 32 phase 1); must match user/src/hello.rs and blk.rs.
const ROLE_VIRTIO_BLK_WRITE: u64 = 30;
const ROLE_VIRTIO_BLK_WRITE_ABANDON: u64 = 31;
/// The virtio-net driver role (milestone 30); must match user/src/hello.rs and blk.rs.
const ROLE_VIRTIO_NET: u64 = 40;

/// Start a driver role against a discovered transport. The shared body of [`start`] (mmio),
/// [`start_pci`] (PCIe), and the writer starters: everything from here on, the DMA region,
/// the Irq routing, the confined `Virtio` capability, the spawn, is bus-agnostic, which is
/// the transport seam doing its job, and role-agnostic, because every role gets the same
/// world and differs only in what it does with it. `rid` is the PCIe requester id the IOMMU
/// keys its tables on; mmio devices have no IOMMU in front of them and pass `None`.
fn wire(
    image: &'static [u8],
    transport: crate::virtio::Transport,
    intid: u32,
    role: u64,
    rid: Option<u32>,
) -> EpId {
    // A DMA page: physical memory the device can reach, mapped into the driver, whose
    // physical address the driver must know (a process sees only virtual addresses). We hand
    // that physical address over in `arg1`.
    let dma = crate::memory::alloc()
        .expect("no DMA frame for the virtio driver")
        .addr();
    // SAFETY: fresh frame, reachable through the direct map. Zero it so stale RAM cannot look
    // like a valid descriptor to the device before the driver writes the real ones.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
    }

    // Route the device's interrupt to an endpoint and enable it, so the driver's `WAIT` on
    // its Irq capability will receive it. See milestone 9a.
    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(intid, irq_ep);
    crate::arch::irq::enable(intid);

    // Where the driver reports the bytes it read.
    let report = crate::sched::create_endpoint();

    // Register the device's transport with the kernel: the kernel owns the registers and the
    // DMA-critical operations, and confines the device to this DMA region. The driver gets a
    // `Virtio` capability, not the registers.
    let vid = crate::virtio::register(transport, dma, FRAME_SIZE, rid);

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: role,
                arg1: dma, // the DMA region's PHYSICAL address (still needed to build requests)
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE), // slot 0: SEND the result
                    irq_cap(intid),                      // slot 1: WAIT / ACK the interrupt
                    virtio_cap(vid),                     // slot 2: drive the device, confined
                ],
                maps: &[Mapping {
                    va: DMA_VA,
                    phys: dma,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the virtio driver");

    report
}

/// Start the driver against the virtio-mmio disk. Returns the endpoint it will report its
/// result on, or `None` if there is no disk attached to enumerate.
pub fn start(image: &'static [u8]) -> Option<EpId> {
    start_role(image, ROLE_VIRTIO_BLK)
}

/// Start the SAME driver binary against the PCIe disk (the PCIe transport, DECISIONS §18):
/// enumeration and bring-up by kernel/src/pci.rs, INTx through the interrupt controller, the
/// identical confinement, now backed by the IOMMU when the machine has one (§20). The driver
/// cannot tell which bus it is on, and that is the point.
pub fn start_pci(image: &'static [u8]) -> Option<EpId> {
    start_role_pci(image, ROLE_VIRTIO_BLK)
}

/// Start the write-path driver (milestone 32 phase 1) against the mmio disk: it writes a
/// pattern to the scratch block, reads it back, re-checks the filesystem around it, and
/// reports the read-back bytes.
pub fn start_writer(image: &'static [u8]) -> Option<EpId> {
    start_role(image, ROLE_VIRTIO_BLK_WRITE)
}

/// The same writer over the PCIe transport.
pub fn start_writer_pci(image: &'static [u8]) -> Option<EpId> {
    start_role_pci(image, ROLE_VIRTIO_BLK_WRITE)
}

/// Start the abandoning writer (the kill-mid-write case): it submits a validated write and
/// dies on purpose before collecting the completion. Reports 1 after the submit, so the test
/// knows the request genuinely left before the death.
pub fn start_write_abandoner(image: &'static [u8]) -> Option<EpId> {
    start_role(image, ROLE_VIRTIO_BLK_WRITE_ABANDON)
}

/// Start the virtio-net driver against the mmio NIC (milestone 30). It does a DHCP round trip
/// over QEMU user-mode networking and reports the offered address. `None` if no NIC is attached.
/// The same `wire` machinery as the disk: the DMA confinement now polices two queues, and the
/// driver drives receive and transmit through the one `Virtio` capability.
pub fn start_net(image: &'static [u8]) -> Option<EpId> {
    let dev = crate::virtio::find_net_device()?;
    Some(wire(
        image,
        crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        },
        dev.intid,
        ROLE_VIRTIO_NET,
        None, // virtio-mmio has no IOMMU in front of it on either board
    ))
}

/// The same net driver over the PCIe transport, behind the IOMMU (milestone 30, §20): the NIC
/// is confined in hardware to its DMA region and shadow page, `iommu_platform=on`, exactly the
/// disk's pattern. The driver cannot tell which bus it is on.
pub fn start_net_pci(image: &'static [u8]) -> Option<EpId> {
    let d = crate::pci::find_net_device()?;
    Some(wire(
        image,
        crate::virtio::Transport::pci(&d),
        d.intid,
        ROLE_VIRTIO_NET,
        Some(d.rid),
    ))
}

/// The heap budget the net server (smoltcp) draws from, in pages: the socket set, per-frame
/// transmit buffers, and caches, plus the program's own page tables. `net_stack` caps its heap at 128
/// pages, so 192 leaves headroom without being unbounded.
const NET_SERVER_BUDGET_PAGES: u64 = 192;
/// smoltcp builds packets on the stack; one mapped stack page is not enough. Eight extra keeps
/// the poll loop clear (`allocator_exerciser` needed three for `alloc` collections; smoltcp asks more).
const NET_SERVER_STACK_PAGES: u64 = 8;

/// Start the **net server** (milestone 30, piece 3): the `net_stack` binary, which runs smoltcp over
/// the confined NIC and does DHCP. Like [`wire`] it hands the confined `Virtio` capability, the
/// interrupt, a DMA page, and a report endpoint; unlike it, the server also gets an **untyped
/// budget** (slot 3) for the heap smoltcp allocates against, and extra stack pages. Returns the
/// endpoint the server reports its acquired address on, or `None` if no NIC is attached.
pub fn start_net_server(image: &'static [u8]) -> Option<EpId> {
    let dev = crate::virtio::find_net_device()?;
    Some(
        wire_net_server(
            image,
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dev.intid,
            None,
        )
        .0,
    )
}

/// The net server over the PCIe transport, behind the IOMMU (§20).
pub fn start_net_server_pci(image: &'static [u8]) -> Option<EpId> {
    let d = crate::pci::find_net_device()?;
    Some(
        wire_net_server(
            image,
            crate::virtio::Transport::pci(&d),
            d.intid,
            Some(d.rid),
        )
        .0,
    )
}

/// [`wire`] for the net server: the same confined transport, interrupt, DMA page, and report
/// endpoint, plus an untyped budget for the heap, extra stack pages, and a **`Stack` endpoint**
/// (slot 4) where clients' socket-contract requests arrive. `net_stack` is its own binary (loaded by
/// name), so no role selector is passed. Returns `(report endpoint, stack endpoint)`; a caller
/// that has no client (the phase-A DHCP tests) ignores the stack, and `net_stack` simply blocks on it.
fn wire_net_server(
    image: &'static [u8],
    transport: crate::virtio::Transport,
    intid: u32,
    rid: Option<u32>,
) -> (EpId, EpId) {
    use crate::cap::untyped_cap;

    let dma = crate::memory::alloc()
        .expect("no DMA frame for the net server")
        .addr();
    // SAFETY: fresh frame via the direct map; zero it so stale RAM cannot look like a valid
    // descriptor to the device before the driver writes the real ones.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
    }

    let irq_ep = crate::sched::create_endpoint();
    crate::sched::bind_irq(intid, irq_ep);
    crate::arch::irq::enable(intid);

    let report = crate::sched::create_endpoint();
    let stack = crate::sched::create_endpoint();
    let vid = crate::virtio::register(transport, dma, FRAME_SIZE, rid);
    let budget = crate::untyped::create(NET_SERVER_BUDGET_PAGES).expect("no untyped for net_stack");

    // The DMA mapping plus the extra stack pages, in one array the spawn closure owns.
    let mut maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; NET_SERVER_STACK_PAGES as usize + 1];
    maps[0] = Mapping {
        va: DMA_VA,
        phys: dma,
        flags: Flags::user_data(),
    };
    for k in 0..NET_SERVER_STACK_PAGES as usize {
        let phys = crate::memory::alloc()
            .expect("no frame for the net server stack")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so the process starts clean.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        maps[k + 1] = Mapping {
            va: USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE,
            phys,
            flags: Flags::user_data(),
        };
    }

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 0, // net_stack is its own binary; no role selector
                arg1: dma,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE), // slot 0: report the acquired address
                    irq_cap(intid),                      // slot 1: WAIT / ACK the interrupt
                    virtio_cap(vid),                     // slot 2: the confined transport
                    untyped_cap(budget),                 // slot 3: the heap's budget
                    endpoint_cap(stack, Rights::READ),   // slot 4: serve clients' requests
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the net server");

    (report, stack)
}

/// The client's budget, in pages: it mints one shared frame and pays for that frame's own page
/// table plus its mapping; small and fixed.
const NET_CLIENT_BUDGET_PAGES: u64 = 16;

/// **Spawn the net server and a client of its socket contract** (milestone 30, piece 3 phase B).
/// Both are the `net_stack` binary (`image`): the server is entry role 0, the client is a nonzero
/// role (the client rides in the same binary to keep the initrd under its 15-file directory
/// limit). They share a `Stack` endpoint: `net_stack` holds `READ` (it serves), the client holds
/// `WRITE` (it requests). The client also gets its own untyped (to mint and delegate the shared
/// frame) and a report endpoint. `cli_arg` selects which exchange the client drives (UDP DNS or
/// TCP echo). Returns the client's report endpoint, or `None` if no NIC is attached.
pub fn start_net_stack(image: &'static [u8], cli_arg: u64, pci: bool) -> Option<EpId> {
    use crate::cap::untyped_cap;

    let (transport, intid, rid) = if pci {
        let d = crate::pci::find_net_device()?;
        (crate::virtio::Transport::pci(&d), d.intid, Some(d.rid))
    } else {
        let dev = crate::virtio::find_net_device()?;
        (
            crate::virtio::Transport::Mmio {
                mmio_phys: dev.mmio_phys,
            },
            dev.intid,
            None,
        )
    };

    let (net_stack_report, stack) = wire_net_server(image, transport, intid, rid);

    // The client: WRITE on the shared stack endpoint, its own untyped, a report endpoint. Two
    // extra stack pages cover its DNS-query building and IPC; it links no heap.
    let cli_report = crate::sched::create_endpoint();
    let cli_budget =
        crate::untyped::create(NET_CLIENT_BUDGET_PAGES).expect("no untyped for the net client");
    let mut cli_stack = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; 2];
    for (k, m) in cli_stack.iter_mut().enumerate() {
        let phys = crate::memory::alloc()
            .expect("no frame for the net client stack")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so the process starts clean.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
        m.phys = phys;
    }

    crate::sched::spawn(move || {
        run(
            image, // the net_stack binary again; a nonzero entry role runs its client half
            Spawn {
                arg0: cli_arg,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(cli_report, Rights::WRITE), // slot 0: report the verdict
                    // slot 1: the stack endpoint, WRITE to send requests and to delegate the
                    // shared frame onto it (the frame it mints already carries GRANT).
                    endpoint_cap(stack, Rights::WRITE),
                    untyped_cap(cli_budget), // slot 2: mint and map the shared frame
                ],
                maps: &cli_stack,
            },
        )
    })
    .expect("could not spawn the net client");

    // net_stack reports its DHCP lease with a blocking `send`; drain it here so net_stack unblocks and
    // enters its serve loop (the client's first request blocks until it does). This also
    // confirms DHCP completed before the client's exchange runs. The client, spawned above,
    // waits at its first request meanwhile.
    crate::sched::ipc_recv(net_stack_report);

    Some(cli_report)
}

/// The networked std client's heap budget and extra stack, both larger than the hand-written
/// client's: it is a full std program (formatting, `Vec`, `String`), so it needs the same
/// generous heap and stack the `std_exerciser` demo does.
const STD_NET_HEAP_PAGES: u64 = 256;
const STD_NET_STACK_PAGES: u64 = 32;

/// **Spawn the net server and a `std::net` client** (milestone 27 phase two): the same
/// `std_exerciser` std binary, but now given the network, so its `UdpSocket::bind` probe succeeds
/// and it drives a real UDP DNS query and a TCP echo round trip through `std::net`, whose PAL
/// binds to this same `net_stack` socket contract. `net_stack` is `net_stack_image` (entry role 0, holding the
/// NIC and `READ` on the `Stack` endpoint); `std_image` is the ordinary std ELF given the std
/// slot convention (heap untyped at 0, stdout at 1) plus the two net slots (the `Stack`
/// endpoint `WRITE` at 2, an untyped budget for its per-socket shared frames at 3). Over the
/// mmio transport; the PAL sits above the transport, so proving it on one is proving it. The
/// same binary spawned without slots 2 and 3 runs the offline transcript instead, which is what
/// makes "no ambient network" visible: authority, not the code, decides. Returns the program's
/// stdout endpoint for the test to reassemble, or `None` if no NIC is attached.
pub fn start_net_std(net_stack_image: &'static [u8], std_image: &'static [u8]) -> Option<EpId> {
    use crate::cap::untyped_cap;

    let dev = crate::virtio::find_net_device()?;
    let transport = crate::virtio::Transport::Mmio {
        mmio_phys: dev.mmio_phys,
    };
    let (net_stack_report, stack) = wire_net_server(net_stack_image, transport, dev.intid, None);

    let report = crate::sched::create_endpoint();
    let heap = crate::untyped::create(STD_NET_HEAP_PAGES).expect("no untyped for the std net heap");
    let frames =
        crate::untyped::create(NET_CLIENT_BUDGET_PAGES).expect("no untyped for the std net frames");

    let mut stackmaps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; STD_NET_STACK_PAGES as usize];
    for (k, m) in stackmaps.iter_mut().enumerate() {
        let phys = crate::memory::alloc()
            .expect("no frame for the std net stack")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so the process starts clean.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
        m.phys = phys;
    }

    crate::sched::spawn(move || {
        run(
            std_image,
            Spawn {
                arg0: 0,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(heap),                   // slot 0: the heap's budget
                    endpoint_cap(report, Rights::WRITE), // slot 1: stdout/stderr
                    endpoint_cap(stack, Rights::WRITE),  // slot 2: the Stack endpoint
                    untyped_cap(frames),                 // slot 3: mint per-socket shared frames
                ],
                maps: &stackmaps,
            },
        )
    })
    .expect("could not spawn the networked std program");

    // Same discipline as start_net_stack: drain net_stack's blocking DHCP report so it reaches its
    // serve loop before the std program's first request, and confirm DHCP completed.
    crate::sched::ipc_recv(net_stack_report);

    Some(report)
}

/// [`wire`] against the enumerated mmio disk, at `role`.
fn start_role(image: &'static [u8], role: u64) -> Option<EpId> {
    let dev = crate::virtio::find_block_device()?;
    Some(wire(
        image,
        crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        },
        dev.intid,
        role,
        None, // virtio-mmio has no IOMMU in front of it on either board
    ))
}

/// [`wire`] against the enumerated PCIe disk, at `role`.
fn start_role_pci(image: &'static [u8], role: u64) -> Option<EpId> {
    let d = crate::pci::find_block_device()?;
    Some(wire(
        image,
        crate::virtio::Transport::pci(&d),
        d.intid,
        role,
        Some(d.rid), // the PCIe requester id, the IOMMU keys its tables on it
    ))
}

const ROLE_VIRTIO_ATTACK: u64 = 8;

/// Spawn a MALICIOUS driver that tries to DMA over kernel memory, for the security test. It
/// holds a real `Virtio` capability and its own DMA region, and points a descriptor at the
/// kernel image. Returns the endpoint on which it reports whether the kernel refused it.
pub fn start_attacker(image: &'static [u8]) -> Option<EpId> {
    let dev = crate::virtio::find_block_device()?;
    let dma = crate::memory::alloc().expect("no DMA frame").addr();
    // SAFETY: fresh frame via the direct map.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
    }
    let vid = crate::virtio::register(
        crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        },
        dma,
        FRAME_SIZE,
        None,
    );
    let report = crate::sched::create_endpoint();

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_VIRTIO_ATTACK,
                arg1: dma,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE),
                    irq_cap(dev.intid), // slot 1 (unused by the attacker; keeps virtio at slot 2)
                    virtio_cap(vid),    // slot 2
                ],
                maps: &[Mapping {
                    va: DMA_VA,
                    phys: dma,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the virtio attacker");

    Some(report)
}

const ROLE_VIRTIO_ATTACK_INDIRECT: u64 = 13;

/// Spawn a malicious driver that tries the **indirect-descriptor** escape: it negotiates
/// `INDIRECT_DESC` and submits one descriptor flagged indirect whose inner table aims the
/// device at the kernel image. Same wiring as [`start_attacker`], different role. The kernel
/// strips the feature and refuses the flag, so the attacker reports `1` (refused). Returns the
/// report endpoint.
pub fn start_attacker_indirect(image: &'static [u8]) -> Option<EpId> {
    let dev = crate::virtio::find_block_device()?;
    let dma = crate::memory::alloc().expect("no DMA frame").addr();
    // SAFETY: fresh frame via the direct map.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(dma) as *mut u8, 0, FRAME_SIZE as usize);
    }
    let vid = crate::virtio::register(
        crate::virtio::Transport::Mmio {
            mmio_phys: dev.mmio_phys,
        },
        dma,
        FRAME_SIZE,
        None,
    );
    let report = crate::sched::create_endpoint();

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_VIRTIO_ATTACK_INDIRECT,
                arg1: dma,
                arg2: 0,
                grants: &[
                    endpoint_cap(report, Rights::WRITE),
                    irq_cap(dev.intid), // slot 1 (unused; keeps virtio at slot 2)
                    virtio_cap(vid),    // slot 2
                ],
                maps: &[Mapping {
                    va: DMA_VA,
                    phys: dma,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn the indirect virtio attacker");

    Some(report)
}
