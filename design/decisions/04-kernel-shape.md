# 4. Kernel shape: monolithic, deferred, with two cheap rules

We are NOT speculatively trait-ifying every subsystem to "keep the microkernel door
open." That builds the wrong abstraction before the requirements are known, and taxes
every file for a door we may never walk through.

Instead, two rules that cost almost nothing and preserve the real option:

1. **A driver never reaches into a kernel global.** It gets what it needs passed in.
2. **The syscall surface stays narrow and explicit.** It is a boundary, not a habit.
