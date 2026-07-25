# Milestone 19: the init task, granular process construction, and the first real workload

The working plan for milestone 19 (design/roadmap.md), and the record of its first decision,
made 2026-07-24.

## Decision 1: how init builds a process — granular, eyes open

Two shapes were on the table. **Composite spawn**: init names a budget and an image, and the
kernel runs its one proven build recipe (B.4's `exec`) paid from init's untyped; two or three
new syscalls, no half-built states, the ELF parser stays in the kernel. **Granular** (seL4's
shape): the kernel exports construction primitives and init parses ELFs and assembles processes
itself; the parser leaves the trusted computing base, at the cost of the widest API this kernel
has grown, designed today with one caller.

The recommendation was composite-first on sequencing grounds (every deferred-until-a-customer
mechanism in this project has come out better for waiting: revocation's tree, the CDT, RETYPE's
object types). Chris initially chose granular to keep the option of running Linux programs;
that reason was pushed back on and withdrawn, because **both shapes preserve that option** (the
compat personality, when it exists, gets the primitives built against its real requirements
either way). The decision was then re-made on the corrected premise, and granular won on its
honest merit: **evicting the ELF parser from the trusted computing base this milestone**, the
small-kernel thesis applied strictly, accepted with its costs in view:

- a wide surface designed against one caller (init), with the compat personality's real needs
  still unknown;
- half-built processes become representable kernel states, and every invariant proved on the
  assumption that processes appear whole gets re-audited;
- a longer road to the first running workload.

Recorded so the day the compat personality arrives and wants something different, the reasoning
that got us here is legible rather than mysterious.

## The surface (sketch: each operation is its own design conversation when its phase arrives)

```text
Untyped::RETYPE_OBJ (type)      endpoint | address space | TCB, out of the caller's untyped
AddressSpace::MAP_INTO          map a frame into ANOTHER space; page tables paid by a
                                caller-named untyped (the frame::MAP precedent: a2 names it)
Tcb::CONFIGURE                  entry, stack pointer, bind an address space
Tcb::CAP_INSERT                 install a capability into the child's cspace (GRANT-gated)
Tcb::START                      make it runnable; refuses an unconfigured TCB
```

Naming leans on what milestone 14 built: a TCB capability carries a generational Tid (stale
names fail safely, the D2 path's payload step arriving on schedule); an address-space capability
names the space through the same registry revocation uses.

## The invariants to re-audit (the cost we accepted, itemized)

- **No start before whole.** `START` on a TCB with no address space or no entry must refuse;
  the states are new, the refusal must be proved reachable-only-as-refusal.
- **Teardown of the half-built.** A TCB configured but never started, an address space with
  mappings but no thread: each must die cleanly through the existing reaper/destroy paths.
- **Budgets under MAP_INTO.** The child's page tables come from a named untyped; exhaustion
  mid-build must strand nothing unreclaimable.
- **The queue discipline.** An unstarted TCB is in no queue and must be unreachable by wake.

## The phases (each green before the next)

- **19a: `RETYPE_OBJ(ENDPOINT)`. (Built.)** The decision its arrival forced: endpoints are
  **page-resident, one object per page** (sub-page packing examined on challenge and declined:
  it saves immaterial memory, forks the memory rule per object type, and buys back occupancy
  machinery exactly when endpoint revocation arrives; it remains a placement optimization
  behind the registry). All endpoints, the kernel's included, now live at the start of a page
  retyped from some untyped region, named generationally (`crates/slots`, the Tid machinery),
  and their host regions are **pinned**: `destroy` refuses them, the recorded debt that
  endpoint revocation will one day retire. Witnessed at both levels: a kernel test (rendezvous
  over a retyped endpoint; a pinned region's destroy provably frees nothing) and an EL0 test in
  which one process mints an endpoint from its own budget, delegates a READ view to a stranger,
  and a word crosses an object no kernel wiring created. One incident for the record: the first
  version wired the demo roles to constants 13/14, already taken by other demos; the compiler
  said "unreachable pattern" at every build, the test pipeline swallowed it, and an hour of
  kernel archaeology followed before running `script/lint`, which fails on exactly that
  warning, would have named it instantly. Lint first, then instrument.
- **19b: address spaces as objects. (Built.)** The retyped page **is the L0 root**, and the
  budget question this phase carried was decided against this doc's own sketch, on challenge:
  `MAP_INTO` does **not** take a per-call untyped. The untyped a space is retyped from becomes
  its backing region, paying for tables and revocation records exactly as an exec-built space's
  does (B.4), so one budget model covers every space and §13 revocation works unmodified. The
  challenge ("isn't per-call seL4's way, and don't we borrow from seL4?") sharpened the
  borrowing principle worth keeping: **we borrow seL4's guarantees, not its shapes** — seL4's
  mapper-pays flavor is a corollary of explicit page-table objects we deliberately never
  adopted, and a per-call override stays additive if a real customer ever appears. User-built
  spaces sit in a generational registry as full `AddressSpace` values (ASID-tagged, revocation-
  registered, region-pinned), immortal until 19c designs teardown, their dormant `Drop` noted as
  a 19c audit item. Witnessed at both levels: kernel (the walker sees the mapping with exact
  flags, `revoke_frame` reaches into the built space, the pinned region's destroy frees
  nothing) and EL0 (a process retypes a space and a frame from its own budget, maps one into
  the other, and break-before-make holds inside the space it built).
- **19c: TCBs as objects.** `RETYPE_OBJ(TCB)`, `CONFIGURE`, `CAP_INSERT`, `START`, and the
  half-built-state audit.
- **19d: init.** The ELF parser moves to userspace (the `elf` crate already compiles anywhere;
  init links it directly — the eviction that motivated this decision). `user.rs`'s service
  construction migrates into init; the kernel's own loader shrinks to loading exactly one
  program: init itself.
- **19e: the workload.** Decision 2 (what runs first, native ABI) happens here, against a
  system that can actually run it.

## What stays deliberately unbuilt

No CNode trees, no derivation tree, no sub-page object packing: unchanged deferrals. The
kernel keeps its one-binary boot loader (for init) permanently; a kernel that can load nothing
cannot boot to userspace at all.
