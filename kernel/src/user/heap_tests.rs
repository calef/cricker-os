use super::*;

/// The full `alloc` surface holds on a real budget: collections allocate, free in arbitrary
/// order, and freed memory is reused rather than leaked. The workload asserts each value
/// internally and faults on any lie, so this test's magic-word check is the "it all ran"
/// bit; the committed count proves the heap both grew (it allocated more than zero pages)
/// and stayed inside its own 64-page cap, i.e. growth is demand-driven, not budget-eating.
#[test_case]
fn a_process_runs_alloc_collections_on_its_own_untyped() {
    let image = program("allocator_exerciser")
        .expect("no allocator_exerciser program in the initrd archive");
    let report = alloc_service::start(image);
    let words = crate::sched::ipc_recv(report);
    assert_eq!(
        words[0], 0xA110_C0DE,
        "allocator_exerciser did not complete its heap workout",
    );
    let committed = words[1];
    assert!(committed > 0, "the heap never grew: nothing was allocated?");
    assert!(
        committed <= 64 * 4096,
        "the heap grew past its own cap: growth policy is broken",
    );
    assert!(
        committed.is_multiple_of(4096),
        "committed bytes must be whole pages",
    );
}
