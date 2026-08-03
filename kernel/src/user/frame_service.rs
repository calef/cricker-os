use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

const ROLE_PRODUCER: u64 = 11;
const ROLE_CONSUMER: u64 = 12;

/// Spawn the pair, each with its own untyped budget, and return the endpoint the consumer
/// reports its verdict on. Eight pages of untyped apiece covers one frame plus the page tables
/// each side needs to map it.
pub fn wire(image: &'static [u8]) -> EpId {
    let channel = crate::sched::create_endpoint();
    let report = crate::sched::create_endpoint();
    let prod_ut = crate::untyped::create(8).expect("no untyped for the frame producer");
    let cons_ut = crate::untyped::create(8).expect("no untyped for the frame consumer");

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_PRODUCER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(prod_ut),                 // slot 0: retype the frame + page tables
                    endpoint_cap(channel, Rights::WRITE), // slot 1: delegate the frame
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the frame producer");

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_CONSUMER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(channel, Rights::READ), // slot 0: receive the frame
                    untyped_cap(cons_ut),                // slot 1: page tables for its mappings
                    endpoint_cap(report, Rights::WRITE), // slot 2: report the verdict
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the frame consumer");

    report
}
