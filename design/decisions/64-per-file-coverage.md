# 64. A per-file coverage number counts where tests are written, not what they reach

`user/src/swish.rs` had **zero** `#[cfg(test)]` blocks, and that was first reported here as "the
shell is untested". It was not. The shell was covered by ~28 QEMU integration cases driving the real
binary, and by 93 host tests in `crates/grant_plan`, which already held its parsing and navigation.

0% was a true fact about a **file** and a false claim about a **component**, and the milestone it
prompted came out smaller and differently shaped once that was checked.

This matters here more than in most codebases, because this project deliberately splits logic from
IO across crate boundaries (§63 above). That split is what makes per-file coverage misleading: the
tests live in the crate, the file that has none is the one holding the syscalls, and a metric that
counts per file will always report the healthy arrangement as the broken one.

Coverage is still worth measuring. What is not worth doing is reading a per-file number as a
statement about a subsystem without first asking which file the tests would be in if the design were
right.
