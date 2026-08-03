use crate::sched;

#[test_case]
fn the_suite_left_no_runnable_thread_spinning() {
    // Quiesce on the CLOCK, not on a yield count. A Finished thread is reaped when its OWN core
    // switches away from it, and DECISIONS §28 scatters threads across cores, so yields on this
    // core do not make a remote core reap: two hundred cheap yields can all complete before
    // another core's next timer tick. Give every core a couple of ticks to get there, then judge.
    // What stays runnable after that is still a genuine leak (a thread that never blocks and
    // never exits), so this tolerates cross-core lag without masking the thing it guards.
    let deadline = crate::arch::timer::now() + 2 * crate::arch::timer::frequency();
    while crate::arch::timer::now() < deadline {
        // Scoped so nothing from `current()` is held across the yield.
        if {
            let me = sched::current();
            sched::runnable_non_idle_count(&me)
        } == 0
        {
            break;
        }
        sched::yield_now();
    }
    let me = sched::current();
    let leaked = sched::runnable_non_idle_count(&me);
    if leaked != 0 {
        sched::dump_threads();
    }
    assert_eq!(
        leaked, 0,
        "{leaked} thread(s) are still runnable after the suite quiesced: a test spawned a \
         thread that never exits. A leaked spinner starves later heavy tests past the watchdog; \
         make the one-shot role exit() after it reports instead of looping forever.",
    );
}
