//! **The net server: smoltcp as a userspace TCP/IP stack over the confined NIC** (milestone 30,
//! piece 3).
//!
//! This is the flagship of the userspace-reuse thesis for networking: the kernel confines the NIC's
//! DMA (pieces 1 and 2), and a real, reused TCP/IP stack (smoltcp, not hand-built) runs it entirely
//! at EL0. The kernel knows nothing about TCP, UDP, or DHCP; it owns only the DMA confinement.
//!
//! Phase A (this file today) proves the integration: bring the NIC up through the `Virtio`
//! capability, drive smoltcp's poll loop over the receive/transmit rings (`vnet`), and run DHCP to
//! completion against QEMU user-mode networking. It reports the acquired address, which the kernel
//! test asserts lands in slirp's 10.0.2.0/24. The capability-shaped socket contract (a `Stack`
//! endpoint, socket ids, per-connection shared frames; DECISIONS §25) layers on top of this loop.
//!
//! # Capability contract
//! - slot 0: the report endpoint (WRITE)
//! - slot 1: the NIC interrupt (WAIT / ACK)
//! - slot 2: the confined `Virtio` transport
//! - slot 3: an untyped budget, for the heap smoltcp allocates against
//! - arg1: the DMA page's physical address

#![no_std]
#![no_main]

extern crate alloc;

use abi::irq;
use alloc::vec;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::socket::dhcpv4;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};
use user_rt::{cntfrq, invoke, now, send};

#[path = "vnet.rs"]
mod vnet;

const REPORT: u64 = 0;
const IRQ: u64 = 1;
const UNTYPED: u64 = 3;

/// The heap smoltcp allocates against (the socket set, per-frame transmit buffers, caches). Capped
/// well under the granted budget so a leak shows up as a fault, not silent growth.
const HEAP_MAX: u64 = 128 * 4096;

#[global_allocator]
static HEAP: user_rt::heap::UntypedHeap = user_rt::heap::UntypedHeap::new();

/// Our MAC. Locally administered; slirp routes DHCP regardless of what it is.
const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

/// smoltcp's clock, from the monotonic counter (`CNTVCT_EL0` / `rdtime`), in milliseconds. u128
/// intermediate so the multiply never overflows for any realistic uptime.
fn instant() -> Instant {
    let ms = (now() as u128 * 1000 / cntfrq() as u128) as i64;
    Instant::from_millis(ms)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, dma_phys: u64, _a2: u64) -> ! {
    HEAP.init(UNTYPED, user_rt::heap::DEFAULT_BASE, HEAP_MAX);

    let mut dev = vnet::VirtioNet::bring_up(dma_phys);

    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(MAC)));
    config.random_seed = now();
    let mut iface = Interface::new(config, &mut dev, instant());

    let mut sockets = SocketSet::new(vec![]);
    let dhcp = dhcpv4::Socket::new();
    let handle = sockets.add(dhcp);

    loop {
        iface.poll(instant(), &mut dev, &mut sockets);

        if let Some(dhcpv4::Event::Configured(cfg)) =
            sockets.get_mut::<dhcpv4::Socket>(handle).poll()
        {
            // Record the lease on the interface, so a later phase's sockets have a source address.
            let octets = cfg.address.address().octets();
            iface.update_ip_addrs(|addrs| {
                let _ = addrs.push(IpCidr::Ipv4(cfg.address));
            });
            // Report the acquired address (big-endian octets as a u32) and rest. The test asserts
            // it lands in slirp's 10.0.2.0/24, which only a real DHCP round trip through smoltcp and
            // the confined NIC can produce.
            send(REPORT, u32::from_be_bytes(octets) as u64, 0, 0);
            loop {
                core::hint::spin_loop();
            }
        }

        // Block until the NIC raises its line again (a receive or transmit completion). The kernel's
        // pending count means a line that already fired makes WAIT return at once. SAFETY: `svc`.
        unsafe {
            invoke(IRQ, irq::WAIT, 0, 0, 0);
        }
        dev.ack_irq();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // A server bug is a dead server: fault, and the kernel reports it. Same shape as every binary.
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("brk #0", options(nostack, nomem))
    };
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("ebreak", options(nostack, nomem))
    };
    loop {
        core::hint::spin_loop();
    }
}
