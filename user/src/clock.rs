//! **The clock service** (milestone 51 lane A; DECISIONS §43, notes/clock.md).
//!
//! The one process that owns the real-time clock's registers and the wall clock's offset. Its
//! entire authority is four things placed before it ran: the propose endpoint (slot 0, RECV), a
//! report endpoint (slot 1, WRITE, for whoever wired it), the **clock page** mapped read/write, and
//! the **RTC's registers** as a device mapping. No initrd, no budget, no network, nothing it could
//! build anything with. A compromised clock service is a machine that believes the wrong time,
//! which is exactly as much damage as owning the clock should be worth.
//!
//! # What it does, in order
//!
//! 1. Stamp the clock page (it starts as an honest [`clock_proto::state::UNKNOWN`]).
//! 2. Read the RTC once, through whichever driver the *machine* named (`a0`, from the device
//!    tree's `compatible`, not from `target_arch`).
//! 3. If the reading is plausible, publish it. **If it is not, publish nothing**: the page stays
//!    unknown and every reader can see that the machine does not know what time it is. This is
//!    DECISIONS §42's no-silent-degradation rule; the alternative is confidently reporting 1970.
//! 4. Serve proposals forever, each judged by `clock_proto::policy` and none of them able to set
//!    the clock outright.
//!
//! # The three authorities are three different objects, and only one of them is here
//!
//! **Reading does not come through this process at all.** A reader holds the clock page read-only
//! and the ambient monotonic counter, and computes the time itself: two loads and an add, no
//! syscall, no round trip. **Setting** is a read/write mapping of that same page, which is a
//! capability this process holds and could hand on; writing the offset *is* setting the clock.
//! **Proposing** is the endpoint below, and it is deliberately not either of those. See the
//! contract crate's module docs for the picture.
//!
//! The one thing worth understanding about the shape: this process has **one blocking wait point**,
//! because the kernel has no wait-any primitive and no threads sharing an address space. So it can
//! serve exactly one endpoint, and a design that wanted `set` and `propose` to both be messages
//! would have needed two servers. Making `set` a page write instead is not a workaround; it is
//! what "set the offset outright" already means (DECISIONS §43).
//!
//! Name: unrecorded. Introduced 2026-07-30. Milestone 63 weighed the plain resource noun against
//! the agent noun for the credential service and chose `credentialer` there, on the ground that the
//! service will never hand you a credential; this program keeps the resource name and no record
//! says whether the same question was asked of it.

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use clock_proto::{ClockPage, policy, propose, rtc, state, status};
use user_rt::{cntfrq, now, recv_cap, reply, send};

/// The propose endpoint (slot 0): the service RECVs proposals on it. Everything that arrives here
/// is a request, never a command.
const PROPOSE_EP: u64 = 0;
/// The report endpoint (slot 1): one message at startup saying what the machine knows. Optional;
/// a wiring that grants no such endpoint gets a refused `SEND` the service ignores.
const REPORT: u64 = 1;

/// The clock page, mapped **read/write**: this process is a setter.
const CLOCK_VA: u64 = 0x00c0_0000;
/// The RTC's registers, device-typed. Only mapped when the machine has an RTC we can drive; `a0`
/// says which, and [`rtc::NONE`] means this address is not mapped and must not be touched.
const RTC_VA: u64 = 0x00d0_0000;

/// The startup report's first word, so a reader of the endpoint knows this is the clock speaking.
const RPT_READY: u64 = 0x_c10c_c0de;

#[unsafe(no_mangle)]
pub extern "C" fn _start(rtc_kind: u64, _a1: u64, _a2: u64) -> ! {
    // SAFETY: the wiring maps the clock page read/write at CLOCK_VA before this program runs, and
    // nothing unmaps it. We are the first to touch it, so stamping it here cannot race a reader
    // that has already seen the magic.
    let page = unsafe { ClockPage::new(CLOCK_VA) };
    page.init();

    // The RTC, read exactly once. A second reading would not be more correct: from here on the
    // monotonic counter is the thing that advances, and re-reading the RTC would only import its
    // drift and its coarse resolution into a clock that already has better.
    if let Some(unix_nanos) = read_rtc(rtc_kind)
        && policy::plausible(unix_nanos)
    {
        page.publish(
            state::RTC,
            clock_proto::offset_for(unix_nanos, monotonic_nanos()),
        );
    }

    let r = page.read();
    send(REPORT, RPT_READY, r.state, wall_now(&page));

    serve(page)
}

/// The serve loop: one endpoint, one wait point, forever.
fn serve(page: ClockPage) -> ! {
    loop {
        let (w0, cap, w1) = recv_cap(PROPOSE_EP);
        if cap == abi::endpoint::NO_CAP {
            // A plain SEND on a CALL-only contract. Nobody is waiting for an answer, so there is
            // nothing to do and nothing to report; drop it rather than replying into a slot we do
            // not hold.
            continue;
        }
        match propose::op(w0) {
            propose::PROPOSE => {
                let r = page.read();
                let current = wall_now(&page);
                let verdict = policy::decide(r.state, current, w1);
                if verdict == status::ACCEPTED {
                    // SYNCED rather than SET: this came from a source the service bounded, and the
                    // provenance is what a caller weighing a certificate expiry actually wants.
                    page.publish(
                        state::SYNCED,
                        clock_proto::offset_for(w1, monotonic_nanos()),
                    );
                }
                reply(cap, verdict, wall_now(&page));
            }
            propose::STATE => {
                reply(cap, page.read().state, wall_now(&page));
            }
            _ => {
                reply(cap, status::BAD_REQUEST, 0);
            }
        }
    }
}

/// Wall-clock nanoseconds now, or 0 when the machine does not know. **Zero is not a time here**, it
/// is the absence of one, and the state word next to it is what says which.
fn wall_now(page: &ClockPage) -> u64 {
    let r = page.read();
    if state::known(r.state) {
        clock_proto::wall_nanos(r.offset_nanos, monotonic_nanos())
    } else {
        0
    }
}

/// Monotonic nanoseconds since boot, from the ambient counter.
///
/// Split into whole seconds and a remainder rather than multiplying the raw tick count by a
/// billion: at 62.5 MHz the naive product overflows a `u64` after about five minutes of uptime, and
/// the resulting wall clock would jump backwards by six centuries at the wrap.
fn monotonic_nanos() -> u64 {
    let freq = cntfrq();
    let ticks = now();
    let secs = ticks / freq;
    let rem = ticks % freq;
    secs * clock_proto::NANOS_PER_SEC + rem * clock_proto::NANOS_PER_SEC / freq
}

/// Read the machine's RTC, whichever one it has. `None` when it has none.
///
/// **Both drivers are compiled on both ISAs.** Neither is behind a `cfg(target_arch)`, because the
/// device is a fact about the board and not about the instruction set: the VisionFive 2 is riscv64
/// and has neither of these. Keying the layout off the ISA would compile clean and read garbage on
/// the first real board, which is precisely the failure rule 5 exists to catch.
fn read_rtc(kind: u64) -> Option<u64> {
    match kind {
        rtc::PL031 => Some(pl031_unix_nanos(RTC_VA)),
        rtc::GOLDFISH => Some(goldfish_unix_nanos(RTC_VA)),
        _ => None,
    }
}

/// **The PL031 driver** (`arm,pl031`, QEMU `virt` on aarch64). It takes a base address and knows
/// nothing else, which is the project's rule 2 in its smallest possible form.
///
/// One 32-bit register at offset 0, `DR`, holding **seconds** since the Unix epoch. The other three
/// registers (match, load, control) are for the alarm interrupt, which nothing here wants: this
/// service reads the clock once at startup and lets the monotonic counter carry the time after
/// that. `DR` is 32 bits wide, so this device's own epoch runs out in 2106; the wire format here is
/// nanoseconds in a `u64`, which runs out in 2554, and neither is a horizon worth engineering for.
fn pl031_unix_nanos(base: u64) -> u64 {
    const DR: u64 = 0x00;
    // SAFETY: `base` is this process's device mapping of a PL031, placed by whoever built us.
    let secs = unsafe { core::ptr::read_volatile((base + DR) as *const u32) } as u64;
    secs * clock_proto::NANOS_PER_SEC
}

/// **The Goldfish RTC driver** (`google,goldfish-rtc`, QEMU `virt` on riscv64). Same rule 2 shape:
/// a base address and nothing else.
///
/// Two 32-bit registers holding **nanoseconds** since the Unix epoch, and the order of the two
/// reads is part of the device's contract rather than a preference: reading `TIME_LOW` **latches**
/// `TIME_HIGH`, so `TIME_HIGH` must be read second. Reading them the other way round is correct
/// except across the low word's wrap, which is once every four seconds and presents as a
/// four-second jump in a clock that is otherwise fine, i.e. the worst kind of bug to have.
fn goldfish_unix_nanos(base: u64) -> u64 {
    const TIME_LOW: u64 = 0x00;
    const TIME_HIGH: u64 = 0x04;
    // SAFETY: `base` is this process's device mapping of a Goldfish RTC, placed by whoever built
    // us. The read order is the device's latching contract, not an optimisation.
    unsafe {
        let lo = core::ptr::read_volatile((base + TIME_LOW) as *const u32) as u64;
        let hi = core::ptr::read_volatile((base + TIME_HIGH) as *const u32) as u64;
        (hi << 32) | lo
    }
}

user_rt::panic_handler!();
