use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

const ROLE_MAKER: u64 = 17;
const ROLE_USER: u64 = 18;

/// Spawn the pair; returns the report endpoint carrying the word that crossed the minted
/// endpoint.
pub fn wire(image: &'static [u8]) -> EpId {
    let channel = crate::sched::create_endpoint();
    let report = crate::sched::create_endpoint();
    let region = crate::untyped::create(4).expect("no region for the maker's budget");

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_MAKER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(region),                  // slot 0: the budget to mint from
                    endpoint_cap(channel, Rights::WRITE), // slot 1: delegate the mint here
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the endpoint maker");

    crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_USER,
                arg1: 0,
                arg2: 0,
                grants: &[
                    endpoint_cap(channel, Rights::READ), // slot 0: receive the delegation
                    endpoint_cap(report, Rights::WRITE), // slot 1: report the word
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the endpoint user");

    report
}
