use super::*;
use crate::cap::{Rights, endpoint_cap};
use crate::sched::EpId;

/// The PL011's physical address on QEMU `virt`. The kernel maps it for its own debug output;
/// here we hand a *second* mapping of the same registers to the userspace server. On real
/// hardware you would give the server exclusive ownership; in QEMU both mappings are fine,
/// and the kernel's is now used only for panics and boot, not for anyone's `print`.
const PL011_PHYS: u64 = 0x0900_0000;

/// Printing-client role (`x0`), matching user/src/hello.rs. (The server is its own binary now,
/// 19f.3, so it has no role; only the demo client is still a role of hello.)
const ROLE_CLIENT: u64 = 2;

/// What a client needs to talk to the console server: two endpoints and the shared page.
#[derive(Clone, Copy)]
pub struct Console {
    pub request: EpId,
    pub reply: EpId,
    pub shared_phys: u64,
}

/// Spawn the console server as a user process and return a handle for wiring up clients.
///
/// The server holds: `RECV` on `request` (slot 0), `SEND` on `reply` (slot 1), the shared
/// page mapped **read-only** (it only reads what clients wrote), and the **UART's registers**
/// mapped as user device memory. That last mapping is the whole milestone: a driver, at EL0,
/// holding its hardware.
pub fn start() -> Console {
    // The console server is its own binary now (19f.3), loaded from the archive by name rather
    // than entered as a role of hello.
    let image = program("console").expect("no console program in the initrd");
    let request = crate::sched::create_endpoint();
    let reply = crate::sched::create_endpoint();
    let shared_phys = crate::memory::alloc()
        .expect("no frame for the shared console buffer")
        .addr();

    // Zero the shared page so a client's first print cannot leak stale RAM.
    // SAFETY: freshly allocated, reachable through the direct map, owned by nobody yet.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(shared_phys) as *mut u8,
            0,
            FRAME_SIZE as usize,
        );
    }

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 0, // no role selector: the console is its own binary
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(request, Rights::READ), // slot 0: RECV requests
                    endpoint_cap(reply, Rights::WRITE),  // slot 1: SEND acks
                ],
                maps: &[
                    Mapping {
                        va: SHARED_VA,
                        phys: shared_phys,
                        flags: Flags::user_rodata(),
                    },
                    Mapping {
                        va: UART_VA,
                        phys: PL011_PHYS,
                        flags: Flags::user_device(),
                    },
                ],
            },
        )
    })
    .expect("could not spawn the console server");

    Console {
        request,
        reply,
        shared_phys,
    }
}

/// Spawn a client wired to `console`: `SEND` on request (slot 0), `RECV` on reply (slot 1),
/// and the shared page mapped **read/write** (it writes the text it wants printed).
pub fn spawn_client(image: &'static [u8], console: Console) {
    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_CLIENT,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(console.request, Rights::WRITE), // slot 0: SEND
                    endpoint_cap(console.reply, Rights::READ),    // slot 1: RECV ack
                ],
                maps: &[Mapping {
                    va: SHARED_VA,
                    phys: console.shared_phys,
                    flags: Flags::user_data(),
                }],
            },
        )
    })
    .expect("could not spawn a console client");
}

/// The user VAs the client and server agree on. Kept here so the kernel and the binary have
/// one source of truth; they must match user/src/hello.rs.
const SHARED_VA: u64 = 0x0000_0000_0060_0000;
const UART_VA: u64 = 0x0000_0000_0070_0000;
