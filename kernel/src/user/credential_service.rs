use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

/// Where the service maps the provisioner's page. Must match user/src/credentialer.rs.
const PROV_VA: u64 = 0x0000_0000_00e0_0000;
/// Where the service and a client map the verify page. Must match both programs.
const VERIFY_VA: u64 = 0x0000_0000_00e1_0000;

/// The service's untyped budget, in pages: 6 MiB. It pays for one thing, the Argon2id scratch,
/// which is `cred::Cost::DEFAULT.blocks()` KiB (4 MiB today), plus the page tables that map it
/// and the allocator's slack. Sized from the cost parameter rather than guessed, so raising the
/// cost is a change in two places that fail loudly together rather than one that fails at run
/// time under load.
const CRED_BUDGET_PAGES: u64 = 1536;

/// Extra stack pages for the service, and this is a **measured** number rather than a cautious
/// one: with the default single page the service takes a data abort at `0x4fff00`, 256 bytes
/// below the stack, on its first derivation. Argon2's inner loop copies whole 1 KiB `Block`s
/// through locals (`block_r`, `block_tmp` in `fill_block`), so one page is not close to enough
/// and no amount of care in *our* code would have made it enough. The net server takes 8 pages
/// for smoltcp for the same kind of reason; this takes 16, because a KDF that overflows its
/// stack fails as a killed process on every login rather than as a bad answer, and the pages
/// are cheap next to the 4 MiB the scratch already costs.
const CRED_STACK_PAGES: u64 = 16;

/// The clients' budgets. A `credentialer_test_client` role holds no untyped at all: it maps one page the wiring
/// placed and calls one endpoint. There is nothing for it to build.
///
/// This constant does not exist. The absence is the point, and it is written down because a
/// reader looking for "what memory does the attacker get" should find the answer rather than
/// conclude it was overlooked.
const _NO_CLIENT_BUDGET: () = ();

/// The `credentialer_test_client` roles; must match `user/src/credentialer_test_client.rs`.
pub const ROLE_HONEST: u64 = 0;
pub const ROLE_ATTACKER: u64 = 1;
pub const ROLE_PROVISIONER: u64 = 2;
pub const ROLE_NTLM: u64 = 3;

/// The flag bits a client packs into its report's third word; must match the same file.
pub const F_CLEAN: u64 = 1 << 0;
pub const F_SESSION_KEY: u64 = 1 << 1;
pub const F_NO_KEY_ON_REFUSAL: u64 = 1 << 2;

/// The report words `credentialer_test_client` and the service send, likewise.
pub const RPT_DONE: u64 = 0x_c2ed_c11e_0000_0001;
pub const RPT_READY: u64 = 0x_c2ed_0000_0000_0001;

/// A running credential service and the endpoints that reach it.
pub struct Wiring {
    /// The service's readiness endpoint. It reports **after** the seal, so receiving on this is
    /// also how a caller knows provisioning is over.
    pub ready: EpId,
    /// The verify endpoint. This is what a client is given, with WRITE.
    pub verify: EpId,
    /// The provision endpoint. Held here only so the provisioner can be spawned against it;
    /// after the seal, the service has deleted its receive end and a `CALL` here would block
    /// forever, which is why nothing sends on it afterwards.
    pub provision: EpId,
}

/// **Wire and spawn the credential service.** It blocks on its provision endpoint immediately,
/// so nothing happens until [`provisioner`] runs.
///
/// `entropy` is the entropy service's request endpoint (DECISIONS §44). It is not optional: a
/// credential service that cannot draw a salt refuses to start, and passing it a dead endpoint
/// is how that path gets tested.
pub fn start(image: &'static [u8], entropy: EpId) -> Wiring {
    let provision = crate::sched::create_endpoint();
    let verify = crate::sched::create_endpoint();
    let ready = crate::sched::create_endpoint();
    let budget =
        crate::untyped::create(CRED_BUDGET_PAGES).expect("no untyped for the credential store");

    // The two shared pages, then the extra stack, in one array the spawn closure owns.
    let mut maps = [Mapping {
        va: 0,
        phys: 0,
        flags: Flags::user_data(),
    }; CRED_STACK_PAGES as usize + 2];
    maps[0] = Mapping {
        va: PROV_VA,
        phys: frame(),
        flags: Flags::user_data(),
    };
    maps[1] = Mapping {
        va: VERIFY_VA,
        phys: frame(),
        flags: Flags::user_data(),
    };
    for k in 0..CRED_STACK_PAGES as usize {
        let phys = crate::memory::alloc()
            .expect("no frame for the credential service's stack")
            .addr();
        // SAFETY: fresh frame via the direct map; zero it so the process starts clean, which
        // for this process also means it does not start with somebody else's bytes where its
        // key material will go.
        unsafe {
            core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
        }
        maps[k + 2] = Mapping {
            va: USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE,
            phys,
            flags: Flags::user_data(),
        };
    }
    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: 0,
                arg1: 0, // no physical address: this process touches no device
                arg2: 0,
                grants: &[
                    endpoint_cap(provision, Rights::READ), // slot 0: write the store, until SEAL
                    endpoint_cap(verify, Rights::READ),    // slot 1: answer questions, forever
                    endpoint_cap(entropy, Rights::WRITE),  // slot 2: salts, naming no device
                    untyped_cap(budget),                   // slot 3: the memory-hard scratch
                    endpoint_cap(ready, Rights::WRITE),    // slot 4: one message, after the seal
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn the credential service");

    Wiring {
        ready,
        verify,
        provision,
    }
}

/// **Spawn the provisioner** and wait for its report. It fills the store and seals it, so when
/// this returns the service is in phase two and the provision endpoint is dead at both ends.
///
/// Its endowment is the provision endpoint, a report endpoint, and the provisioner's page. It
/// holds no verify endpoint, which is the mirror of the client's position: neither party can do
/// the other's job, and neither is prevented from it by a check.
pub fn provisioner(image: &'static [u8], w: &Wiring) -> [u64; 5] {
    spawn_cli(image, ROLE_PROVISIONER, w.provision, PROV_VA)
}

/// **Spawn a client** in `role` against the verify endpoint, and wait for its report.
pub fn client(image: &'static [u8], w: &Wiring, role: u64) -> [u64; 5] {
    spawn_cli(image, role, w.verify, VERIFY_VA)
}

/// The one spawn site the three `credentialer_test_client` roles share, because the whole claim is that they
/// differ in their endowment and not in their code. Changing `endpoint` and `va` here is the
/// entire difference between a provisioner and an attacker.
fn spawn_cli(image: &'static [u8], role: u64, endpoint: EpId, va: u64) -> [u64; 5] {
    let report = crate::sched::create_endpoint();
    let maps = [Mapping {
        va,
        phys: page_for(va),
        flags: Flags::user_data(),
    }];
    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: role,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(endpoint, Rights::WRITE), // slot 0: the service
                    endpoint_cap(report, Rights::WRITE),   // slot 1: say what happened
                ],
                maps: &maps,
            },
        )
    })
    .expect("could not spawn a credential client");
    crate::sched::ipc_recv(report)
}

/// The two shared frames, allocated once and remembered, because the service and its
/// counterparty must map the **same** physical frame and the spawns happen at different times.
///
/// Plain atomics rather than a lock: the only writer is the boot/test thread.
static FRAMES: [core::sync::atomic::AtomicU64; 2] = [
    core::sync::atomic::AtomicU64::new(0),
    core::sync::atomic::AtomicU64::new(0),
];

/// Allocate the next shared frame, in the order the service's `maps` array wants them.
fn frame() -> u64 {
    use core::sync::atomic::Ordering;
    let i = usize::from(FRAMES[0].load(Ordering::Acquire) != 0);
    let phys = crate::memory::alloc()
        .expect("no frame for a credential page")
        .addr();
    // SAFETY: a fresh frame, direct-mapped, owned by nobody else. Zeroed so a client's first
    // look at the page cannot find somebody else's memory, which for this contract would mean
    // finding it where a secret is supposed to go.
    unsafe {
        core::ptr::write_bytes(mmu::phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE as usize);
    }
    // Release: the frame must be zeroed before another thread can observe the pointer to it.
    FRAMES[i].store(phys, Ordering::Release);
    phys
}

/// Which frame backs a given virtual address, for the counterparty's mapping.
fn page_for(va: u64) -> u64 {
    use core::sync::atomic::Ordering;
    let i = usize::from(va == VERIFY_VA);
    let phys = FRAMES[i].load(Ordering::Acquire);
    assert_ne!(
        phys, 0,
        "the credential service was not wired before a client"
    );
    phys
}

/// Read the shared frame behind `va` directly, which is a thing only the kernel can do and is
/// how a test checks a claim about what is *not* in a page.
pub fn peek(va: u64, out: &mut [u8]) {
    use core::sync::atomic::Ordering;
    let i = usize::from(va == VERIFY_VA);
    let phys = FRAMES[i].load(Ordering::Acquire);
    // SAFETY: a frame this module allocated and still owns, read through the direct map.
    let page = unsafe {
        core::slice::from_raw_parts(mmu::phys_to_virt(phys) as *const u8, FRAME_SIZE as usize)
    };
    let n = out.len().min(page.len());
    out[..n].copy_from_slice(&page[..n]);
}

/// The verify page's address, for a test that wants to look at it.
pub const fn verify_page_va() -> u64 {
    VERIFY_VA
}

/// Unpack the `k`th reply code from a `credentialer_test_client` report's second word. One byte per code; see
/// `user/src/credentialer_test_client.rs` `Codes`.
pub const fn nth(packed: u64, k: u32) -> u64 {
    (packed >> (8 * k)) & 0xff
}
