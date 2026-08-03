use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

/// The roles of the `ntp` binary. Must match user/src/ntp.rs.
pub const ROLE_CLIENT: u64 = 0;
pub const ROLE_SERVER: u64 = 1;
pub const ROLE_PROBE_CLOCK: u64 = 2;

/// The words the `ntp` binary reports. Must match user/src/ntp.rs.
///
/// The whole vocabulary is here even though the tests assert on five of the eight: the point of
/// a mirror is that it is complete, and a failing test that prints `3` is only readable if `3`
/// has a name on this side too.
#[allow(dead_code)]
pub mod rpt {
    pub const SYNCED: u64 = 1;
    pub const REJECTED: u64 = 2;
    pub const NO_REPLY: u64 = 3;
    pub const NO_ENTROPY: u64 = 4;
    pub const NET_ERROR: u64 = 5;
    pub const PROBING: u64 = 6;
    pub const SERVED: u64 = 7;
    pub const BAD_LOCAL_TIME: u64 = 8;
}

/// Which reply the test server sends. Must match `user/src/ntp.rs`'s `srv`.
pub mod srv {
    pub const GOOD: u64 = 0;
    pub const BAD_ORIGIN: u64 = 1;
    pub const KISS_OF_DEATH: u64 = 2;
    pub const SHORT: u64 = 3;
}

/// The small integers the client reports a `ntp_proto::Reject` as. Must match
/// `user/src/ntp.rs`'s `reject_code`; kept as distinct values rather than a bool because which
/// check refused a packet is the difference between a broken server and an attack.
pub mod reject {
    pub const LENGTH: u64 = 1;
    pub const KISS_OF_DEATH: u64 = 4;
    pub const ORIGIN_MISMATCH: u64 = 7;
}

/// How many requests the client makes before giving up. Must match `ATTEMPTS` in user/src/ntp.rs.
pub const ATTEMPTS: u64 = 3;

/// Each role mints or maps exactly one shared frame and pays for its page tables. Small and
/// fixed: neither role links a heap.
const BUDGET_PAGES: u64 = 16;
/// Extra stack pages. The client builds a 48-byte packet and does IPC; the same three `socket_test_client`
/// and its relatives get is plenty and leaves no doubt.
const STACK_PAGES: u64 = 3;

/// A running test server: the endpoint a client speaks the socket contract on, and the endpoint
/// it reports the first request it saw on.
pub struct Server {
    /// The socket contract's endpoint. The server holds `READ`; a client is given `WRITE`.
    pub stack: EpId,
    /// **Drain this before waiting on the client.** The server reports once, with a blocking
    /// `send`, and the client's `RECV` is queued behind it.
    pub report: EpId,
}

/// Extra stack for a spawned role, allocated and zeroed. Returned by value so the spawn closure
/// owns it.
fn stack_pages() -> [Mapping; STACK_PAGES as usize] {
    let mut maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; STACK_PAGES as usize];
    for (k, m) in maps.iter_mut().enumerate() {
        let phys = crate::memory::alloc()
            .expect("no frame for an ntp role's stack")
            .addr();
        // SAFETY: a fresh frame through the direct map; zero it so the process starts clean.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
        m.phys = phys;
    }
    maps
}

/// Spawn the test server: `variant` selects the reply it builds, `claimed_nanos` is the
/// wall-clock time it claims. The kernel supplies the claimed time because the server holds no
/// clock capability of its own, which keeps every test's expectation an exact number rather
/// than a window.
pub fn start_server(image: &'static [u8], variant: u64, claimed_nanos: u64) -> Server {
    let stack = crate::sched::create_endpoint();
    let report = crate::sched::create_endpoint();
    let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for the ntp server");
    let maps = stack_pages();

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_SERVER,
                arg1: variant,
                arg2: claimed_nanos,
                grants: &[
                    endpoint_cap(report, Rights::WRITE), // slot 0: the one report
                    endpoint_cap(stack, Rights::READ),   // slot 1: serve the socket contract
                    untyped_cap(budget),                 // slot 2: map the client's frame
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the ntp test server");

    Server { stack, report }
}

/// **The client's endowment, in one place.** `entropy` is an `Option` on purpose: a client wired
/// without it is the test that proves the refusal is loud, and the empty slot is what "no
/// entropy capability" actually is.
///
/// `role` picks the client proper or the clock-page probe, which is given **the same five
/// slots** so that what the probe fails to reach, it fails to reach as a fully endowed NTP
/// client rather than as a stripped-down one.
fn spawn_role(
    image: &'static [u8],
    role: u64,
    a1: u64,
    a2: u64,
    stack: EpId,
    propose: EpId,
    entropy: Option<EpId>,
) -> EpId {
    let report = crate::sched::create_endpoint();
    let budget = crate::untyped::create(BUDGET_PAGES).expect("no untyped for the ntp client");
    let maps = stack_pages();
    // Slot 4 is granted or it is not; there is no third state and no flag inside it. An
    // ungranted slot answers a `CALL` with `NoSuchSlot`, which is how the client tells "there
    // is no entropy service" from "the service has none".
    let n_grants = if entropy.is_some() { 5 } else { 4 };
    let entropy = entropy.unwrap_or(0);

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: role,
                arg1: a1,
                arg2: a2,
                grants: &[
                    endpoint_cap(report, Rights::WRITE),  // slot 0: the verdict
                    endpoint_cap(stack, Rights::WRITE),   // slot 1: the network
                    untyped_cap(budget),                  // slot 2: the shared frame
                    endpoint_cap(propose, Rights::WRITE), // slot 3: ask, never tell
                    endpoint_cap(entropy, Rights::WRITE), // slot 4: the nonce
                ][..n_grants],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the ntp client");

    report
}

/// Spawn the NTP client against `stack`, told which server to ask (`ip` packed big-endian,
/// `port`). Returns its report endpoint.
pub fn start_client(
    image: &'static [u8],
    stack: EpId,
    propose: EpId,
    entropy: Option<EpId>,
    ip: u32,
    port: u16,
) -> EpId {
    spawn_role(
        image,
        ROLE_CLIENT,
        ip as u64,
        port as u64,
        stack,
        propose,
        entropy,
    )
}

/// Spawn the clock-page probe: the client's endowment, pointed at `va`.
pub fn start_probe(
    image: &'static [u8],
    stack: EpId,
    propose: EpId,
    entropy: Option<EpId>,
    va: u64,
) -> EpId {
    spawn_role(image, ROLE_PROBE_CLOCK, va, 0, stack, propose, entropy)
}
