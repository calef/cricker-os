//! **The operator: replace a running component under a talking client** (milestone 23,
//! DECISIONS §41).
//!
//! This is the milestone's flagship, and it is an unprivileged process. The kernel does not know a
//! swap is happening, has no notion of a component, and gained no method for one. What it supplies
//! is what it already supplied: endpoints that name no peer (§12), a death message (§26), a reap
//! that needs no construction authority (§32), and revocation (§13, §16, and §41's device
//! take-back). Everything else is code here.
//!
//! # Two roles, two rungs of the latency ladder
//!
//! - [`ROLE_DIRECT`](swap_proto::ROLE_DIRECT): the default rung. The stable name a client holds is the
//!   endpoint object itself, and the swap changes who is parked in `RECV_CAP` on it. **No process
//!   sits in the data path**, so the steady state costs exactly what an unbrokered call costs.
//! - [`ROLE_QUEUED`](swap_proto::ROLE_QUEUED): the opt-in rung. A `broker` stands between producer and
//!   backend so the producer never blocks on an absent consumer. One extra hop, priced by the
//!   `broker_rtt` benchmark, and chosen per channel rather than imposed on every IPC.
//!
//! # The direct swap, step by step, and why the order is this
//!
//! ```text
//!   1 BUILT    lay the replacement out, endow it, retype its TCB -- but do NOT configure or start
//!              it. A thread that has never been started is in nobody's queue, so it cannot take a
//!              request the incumbent is still there to serve.
//!   2 DRAINED  CALL OP_QUIESCE on the service endpoint itself. The endpoint's sender queue is
//!              FIFO, so by the time this arrives the incumbent has answered every request queued
//!              ahead of it. It replies and stops receiving.
//!   3 REVOKED  Frame::REVOKE the device capability: gone from every holder but us.
//!   4 STARTED  map the registers into the replacement, CONFIGURE, START. The down window ends.
//!   5 REAPED   the incumbent is told to touch the device it no longer has; it faults, its death
//!              arrives on the supervision endpoint, and Endpoint::REAP collects the corpse.
//! ```
//!
//! **The roadmap put "start the new server" first and the revoke second, and building it that way
//! does not work.** §13 revocation is by *physical page*: a revoke that ran after the replacement
//! had been endowed with the device would take the replacement's copy too, and since the kernel
//! mints a device capability once, at boot, nothing could ever hand one back. So the replacement is
//! built and endowed with everything *except* the device, and receives the registers on the far
//! side of the revoke. What had to move is the endowment, not the build, which is why the down
//! window is still four syscalls wide. Recorded rather than quietly reordered.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46), replacing `swapd`.
//! Refused `swapd` (the `-d` claim).

#![no_std]
#![no_main]

// Two shared modules: the swap system's protocol and the supervision tree's loader. Each binary
// uses a different slice of both, so the unused halves are expected (§38).
use component_plan::Provisions;
use supervision_proto::ChildEndowment;
use swap_proto::log_checks as lc;
use user_rt::{cap_delete, invoke, recv, recv_fault, send};

/// Where the kernel maps the initrd archive, read-only. Must match the kernel's spawn path.
const INITRD_VA: u64 = 0x2000_0000;

/// What the kernel grants us, and nothing else.
const ROOT_UT: u64 = 0; // the construction budget: what every process here is built out of
const REPORT: u64 = 1; // WRITE|GRANT, so each child gets its own narrowed view
const DEVICE: u64 = 2; // the UART's registers, WRITE|GRANT: ours to lend, and ours to take back

// Pages per process we build is NOT here any more. It is `swap_proto::INSTANCE_PAGES`, declared by
// each contract, and it reaches this program through `component_plan::Plan::pages`. It is a **peak**,
// because this operator never destroys a region: all five splits are live at once and the budget has
// to cover them together.

/// The channels this operator owns, created once out of its own budget.
struct Wiring {
    /// The stable name. One endpoint object; every client capability to it is handed out before
    /// either component exists, and nothing ever creates a second one.
    svc: u64,
    /// Our children's deaths, kernel-stamped (DECISIONS §26).
    faultep: u64,
    /// Our own coordination channel. Separate from the report endpoint on purpose: the report
    /// endpoint belongs to the test, and an operator that read it would be stealing the record.
    note: u64,
    /// How we speak to a component that has quiesced, and so is no longer listening on `svc`.
    poke: u64,
    /// The witness page, mapped read/write into us and into every component.
    logframe: u64,
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(role: u64, initrd_len: u64, _a2: u64) -> ! {
    // SAFETY: the kernel mapped `initrd_len` bytes of reserved RAM read-only at INITRD_VA.
    let archive =
        unsafe { core::slice::from_raw_parts(INITRD_VA as *const u8, initrd_len as usize) };
    let Ok(fs) = nifefs::Fs::parse(archive) else {
        bail(1)
    };

    let w = Wiring {
        svc: obj(abi::objtype::ENDPOINT, 5),
        faultep: obj(abi::objtype::ENDPOINT, 6),
        note: obj(abi::objtype::ENDPOINT, 7),
        poke: obj(abi::objtype::ENDPOINT, 8),
        logframe: frame(9),
    };
    // SAFETY: plain syscall; the kernel validates the frame, the va and the budget.
    if unsafe { invoke(w.logframe, abi::frame::MAP, swap_proto::LOG_VA, 1, ROOT_UT) } != 0 {
        bail(10)
    }
    for i in 0..swap_proto::PAGE {
        // SAFETY: a page we just mapped read/write into our own address space.
        unsafe { core::ptr::write_volatile((swap_proto::LOG_VA as *mut u8).add(i as usize), 0) };
    }

    match role {
        swap_proto::ROLE_QUEUED => queued(&fs, &w),
        _ => direct(&fs, &w),
    }
}

// ===============================================================================================
// The default rung: the endpoint is the stable name and nothing stands in the data path.
// ===============================================================================================

fn direct(fs: &nifefs::Fs, w: &Wiring) -> ! {
    let v1 = image(fs, "rust_swappable", 2);
    let v2 = image(fs, "c_swappable", 3);
    let client_img = image(fs, "chatty", 4);

    // **What this operator will route, per child, by the name the component uses for it.** This is
    // the whole of what used to be four literal endowment arrays: no rights are spelled here, no
    // addresses, and no slot order, because every one of those is declared by the component and
    // computed by `component_plan`. What is left is the only thing an operator actually knows, which
    // is which of *its own* objects answers to which name.
    //
    // Two tables rather than one, and the reason is not that the manifests would take too much from
    // a wide one (they take what they declare either way). It is that the two bounds are
    // independent: the manifest bounds what a component may *ask* for, and a routing table bounds
    // what this operator *offers* it. A client that could be routed the device is a client one
    // manifest edit away from holding it.
    let to_component = Provisions {
        held: &[
            ("service", w.svc),
            ("report", REPORT),
            ("operator", w.note),
            ("control", w.poke),
            ("witness", w.logframe),
            ("uart", DEVICE),
        ],
    };
    let to_client = Provisions {
        held: &[("service", w.svc), ("report", REPORT), ("operator", w.note)],
    };

    // ------------------------------------------------------------------------------------------
    // **The control that must fail, before anything exists.** The queue broker's declaration names
    // `requests` and `backend`, and this channel routes neither: the queued rung is the other role's
    // system. So a component this operator cannot provide for is refused as a *value*, with nothing
    // built, nothing mapped and nothing started, which is what makes a manifest a request rather
    // than an instruction. A run in which this *succeeds* is failed loudly, the same way an
    // instance's post-revoke probe surviving is.
    // ------------------------------------------------------------------------------------------

    match component_plan::plan(&swap_proto::BROKER, &to_component) {
        Ok(_) => bail(60),
        Err(refusal) => send(REPORT, swap_proto::RPT_REFUSED, refusal.code(), 0),
    };

    let Ok(component) = component_plan::plan(&swap_proto::CONSOLE, &to_component) else {
        bail(61)
    };
    let Ok(client) = component_plan::plan(&swap_proto::CLIENT, &to_client) else {
        bail(62)
    };

    // ------------------------------------------------------------------------------------------
    // The incumbent.
    // ------------------------------------------------------------------------------------------

    start_child(&v1, &component, w.faultep, [1, 0, 0], 11); // a device, log entries from 0

    // ------------------------------------------------------------------------------------------
    // Step 1: build the replacement, **before anyone is talking**. Endowed with everything except
    // the device, and not configured, so it cannot run and cannot race the incumbent for requests.
    //
    // Doing this first is the roadmap's own ordering, and there is a measurement behind keeping it:
    // building a process is a few hundred syscalls, and when this ran *after* the swap trigger the
    // client got through its entire conversation on RISC-V before the operator was ready. Laying
    // the replacement out ahead of time is what keeps the down window four syscalls wide instead of
    // "however long a build takes".
    // ------------------------------------------------------------------------------------------

    // The one endowment this operator still writes by hand, and it is the deferral rather than the
    // authority: `maps_without_devices` is the declaration minus what a revoke is about to take, and
    // `devices` below is the rest of it. `component_plan` sorts device mappings last so both halves
    // are slices of one plan and neither can be built by hand.
    let Ok(b_region) = supervision_proto::untyped_split(ROOT_UT, component.pages()) else {
        bail(20)
    };
    let Ok((b_tcb, b_aspace)) = supervision_proto::build_child_space(
        ROOT_UT,
        b_region,
        &v2,
        &ChildEndowment {
            caps: component.caps(),
            maps: component.maps_without_devices(),
            blobs: &[],
            fault: Some(w.faultep),
            ..ChildEndowment::new()
        },
    ) else {
        bail(21)
    };
    send(
        REPORT,
        swap_proto::RPT_STEP,
        swap_proto::step::BUILT,
        swap_proto::V2,
    );

    // ------------------------------------------------------------------------------------------
    // The client that will talk across the swap, and the wait for its conversation to be well under
    // way. The incumbent tells us on its own channel after it has served SWAP_TRIGGER requests, so
    // the swap lands in the middle of a live conversation rather than at a moment we picked by
    // counting yields.
    // ------------------------------------------------------------------------------------------

    start_child(
        &client_img,
        &client,
        w.faultep,
        [swap_proto::ROLE_CLIENT, 0, 0],
        14,
    );
    expect_note(w.note, swap_proto::NOTE_SWAP_NOW, 17);

    // ------------------------------------------------------------------------------------------
    // Step 2: drain. The quiesce request travels on the endpoint being drained, so FIFO ordering
    // does the waiting for us.
    // ------------------------------------------------------------------------------------------

    let (verdict, served) = user_rt::call(w.svc, swap_proto::OP_QUIESCE, 0);
    if verdict != swap_proto::QUIESCED {
        bail(22)
    }
    send(
        REPORT,
        swap_proto::RPT_STEP,
        swap_proto::step::DRAINED,
        served,
    );

    // ------------------------------------------------------------------------------------------
    // Step 3: take the registers back. Every other holder loses the capability and the mapping; we
    // keep ours, which is what makes this a transfer rather than a demolition (DECISIONS §41).
    // ------------------------------------------------------------------------------------------

    // SAFETY: plain syscall on the device capability the kernel granted us at spawn.
    if unsafe { invoke(DEVICE, abi::frame::REVOKE, 0, 0, 0) } != 0 {
        bail(23)
    }
    send(REPORT, swap_proto::RPT_STEP, swap_proto::step::REVOKED, 0);

    // ------------------------------------------------------------------------------------------
    // Step 4: endow the replacement with the registers and start it. It drains whatever parked on
    // the service endpoint's sender queue while nobody was receiving.
    // ------------------------------------------------------------------------------------------

    for &(va, slot, mode) in component.devices() {
        // SAFETY: plain syscall; the aspace capability is still ours until CONFIGURE consumes it.
        if unsafe { invoke(b_aspace, abi::aspace::MAP_INTO, va, slot, mode) } != 0 {
            bail(24)
        }
    }
    if supervision_proto::configure_child(b_tcb, b_aspace, v2.entry()).is_err() {
        bail(25)
    }
    if !supervision_proto::tcb_start(b_tcb, 1, 0, 0) {
        bail(26)
    }
    cap_delete(b_tcb);
    cap_delete(b_region);
    send(
        REPORT,
        swap_proto::RPT_STEP,
        swap_proto::step::STARTED,
        swap_proto::V2,
    );

    // ------------------------------------------------------------------------------------------
    // Step 5: the receipt for the revoke, and the teardown.
    //
    // The incumbent is quiesced and alive, holding a virtual address that used to be a UART. Tell
    // it to read one register. If step 3 was real it faults, and the kernel tells us where.
    // ------------------------------------------------------------------------------------------

    send(w.poke, swap_proto::POKE_PROBE, 0, 0);
    let mut corpses = 0u64;
    let revoke_enforced = wait_for_fault(w.faultep, &mut corpses, swap_proto::DEV_VA);

    // The conversation is still running: the client is somewhere in its sixties, being served by the
    // replacement. Wait for it to finish before reading the witness page, or the requests it has not
    // made yet would read as requests nobody served.
    expect_note(w.note, swap_proto::NOTE_CLIENT_DONE, 28);
    reap_to(w.faultep, &mut corpses, 2); // the incumbent and the client

    // ------------------------------------------------------------------------------------------
    // The attacker, once the honest system has finished being interesting. It gets exactly the
    // client's capabilities and tries to become the server on the endpoint it is a client of.
    // ------------------------------------------------------------------------------------------

    // The attacker is wired from the client's own declaration and the client's own routing table, so
    // "exactly the honest client's capabilities" is now a property of the code rather than of two
    // arrays a reader has to compare.
    start_child(
        &client_img,
        &client,
        w.faultep,
        [swap_proto::ROLE_USURPER, 0, 0],
        30,
    );
    expect_note(w.note, swap_proto::NOTE_ATTACK_DONE, 33);
    reap_to(w.faultep, &mut corpses, 3); // and the attacker

    // Retire the replacement and collect it too, so the run ends with every region back in this
    // budget. Not tidiness: this is the same lifecycle machinery the swap itself is made of, run to
    // the end, and the test asserts on the memory coming home.
    retire(w, &mut corpses, 4, 34);

    // Our own witness: the log page, read in our address space, after every writer is gone.
    // Independent of the client's verdict, which was computed from replies in a different address
    // space and reaches the test on its own.
    send(
        REPORT,
        swap_proto::RPT_LOG,
        verdict_from_log(0, revoke_enforced),
        changed_at(0),
    );
    user_rt::exit()
}

// ===============================================================================================
// The opt-in rung: a queue broker between producer and backend, so the producer never blocks on an
// absent consumer.
// ===============================================================================================

fn queued(fs: &nifefs::Fs, w: &Wiring) -> ! {
    let v1 = image(fs, "rust_swappable", 2);
    let v2 = image(fs, "c_swappable", 3);
    let client_img = image(fs, "chatty", 4);
    let broker_img = image(fs, "broker", 40);

    // `svc` is the *back* endpoint here: what the broker forwards to and what a backend receives
    // on. `front` is what the producer holds, and it is the stable name on this channel.
    let front = obj(abi::objtype::ENDPOINT, 41);
    let base = swap_proto::BROKER_LOG_BASE;

    // **Three routing tables, and the interesting thing about them is `service`.** The producer's
    // declaration is `swap_proto::CLIENT`, byte for byte the one the direct channel's client is
    // wired from, and it asks to use an endpoint it calls `service`. Here that name resolves to the
    // broker's front endpoint instead of to the component's. One declaration, two routings, and the
    // program cannot tell which it got: that is the indirection the manifest buys, and it is what
    // makes a component's *peer* substitutable and not only the component.
    let to_backend = Provisions {
        held: &[
            ("service", w.svc),
            ("report", REPORT),
            ("operator", w.note),
            ("control", w.poke),
            ("witness", w.logframe),
        ],
    };
    let to_broker = Provisions {
        held: &[
            ("requests", front),
            ("backend", w.svc),
            ("report", REPORT),
            ("operator", w.note),
        ],
    };
    let to_producer = Provisions {
        held: &[("service", front), ("report", REPORT), ("operator", w.note)],
    };

    // The control that must fail on this channel, and it is the mirror of the direct one: no device
    // is routed here, so the console component's declaration cannot be satisfied and is refused
    // before anything is built. This is the same refusal from the other side, which is why both
    // roles report it: a mechanism that only worked for the one component it was written against
    // would not be a mechanism.
    match component_plan::plan(&swap_proto::CONSOLE, &to_backend) {
        Ok(_) => bail(60),
        Err(refusal) => send(REPORT, swap_proto::RPT_REFUSED, refusal.code(), 0),
    };

    // No device on this channel: the backend behind a broker is a plain service, and mixing the
    // device story into the queue story would make it unclear which mechanism carried which claim.
    let Ok(backend) = component_plan::plan(&swap_proto::BACKEND, &to_backend) else {
        bail(61)
    };
    let Ok(broker) = component_plan::plan(&swap_proto::BROKER, &to_broker) else {
        bail(62)
    };
    let Ok(producer) = component_plan::plan(&swap_proto::CLIENT, &to_producer) else {
        bail(63)
    };

    start_child(&v1, &backend, w.faultep, [0, base, 0], 42);
    start_child(&broker_img, &broker, w.faultep, [0, 0, 0], 43);
    start_child(
        &client_img,
        &producer,
        w.faultep,
        [swap_proto::ROLE_PRODUCER, 0, 0],
        44,
    );

    expect_note(w.note, swap_proto::NOTE_SWAP_NOW, 45);

    // Tell the broker to take custody. From here until BOP_UP there is no backend at all on this
    // channel, and the producer keeps running: that window is the whole reason this rung exists.
    let (r, _) = user_rt::call(front, swap_proto::BOP_DOWN, 0);
    if r != 0 {
        bail(46)
    }

    // Quiesce the backend and let it die, exactly as on the direct channel, minus the device.
    let (verdict, served) = user_rt::call(w.svc, swap_proto::OP_QUIESCE, 0);
    if verdict != swap_proto::QUIESCED {
        bail(47)
    }
    send(
        REPORT,
        swap_proto::RPT_STEP,
        swap_proto::step::DRAINED,
        served,
    );
    send(w.poke, swap_proto::POKE_QUIT, 0, 0);
    let mut corpses = 0u64;
    reap_to(w.faultep, &mut corpses, 1);

    // The replacement, in a different language, on the same back endpoint, wired from the same
    // declaration the outgoing one was.
    start_child(&v2, &backend, w.faultep, [0, base, 0], 48);
    send(
        REPORT,
        swap_proto::RPT_STEP,
        swap_proto::step::STARTED,
        swap_proto::V2,
    );

    // Release the backlog. The broker drains in arrival order before it answers, so this call
    // returning means every buffered item has reached the new backend.
    let (r, _drained) = user_rt::call(front, swap_proto::BOP_UP, 0);
    if r != 0 {
        bail(49)
    }

    expect_note(w.note, swap_proto::NOTE_CLIENT_DONE, 50);
    reap_to(w.faultep, &mut corpses, 2); // and the producer

    // Shut the channel down so the run leaves nothing running and nothing spent: the broker exits,
    // then the backend quiesces and exits, and both corpses are collected.
    let _ = user_rt::call(front, swap_proto::OP_QUIESCE, 0);
    expect_note(w.note, swap_proto::NOTE_BROKER_DONE, 51);
    reap_to(w.faultep, &mut corpses, 3); // and the broker
    retire(w, &mut corpses, 4, 52); // and the replacement backend

    send(
        REPORT,
        swap_proto::RPT_LOG,
        verdict_from_log(base, false),
        changed_at(base),
    );
    user_rt::exit()
}

// ===============================================================================================
// The pieces both roles share.
// ===============================================================================================

/// Split a region, build a child in it, start it, and drop both capabilities. Neither is the thing
/// itself: a TCB capability is not the thread (dropping it leaves the thread running), and since
/// DECISIONS §32 the region capability is not the reap either.
///
/// **It takes a plan rather than an endowment**, which is milestone 23's manifest lane in one
/// signature: the caller says which component and which supervision endpoint, and every capability,
/// every mapping and the region size come from the component's own declaration. `args` stays a
/// caller's business because it is configuration and not authority (which log entries this instance
/// writes, and which of `chatty`'s three roles it is playing).
fn start_child(
    elf: &elf::Elf,
    plan: &component_plan::Plan,
    faultep: u64,
    args: [u64; 3],
    stage: u64,
) {
    let Ok(region) = supervision_proto::untyped_split(ROOT_UT, plan.pages()) else {
        bail(stage)
    };
    let endow = ChildEndowment {
        caps: plan.caps(),
        maps: plan.maps(),
        blobs: &[],
        fault: Some(faultep),
        ..ChildEndowment::new()
    };
    let Ok(tcb) = supervision_proto::build_child(ROOT_UT, region, elf, &endow) else {
        bail(stage + 1)
    };
    if !supervision_proto::tcb_start(tcb, args[0], args[1], args[2]) {
        bail(stage + 2)
    }
    cap_delete(tcb);
    cap_delete(region);
}

/// Wait for one death, report it, and collect the corpse through the supervision endpoint it
/// arrived on (DECISIONS §32). Returns `(event, fault address)`.
///
/// **Every child this operator starts is supervised and every corpse is collected**, so a run ends
/// with all five instance regions back in this budget. The one that is not tidiness is the one
/// below; the rest are here so the test can assert that a swap system reclaims itself.
fn collect_corpse(faultep: u64, collected: &mut u64) -> (u64, u64) {
    let (event, tid, _pc, addr, _) = recv_fault(faultep);
    send(REPORT, swap_proto::RPT_DEATH, tid, event);
    // We hold no capability to that region: we deleted it the moment the child was started, and the
    // authority for this is the supervision relationship, not the memory.
    if user_rt::reap(faultep, tid) != 0 {
        bail(27)
    }
    *collected += 1;
    (event, addr)
}

/// Collect corpses until `target` of them have been collected in total.
///
/// **Counted, not ordered.** Deaths arrive in whatever order the children happen to die in, and a
/// program that waited for a *particular* child's death would hang the first time two of them
/// finished the other way round. Counting is enough because the only thing done with a corpse here
/// is reap it, and the one death whose identity matters is picked out by [`wait_for_fault`].
fn reap_to(faultep: u64, collected: &mut u64, target: u64) {
    while *collected < target {
        collect_corpse(faultep, collected);
    }
}

/// **Wait for the receipt.** Collect corpses until one is a *fault*, and say whether it faulted on
/// the page `expect_addr` sits in.
///
/// A loop rather than a single receive because the client may finish and exit at any moment, and a
/// clean exit arriving first must not be mistaken for the answer. Telling them apart is exactly
/// what DECISIONS §26's two event codes are for.
///
/// The page, not the byte: the fault address the kernel reports is the *register* the component was
/// reading (`DEV_VA + 0x18` for a PL011's flag register), and what the revoke took away is the page.
/// Comparing the whole address would have been comparing the register layout.
fn wait_for_fault(faultep: u64, collected: &mut u64, expect_addr: u64) -> bool {
    loop {
        let (event, addr) = collect_corpse(faultep, collected);
        if event == abi::fault::EVENT_FAULT {
            send(REPORT, swap_proto::RPT_SITE, addr, expect_addr);
            send(REPORT, swap_proto::RPT_STEP, swap_proto::step::REAPED, 0);
            return addr & !(swap_proto::PAGE - 1) == expect_addr;
        }
    }
}

/// Retire the last live instance on a channel: quiesce it, tell it to go, collect its corpse. The
/// swap's own machinery, run once more with nothing to replace.
fn retire(w: &Wiring, collected: &mut u64, target: u64, stage: u64) {
    let (verdict, _) = user_rt::call(w.svc, swap_proto::OP_QUIESCE, 0);
    if verdict != swap_proto::QUIESCED {
        bail(stage)
    }
    send(w.poke, swap_proto::POKE_QUIT, 0, 0);
    reap_to(w.faultep, collected, target);
}

fn expect_note(note: u64, want: u64, stage: u64) {
    let (kind, _, _) = recv(note);
    if kind != want {
        bail(stage)
    }
}

fn image<'a>(fs: &nifefs::Fs<'a>, name: &str, stage: u64) -> elf::Elf<'a> {
    match fs.read(name).map(elf::Elf::parse) {
        Some(Ok(e)) => e,
        _ => bail(stage),
    }
}

fn obj(objtype: u64, stage: u64) -> u64 {
    match supervision_proto::retype_obj_from(ROOT_UT, objtype) {
        Ok(s) => s,
        Err(()) => bail(stage),
    }
}

fn frame(stage: u64) -> u64 {
    match supervision_proto::retype_frame_from(ROOT_UT) {
        Ok(s) => s,
        Err(()) => bail(stage),
    }
}

/// **The operator's verdict**, read out of the log page byte by byte.
fn verdict_from_log(base: u64, revoke_enforced: bool) -> u64 {
    let mut bits = lc::NO_GAP | lc::MONOTONE;
    let mut seen_v1 = false;
    let mut seen_v2 = false;
    let mut last = 0u64;
    for seq in 0..swap_proto::REQUESTS {
        let v = swap_proto::log_get(base + seq);
        if v == 0 {
            bits &= !lc::NO_GAP; // a request nobody served: lost in the down window
        }
        if v < last {
            bits &= !lc::MONOTONE; // the old instance answered after the new one: two owners
        }
        if v == swap_proto::V1 {
            seen_v1 = true;
        }
        if v == swap_proto::V2 {
            seen_v2 = true;
        }
        last = v;
    }
    if seen_v1 && seen_v2 {
        bits |= lc::BOTH_VERSIONS;
    }
    if revoke_enforced {
        bits |= lc::REVOKE_ENFORCED;
    }
    bits
}

/// The sequence number the version changed at, so the test can see the swap landed inside the
/// conversation rather than at one of its ends.
fn changed_at(base: u64) -> u64 {
    let mut last = 0u64;
    for seq in 0..swap_proto::REQUESTS {
        let v = swap_proto::log_get(base + seq);
        if last != 0 && v != last {
            return seq;
        }
        last = v;
    }
    0
}

/// Report which stage failed, then trap. A half-built system is not worth limping along, and the
/// stage code turns "nothing happened" into a legible failure.
fn bail(stage: u64) -> ! {
    send(REPORT, swap_proto::RPT_FAILED, stage, 0);
    swap_proto::fail()
}

user_rt::panic_handler!();
