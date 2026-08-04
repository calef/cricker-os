# 7. Testing: QEMU harness + host-testable crates, from commit one

**Status: DECIDED.**

A custom test harness boots the kernel in QEMU, runs tests, and exits with a status code
`cargo test` understands. Separately, pure logic (allocator algorithms, page-table math,
scheduling policy, filesystem parsing) lives in crates that compile for the *host*, so
most tests run in milliseconds with no emulator.

Front-loads about a day. Prevents a year of debugging by `println!`.
