use credential_service as cs;

use super::*;

/// Wire the whole system once: the entropy service, the credential service, and the
/// provisioner that fills and seals the store. Returns the wiring, the provisioner's report,
/// and the service's readiness report.
///
/// Once per boot, because the seal is irreversible: a second provisioner would `CALL` an
/// endpoint whose receiver is gone and block forever, which is the correct behaviour and a
/// terrible test.
///
/// **The readiness message is taken here and not in the test that asserts on it**, which cost a
/// hang to learn. A `SEND` on this kernel's endpoints is a rendezvous: it blocks until somebody
/// receives. The credential service sends its readiness word between the seal and the serve
/// loop, so a test that let a client `CALL` before draining that message would find a service
/// that had not reached its serve loop, and the whole boot would deadlock with a sender and a
/// caller both waiting on nobody. The entropy service does not show this because its wiring
/// receives immediately.
/// **`pub(super)` since milestone 54's identity item**, because the SMB gate needs the same sealed
/// store: its adapter authenticates a real NTLMv2 session against this service, so the SMB test
/// calls this to get the verify endpoint. Calling it from either place is safe in either order,
/// which is what the once-per-boot latch below is for; what would not be safe is a *second*
/// wiring, and there is no way to ask for one.
pub(super) fn provisioned() -> (cs::Wiring, [u64; 3], [u64; 3]) {
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    // Plain atomics rather than a lock or a `Once`: the only writer is the test thread, and
    // the tree's `spin` is built without the `once` feature. Release/Acquire on `DONE` is what
    // publishes the nine words next to it (rule 4: assume weak ordering).
    static DONE: AtomicBool = AtomicBool::new(false);
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU64 = AtomicU64::new(0);
    static SAVED: [AtomicU64; 9] = [ZERO; 9];

    if !DONE.load(Ordering::Acquire) {
        let rng = program("entropy").expect("no entropy program in the initrd archive");
        let e = entropy_service::ensure(rng, entropy_service::Bus::Mmio)
            .expect("no virtio-rng device on the mmio bus");
        if let Some(r) = e.wait_for_ready() {
            assert_eq!(
                r[0],
                entropy_proto::READY,
                "the entropy service did not come up, so no salt could be drawn",
            );
        }
        let svc = program("credentialer").expect("no credentialer program in the initrd archive");
        let cli = program("credentialer_test_client")
            .expect("no credentialer_test_client program in the initrd archive");
        let w = cs::start(svc, e.request);
        let report = cs::provisioner(cli, &w);
        let ready = crate::sched::ipc_recv(w.ready);
        for (slot, v) in SAVED.iter().zip([
            w.ready,
            w.verify,
            w.provision,
            report[0],
            report[1],
            report[2],
            ready[0],
            ready[1],
            ready[2],
        ]) {
            slot.store(v, Ordering::Relaxed);
        }
        DONE.store(true, Ordering::Release);
    }
    let v = |i: usize| SAVED[i].load(Ordering::Relaxed);
    (
        cs::Wiring {
            ready: v(0),
            verify: v(1),
            provision: v(2),
        },
        [v(3), v(4), v(5)],
        [v(6), v(7), v(8)],
    )
}

/// **Phase one lands, and the store's capacity is real.** Three identities go in, the fourth is
/// refused with `FULL` rather than quietly replacing somebody, and the seal is accepted.
///
/// The service's own readiness message is checked here too, because it is sent *after* the
/// seal: receiving it is the kernel's evidence that the provisioning loop was left, not merely
/// that a `SEAL` was answered.
#[test_case]
fn provisioning_fills_the_store_and_the_seal_closes_it() {
    let (_, report, ready) = provisioned();
    assert_eq!(report[0], cs::RPT_DONE, "the provisioner did not report");
    let codes = report[1];
    // Three logins, then three shares. Six secrets, not three people with two credentials each:
    // a secret here is scoped to the resource it authenticates to (milestone 65).
    for k in 0..3 {
        assert_eq!(
            cs::nth(codes, k),
            cred_proto::OK,
            "identity {k} was not stored (reply {}), codes {codes:#018x}",
            cs::nth(codes, k),
        );
    }
    for k in 3..6 {
        assert_eq!(
            cs::nth(codes, k),
            cred_proto::OK,
            "share {} was not stored (reply {}), codes {codes:#018x}",
            k - 3,
            cs::nth(codes, k),
        );
    }
    assert_eq!(
        cs::nth(codes, 6),
        cred_proto::FULL,
        "a seventh secret in a six-slot store must be refused, not silently accepted",
    );
    assert_eq!(
        cs::nth(codes, 7),
        cred_proto::OK,
        "the seal was not accepted"
    );
    assert_eq!(
        report[2] & cs::F_CLEAN,
        cs::F_CLEAN,
        "the provisioner's page still held bytes after the seal: a plaintext secret survived \
         in a frame the provisioner maps",
    );

    assert_eq!(
        ready[0],
        cs::RPT_READY,
        "the credential service did not reach phase two (it reported {:#x}; a 0xDEAD_.. word's \
         low byte names the step, see user/src/credentialer.rs)",
        ready[0],
    );
    assert_eq!(
        ready[1], 6,
        "the sealed store does not hold three logins and three shares"
    );
    assert!(
        ready[2] >= 1024,
        "the wired Argon2id cost is {} KiB, which is not memory-hard in any useful sense",
        ready[2],
    );
}

/// **The headline.** A userspace client holding one endpoint, no store, no entropy and no
/// budget gets the right answer to four questions: the right secret, the wrong secret, an
/// identity nobody provisioned, and one person's password against another person's account.
///
/// The last two are one assertion in spirit and two in fact, because they are the failures that
/// look alike from outside and come from different bugs: a lookup that ignores the identity,
/// and a comparison that ignores the pairing.
#[test_case]
fn a_client_gets_a_correct_yes_or_no_and_nothing_else() {
    let (w, _, _) = provisioned();
    let cli = program("credentialer_test_client")
        .expect("no credentialer_test_client program in the initrd archive");
    let r = cs::client(cli, &w, cs::ROLE_HONEST);
    assert_eq!(r[0], cs::RPT_DONE, "the client did not report");
    let codes = r[1];
    assert_eq!(
        cs::nth(codes, 0),
        cred_proto::MATCH,
        "the right secret for a provisioned identity was refused, codes {codes:#018x}",
    );
    assert_eq!(
        cs::nth(codes, 1),
        cred_proto::MISMATCH,
        "the wrong secret was accepted, codes {codes:#018x}",
    );
    assert_eq!(
        cs::nth(codes, 2),
        cred_proto::MISMATCH,
        "an identity nobody provisioned was accepted, codes {codes:#018x}",
    );
    assert_eq!(
        cs::nth(codes, 3),
        cred_proto::MISMATCH,
        "one identity's secret opened another's account, codes {codes:#018x}",
    );
    assert_eq!(
        r[2] & cs::F_CLEAN,
        cs::F_CLEAN,
        "the shared page was not empty after the last reply: either the client's presented \
         secret or something of the store's is still sitting in a frame two processes map",
    );
}

/// **The same endowment, used to attack.** The attacker holds exactly what the honest client
/// holds, and tries to write the store through it: `PUT`, `SEAL`, an undefined opcode, and a
/// request whose lengths are outside the contract. Every one is refused, and the credential it
/// tried to install does not work.
///
/// It is refused because there is nothing to refuse: the provision endpoint was deleted at both
/// ends before this program was spawned, so `PUT` here is not a privileged request arriving at
/// a guard, it is a word arriving at a loop that implements one opcode.
#[test_case]
fn the_same_endowment_cannot_write_the_store() {
    let (w, _, _) = provisioned();
    let cli = program("credentialer_test_client")
        .expect("no credentialer_test_client program in the initrd archive");
    let r = cs::client(cli, &w, cs::ROLE_ATTACKER);
    assert_eq!(r[0], cs::RPT_DONE, "the attacker did not report");
    let codes = r[1];

    // **`PUT` gets MISMATCH, not MALFORMED, and that is the model working.** `provision::PUT`
    // and `verify::VERIFY` are both opcode 1: the two spaces are independent because the
    // *endpoint* gives a number its meaning. So what the attacker actually sent was a verify of
    // an identity and a secret, and the honest answer is no. There was never a privileged
    // request to refuse. This assertion was written expecting MALFORMED and the machine
    // corrected it, which is worth recording here rather than quietly renumbering the opcodes
    // to make the mistake more legible: renumbering would imply the service distinguishes a
    // forbidden opcode from an unknown one, and it does not.
    assert_eq!(
        cs::nth(codes, 0),
        cred_proto::MISMATCH,
        "a PUT on the verify endpoint is a verify of an identity nobody provisioned, so the \
         answer must be MISMATCH; codes {codes:#018x}",
    );
    // **And `SEAL` now gets MISMATCH too, which the machine corrected a second time.** This
    // assertion expected MALFORMED and had been right for as long as the verify endpoint served
    // one opcode. Milestone 65 gave it a second, `NTLM_PROOF`, at number 2, which is
    // `provision::SEAL`'s number: so an attacker's `SEAL` is now read as an NTLM proof for a
    // resource nobody provisioned, and the honest answer to that is no.
    //
    // Worth recording rather than renumbering, for the reason above and for one more. The opcode
    // spaces colliding is not a coincidence to be tidied away, it is what "the endpoint gives a
    // number its meaning" *looks like* when a space grows: adding an opcode on one endpoint
    // silently changed what a word means on the other, and nothing broke, because a word arriving
    // at a serve loop never carried authority in the first place.
    assert_eq!(
        cs::nth(codes, 1),
        cred_proto::MISMATCH,
        "a SEAL on the verify endpoint is an NTLM_PROOF for a resource nobody provisioned, so \
         the answer must be MISMATCH; codes {codes:#018x}",
    );
    for (k, what) in [
        (2, "an undefined opcode"),
        (3, "a request with lengths outside the contract"),
        (4, "PUT_NTLM"),
    ] {
        assert_eq!(
            cs::nth(codes, k),
            cred_proto::MALFORMED,
            "{what} on the verify endpoint was answered {} rather than MALFORMED, codes \
             {codes:#018x}",
            cs::nth(codes, k),
        );
    }
    assert_eq!(
        cs::nth(codes, 5),
        cred_proto::MISMATCH,
        "the attacker installed a working credential for itself, codes {codes:#018x}",
    );
    assert_eq!(
        r[2] & cs::F_CLEAN,
        cs::F_CLEAN,
        "the attacker left bytes in the shared page",
    );
}

/// **The headline of milestone 65.** A userspace program with one endpoint, no key, no store and
/// no entropy answers a client's NTLMv2 authentication correctly, and comes away with the session
/// key it needs to sign the session and with nothing it could carry anywhere else.
///
/// Everything asserted here is a number [MS-NLMP] §4.2.4 prints. The client sends the published
/// `NTProofStr`; the service, which alone holds the `NTOWFv2`, agrees and publishes §4.2.4.1.2's
/// `SessionBaseKey`. Nothing in this test or in the program under test computes an expected value,
/// which is what makes it a compatibility test and not a self-consistency one.
///
/// **This is the property Samba cannot offer.** There, `smbd` opens the password database, so
/// compromising it leaks every hash: crackable offline, reusable wherever the password was reused.
/// Here the SMB server's whole endowment is an endpoint, and revoking it ends the access.
#[test_case]
fn an_smb_server_authenticates_a_session_without_ever_holding_the_key() {
    let (w, _, _) = provisioned();
    let cli = program("credentialer_test_client")
        .expect("no credentialer_test_client program in the initrd archive");
    let r = cs::client(cli, &w, cs::ROLE_NTLM);
    assert_eq!(r[0], cs::RPT_DONE, "the NTLM client did not report");
    let codes = r[1];

    assert_eq!(
        cs::nth(codes, 0),
        cred_proto::MATCH,
        "the published NTLMv2 proof for a provisioned share was refused, codes {codes:#018x}",
    );
    assert_eq!(
        r[2] & cs::F_SESSION_KEY,
        cs::F_SESSION_KEY,
        "the session key the service published is not [MS-NLMP] §4.2.4.1.2's",
    );

    assert_eq!(
        cs::nth(codes, 1),
        cred_proto::MISMATCH,
        "a proof with one bit flipped was accepted, codes {codes:#018x}",
    );
    assert_eq!(
        r[2] & cs::F_NO_KEY_ON_REFUSAL,
        cs::F_NO_KEY_ON_REFUSAL,
        "a refused proof still got a session key: the release rule is the whole security \\
         statement of this opcode",
    );

    // A record that has a password but no NTLM secret stores a key of *zeros*, which is a value an
    // attacker knows, so a proof computed under it is the strongest forgery available. It must
    // fail, and it must fail exactly as a resource nobody provisioned does: distinguishing them
    // would turn this endpoint into an oracle for which shares exist.
    assert_eq!(
        cs::nth(codes, 2),
        cred_proto::MISMATCH,
        "a password-only identity answered an NTLM challenge, codes {codes:#018x}",
    );
    assert_eq!(
        cs::nth(codes, 3),
        cred_proto::MISMATCH,
        "an unprovisioned resource answered an NTLM challenge, codes {codes:#018x}",
    );

    // One password, two derivations: the same share answers an ordinary verify.
    assert_eq!(
        cs::nth(codes, 4),
        cred_proto::MATCH,
        "a share provisioned for NTLM stopped answering its own password, codes {codes:#018x}",
    );
    assert_eq!(
        r[2] & cs::F_CLEAN,
        cs::F_CLEAN,
        "the shared page was not empty after the last reply",
    );
}

/// **The kernel looks at the frame after an NTLM exchange**, which is the check the client cannot
/// make for itself: a program can only see what it was given, and the claim here is about what the
/// service did *not* put in a frame two processes map.
///
/// The published `NTOWFv2` for this share is the byte string an attacker actually wants, and
/// `NTProofStr` and the session key are what a naive service would leave lying in the page. None
/// of the three is there.
#[test_case]
fn no_ntlm_key_material_survives_in_the_shared_frame() {
    let (w, _, _) = provisioned();
    let cli = program("credentialer_test_client")
        .expect("no credentialer_test_client program in the initrd archive");
    let _ = cs::client(cli, &w, cs::ROLE_NTLM);

    let mut page = [0u8; 4096];
    cs::peek(cs::verify_page_va(), &mut page);
    for (what, bytes) in [
        // NTOWFv2, [MS-NLMP] §4.2.4.1.1. The one value that must never leave the service.
        (
            "the stored NTLM key",
            [
                0x0c, 0x86, 0x8a, 0x40, 0x3b, 0xfd, 0x7a, 0x93, 0xa3, 0x00, 0x1e, 0xf2, 0x2e, 0xf0,
                0x2e, 0x3f,
            ],
        ),
        // The session key, §4.2.4.1.2. Allowed to cross, but only for as long as the exchange it
        // belongs to; leaving it in the frame would make it outlive that.
        (
            "the session key",
            [
                0x8d, 0xe4, 0x0c, 0xca, 0xdb, 0xc1, 0x4a, 0x82, 0xf1, 0x5c, 0xb0, 0xad, 0x0d, 0xe9,
                0x5c, 0xa3,
            ],
        ),
        // NTProofStr, §4.2.4.1.3, which the client itself put in the page.
        (
            "the presented proof",
            [
                0x68, 0xcd, 0x0a, 0xb8, 0x51, 0xe5, 0x1c, 0x96, 0xaa, 0xbc, 0x92, 0x7b, 0xeb, 0xef,
                0x6a, 0x1c,
            ],
        ),
    ] {
        assert!(
            !page.windows(bytes.len()).any(|s| s == bytes),
            "{what} is still in the frame the client and the service share",
        );
    }
    if let Some(i) = page.iter().position(|&b| b != 0) {
        panic!(
            "byte {i} of the shared frame is {:#04x} after the NTLM exchange",
            page[i],
        );
    }
}

/// **The service kept serving.** Every refusal above must be a reply, not a crash: a credential
/// service that a malformed request can kill is a login outage anybody can cause, and it is the
/// exact shape of the `argon2` cost-overflow panic `cred::Cost::new` exists to prevent.
#[test_case]
fn the_service_survives_everything_the_attacker_did() {
    let (w, _, _) = provisioned();
    let cli = program("credentialer_test_client")
        .expect("no credentialer_test_client program in the initrd archive");
    let _ = cs::client(cli, &w, cs::ROLE_ATTACKER);
    let r = cs::client(cli, &w, cs::ROLE_HONEST);
    assert_eq!(
        cs::nth(r[1], 0),
        cred_proto::MATCH,
        "the credential service stopped answering correctly after an attacker talked to it",
    );
}

/// **The kernel looks at the frame itself.** The client asserted the page was clean; this
/// reads the physical frame through the direct map, which no userspace program could do, and
/// checks that the shared page carries neither the secret that was presented nor any nonzero
/// byte at all.
///
/// Checking for the literal secret as well as for zero is not redundant. "All zero" is the
/// property that holds today; "does not contain the secret" is the property that must hold if
/// the wipe is ever narrowed, and a test that only checked the first would go green on a
/// change that broke the second.
#[test_case]
fn the_shared_frame_holds_nothing_after_an_answer() {
    let (w, _, _) = provisioned();
    let cli = program("credentialer_test_client")
        .expect("no credentialer_test_client program in the initrd archive");
    let _ = cs::client(cli, &w, cs::ROLE_HONEST);

    let mut page = [0u8; 4096];
    cs::peek(cs::verify_page_va(), &mut page);
    let secret = b"correct horse battery staple";
    assert!(
        !page.windows(secret.len()).any(|s| s == secret),
        "the presented secret is still in the frame the client and the service share",
    );
    if let Some(i) = page.iter().position(|&b| b != 0) {
        panic!(
            "byte {i} of the shared frame is {:#04x} after the exchange: the service left \
             something in a page a client maps",
            page[i],
        );
    }
}
