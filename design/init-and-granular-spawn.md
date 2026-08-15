# Milestone 19: the init task, granular process construction, and the first real workload

The working plan for milestone 19 (design/roadmap/19-real-workload.md), and the record of its first decision,
made 2026-07-24.

## Decision 1: how init builds a process, granular, eyes open

Two shapes were on the table. **Composite spawn**: init names a budget and an image, and the
kernel runs its one proven build recipe (B.4's `exec`) paid from init's untyped; two or three
new syscalls, no half-built states, the ELF parser stays in the kernel. **Granular** (seL4's
shape): the kernel exports construction primitives and init parses ELFs and assembles processes
itself; the parser leaves the trusted computing base, at the cost of the widest API this kernel
has grown, designed today with one caller.

The recommendation was composite-first on sequencing grounds (every deferred-until-a-customer
mechanism in this project has come out better for waiting: revocation's tree, the CDT, RETYPE's
object types). calef initially chose granular to keep the option of running Linux programs;
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
  borrowing principle worth keeping: **we borrow seL4's guarantees, not its shapes**, seL4's
  mapper-pays flavor is a corollary of explicit page-table objects we deliberately never
  adopted, and a per-call override stays additive if a real customer ever appears. User-built
  spaces sit in a generational registry as full `AddressSpace` values (ASID-tagged, revocation-
  registered, region-pinned), immortal until 19c designs teardown, their dormant `Drop` noted as
  a 19c audit item. Witnessed at both levels: kernel (the walker sees the mapping with exact
  flags, `revoke_frame` reaches into the built space, the pinned region's destroy frees
  nothing) and EL0 (a process retypes a space and a frame from its own budget, maps one into
  the other, and break-before-make holds inside the space it built).
- **19c.1: kernel stacks move to the kernel's own budget. (Built.)** The kernel-stack payer
  question, decided across three rounds (notes/kernel-budget.md): not creator-paid-per-process
  but kernel-budget-paid from a boot-carved `kmem` region with recycling, because a thread
  cannot swap the stack it runs on, so every kernel stack is kernel-created and one budget
  covers all. This extends milestone 14's no-open-ended-kernel-spending thesis to its last
  uncovered draw; a steady-state test proves the frame count is flat across spawn/reap. The
  owned-vs-borrowed `KernelStack` split feared in the debate turned out to be zero lines: one
  owner. Split out as its own green step before TCB objects, so stack-sourcing and embryo TCB
  states are not two delicate changes in one breath.
- **19c.2: TCBs become page-resident; the static pool deleted. (Built.)** The "where do we
  want to end up" question reversed the first recommendation: not a pooled TCB but page-resident,
  no static pool, kernel TCBs from `kmem` and user TCBs (19c.3) from the creator's untyped. B.2
  itself scheduled this ("the pool upgrades to retype-backed storage when init lands"). Pure
  storage rework, behavior-preserving; the generational table already stored names not addresses,
  so it was small.
- **19c.3: TCBs as objects. (Built.)** `RETYPE_OBJ(TCB)` (page-resident, the creator's untyped
  pays and is pinned), plus `CONFIGURE` (bind an aspace, consumed out of the 19b registry into
  the TCB so it now dies with the thread; set entry and user stack), `CAP_INSERT` (GRANT-gated,
  narrowing, the child's initial authority one grant at a time), and `START` (arm the kernel
  stack via 19c.1, build the EL0 entry context, queue). A new `State::Embryo` and a
  `user_entry_trampoline` (the EL0 mirror of `thread_trampoline`) carry it. **The half-built
  audit, discharged:** no start before whole (`START` refuses an embryo with no space or no
  entry), queue discipline (an embryo is in no queue and `START`-twice is refused), and a
  user-built thread reaps cleanly through the existing reaper (its region-owned TCB page and
  pinned aspace region stay for object revocation, the documented debt). Witnessed by a kernel
  test that builds a child entirely from the four verbs, with a hand-assembled EL0 stub for
  code, and receives the word the child SENDs through a capability it was granted: a thread no
  `spawn` created, running code no wiring wrote.
- **19d.1: init parses a real ELF in userspace and builds a running child. (Built.)** The `elf`
  crate links into the user binary; `spawn_init` (the one program the kernel still loads) hands
  init the initrd mapped read-only, a building untyped, and a report endpoint. init's `build_child`
  mirrors the kernel's `map_segments` entirely through the granular verbs: retype an aspace, copy
  each segment into retyped frames and `MAP_INTO` the child (a new `MAP_CODE` mode + kernel
  I-cache sync for executable pages), retype and endow a TCB, configure, start. The child runs
  code the kernel never parsed and reports home. `SYS_CAP_DELETE` was added (a loader recycles a
  16-slot cspace over hundreds of frames); `START` gained an initial-`x0` so init tells a child
  its role. Witnessed end to end: `userspace_init_parses_an_elf_and_builds_a_running_child`, four
  clean runs. See notes/init-and-loading.md.
- **19d.2: init becomes the boot path, incrementally. (In progress.)** Migrating service
  construction into init, one service green before the next. The first step surfaced the real
  blocker: every boot service needs *device* access, which the kernel wired at spawn with no
  delegatable authority. So this splits:
  - **19d.2a: device capabilities. (Built.)** `Object::DeviceFrame(phys)`, a delegatable
    authority to map a specific device's MMIO **device-typed** (nGnRnE, uncacheable: a plain
    `Frame` maps normal cacheable memory, which would corrupt MMIO). `MAP_INTO` accepts it. The
    kernel mints one per known device (the UART) and grants it to init; init delegates it into a
    driver child it builds and maps the registers in. Witnessed: an init-built driver reads the
    PL011's PrimeCell id (`0xB105F00D`), proving the delegated device-typed mapping is a real view
    of the actual hardware. This turns "the kernel maps the UART" into "device access is a
    capability", the mechanism every real driver-under-init needs.
  - **19d.2b: the delegatable authorities a driver needs, each green.** The point of 2b turned
    out to be proving init can delegate *every kind* of authority a driver needs, not re-loading
    every driver. **Three, all built:**
    - **Endpoints + device MMIO + an IPC protocol (the console).** init constructs the real
      console print server, wires it a request/reply channel and a shared page it created and the
      UART it delegated, then plays the client: writes a line, the server prints it to the real
      UART (in the log) and acks, init reports the length. `build_child` was generalized to
      caps-to-insert + pages-to-map, the shape every init-built service uses.
      (`userspace_init_brings_up_the_console_server`.)
    - **Interrupt capabilities (the last authority kind).** init holds an Irq cap for a routed
      test interrupt, builds a child, delegates it the cap, starts it; the child blocks in `WAIT`,
      the interrupt fires, and it is delivered as a message through the delegated capability.
      (`userspace_init_delegates_an_interrupt_to_a_child`.) Bug found and fixed: the route must be
      set up *before* the child could receive, and an interrupt that fires unrouted is dropped, not
      queued -- so `spawn_init` routes before spawning init, and the pending-signal count carries
      the early fire to the child's later `WAIT`.

    **The two full drivers that remain, and their honest blockers (not 2b work):**
    - **Input** is driven by host keystrokes the automated harness cannot inject, so it is built
      and validated in the interactive boot (19d.2c), not an isolated test. Its mechanism (IRQ +
      device MMIO delegation) is exactly the two proved above.
    - **Virtio** needs the driver to know its DMA region's *physical* address, but a process only
      knows virtual addresses; disclosing a frame's physical address for DMA is milestone-16
      (IOMMU/DMA) territory, and `START` would also need to pass more than one initial argument.
      Deferred to land with 16.
  - **19d.2c: init becomes the boot path, whole interactive system. (Built.)** Behind the
    `initboot` feature the kernel stops wiring services and hands the machine to init
    (`boot_via_init` -> `spawn_init`). init then builds the **entire interactive system** out of
    its own budget: the console server, the input driver (on the UART receive interrupt init
    delegated), and the shell, wired together with endpoints and shared pages init created, plus
    init staying alive as a stub spawn service. A bounded `script/initboot` boot reaches the
    live shell prompt (`nife shell ... $`), every service an init-built child; the kernel
    wires none of it. Interactive typing is validated by hand (the harness cannot inject
    keystrokes); the default, shell, bench, and test boots are unchanged, 105 tests still pass.

    Two real bugs fixed in the bring-up, both recorded: **stack size** -- init loads whole ELFs
    with deep call chains and the shell is a big program, so one page overflows; init now gets 8
    stack pages and each child 4. **Scratch collision** -- `build_child` mapped each child's
    frames into init's own scratch window to fill them, resetting the scratch VA per call, but
    init never unmaps, so the second child collided with the first's mappings; a persistent
    global scratch bump fixes it. The console-only core hid both because it built one child.
- **19f: dedicated binaries, delivered as named programs. (Planned.)** Everything through 19d.2 is
  still *one* multi-tool binary (`hello`) whose programs are roles selected by `x0`; init, the
  child, and the console server are the same ELF loaded more than once. A real system has a
  distinct binary per program, each exactly its own job (and thus its own least authority). This
  needs a **program-delivery mechanism**, not just a rebuild: the kernel hands init a single initrd
  blob today, so delivering several named programs wants either a bundled archive (Linux-style
  initramfs the kernel hands init, which init indexes by name) or loading from the `nifefs`
  filesystem on the virtio disk (programs as files, `exec`'d by path, like Unix `/bin`). The
  filesystem route carries the classic bootstrap: reading the disk needs the virtio driver running,
  which is itself a program to load, so a small initramfs brings up init plus the disk drivers,
  which then mount the disk and load the rest. This is where "console server as its own binary"
  lands, and it makes per-program least authority real. Prior art: Linux initramfs (cpio) then
  pivot to the real root; every Unix's `/bin` plus an `exec` by path.
- **19e: the workload.** Decision 2 (what runs first, native ABI) happens here, against a
  system that can actually run it.

## What stays deliberately unbuilt

No CNode trees, no derivation tree, no sub-page object packing: unchanged deferrals. The
kernel keeps its one-binary boot loader (for init) permanently; a kernel that can load nothing
cannot boot to userspace at all.
