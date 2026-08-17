# 112. The SAFETY comments that bind nobody

**Status: BUILT** 2026-08-05 (pull request #120, merge `4904cce0`). Raised 2026-08-04 by milestone
82's lane, which set out to burn down `unsafe fn` violations, found **zero** of them, and reported this
instead. Recorded in `notes/unsafe-obligations.md`'s BUGS. It is a change to the kernel's soundness
surface, which is why 82 deliberately did not take it on a lint milestone. The status read
`IN-PROGRESS since 2026-08-04, a developer holds it on milestone/112-safety-comments` for twelve days
after that merge; found 2026-08-17 by the status-accuracy sweep. §76's defect class.

**Three of the four converted, and the fourth was decided rather than skipped**, which is what the
deleted gate line asked for when it said the answer may not be four conversions. `write_satp` is now
`unsafe fn` (`kernel/src/arch/riscv64/mmu.rs:80`), and its `# Safety` section says out loud that it is
unsafe because aarch64's `set_ttbr0` is, which closes the ISA asymmetry in the direction of the
stricter ISA. `switch_user_root` is `unsafe fn` on both ISAs
(`kernel/src/arch/aarch64/mmu.rs:913`, `kernel/src/arch/riscv64/mmu.rs:796`). `stack::paint` is
`unsafe fn` (`kernel/src/stack.rs:436`), and its sibling `high_water` went with it. **`virtio::pread`
stayed a safe fn on purpose**, with the argument written where a reader meets it
(`kernel/src/virtio.rs:233`, "Why these two stay safe fns"): it is private, all twenty call sites are
one `impl Transport` block, and the compiler closes the caller set, so the obligation really is
discharged by the module rather than onto nobody.

**The call sites carry the cost the block predicted.** `kernel/src/sched.rs:1477` is the context
switch, and its SAFETY comment argues liveness from `on_cpu` and `Running` after `SCHED` is released
rather than restating the signature; `kernel/src/sched.rs:2827` carries its own separate argument.

**One gate did arrive, adjacent to the headline property rather than on it**: `script/lint` now checks
that every `unsafe fn` states its contract in a `# Safety` section. It checks that a contract is
*written*, never that it is true, and it found one violation on its first run (`fs_server`'s
`file_page`, fixed in the same pull request). The decision table is notes/unsafe-obligations.md:250.

**The finding.** Four **safe** functions carry a `// SAFETY:` comment that discharges an obligation
onto "the caller", and their signatures impose that obligation on nobody. Any safe code may call
them without meeting it, and both `unsafe` lints are satisfied, because there is no `unsafe fn` and
no undocumented block for either to fire on.

| Site | The comment's claim |
|---|---|
| `kernel/src/virtio.rs:233` `pread` | "the caller passes addresses inside a device-mapped BAR or mmio window" |
| `kernel/src/stack.rs:121` `paint` | "the caller hands us a mapped, unused stack region" (`#[cfg(test)]`, so test builds only) |
| `kernel/src/arch/aarch64/mmu.rs:843` `switch_user_root` | "the caller passes either a live `AddressSpace`'s composed value or ..." |
| `kernel/src/arch/riscv64/mmu.rs:48` `write_satp` | "the caller guarantees `satp` names a well-formed Sv39 root" |

**The last one is an ISA asymmetry**, and it is the sharpest argument in the set. aarch64's
equivalent, `set_ttbr0`, **is** an `unsafe fn`. The same register write, the write that installs a
user address space, is a contract on one architecture and an ordinary call on the other. Rule 5 says
a capability ships on every supported architecture or a scope note records the gap; here the *rule
about the code* differs by ISA, which is the same defect one level up.

**Why 82 did not just fix it.** Turning these four into `unsafe fn`s puts an `unsafe` block and a
real SAFETY comment at **every call site, including the context switch**. That is the hottest path in
the kernel and the place where an added obligation is most likely to be discharged by ritual rather
than by thought. It is a change to what the kernel promises about itself, so it earns a review of its
own rather than a ride on a lint milestone.

**Not every "caller" in a SAFETY comment is this**, and the milestone has to tell them apart or it
will churn a dozen sites for nothing. `sched.rs`'s `ipc_call` and `user_rt`'s `cap_delete` mean the
calling *thread* and the calling *process*; `interrupts::enable` says outright that the operation is
sound and only the timing is the caller's problem. **The pattern to look for is a safe fn that would
be unsound if its own sentence were false.**

**The related finding, which is the reason to read the whole set rather than the four.** Of the 33
`unsafe fn`s in the tree, **eleven contain no unsafe operation at all**. Their unsafety is a contract
about *meaning*, not a memory operation a compiler can point at: `set_ttbr0` is an `unsafe fn`
because writing `TTBR0_EL1` is the most consequential thing in the kernel, and `aarch64-cpu` hands
that write over as a safe call. `Clock::new` takes a virtual address the caller promises is a mapped
clock page. `assume_no_stale_entry` is named for its contract. For those eleven the lint composition
buys nothing, and **the rustdoc `# Safety` section is the only enforcement there is**, for a third
of the tree's unsafe functions. That is worth knowing before deciding how many of the four to
convert, because conversion moves a site *into* that same category rather than out of it: an
`unsafe fn` with a contract nothing checks is better than a safe fn with a contract nothing checks,
and it is not a proof.

## Scope note

**Four sites, and the answer may not be four conversions.** `stack::paint` is `#[cfg(test)]` and its
caller is one loop in `smp.rs`; `write_satp` has the parity argument and probably converts;
`switch_user_root` and `pread` are the ones where the call-site cost is real and the decision is a
judgement. Decide per site with an argument, the way milestone 78 decides per assertion, rather than
producing a rule.

**No new gate is proposed here.** Neither lint can read a comment, and a lint that spots "SAFETY on a
safe fn" would fire on the legitimate uses above. If the survey finds the distinction mechanical
after all, that is a finding for `DECISIONS` §61's ledger (a lint is adopted on evidence from this
tree), not an assumption to start from.
