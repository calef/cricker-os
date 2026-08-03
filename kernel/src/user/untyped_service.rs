use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

const ROLE_UNTYPED_DEMO: u64 = 7;

/// Carve `pages` of memory into an untyped region, hand it to a fresh process, and return the
/// region id, the endpoint the process reports on, and the thread it runs as. The kernel's ONE
/// allocation is the untyped itself; everything the process maps afterward spends that, not the
/// allocator.
///
/// **The `Tid` is returned because this process does not exit.** It reports its result and then
/// spins, deliberately, so that the free-frame count its caller reads is the measurement's
/// rather than a teardown's. That makes it a thread only the caller can end, and a caller that
/// drops the name has leaked a spinning thread onto every test that runs after it.
pub fn start(image: &'static [u8], pages: u64) -> Option<(u64, EpId, crate::thread::Tid)> {
    let region = crate::untyped::create(pages)?;
    let report = crate::sched::create_endpoint();

    let tid = crate::sched::spawn(move || {
        run(
            image,
            Spawn {
                arg0: ROLE_UNTYPED_DEMO,
                arg1: 0,
                arg2: 0,
                grants: &[
                    untyped_cap(region),                 // slot 0: the memory budget
                    endpoint_cap(report, Rights::WRITE), // slot 1: report the result
                ],
                maps: &[],
            },
        )
    })
    .expect("could not spawn the untyped demo");

    Some((region, report, tid))
}
