//! **The queue broker: the latency ladder's middle rung** (milestone 23, DECISIONS §41).
//!
//! The default rung has no process in it at all: a client CALLs a stable endpoint, whoever is
//! parked in `RECV_CAP` on it answers, and a swap changes who that is. That costs nothing, and it
//! is what `swapper`'s direct system uses. It has one property a producer may not be able to live
//! with: while nobody is receiving, a caller **blocks**. Its request is safe (it parks on the
//! endpoint's own sender queue and the next server drains it), but the caller is stopped until then.
//!
//! This program is the answer for a channel that cannot afford that. It stands between a producer
//! and a backend and takes custody of requests while the backend is away, so the producer's call
//! always returns:
//!
//! ```text
//!   steady state (backend up)      producer --CALL--> broker --CALL--> backend
//!                                                            <--reply--
//!                                           <--reply--                      2 round trips
//!
//!   down window (backend gone)     producer --CALL--> broker  [enqueue]
//!                                           <-ACCEPTED-                     1 round trip, no block
//! ```
//!
//! **The tax is real and it is why this is opt-in.** Interposing a process turns one rendezvous
//! into two, and the kernel-side `broker_rtt` benchmark prices it against `call_reply` (the same
//! client and backend with nothing in between). Direct synchronous rendezvous stays the fast path;
//! this rung is chosen per channel, for channels that cross a lifecycle boundary.
//!
//! **The buffer is this process's own memory, and it is bounded.** The kernel does not grow: it
//! keeps synchronous rendezvous with no allocation, and the queue is userspace policy in a
//! userspace `.bss`, inside the region this instance was built in. A runaway producer fills it and
//! gets [`QUEUE_FULL`](swap_proto::QUEUE_FULL) back, which is backpressure as a value the producer can
//! read rather than a policy hidden inside a server.
//!
//! **What it does not do**, stated so the ladder's top rung is not implied: it does not write to
//! storage, so it does not survive its own crash. That is the durable rung, and it is not built.
//!
//! Name: ratified 2026-07-30 (calef, DECISIONS §39, landed by milestone 46), replacing `brokerd`.
//! Refused `brokerd` (the `-d` suffix claims the Unix process model DECISIONS §39 rejects, before a
//! reader has seen a line of code; that model is defined by the ambient authority this OS
//! deliberately lacks).

#![no_std]
// Program entry points, not the crates/ library surface milestone 68's ratchet tracks
// (DECISIONS §107): each `[[bin]]` is its own crate root with one `_start`, and 58 of them
// documenting an OS-facing ABI entry point is not what the lint is for.
#![allow(missing_docs)]
#![no_main]

use user_rt::{call, recv_cap, reply, send};

// A source file shared by several binaries through `#[path]`, and each uses a different slice of it,
// so the unused halves are expected (§38).

/// What `swapper` endowed us with, in order. No budget, no device, no way to build anything: a
/// compromised broker can reorder or drop the one channel it was placed on, and nothing else.
// Derived from this program's own declaration (`swap_proto::BROKER`, milestone 23's manifest), so
// the slot a name lands in and the slot this code reads are one number rather than two that agree.
// `requests` is the only `Serve` here: a broker answers producers and asks its backend, and those
// two directions on one hop are the whole of what a queue rung is.
const FRONT: u64 = component_plan::slot_of(&swap_proto::BROKER, "requests");
const BACK: u64 = component_plan::slot_of(&swap_proto::BROKER, "backend");
const RPT: u64 = component_plan::slot_of(&swap_proto::BROKER, "report");
const NOTE: u64 = component_plan::slot_of(&swap_proto::BROKER, "operator");

/// How deep the backlog goes. Fixed storage in our own `.bss`, which lives in the region this
/// instance was built in, so the bound is a bound on *this process's* memory and the kernel's
/// footprint is unchanged. Sized to hold a whole run's worth of requests with room to spare, so a
/// full queue in the test means a bug rather than an undersized constant.
const CAPACITY: usize = 128;

struct Queue {
    items: [u64; CAPACITY],
    head: usize,
    count: usize,
}

impl Queue {
    fn push(&mut self, item: u64) -> bool {
        if self.count == CAPACITY {
            return false;
        }
        self.items[(self.head + self.count) % CAPACITY] = item;
        self.count += 1;
        true
    }

    fn pop(&mut self) -> Option<u64> {
        if self.count == 0 {
            return None;
        }
        let item = self.items[self.head];
        self.head = (self.head + 1) % CAPACITY;
        self.count -= 1;
        Some(item)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_a0: u64, _a1: u64, _a2: u64) -> ! {
    let mut q = Queue {
        items: [0; CAPACITY],
        head: 0,
        count: 0,
    };
    // Pass-through until told otherwise. The broker does not probe the backend and cannot: liveness
    // is exactly the thing a synchronous rendezvous cannot report on, and guessing at it with a
    // timeout is the poor death detector DECISIONS §26 argued against. The operator, which is the
    // one that takes the backend away, is the one that says so.
    let mut up = true;
    let mut buffered = 0u64;
    send(RPT, swap_proto::RPT_UP, 0, 0);

    loop {
        let (op, slot, arg) = recv_cap(FRONT);
        if slot == abi::endpoint::NO_CAP {
            continue; // the contract says CALL; with no reply capability there is nobody to answer
        }
        match op {
            swap_proto::OP_PUT if up => {
                // **Pass-through.** No copy, no queue, no scheduling policy: forward the two words
                // and hand the backend's own answer straight back. This is the steady state, and it
                // is the whole of the tax `broker_rtt` measures.
                let (r0, r1) = call(BACK, swap_proto::OP_PUT, arg);
                reply(slot, r0, r1);
            }
            swap_proto::OP_PUT => {
                // The backend is away. Take custody and answer immediately, so the producer keeps
                // running rather than parking on an endpoint nobody is receiving on.
                if q.push(arg) {
                    buffered += 1;
                    reply(slot, swap_proto::ACCEPTED, q.count as u64);
                } else {
                    reply(slot, swap_proto::QUEUE_FULL, q.count as u64);
                }
            }
            swap_proto::BOP_DOWN => {
                up = false;
                reply(slot, 0, q.count as u64);
            }
            swap_proto::BOP_UP => {
                // Drain in arrival order, before answering the operator, so "the broker is up
                // again" and "the backlog is delivered" are the same event as far as anyone
                // watching this endpoint is concerned.
                let mut drained = 0u64;
                while let Some(item) = q.pop() {
                    let _ = call(BACK, swap_proto::OP_PUT, item);
                    drained += 1;
                }
                up = true;
                send(RPT, swap_proto::RPT_DRAINED, drained, buffered);
                reply(slot, 0, drained);
            }
            swap_proto::OP_QUIESCE => {
                send(RPT, swap_proto::RPT_QUIESCED, 0, buffered);
                reply(slot, swap_proto::QUIESCED, buffered);
                send(NOTE, swap_proto::NOTE_BROKER_DONE, buffered, 0);
                user_rt::exit()
            }
            _ => {
                reply(slot, swap_proto::BAD_REQUEST, 0);
            }
        }
    }
}

user_rt::panic_handler!();
