# 43. A second security audit, with a different lens

**Status: BUILT.** In two passes, and the record lagged the tree both times (§76's shape, the
third instance found in one week). The headline lens, time-of-check to time-of-use across the
shared pages, was executed and merged 2026-08-04: notes/shared-page-audit.md, seven findings,
their fixes cited as milestone 43 across the tree, while this status sat at NOT-STARTED for
eleven days. The further-lens pass ran 2026-08-15: notes/untrusted-input-audit.md, aimed at the
crates that landed after the first pass (nvme, mdns_proto, cred/ntlm), one finding (the NVMe
driver's panic-on-device-written-fields, recorded with its lane candidate), the rest cleared
with caveats. The remaining candidate lenses the block names below (capability-lifetime races,
the unsafe census) are each a whole audit and belong to §74's cadence, not to this block.

**In brief.** The first audit (notes/arch-audit.md) read the **assembly and arch layer** and found three real bugs: the `eret`/`sret` privilege-escalation staging race, a stale `tp` on S-mode trap return corrupting cross-hart per-CPU data, and the PLIC's lock-free read-modify-write. A second pass should deliberately NOT re-read that, and should take the surface that has appeared since. Headline lens: **time-of-check to time-of-use across shared pages.** Every service contract now moves bulk data through a page shared with the client (blk, file, gfx, compose, line_editor, net_stack), so a server that validates a length or an offset from the request word and *then* reads the page has a double-fetch window a malicious client controls; 19 files touch that pattern. Further lenses: integer overflow in the wire's size and offset arithmetic (`fs_proto` packs a 40-bit length, and `TRUNCATE` takes a size in the second word); capability lifetime races between revocation and an in-flight use, now that generational names, `Untyped::DESTROY` and `Endpoint::REAP` all reclaim; and a census of the **804** `unsafe` occurrences, triaging which carry a stated safety argument

**Why it matters.** **the attack surface roughly doubled after the first audit was written**: the compositor's shared surfaces, the C seam, the reap right, `std::fs`/`std::net`, and the FS service all arrived afterwards. The first audit's value came from reading for a *pattern* rather than waiting for a failure (it found the PLIC race that way), so the return on a second pass depends entirely on choosing a lens the first one did not use. Double-fetch is that lens: it is invisible to every gate we run, because both the check and the use are individually correct
