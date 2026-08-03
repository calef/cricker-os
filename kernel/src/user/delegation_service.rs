use super::*;
use crate::cap::{Rights, endpoint_cap};
use crate::sched::EpId;

const ROLE_GRANTER: u64 = 9;
const ROLE_RECEIVER: u64 = 10;

/// The word the receiver sends back through the delegated capability, so a test can confirm a
/// capability minted by one process works when invoked by another.
pub const USED_WORD: u64 = 0x5A;

/// Spawn the pair and return `(resource endpoint, report endpoint)`. The granter delegates its
/// `resource` capability (held `WRITE | GRANT`) to the receiver, narrowed to `WRITE`. The
/// receiver `SEND`s [`USED_WORD`] on the received capability (a `RECV` on `resource` collects
/// it) and reports a two-bit verdict on `report`.
pub fn wire(image: &'static [u8]) -> (EpId, EpId) {
    let channel = crate::sched::create_endpoint(); // granter SEND_CAP -> receiver RECV_CAP
    let resource = crate::sched::create_endpoint(); // the capability being delegated
    let loopback = crate::sched::create_endpoint(); // the receiver's refused re-delegation target
    let report = crate::sched::create_endpoint(); // the receiver's verdict

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_GRANTER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(channel, Rights::WRITE), // slot 0: SEND_CAP over it
                    endpoint_cap(resource, Rights::WRITE.union(Rights::GRANT)), // slot 1: delegate this
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the delegation granter");

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_RECEIVER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(channel, Rights::READ),   // slot 0: RECV_CAP
                    endpoint_cap(report, Rights::WRITE),   // slot 1: report the verdict
                    endpoint_cap(loopback, Rights::WRITE), // slot 2: attempt re-delegation here
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the delegation receiver");

    (resource, report)
}
