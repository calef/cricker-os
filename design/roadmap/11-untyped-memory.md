# 11. Untyped memory, and the number that proves the kernel stops allocating

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). The one early milestone that was not in the
first commit's plan: `491f23d` (2026-07-14) added it to the table as "§10's deferred axis. The
allocators leave," and `9b089e0` built it the next day.

The commit's headline is a single measurement: a process mapped 23 pages out of an untyped it
was handed, and the kernel's used-frame count went 991 -> 991. A test asserts that equality, and
the commit records verifying the test catches the failure it exists for: with the kernel
allocator as the source instead, the same process maps 30,000+ pages and the kernel loses
30,000+ frames.

The mechanism, as first built: `Object::Untyped(region)` is a capability to a run of physical
pages with a bump watermark, and retyping a page charges the process's own untyped for the page
and for any page tables the mapping needs. Kernel-memory exhaustion by asking stops being an
attack class because a process cannot make the kernel allocate.

This was the last of the eleven; the security audit (`d82a7ce`), the DMA-confinement work, and
everything the roadmap now tracks came after, and milestone 14 finished what this one started by
removing the kernel heap outright.
