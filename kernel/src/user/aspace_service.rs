use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

const ROLE_BUILDER: u64 = 19;

/// Spawn the builder; returns the report endpoint carrying its verdict bits.
pub fn wire(image: &'static [u8]) -> EpId {
    let report = crate::sched::create_endpoint();
    let region = crate::untyped::create(8).expect("no region for the builder");

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_BUILDER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(region),                 // slot 0: the budget
                    endpoint_cap(report, Rights::WRITE), // slot 1: the verdict
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the aspace builder");

    report
}
