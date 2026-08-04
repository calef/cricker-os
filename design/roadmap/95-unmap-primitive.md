# 95. An unmap primitive, and the mappings init never lets go

**Status: NOT-STARTED.** Raised 2026-08-04 from milestone 22's closing lane, which named it as the
largest residual left standing after the interactive boot gave away its authority.

**Gate: DECISION.** An unmap method is a syscall-surface addition, which the block calls a design
fork for Chris before it is a task: whether unmap belongs on the address space or on the frame,
what it does to a mapping another holder also has, and whether restructuring the loader to map one
page at a time avoids the new method entirely.

**The finding.** `build_child` maps each page it lays down for a child into init's own address
space to write it, and **never unmaps it**, because nothing in the ABI can: there is no unmap.
Reaping a job hides the problem for jobs, since §13's revoke takes mappings with the region, but
the boot servers are never reclaimed, so init keeps a writable window onto every page of
every server it built, for the life of the machine.

That is a real hole in the story milestone 22 otherwise tells. Init drops its construction budget,
its device capability and its interrupt, and the kernel itself confirms `RETYPE` now answers
`NoSuchSlot`; and then init can still write into the filesystem server's text. The lane recorded
it rather than shrinking a budget to make the number look better, which is the right call and
leaves the work here.

**What this milestone is.** A way for a holder to give up a mapping it made: an unmap method on
the address-space object, symmetric with the map that created the window. That is a syscall
surface addition, so it is **a design fork for Chris before it is a task** (CLAUDE.md: a new
method is fine within the model, but its semantics are recorded in `design/decisions/`, and a brand-new
syscall number is a fork). The questions the fork has to answer: whether unmap is a method on the
address space or on the frame capability; what it does to a mapping some other holder also has
(§13 already decides revoke's answer, and this must not contradict it); and whether unmapping is
enough or the scratch window wants a narrower shape, mapping one page at a time and releasing it
before the next, which needs no new method at all if the loader is restructured instead.

That last possibility is why this is not obviously a syscall: **the cheapest fix may be a loader
that never holds more than one page**, and the measurement that decides it is how much slower a
one-page-at-a-time loader boots.

## Scope note

The proof, when it happens, is the shape milestone 22 already used: init writes to a boot server's
page and faults, as a negative control, rather than an inventory of what init holds. Until then
the residual is recorded where a reader meets it, in notes/trusted-init.md's BUGS.
