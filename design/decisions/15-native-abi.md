# 15. The native ABI: formalize the convention, defer the BootInfo (milestone 19e)

**Status: DECIDED.**

Decided 2026-07-25, at milestone 19e, recorded as "Decision 2" in design/init-and-granular-spawn.md, against a
system that could finally run and deliver distinct programs (19f). The full contract is written up in
notes/abi.md; this records the decision and why.

§10 already settled the model (capability-based, not Unix). What 19f forced open was the smaller
question: what is the contract between a program and the system? The syscall convention (`svc`, four
numbers, everything through `SYS_INVOKE`) and the object surface were already built and stable. The
one genuinely open piece was **how a program meets its initial capabilities and arguments**.

**The decision: write down the convention we already run, and do not build a self-describing
environment yet.** A program is entered at `_start(x0, x1, x2)` with its cspace pre-populated by its
loader at slots the program hardcodes, per a contract published in that program's own source.

Rejected for now: a **BootInfo** page (seL4's model), a structured block the loader hands the program
describing its capabilities and arguments, so the program *discovers* its world instead of assuming a
layout. Not rejected because it is bad; it is the right tool. Rejected because it is a mechanism
without a requirement here: init builds every program and knows every layout, so out-of-band
agreement between one parent and its own children is sufficient, and it is exactly what seL4 does for
every task below its root. BootInfo earns its place when a loader must start programs it did not build
and whose layout it cannot know, which is milestone 23 (live component replacement), with competing
vendors. Building it now would be an abstraction ahead of its requirement, which rule 3 and the §5
asymmetry both warn against.

The coupled half, **what runs first**: a native compute workload (CoreMark), because the disk is
still blocked (milestone 16) and compute is the honest "real workload" a program can do now. Native,
not Linux-compat: §10 records that a POSIX shim is *additive* and can come later without a rewrite, so
there is no reason to pay for it before running something native.
