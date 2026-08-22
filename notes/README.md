# Concept notes

*Name: ratified (§75 covers this directory). `notes` predates every convention here and stays for the reason `elf` and `dtb` stay: it is the plain word for what the files are, and renaming it would spend a reader's recognition to buy nothing.*

Running glossary for nife. Written as concepts come up, not up front. If something
in the code or the conversation doesn't make sense, it belongs here.

## Start here

- [**Acronyms**](acronyms.md): every one this project has thrown at you, expanded, with a
  link to the note that explains it properly. IRQ, GIC, PMR, ESR, TTBR, PXN, DAIF, BBM, and
  the forty others. Look here first.

## Tooling

- [QEMU](qemu.md): the software computer we develop on. Why we need it, what the `virt`
  machine is, what each flag does.

- [Semihosting](semihosting.md): how the kernel asks QEMU to exit with a status code, so
  that `cargo test` can read it. Also: it's a syscall ABI where the OS on the other side is
  the emulator, which makes it a preview of milestone 7 running backwards.

- [The `script/` entry points](scripts.md): the "Scripts to Rule Them All" front door:
  `setup`, `test`, `server`, `console`, and friends, thin wrappers over `cargo xtask` so every
  repo has the same first command. Also: why `script/` and `scripts/` both exist.

- [The merge queue, and the two things that watch it](merge-queue.md): `scripts/merge-drain.sh`
  lands every pull request that does not need calef; `scripts/trunk-health.sh` says when `main` goes
  red and when it recovers. Both exist because three duties on 2026-08-04 belonged to whoever
  happened to notice, and the steward that was supposed to cover them reported without acting. Why
  the drain is deliberately serial (`cpu matrix` is load-sensitive, so parallel updates manufacture
  their own failures), and why the prevention half is a GitHub rule rather than either script.
- [Counted claims](counted-claims.md): a number in the prose is a claim, and unlike a name it is one
  a machine can check. The `<!--count:NAME-->` marker, the registry of derivations in `script/lint`,
  and why it is a ratchet rather than a sweep. Three claimed counts were tested against the tree and
  all three were wrong, two of them disagreeing with each other as well; the gate's own first run
  found three proof harnesses that nothing was proving. Name provisional.
- [The register of measures](register-of-measures.md): milestone 134. Which numbers this kernel
  holds itself to, which it merely knows, and which it has defined and cannot yet take. The test
  for what belongs (something depends on its value, and it moves without anybody editing it), the
  three states, and the deliberate exclusions with the half of the test each one failed. Carries
  the unsafe census and the `count-at-most` ceiling it needed: outside `kernel/src/arch/` the raw
  count quadrupled while the density more than halved, so the ceiling holds a ratio. Name
  provisional.

- [Citations that name what they cite](citations.md): why a footnote in this tree carries a name
  and not just a number, and what `script/citations` can and cannot prove about it. The two older
  gates check that `§N` resolves to *some* decision; this one checks it resolves to the one the
  author meant. Twenty-eight comments credited the `^C` work to a milestone about an aarch64 board
  and no gate could see it.

- [Naming things](naming.md): components, crates, scripts, branches, and which document goes
- [Cobble, the mascot](mascot.md): the name, its claim, the held-not-embedded ruling, and the
  alternatives set aside. Drawn by Clay.
  where. Why nothing here is named `-d` any more (DECISIONS §39), why `§N` and "milestone N" are
  different numbers over the same integers (it has already produced a wrong citation), and
  which four of these conventions `script/lint` checks. The jargon half cannot be checked:
  `linedisc` would have passed every rule.

- [The untracked-work sweep, and what each finding became](untracked-work-sweep.md): milestone 94's
  one-time read of the tree for work somebody had identified and never given a home, and the state
  each finding ended in. Twelve became milestones 100 through 111, three folded into blocks that
  already owned them, ten were already tracked, and nine are recorded-accepted and blessed where
  they sit. Also why `git grep -w TODO` is 82% false positives here, which is what shaped the lint,
  and an honest BUGS entry about the record that had to be re-derived because it lived in a pull
  request body.

- [Handing a session over](session-handoff.md): what a fresh context needs to pick this up:
  the standing autonomy grant, the merge and lane discipline, which gates are cheap and which
  are slow, and the traps that have cost real time more than once (leaked QEMU, fixed-iteration
  waits that measure host load, ceilings that grow with the suite rather than the system).

## Devices

- [The device tree](device-tree.md): the machine describing itself. Everything in it is
  big-endian, and the width of an address is declared by the *parent* node. Those are the
  two things most likely to be silently wrong.
- [ISA discovery](isa-discovery.md): milestone 60, one record per architecture for what the machine
  actually is, populated once at boot and printed at boot. Why RISC-V needs a parser and aarch64
  needs only a decoder (ARM never removed the CPU's self-description; RISC-V did, on purpose), how
  many call sites genuinely vary and which two of the four candidates dropped out, the three shapes
  that would have broken on the VisionFive 2, and the two things the machine corrected after the
  host tests were green. Its sequel, milestone 100, retires the last subsystem that assumed a board
  rather than reading it: the PSCI conduit and function id, the core list, and RISC-V's counter rate
  all come out of the device tree, and a core whose hardware id is not its logical id is now refused
  by name instead of silently never started.
- [The UART](uart.md): the serial port, and why every kernel learns to drive one first.
  What "asynchronous" actually means (there is no clock wire), and a line-by-line read of
  our own PL011 driver.

## Architecture

- [Registers](registers.md): 248 bytes of storage inside the CPU, and why that's the
  whole ballgame. **The most fundamental note here.** The register file *is* the CPU's
  state, which is why context switches and interrupts work the way they do.
- [Harts and PEs](harts-and-pes.md): the precise words for "one thing that runs an instruction
  stream" (RISC-V's hart, ARM's PE), why "core" is too ambiguous to build specs on, and the day
  the distinction earned its keep here (the icount clock counts harts, not cores).
- [aarch64](aarch64.md): the instruction set. Registers, exception levels (EL0-EL3),
  system registers, and why the target triple is spelled the way it is.
- [The stack, `sp`, and `x30`](stack.md): the stack is just RAM plus an agreement. Why
  `bl` doesn't push, why `sp` must be 16-byte aligned, and why there's one `sp` per
  exception level. Then the two guard-page incidents: 2026-08-14's real overflows, and
  2026-08-16's pair that were **not** overflows and said they were, because the report inferred
  `sp` instead of reading it. Then milestone 124's per-CPU interrupt stack, which stopped a
  preemption being billed to the thread it interrupted, and is honest that the static bound moved
  by 256 bytes while the *shape* of the cost changed entirely.
- [Adding a user program](adding-a-program.md): the six steps, task-oriented, written because
  milestone 117's first stranger run found that no file described them. The seven names the program
  is spelled out under, which of them the compiler catches and which it does not, the wire id that
  is expensive to change, and the manifest that declares a direction rather than a name. Walked and
  corrected three times; its `BUGS` says why that keeps being necessary.
- [The stranger test](stranger-test.md): milestone 117's instrument, written before its first
  run so the result cannot be graded generously afterwards. The protocol (a fresh context, no
  brief, no help mid-run, every question a defect), the eight-question rubric for the mental
  model, and the honest limit that an agent is not a person, so every number it produces is a
  lower bound on the friction a human would meet. Three runs are recorded. Run 3 is the one whose
  isolation held (a separate process that could not load the tree's own instructions, verified
  twice), and the one that found the harness leaking harder than the tree does.
- [Stack high-water](stack-high-water.md): milestone 84. Paint every kernel-owned stack, scan
  after the suite, report the deepest byte. The inventory (boot, secondary, thread and, since
  milestone 124, per-CPU interrupt stacks), the measured numbers, and the honest limits of a
  watermark. Milestone 90 closed the asymmetry that inventory found: the per-CPU secondary stacks
  left `.bss` for a region with an unmapped page under each, proven by a page-table walk. Its
  longest-standing BUGS entry, that nothing walks the call graph, closed on 2026-08-16 with
  `script/stack-depth-check`, whose static bound and this note's measurement agree to the byte.
- [Reading aarch64 assembly](reading-assembly.md): five rules that decode almost
  everything, the addressing-mode table, and a line-by-line walkthrough of `boot.s`.
  **Start here if a code block looks like noise.**

## Memory

- [Tearing down an address space](teardown.md): two ways to reclaim page-table frames
  (walk-and-reclaim vs record-all-frames), why a space that dies all at once wants the
  second, why kernel stacks want neither, and how a stale TODO nearly grew an unused method.
- [The heap and the slab](heap.md): why the stack isn't enough (its lifetimes must nest, and a returned
  Vec's don't), why fragmentation is the permanent enemy, and why Rust's ownership system is
  really a heap-correctness checker. **Retired from the kernel at milestone 14** (the kernel
  cannot allocate now; design/kernel-objects-from-untyped.md is the story of how), and the
  `heap`/`slab` crates were deleted outright on 2026-07-27 once nothing referenced them: the
  git history preserves the work, and a demonstrator's tree should hold what it ships. The
  note stays; building the allocator and then earning its deletion were both the point.
  **Milestone 27 brought the heap back in userspace**: `crates/user_heap` (the algorithm,
  host-tested) plus `user_rt::heap` (a `GlobalAlloc` that grows out of the process's own
  untyped via `untyped::MAP`); the note's last section is that story.
- [Physical memory](physical-memory.md): the frame allocator. Why a bitmap and not a free
  list, the bootstrap problem (the allocator's first act is to allocate itself), and why
  `mark_used` rounds *outward*.
- [The higher-half kernel](higher-half.md): why the kernel MUST be in TTBR1 (or the first
  context switch would delete it), and the two facts that let a kernel linked at a high
  address boot from a low one: `adrp` is PC-relative, and bits 63:48 aren't translated.
- [aarch64 page tables](page-tables.md): the structure the MMU walks. The trap bits (AF,
  PXN, AttrIndx), why W^X is enforced by construction, and the thing a failing host test
  taught us: bits 63:48 aren't translated, they choose which TABLE to use.
- [The MMU](mmu.md): virtual vs. physical addresses, page tables, the TLB, page faults,
  and why turning it on is the scariest moment in the kernel.

## Rust

- [Vec, Box, String, BTreeMap](collections.md): the four types the heap gave back. Why
  `Box` is what makes a recursive type finite, why `Vec` doubles, why `&str` works in
  `no_std` and `String` doesn't, and why a kernel uses `BTreeMap` and not `HashMap`.
- [`no_std`](no-std.md): why the kernel can't use the standard library, what `core` still
  gives us, and how we earn each missing piece back by building the thing `std` assumed.

- [Interrupts: the GIC and the timer](interrupts.md): the preemption source. Why the timer **9a**: and a hardware interrupt can become a message to a userspace driver.
  is a per-core PPI, why GIC priorities run backwards, and the bug we shipped: re-arming with
  a *relative* countdown silently lost 30% of our ticks.
- [Exceptions](exceptions.md): faults, interrupts, and syscalls are **the same mechanism**
  on aarch64, which is why we build the plumbing once. The vector table's shape is dictated
  by silicon. Also: why `brk` needs `elr += 4` and `svc` doesn't, and the exception-return race
  that put user code at EL1 for two instructions' worth of bad luck.
- [Auditing the hand-written arch assembly](arch-audit.md): the sweep prompted by that race, for
  every sibling of its bug class on both ISAs: state staged in single-copy hardware registers across
  more than one instruction. Three findings, the candidates cleared and why, and the meta-point that
  the Kani proofs stop at the pure-logic crates, so assembly is the **least-verified code in the
  TCB** and this is how we pay for that.

- [Threads, the context switch, and preemption](threads.md): a thread is a stack plus a set
  of register values, and here that's literal: 8 bytes. The context switch is fifteen
  instructions and **the last one returns into a different thread.**

- [The scheduler: placement, stealing, wakes](scheduler.md): DECISIONS §28 as built: per-core
  run queues, two-choice spawn placement, message-shaped stealing, and the wake split (local for an
  IPC rendezvous, load-aware for a device interrupt). The costs migration made real: the RISC-V
  `tp` fix and the progress-based hang watchdog.

- [Capabilities, and why the kernel has no `open()`](capabilities.md): a capability is a file
  descriptor that can point at *anything*. Unix already had them; it just also built a back door.
  The milestone 7 decision, and the confused deputy. **7d**: three syscalls, a capability is the
  only way to print, and `AT S1E0R` is how the kernel refuses to read its own memory on a user's
  behalf. **7e**: endpoints and synchronous IPC, and the scheduler learns a thread can be
  `Blocked` waiting for a message it can only reach by a capability.
- [Who does IPC name?](ipc-naming.md): an endpoint, never the peer. The sender names a
  channel it holds a capability to; the receiver is anonymous. No global namespace, which is
  no-ambient-authority made concrete. Even a hardware interrupt names an endpoint.
- [The native ABI](abi.md): the contract a program runs against (milestone 19e, "Decision 2"):
  one `svc` and four syscall numbers, the whole object world behind `SYS_INVOKE`, `_start(x0,x1,x2)`,
  and how a program meets its capabilities by convention rather than discovery. Why we wrote the
  convention down instead of building a BootInfo, and what a POSIX shim would cost (nothing, later).
- [Rust `std` on the native ABI](std.md): milestone 27: std's platform layer implemented directly
  on the capability ABI (Hermit's shape, not a POSIX shim). Heap from an untyped budget, stdout to an
  endpoint, time from the virtual counter, `panic!` faults, `thread::spawn` honestly `Unsupported`,
  and (phase two) `std::net` bound to net_stack's socket contract and `std::fs` bound to the FS service.
  Since milestone 64 that includes the **inbound** half: `TcpListener` binds a port the program's
  stack was *granted*, and the same binary prints `listen refused` on a stack that was granted none.
  What a path *means* with no global namespace ("under the directory I hold", so `..` and an absolute
  path are refused as un-nameable rather than served), how a program detects it holds no filesystem
  without faulting on an unmapped page, how build-std runs against a hardlink-cloned patched
  rust-src, why the symlink farm was measured to fail, and the honest caveats (what the PAL still
  refuses and why, and the std-internals coupling a nightly bump can break), including what still
  ends a nife process and the gate that now enumerates it. The "no create or
  truncate verb, monotonic-only clock, non-crypto random" caveats this line used to list are all
  gone: milestone 31 phase 2 bound `CREATE` and `TRUNCATE`, milestone 51 gave `SystemTime` a real
  wall clock, and milestone 56 put `std::random` on the entropy service.
- [Somebody else's crate on nife](crates-io-on-nife.md): milestone 64's measurement, and what
  closing the top of its ranked list took. Fifty crates.io crates built against the patched `std` to
  find out what actually stops them: **39 of 50 build unchanged**, and of the 11 failures **eight are
  one crate that is not part of std** (`getrandom` had no `nife` backend; it does now, and `rand` and
  `uuid` build). The four failure classes, the prioritised gap list milestones 99 and 66 consume, why
  the split was recorded as 35/15 and is 39/11, why nine of the top gaps turned out to be **bindings
  rather than missing verbs**, which gaps were declined and for what reason, and the sting in two
  places: `tempfile` compiles and returns "not supported" from every call, and `std::env::vars()`
  used to abort the process. Five std calls have now been found that **compile perfectly and kill
  the process**, the last of them `std::process::exit`, which is why the reading that found them is
  a check now (`cargo xtask std-aborts`, described in std.md).
- [The `thread::spawn` fork](thread-spawn-fork.md): milestone 64's rank-3 gap, written up against
  the six-questions framework ahead of a decision (pull request #394). Why a std thread needs one
  shared, growable heap and nife gives every TCB a privately owned, consumed `AddressSpace`; the
  two real shapes a fix could take (a kernel-level shared VSpace, seL4's own answer and this
  kernel's stated lineage, versus sibling processes kept aliased by replicated frame mappings) and
  why the second one's apparent "no syscall touched" cheapness collapses against what Rust's own
  allocator needs from a live, growing heap; and why declining, for now, is the one option that
  forecloses nothing later.
- [Running a foreign language: the C seam](c-seam.md): milestone 36: memory-unsafe C, compiled by
  bare-metal clang, confined and restarted. Why C is the *best* demonstration of "a verified core that
  confines unverified workloads" rather than a dilution of it, and the seam's rules: a Rust `user_rt`
  shell holds every capability and makes every syscall so the C can hold none and make none, which is
  why a foreign component cannot widen the syscall surface. The libc question answered by tier (the
  object demands five symbols, the linker demands two, because `compiler_builtins` already supplies
  three), why a Rust `memcpy` shim calls itself and what that looks like when it does, `malloc` from
  the process's own untyped budget, and one clang required to target both ISAs so parity stays a gate.
  Also: the two witnesses that prove nothing outside the grant changed, and the reap-only right the
  supervisor would rather have held.
- [How authority moves, narrows, and ends](capability-lifecycle.md): capabilities spread by
  copy-with-narrowing (never widening), `SEND_CAP` is share not move, the two independent
  narrowings (rights vs. GRANT), and why there's no revocation yet (a control gap, not a
  safety hole: spend-only untyped keeps shared frames valid).
- [Object revocation: tearing a process back down](object-revocation.md): reclaiming the TCBs,
  address spaces, and endpoints a process built (extends §13 from frames to objects). Region
  ownership plus generational staleness instead of a capability derivation tree, why destroy is
  the owner's explicit act and must stay off the scheduler lock, `Untyped::SPLIT`/`DESTROY`, and
  the generational region slots that make a repeatable spawn loop finally possible.
- [Ending a permanently blocked thread](blocked-thread-teardown.md): research and four proposals,
  no decision. `Untyped::DESTROY` arms a kill that `schedule()` spends only for a `Running` thread,
  so a permanently `Blocked` one is refused forever and its region never comes back. The finding that
  reframes it: **the mechanism is about thirty lines and the authority is the whole problem**, because
  `WaitRole` already enumerates the three places a blocked thread can be and the abort-and-wake pair
  already exists. A live hazard any proposal must solve (a `Reply` capability names a thread, not a
  call, so waking a caller lets a stale reply forge an answer to a later one), corroborated verbatim
  by L4Re's own timeout warning. Prior art read from primary sources: seL4's `suspend` = `cancelIPC`
  + `Inactive`, Mach's abort-versus-abort-safely split, L4's `ex_regs` cancel flag, Zircon deleting
  thread killing outright in RFC-0007, QNX unblocking a REPLY-blocked client on server death, and why
  Linux had to invent `TASK_KILLABLE`. Starts where notes/hung-component.md's case (c) stopped.
- [Supervision: a thread's death becomes a message](supervision.md): milestone 22's fault endpoint; §32's `Endpoint::REAP` lets a supervisor collect a corpse without being able to build one
  (DECISIONS §26). The kernel is the only witness to a fault, so it delivers a five-word message
  (event, tid, pc, addr, reserved) to the supervision endpoint a thread was spawned holding; the
  corpse is dead-until-reaped so the supervisor can inspect it and reap it with §16 revocation. No
  new syscall or method: a spawn-slot convention and a message-format convention. Restart policy
  stays in userspace; the kernel never relaunches anything.
- [Trusted init: measuring the boot program, and then everything init loads](trusted-init.md): milestone 22
  phase B.1. init's bytes used to be loaded on trust; now the build hashes the boot program and the
  kernel refuses to enter anything else, digest compiled into its own image ("this kernel runs exactly
  this init"). Why SHA-256 hand-written and shared by the build and the kernel, why an unmeasured
  program is a refusal and not a pass, how the build composes without a chicken-and-egg, and the
  signature variant's cost (Ed25519 in the TCB, key custody) recorded rather than built. Phase B.2 is
  the other half, what a broken init can still reach: a four-program tree where construction moves to a
  sub-server holding one program image, the supervisor holds no memory at all, and the root deletes its
  budget, proven by authority (a dropped untyped answers `NoSuchSlot`) rather than by timing. The
  interactive boot is migrated too: init keeps the ELF loader (moving it would relocate the authority
  rather than reduce it) but drops the root untyped for a bounded job pool, gives back the UART and its
  interrupt, and builds every job in a region `job_undertaker` returns when the job ends, so a bounded
  budget is affordable. Honest limits included: recovery is LIFO, and init still maps every page it
  ever laid down for a child. Milestone 104 then continues the chain past init: the build packs a table
  of every program's digest into the archive, the kernel's trust root vouches for that table exactly as
  it vouches for init (one digest, no policy, no 14 MB hash), and init refuses to load anything it
  cannot match. One rule, `init runs nothing it cannot vouch for`, with a refused program treated
  exactly as a missing one, so what a refusal costs is decided by what the program was for rather than
  by a second policy.
- [Delegating a capability](delegation.md): a capability system where processes can't pass
  capabilities isn't one. A process now delegates a capability to another over an IPC endpoint
  (`SEND_CAP`/`RECV_CAP`), narrowing the rights, and only if it holds `GRANT`. Authority composes
  between processes at runtime instead of being wired by the kernel at spawn.
- [Frame capabilities](frames.md): shared memory a process owns rather than one the kernel wires, and
  why a page handed over at spawn could never be taken back
  in. Retype a page out of untyped into a `Frame`, map it, and delegate a read-only view to a peer
  that maps the same physical page. §10's "shared memory carries data," composed by the processes;
  the IPC rendezvous that carries the frame is also the edge that orders the memory.

- **7c update in [elf.md](elf.md)**: the kernel now *loads* one. An ELF names its own load
  address, so a hostile one names the kernel's; it is refused by a `Half::Low` guard that has
  been sitting in `paging` since milestone 4, waiting for exactly this file.

- [virtio-blk, driven from userspace](virtio.md): milestone 9: a real block device driven by a
  process at EL0, with DMA, a virtqueue, and the completion arriving as an interrupt-message. Plus
  the two scheduler bugs it flushed out: no idle thread, and interrupts restored under the lock.

- [PCIe, and driving a disk over it](pcie.md): the PCIe transport (DECISIONS §18): ECAM, BARs,
  the capability list, why the kernel is the firmware here (OpenSBI does no PCI), the transport
  seam that runs one driver over two buses, and INTx through the PLIC. The hardcodes are held by
  witnesses against the machine's own device tree.
- [NVMe: the first non-virtio disk](nvme.md): milestone 53's storage half: a real device family's
  block driver (queues, phase tags, doorbells, PRPs) over the §18 PCIe transport, confined by the
  IOMMU alone because no kernel seam can validate addresses the controller fetches from memory.
  Kernel-resident for now, and the BUGS section says why.
- [A shell at EL0](shell.md): milestone 10: an interactive shell, a userspace input driver
  (console receive), and worker processes spawned on command. Proof the whole stack works, as a
  conversation between processes the kernel only routes.
- [The line discipline as a userspace component](line-discipline.md): milestone 28: the tty
  layer as a process (`line_editor`) on plain endpoints, a sans-IO editing engine host-tested against a
  screen model, why it was built rather than porting `noline`/`embedded-cli`, and the Reply-cap
  argument that makes it deadlock-free.
- [The terminal contract](terminal-contract.md): milestone 28: the interface a terminal
  presents (the `OP_WRITE`/`OP_READLINE`/`OP_BYTES` IPC protocol, the read flags, the shared
  pages, and the honest limits), written down so milestones 29 and 31 implement against a
  contract, not against today's component.
- [The sink protocol](sink-protocol.md): milestone 50's protocol lane: the four "write these bytes
  there" protocols become one, so a terminal, a file and a pipe are substitutable in a capability
  slot and a program stops being able to tell which it has. Register-only and SEND, both forced
  rather than chosen (a sink that needs a page mapped at an agreed address is no longer one grant,
  and a CALL would make every program on the right of a `|` know it was there). The finding
  underneath: the kernel could not tell **"gone"** from **"never had one"**, both arrived as
  `NoSuchSlot`, so the only available behaviour was the wrong one for a pipeline. `abi::Error::Gone`
  is the fix, `SIGPIPE` arrives through std's own `is_ebadf` seam, and the indifference test runs one
  ELF against two destinations that share nothing but sixteen bytes of message. Since 2026-08-03 it
  runs against a **third**: `user/src/terminal_sink_caretaker.rs` makes the terminal a sink, which is a
  separate process for a capability reason (its endpoint also carries `OP_READLINE`) and which
  needed a register-only `OP_PRINT`, because `OP_WRITE` reads from the one client page init maps.
- [The manual](manual.md): milestone 40's documentation service. A streaming markdown renderer that
  allocates nothing, because `doc` reads its input as sixteen-byte sink messages and a renderer that
  held a document would need a memory grant to do it. Why the roadmap's `pulldown-cmark` was
  reversed (it is not `no_std`, and a std program on this system has no stdin and no argv, so it
  could neither page nor be told which page to show), and what replaces a conformance suite when the
  corpus is closed and in-tree: every letter of every note reaching the output, in order, checked on
  the real files. The viewer designates **nothing** (its manifest is byte-identical to `wc`'s), the
  index is host-built because enumeration is authority, and its layout is bent around a reader that
  holds exactly one 4 KiB page. The costs are measured and one of them is unflattering: the index is
  1.55x the markdown it indexes.
- [Pipes and redirection](pipes.md): milestone 50's operators lane: `>`, `<` and `|` at the prompt,
  which turn out to be one substitution rather than three features. The grammar and its three
  non-Unix refusals; the input slot's shape, decided as *the sink contract received rather than
  sent*; the two manifest declarations that came out of the build (`OutputSpec`, because not every
  program's slot 0 carries bytes, and `InputSpec`, which produces a refusal Unix cannot: `wc` with
  nothing feeding it is caught at the prompt instead of hanging). A pipe is an endpoint retyped out
  of a region the shell splits off its own budget, and destroying that region is what turns a dead
  reader into `Gone` for a live writer. A builtin can lead a pipeline because the shell can be a
  writer. Both directions are proven the same way, one binary against two sources or two
  destinations. And the finding that finished it: the file behind a `>` is **the shell's own
  filesystem session**, not a sink process, because `fs_proto` shares one page between the FS server
  and its clients and `ls > out.txt` is a line where the shell must read the filesystem while the
  redirection is being written. Since 2026-08-04 it also holds the constraint the second reader
  found: **a process has one wait point**, so a shell that feeds a stage cannot also receive from
  it, and a line whose bytes all come from the shell needs one stage that reads to the end. No
  interleaving schedule fixes that, and the two shapes that would (a pull-based source, a buffering
  component) are both design forks and are weighed there.
- [`swish` the language](swish-language.md): milestone 67: quoting, sequencing, and the one design
  fork inside them. **Quoting was an authority gap rather than a convenience**: a file called `my
  notes.txt` could not be named, and a resource you cannot name is a resource you cannot grant. It
  **delimits a word and never rewrites one**, because every token here is a slice of the line you
  typed and a shell with no allocator has nothing to join pieces into, so there is no backslash
  escape and `a"b"` is refused rather than misread. The one thing it does to authority is *narrow*:
  it suppresses expansion, so `rm "*.txt"` hands over one name where `rm *.txt` hands over the set,
  and it stops `-r` widening a directory grant into a subtree walk. Sequencing splits **outermost**,
  which is bash's binding and also what keeps a pipeline region scoped to its segment. And the fork:
  **a refusal is not an error and gets its own status**, because "did my command fail" and "was I
  able to ask" are different questions and Unix cannot tell them apart.
- [The command line as a grant expression](grant-expression.md): milestone 31: naming a resource
  in a command is how you grant it (Miller's "designation is authorization"), the inversion of
  Unix's ambient authority at the one interface a human touches. The shell's own budget, the
  `SEND_CAP`-to-init spawn protocol, `--mem N` made real by the `budgeter` program, the "you
  hold no such capability" refusal, and the `SPLIT`-grants-`GRANT` fix that let untyped be delegated.
  Phase 2 adds **per-file grants**: a caretaker process narrowing a directory capability to one file
  in one direction, proven by a read-only and a writable attacker, and why the second one is what
  makes the first mean anything. Milestone 47 then deleted two words from the grammar (`run` and
  `file:`), because the manifest was already doing the work the designator claimed credit for, and
  records what a shell that could delegate a **clock** to `date` would need. Phase 3 (2026-08-17)
  closes it: **init builds a `fs_subtree_caretaker` per directory grant**, so `rm` runs at the real
  prompt, and the note now records how the four obstacles it predicted before building actually came
  out, including the one it got wrong.
- [Live component replacement](live-replacement.md): milestone 23, the flagship: a running
  component swapped under a client that is talking to it. Why there is **no broker in the fast path**
  (the endpoint is the stable name, so the swap costs nothing and the kernel's own sender queue
  buffers the down window), why revoking a *device* means taking it back rather than destroying it,
  the drain that is just a message travelling in band, and the two witnesses in two address spaces
  that say the client's stream was unbroken. The replacement is written in C. Also the latency
  ladder's two built rungs with the number that makes "opt-in, never the default" a rule, and an
  honest list of what state handoff, manifests and hung components still need.
- [The component manifest](component-manifest.md): milestone 23's second residual, and the answer to
  the question it forces: a component's declaration is a **sibling** of `grant_plan::Manifest`, not a
  subtype, because that one says what a human at a prompt may designate and this one says what a
  supervisor must route before a component can serve anyone. The four differences that make it
  falsifiable, why a manifest belongs to the **contract** and not to the build, and the Fuchsia split
  that keeps it from being a privilege-escalation surface (**a manifest is a request; the provisions
  are the authority**). Also the slot agreement that stopped being a comment, why structure is a
  compile error while provisioning is a runtime refusal, and an honest account of the wire format that
  true vendor shipping still needs and this lane deliberately did not decide.
- [The hung component](hung-component.md): milestone 23's third residual, the case DECISIONS §32
  named and declined. Every failure this system handles is a **death**, and a component that stops
  answering without dying produces none of it: it reads `BLOCKED`, which is what a healthy server
  between requests reads as, and `Endpoint::REAP` answers `StillAlive`. Three hang shapes with three
  different answers, and the finding that the "stronger right" §32 points at is **insufficient** for
  the worst of them, because a permanently blocked thread never reaches `schedule()` to spend the kill
  a `DESTROY` arms. Also the half-correction it owes §32: the **service** is restored with no new
  authority, and only reclaiming the memory needs one. Why `abi::Error::Gone` never reaches a caller
  stranded mid-`CALL`, why a deadline belongs to the supervisor and is denominated in **progress
  rather than time**, and the two decisions calef owes before a watchdog can exist.
- [The process view](process-view.md): milestone 126's view stratum: `ps` and `pgrep` work and
  neither can enumerate the machine. `endpoint::SURVEY` reads one supervision subtree, which is a
  scope the kernel already maintains and so cannot drift out of agreement with reality; a wide grant
  is fine and `caps ps` prints it, which is the distinction `/proc` has no way to express. Also why
  refused, empty and populated are three answers rather than two and what `pgrep` adds as a fourth,
  why the walk gives `SCHED` back between entries and what that costs, why a view riding on `READ`
  was wider than looking needs and what `Rights::ENUMERATE` fixed, and **why there is no `pkill`**:
  a tid is a name, `Tcb` has no `DESTROY`, so the demonstration is asymmetric on purpose and the
  write-up says so rather than dropping a promised comparison.
- [Scheduled execution](scheduled-execution.md): milestone 129: a cron whose every entry is a grant.
  An entry is a schedule plus a grant expression checked at registration by the same
  `grant_plan::plan` the prompt uses, so what a scheduled child will hold is printable before the
  first tick, which is a sentence Unix cron has no vocabulary for. Why the interesting half is what a
  schedule is **refused** (`every 1s date` is legal, runs in any crontab, and is turned away here for
  want of a clock capability), why "the line is wrong" and "this scheduler holds nothing to back it"
  are two answers and not one, why the fire arithmetic skips a stall instead of catching up, and the
  one missing kernel primitive that shapes the whole program: there is no timed wait, so a scheduler
  yield-polls and reaps its children lazily. This is milestone 106's fifth consumer and the first
  whose whole purpose is a deadline.
- [The program manifest](program-manifest.md): milestone 31: a program's declared endowment,
  checked against the command at the prompt so a mismatch is a legible refusal, not a mystery hang.
  SHILL's contract shrunk to phase 1, and milestone 23's component contract in embryo.
- [Wall-clock time](clock.md): milestone 51 lane A: the machine stops reporting 1970 plus uptime.
  Wall clock is **counter plus offset**, so adjusting it cannot perturb `Instant` by construction
  rather than by discipline. The three authorities are three different objects and needed nothing
  new in the kernel: reading is a read-only page (two loads and an add, no syscall), setting is the
  same page mapped writable, proposing is an endpoint the service may refuse. Also the seqlock whose
  memory ordering is load-bearing on a weakly ordered machine, why the backward step limit is three
  orders of magnitude tighter than the forward one, the two RTC drivers found by `compatible`
  because no node-name prefix matches both boards, and the "I do not know what time it is" state
  that is the default rather than an afterthought.
- [What a timed wait costs](timed-wait.md): milestone 106's fork, priced rather than built. The
  block said a deadline in the blocked state "means the scheduler carries a timer wheel or an ordered
  deadline list", and every clause of that turns out to be wrong or beside the point: the per-thread
  word is **free** (a TCB is 744 bytes in a 4096-byte page since 19c.2, so the static-BSS premise is
  stale), the **idle tick is one comparison for all three data structures** so the always-paid cost
  does not distinguish them, the ordered list is the **worst** option above one waiter while scanning
  wins until about 64, and milestone 124's no-context-switch-from-the-interrupt-stack proof **holds
  measured on both ISAs** because `irq_notify` already wakes a blocked thread from that stack. Also
  the +30/+31 instructions a deadline check adds to the tick, the 97-of-128 blocked-thread peak the
  suite really reaches, the 10^5-to-1 ratio against today's yield-spin, the two things a deadline on
  `RECV`/`CALL` needs that a `SYS_SLEEP` does not (a targeted unlink on a singly-linked queue, and
  nothing else, because `abort()` already exists), and the stale-`Reply` hazard that wants a lane of
  its own. Names no winner: the fork is calef's.
- [`date`](date.md): milestone 51's deliverable: the command that makes the wall clock visible, and
  the first thing to put the clock service and the calendar crate in one process. It reads and
  cannot set, which is a fact about its wiring (a read-only mapping) rather than a missing flag, so
  there is no `date -s` and its absence is not a `TODO`. Also the provenance line, which renders
  `clock_proto`'s four states for a person and is a distinction no Unix `date` can print; why the
  unknown clock is a sentence rather than a panic or a 1970; why the "have I got a clock" probe must
  not touch the page; and the guest test that closes DECISIONS §43's "the unknown-clock path is not
  proven in the guest", because a frame nobody published to *is* that machine.
- [`time`](time-command.md): milestone 86: the shell's second prefix word, and the one design
  question it had. The clock is **the shell's**, so a command that holds no clock at all is timed
  anyway, which is the Unix behaviour and the capability-model answer at once: a duration is
  observable to whoever can watch a thing start and stop, and delegating a clock to the child would
  have changed what the child can do. The shell holds the page `READ` without `GRANT`, so it can
  measure and cannot pass a clock on. Also what the number is not (CPU time, a benchmark, a promise
  the clock stood still), the stepped-clock line that reads the page's generation to say so, the two
  refusals borrowed from `date`, and an honest `BUGS` entry recording that a duration could have been
  computed from the ambient counter with no capability at all.
- [Entropy](entropy.md): milestone 56's first half: `std::random` stops being splitmix64 seeded
  from boot-relative time. One process holds a virtio-rng device; everything else holds an endpoint
  that means "you may obtain randomness" and names no device, which is the fourth appearance of
  attenuation by operation rather than by object. The service passes the device's bytes through and
  computes nothing, because whitening without a one-way function is a reversible permutation that
  obscures the claim rather than strengthening it. Also why the bytes ride in the reply instead of a
  shared page, the fork on `std::random` (transparent, split on std's own seam, so the caller that
  promises cryptographic strength panics rather than degrading), and the INTx-sharing finding that
  made this driver look at the used ring before it blocks.
- [The framebuffer contract](framebuffer-contract.md): milestone 29, the display ladder's first
- [Credentials](credentials.md): milestone 56's second half: an identity and a secret you can check
  and cannot read. The tension it answers is that a secret is a bearer token while a capability is
  an unforgeable reference, so knowledge cannot be revoked and everything else here can; the answer
  is to hand out the operation instead. Also why writing the store is a **phase** and not an
  operation (this kernel has one wait point, so the provision endpoint is deleted at both ends
  rather than guarded), why we depend on RustCrypto's `argon2` rather than write or vendor one and
  run the RFC 9106 vectors to prove it, the debug-build overflow panic our exhaustive corruption
  test found inside that dependency, and an honest list of what this does not protect against.
- [NTLM](ntlm.md): milestone 65, the other half of the same store: **hold the key, expose the
  operation, never the key.** NTLMv2 does not verify a presented secret, so "secret in, boolean
  out" does not describe it: the server holds a key and computes a MAC, which is why this needed a
  milestone rather than another opcode. Why the store holds `NTOWFv2` rather than the NT hash (the
  account name and domain are bound at provisioning, so a caller cannot choose half the key
  derivation), what crosses the shared frame and what never does, why the `SessionBaseKey` is
  released only against a proof that verified, and why the client-side operation the roadmap named
  is deliberately absent. Also three broken primitives shipped on purpose with their blast radius
  stated, the published vectors from RFC 1320, RFC 2202 and [MS-NLMP] §4.2.4 that pin them, the
  four-zero transcription error the machine caught, and an honest `BUGS` list starting with
  revocation being per holder rather than per secret.
- [The framebuffer contract](framebuffer-contract.md): milestone 29, the display ladder's first
  rung: the confined virtio-gpu driver, the client that draws, and the shared-surface contract
  between them, written down so milestone 33's compositor implements against a contract. Also the
  memory story (a framebuffer is a bigger grant, never an exemption), the confinement hazard a GPU
  adds that a disk does not (backing addresses ride in a command payload the transport validator
  cannot see, so the IOMMU is the barrier), and how the pixels are proven in two halves: the
  framebuffer from inside the guest by two witnesses, the scanout from the host by driving QEMU's
  monitor and checking a `screendump` against the same pattern definition.
- [The compositor](compositor.md): milestone 33, the display ladder's second rung: one screen
  multiplexed among mutually distrusting clients. The idea it rests on is that **the shared doorbell
  carries no authority**: a shared endpoint has no sender identity, so every per-client fact lives in
  per-client memory and every privileged answer travels through privileged memory, which leaves a
  compositor with no authorization code in it at all. Also: how a client is *proved* unable to touch
  its neighbour's pixels (an attacker handed the exact address, adjacent frames, and four witnesses),
  the two dialects of "you hold no such capability" (an empty cspace slot and an unmapped page),
  enumeration and screenshots as read-only mappings rather than verbs, focus as a capability, and the
  wait-any primitive whose absence shaped the whole design.
- [Glyphs, the VT engine, and input](glyphs.md): milestone 29's remaining increment: the piece that
  makes the framebuffer readable. An original 7x8 bitmap font drawn in the Kaypro II's style (and
  why the ROM itself is excluded on licence while its look is not protected, which matters because a
  font is compiled into the image), a sans-IO VT engine checked against the *real*
  line discipline's echo rather than a list of escape sequences, and a display terminal that is a
  client at **both** display seams with exactly `painter`'s and `window`'s authority, which is how
  "neither contract needed changing to carry text" became a spawn literal instead of a claim. Also:
  why the expected picture is a value three witnesses compute independently and what the host's
  one-letter-wrong negative control is for, the deadlock that stopped a terminal ringing the doorbell
  for a keystroke (and why the design it ruled out was the worse one), a virtio keyboard whose power
  to type is a page nobody else maps, and the honest limits (no scrollback, no UTF-8, a US layout).
  Ends with what adopting libghostty-vt would now cost, as a recommendation rather than a decision.

- [Running under virtualization on Apple Silicon](virtualization.md): `cargo xtask run --hvf`
  puts the kernel on the real M3 core via Apple's Hypervisor.framework. It found two QEMU-shaped
  assumptions on the first boot: the physical timer (fixed, we use the virtual timer now) and
  semihosting (emulation-only, so tests stay on TCG).

- [Untyped memory: the kernel stops allocating](untyped.md): milestone 11: a process spends
  pages out of a capability to raw memory it was handed, and the kernel's free-frame count does not
  move while it allocates. A process cannot make the kernel allocate, so it cannot exhaust it.

- [Per-process resource quotas](quotas.md): a spawner may have at most N children alive; the slot
  returns when a child is reaped, riding the thread's lifetime, so a spawn flood is bounded with no
  bookkeeping. **Opens with where it stands (milestone 41): the mechanism has had no caller since
  §28 retired the kernel-wired shell, because the bound moved into the untyped budget a process
  spawns out of.** Read the rest as the mechanism as designed.
- [Confining DMA without an IOMMU](dma.md): the device bypasses the MMU, so a hostile driver
  could DMA over the kernel. Closed by kernel-mediated descriptor validation: the kernel owns the
  ring addresses and the notify, and refuses any descriptor outside the driver's own DMA region.
  Now also the write direction (milestone 32: same check, both hazards) and the kill-mid-write
  record, including the DMA-frame-reclaim caveat. **Opens (milestone 35) with the map of what is
  proved and what is only mitigated**, because "DMA confinement is proved" is wrong said flat: every
  address arriving in a *descriptor* is machine-checked, but a virtio-gpu's backing addresses arrive
  in a *command payload* the validator structurally cannot see, so only an IOMMU stops those, and on
  a board without one (the VisionFive 2, milestone 16a) nothing does.
- [Confining DMA with an IOMMU](iommu.md): the hardware version (milestone 16b, DECISIONS §20), on
  both ISAs behind one seam: the format-generic `paging` crate builds a device's DMA domain (an
  identity map over the frames it may reach) the same way it builds a process address space, and two
  arch drivers (SMMUv3, RISC-V IOMMU v1.0.1) attach it. The disk and attacker suites run behind it;
  a confinement test makes the IOMMU fault an escaping DMA, so a silent bypass fails loudly. The
  shadow ring stays as defence in depth.
- [The network stack as a confined component](net.md): milestone 30 (DECISIONS §21). Multi-queue
  DMA confinement (built, both ISAs): the validator grows a second queue and the receive direction,
  where the device writes into driver memory, proved by the same address-bounding check. Then the
  prior art (seL4 dataports, Fuchsia Netstack3, Plan 9 /net as the counter-design), the socket
  contract proposal and its open fork, the smoltcp 0.13.1 pin, and the driver/server work that
  follows. The inbound half (milestone 107) is a listener that is not a connection and a port that
  is a grant; milestone 64 put `std::net::TcpListener` on it, so an ordinary Rust program can serve.
- [SMB: the network file service a Mac can mount](smb.md): milestone 54, **BUILT 2026-08-17** and the
  head of the customer path. The `smb_proto` wire crate (SMB 2.1, NTLMSSP under minimal SPNEGO, the
  per-connection state machine as host-testable pure logic), the `smb_server` adapter holding one
  network endpoint and one share, the wire decisions listed for review, the two-prober QEMU gate
  that rides milestone 107's spawn, and the mount instructions a real macOS `mount_smbfs` has
  already followed successfully (2026-08-15), with an honest BUGS section led by what Finder's
  own dialog has not yet exercised. Since 2026-08-16 the share is a **tree** and the volume's
  numbers are the image's: `smb_proto::path` parses a share-relative path once at the wire's edge
  so `..` dies where the bytes arrive, and `fs_proto`'s `STATFS` reaches the volume classes a
  Time Machine sparsebundle is sized against. And since 2026-08-17 a share can require an **NTLMv2
  proof** while the server holds no key: the `authenticator` seam carries only public bytes and a
  MAC, `ntlm` is a *dev*-dependency of the protocol crate so the shipping code cannot compute a
  proof, and the gate has a host process authenticate for real over the challenge the guest chose
  while the kernel checks the page between the adapter and the credential store. The BUGS section is
  blunt that the *demo* boot still admits guests, because nothing can yet tell a running system a
  password.
- [mDNS/DNS-SD: the Time Machine advertisement](mdns.md): milestone 55's second protocol. The
  reference router's actual `_smb`/`_adisk`/`_device-info` records, captured 2026-08-15 and decoded
  (one `_adisk` instance with the disks inside its TXT, SRV port 0 on the flag services, and a
  measured `model=MacSamba` against a config that says TimeCapsule), which are `mdns_proto`'s test
  vectors. Then the smoltcp 0.13.1 multicast verdict: the `multicast` feature exists and the tree
  has it off, so receiving on 224.0.0.251 needs a feature line, a join call, and the three pieces
  of socket surface the note lists; the responder program waits on those.
- [NTP: the wire format, and the client that carries it](ntp.md): milestone 51 lanes C and D. The
  48-byte NTPv4 packet, the 1900-epoch fixed-point timestamp and the **fixed era pivot** chosen for
  the 2036 rollover (and why picking the era nearest to "now" is worse), the offset and delay
  arithmetic, and the seven response checks that are the entire spoofing resistance of
  unauthenticated NTP. Then the client: five capability slots, **none of them the clock page**, so a
  compromised time client can lie inside the service's bounds and can do nothing else. The nonce
  comes from the entropy service or the client refuses to send anything, the poll interval is a
  yield-spin because there is no timed wait, and the test server is honest about what substituting a
  peer at a capability boundary does and does not prove. Scope recorded plainly: no NTS, and why that
  is a separate decision rather than a stretch goal. Also the place where a model checker turned out
  to be the wrong tool and exhausting a 10^9 domain was the right one.
- [Hardening the repository itself](repo-hardening.md): milestone 44's other half: the GitHub
  settings that cannot be committed. Private vulnerability reporting, and the exact ruleset for
  `main` with the seven required checks, written to be followed rather than interpreted. Includes the
  measured caveat behind CodeQL's "0 alerts" and the reason `--auto` merging silently did nothing.
- [Auditing the shared pages](shared-page-audit.md): the **second** security audit, with the lens the
  first one lacked. Every service contract now moves bulk data through a page shared with its client,
  so the question is whether a value a server **checks** and a value it **uses** are two reads of
  memory somebody else can write in between. Seven findings, five fixed; the structural reason there
  were not more (every length travels in a register, never in the page); and the two patterns that
  recurred, a guarantee assumed from the wrong side of a boundary and half a discipline written by
  instinct.
- [Auditing untrusted counterparty input](untrusted-input-audit.md): milestone 43 continued, taking
  the block's further lenses (network and bus input, and §79's secret-material rules) to the crates
  that landed after the shared-page pass. The question is whether a value a hostile counterparty
  supplies in one message or completion is bounded before it is believed. One finding: the NVMe kernel
  driver panics on two device-written completion fields, the reciprocal of shared-page-audit.md's
  finding 6 one layer down (the IOMMU confines placement, not values). `mdns_proto`'s decoder and the
  cred/ntlm secret handling are cleared, with the reachability and scope caveats attached.
- [A security audit](security.md): an adversarial four-part review of the whole kernel. The
  MMU and capability confinement held up; two panics on untrusted input were fixed; the DMA/no-IOMMU
  limitation and the missing resource quotas are named rather than hidden.
- [Rustdoc coverage](doc-coverage.md): the doc-example floor and the `missing_docs` ratchet
  (milestone 68's two unfinished halves). Every crate now has a worked example (49 doctests became 116); item documentation is
  a 401-item worklist with a per-crate opt-in in the 23 crates already clean. Two findings worth
  carrying off: the block's counts had moved in both directions (three of the four crates it named as
  hard were done, and five new crates arrived with nothing), and `rustdoc --show-coverage` measures
  something different from `missing_docs`, so the number the block used to defer that lint was not
  about that lint. The BUGS section names the five crates whose doctests no gate runs.
- [The documentation sweep](documentation-audit.md): how to run one, and what counts as a finding.
  Milestone 92's audit mechanism pointed at a second target, sharing its index, its tripwire and its
  three dispositions rather than growing a twin. The disease is **claim rot**: a path that was
  renamed, a number that grew, a "currently" describing a state that ended, a plan a later decision
  superseded. Where the boundary with milestone 125 (a number in the prose is a claim) and milestone 117 (the
  stranger test) falls, why `script/audits --worklist` is a heuristic and not a signal, and the rule
  that makes the mechanism compound: **every sweep converts at least one class of claim into one a
  gate re-derives.**
- [Machine-checked proofs (Kani)](verification.md): the verification thesis (DECISIONS §14) in
  practice: the capability model is proved for *every* input, not just tested on the cases we wrote.
  Run by `script/verify`. Milestone 18 completed the spread inward: `capability`, then IPC (rendezvous and
  the one-shot Reply), then the MMU isolation invariants, each proof landing on code the kernel runs.
  Milestone 35 reached the last unproved isolation boundary (the DMA validator and the IOMMU domain's
  page set) and added the two things that keep a proof honest: **the bounds with their justifications**,
  and a plain statement of **what the proof does not establish** (addresses carried in a device command
  payload rather than a descriptor). Also the record of a declined proof being reversed by aiming at a
  smaller target, and of every new property being falsified before it was believed. Milestone 51's
  calendar added the finding that a 64-bit division and a symbolic-length slice cost far more than
  the logic wrapped around them.
- [Fuzzing the parse surface](fuzzing.md): milestone 42's second leg, and the complement to the
  proofs above. Starts with the question that decides whether it is worth having at all, given 107
  Kani harnesses: **what does fuzzing find that Kani does not**, answered against three worked cases
  in this tree rather than in general (`elf`'s totality proof that did not return, `dtb`'s proved
  leaves under unproved walkers, and `nifefs`, where the gap was not a bound at all but a property
  nobody had written down). Four targets chosen on one rule, does this read bytes from outside the
  trust boundary, with the crates deliberately *not* fuzzed named and argued. Three bugs, one found by
  the fuzzer, one by a round-trip property, and one by reading the code while writing a target that
  then failed to rediscover it in ten minutes. The CI budget and why fuzzing cannot be a gate anyone
  waits on, the corpus discipline (seeds committed, working corpus not, crashes become host tests),
  and a BUGS section that says what a green run does not mean.
- [Dynamic undefined-behavior checking (Miri)](undefined-behavior.md): milestone 79, the third leg
  of the analysis surface. What
  Miri checks that Kani, the fuzzers, and clippy cannot (aliasing, provenance, uninitialized reads,
  leaks, at the tree's 224 `unsafe` occurrences), what the first full run found, and the honesty
  clause: the exhaustive suites sample themselves under `cfg(miri)`, so "Miri-clean" means the
  sampled paths, never the exhaustive claims. Run by `script/undefined-behavior-check`, weekly in CI plus on demand.
- [Interleavings, model-checked (loom)](interleaving.md): milestone 80, the fourth leg. Kani's
  harnesses are single-threaded, Miri runs *one* interleaving, and QEMU's TCG explores almost none of
  the orderings aarch64 and riscv64 permit, so CLAUDE.md's fourth rule (assume weak memory ordering)
  had no instrument that could falsify a violation of it. Loom searches the space. The survey that
  found four of the five candidate protocols have **no atomics at all** (they are under the ranked
  interrupt-safe lock, which is §9 working); the pilot on the work-steal handshake, which passed and
  is worth having anyway; and the real find, **a torn read in the clock page's seqlock** that was
  missing the store-store barrier between claiming the sequence and writing the data, unreachable on
  x86 and invisible to every other gate. Including which fixes do *not* work: `AcqRel` and `SeqCst`
  on the claim both still tear. Extended 2026-08-14 with the scheduler's block/wake protocol
  (`crates/wake_handshake`, the fourth bench stop's retrofit): a lock-based protocol whose search
  space is the gaps between critical sections, with each of its three recorded races held as a
  harness plus a `#[should_panic]` reconstruction. Run by `script/interleaving-check`.
- [Mutation testing](mutation-testing.md): milestone 85, and the question coverage cannot ask:
  **would any test notice if this line were wrong?** cargo-mutants (pinned in
  `.cargo-mutants-version`, exclusions with reasons in `.cargo/mutants.toml`) rewrites one function
  at a time and reruns the tests; the survivors are the product. The per-crate baseline, the
  calibration verdict on the exhaustive crates (`ntp_proto`, `gpt`), the three-way triage rule
  (write the test, record the exclusion, or defer on the record), and why the weekly `mutation
  testing` workflow is a report rather than a gate.
- [Where an unsafe obligation is written, and where it is only implied](unsafe-obligations.md):
  milestone 82, and the two lints that are meant to compose into "every unsafe operation sits next
  to the written invariant that makes it sound". The survey found **zero violations before anything
  was changed**, because every package we own is edition 2024, where `unsafe_op_in_unsafe_fn` is
  warn-by-default, and `script/lint` runs `-D warnings`; the rule had been a hard gate with nobody
  writing it down. What the lint pair still cannot reach is the useful half: eleven of the tree's 33
  `unsafe fn`s contain **no unsafe operation at all**, so their rustdoc `# Safety` section is the
  only enforcement there is; four safe fns carry a SAFETY comment that discharges an obligation onto
  "the caller" that the signature imposes on nobody (one of them an aarch64/riscv64 asymmetry over
  the same register write). The `#[cfg(kani)]` blind spot is **closed** by milestone 113: a
  fourteenth clippy configuration compiles the proof harnesses against a five-item shim, because the
  other candidate (`-D warnings` on `script/verify`) finds **none** of what is there, `cargo kani`
  driving a rustc where no `clippy::` lint exists. It found 26 warnings in 9 crates, 13 of them
  undocumented `unsafe` (the hand count of 11 had missed two `unsafe impl`s) and 13 with nothing to
  do with unsafe at all. The four safe fns are **decided** by milestone 112: three become `unsafe fn`
  and `virtio::pread` does not, because it is private and the compiler closes its caller set, which
  is what a module invariant is. The test that separates them is not "does the comment say caller"
  but **could the parameter have been produced without meeting the obligation** (`endpoint_of` takes
  `&Scheduler`, which only the lock guard can mint, so its identical-sounding sentence binds). A
  newtype cannot rescue the context switch: the dangerous half of the obligation is liveness and a
  `Copy` wrapper launders it. Taking `pread`'s comment seriously found a **real bug**, a userspace
  driver able to ring a virtio queue it never set up and make the kernel store through
  `phys_to_virt(0)`. The honest verdict on the headline question is that it **cannot be a gate**
  (33 real hits, 19 of them legitimate, and the pattern misses "as above" and the passive voice); the
  adjacent property that can be is now one: every `unsafe fn` has a `# Safety` section, checked in
  `script/lint`, which found one violation.
- [The calendar crate](calendar.md): milestone 51's pure-computation lane: Unix seconds to a civil
  date and back, weekday, day of year, five formats, an RFC 3339 parser. Why 1900 is not a leap year
  and truncating division reports 1970 for the last day of 1969, why the range stops at year 9999,
  why leap seconds are a named error rather than a clamp, and the scope note that a fixed UTC offset
  is in and the IANA database is out (it is a data-distribution problem, not a calendar one).
- [The glob matcher](glob.md): milestone 47's pure-computation lane: `*`, `?`, `[a-z]`, `[!a]` and
  escaping over bytes, with no filesystem in it. Why the matcher is separate from the granting (the
  interesting question is what a match *grants*, and "the expansion you see is the grant" only holds
  if there is one matcher), why **`**` is out permanently** (it is a traversal feature, and descending
  needs a capability, so putting it in a string matcher hides an authority question in a pure
  function), and why zsh's qualifiers are out (they need a read right beyond enumerate). Then the
  single-backtrack-point algorithm and the reason a hostile pattern cannot be made to hang, with the
  bound computable before the match starts. Also the second place a model checker was the wrong tool:
  equivalence with exhaustive search was settled by enumerating 2.7 million pattern/name pairs
  completely, and the blowup test runs at 100,000 bytes.
- [Globbing, and the expansion you see is the grant](glob-grant.md): milestone 47's globbing lane:
  what a match *grants*, which is a directory capability attenuated to a **name set**. Why that is a
  small change (`fs_file_caretaker` already serves a namespace of exactly one name, so this widens
  the namespace and nothing else, with nothing new in the kernel), and why the demonstration is the
  **pairing**: `echo *.txt` prints literally the authority `rm *.txt` would transfer, which Unix
  cannot claim because its `rm`'s authority never came from the command line. The structural
  consequence the roadmap predicted, landed: `plan_against` fills its slots by index and sees the
  **set** rather than the pattern, because the endowment is the set. Why `fs_nameset_caretaker` is a
  **third** caretaker and not a mode on `fs_subtree_caretaker` (that program performs no checks at
  all, and a name filter is a check on seven verbs), its one rule (a name not in the set does not
  exist here) and the `RENAME` destination check that would have been easy to miss. A correction:
  the argument that bash's pass-the-pattern-through is harmless because `*` is refused downstream is
  **false**, and the real cost is a grant that acquires a referent later. And `ARG_MAX` as a
  capability limit, with the bound set at eight by a stack overflow rather than by reasoning.
- [Generational names](generational-names.md): milestone 14 phase A: the thread table becomes a
  fixed generational slot table (`crates/slots`). A Tid is `(generation, slot)`; a dead thread's
  name can never resolve again, even after slot reuse. Bounded like an array, safe like a
  never-reused counter, and the first step toward capability-only thread naming.
- [Intrusive queues](intrusive-queues.md): milestone 14 phase A.2: the run queues and migration
  inboxes become intrusive (`crates/intrusive`); the link lives inside the TCB, a push is two
  pointer writes that cannot allocate or fail, and a pop hands back the thread itself. One link
  means one queue, which is the scheduler's state machine made physical.
- [Benchmarks with teeth](benchmarks.md): milestone 21: two instruments, because gating and
  truth exclude each other. Deterministic icount counts gate commits against a committed
  baseline (`script/bench --check`); HVF runs the kernel natively on the M-series core for real
  magnitudes. The first real numbers (debug): IPC round trip ~705 ns, call/reply ~886 ns. The L4
  calibration built on them was corrected on 2026-08-04: it compared the kernel-side, debug,
  round-trip number against seL4's EL0, release, one-way number, three errors that partly cancelled.
  Milestone 38 added filesystem throughput against ext4 at a matched tier and APFS natively: the
  confined-server tax is 0.07% of a file request, our userspace block server is at parity with
  Linux's block layer, and every 4 KiB request moves a 128 KiB RedoxFS record.
- [The PMU, and the two clocks in a core](pmu.md): the cycle counter (`PMCCNTR`) versus the
  generic timer (`CNTVCT`), and why the coarse, boring timer is the one that survives
  virtualization. The reason our bench runs on a laptop and `sel4bench` does not.
- [ASIDs: tagged address spaces](asids.md): milestone 15: every user mapping is `nG`, each
  address space owns one ASID for life, the tag rides in TTBR0 with the root, and the context
  switch flushes nothing. Why a bitmap suffices where Linux needs generations (milestone 14
  bounded the spaces), and the witness test that would catch a broken tag.
- [The RISC-V TLB shootdown](riscv-tlb-shootdown.md): milestone 58: RISC-V carried an ASID it got
  no benefit from, because every context switch threw the whole TLB away. Why `sfence.vma` needs a
  distributed protocol (SBI RFENCE, and its acknowledgement) where `tlbi aside1is` needs one
  instruction, why removing the flush is gated on a *measurement* of `satp.ASID`'s width rather than
  on the specification, the test that fails without the shootdown, and the honest benchmark: the win
  is invisible under icount and needs the board.
- [init, and loading a program from userspace](init-and-loading.md): milestone 19d: the ELF
  parser leaves the kernel for init, an ordinary confined program. How init loads a child through
  the granular verbs (retype, copy-and-map each segment, endow, configure, start), why
  SYS_CAP_DELETE exists (a loader recycles a 16-slot cspace over hundreds of frames), and the two
  hardware details a userspace loader must respect (I-cache coherency, cross-space W^X).
- [The kernel's own budget](kernel-budget.md): milestone 19c.1: kernel stacks stop drawing
  open-endedly from the frame allocator and draw from one boot-carved region (`kmem`) with
  page recycling, so the kernel cannot spend beyond its carve. The three-round decision behind
  it, and the fact that collapsed it: a thread cannot swap the stack it runs on, so every
  kernel stack is kernel-created and one budget covers all of them.
- [The TCB](tcb.md): what a Thread Control Block is (our `Thread` struct, field by field), the
  acronym collision with Trusted Computing Base, and why TCBs live in a static pool rather than
  being retyped from kernel untyped (the phase B.2 decision: same machine behavior, and seL4's
  retype only earns its ledger once userspace is the one paying).

## The point of all this

- [The console driver leaves the kernel](userspace-drivers.md): milestone 8: the console is now a
  userspace process that owns the UART, reached by IPC, and the kernel is no longer on the data
  path. The 7d confused-deputy bug is *dissolved*, not defended against.
- [Userspace](userspace.md): the line. And as of 7a it is **real**: entering EL0 turns out to be
  *returning from an exception that never happened*, and the two bugs on the way there were worth
  more than the code
  between "a Rust program that boots" and "an operating system." Three walls, all of them
  hardware. **Read this to understand why the milestone order is what it is.**

## Design

- [Why this isn't a general-purpose OS](why-not-general-purpose.md): what an application
  would actually hit (no POSIX/libc, no writable FS, no network, no GUI), why that's a
  deliberate teaching-subset choice rather than a limit of the model (Fuchsia is a
  general-purpose capability microkernel), and what it would take to grow toward one.
- [RedoxFS std-footprint audit](redoxfs-audit.md): milestone 32's engine, costed by building
  it: the no_std core compiles for both bare-metal targets three imports away from clean, the
  Disk trait is a blk-IPC client's exact shape, and the one real cost (a userspace GlobalAlloc)
  was already on milestone 27's books.
- [The RedoxFS filesystem server](fs-server.md): milestone 32 phase 2: RedoxFS confined as a
  userspace component behind a capability-shaped contract (the endpoint IS the directory
  capability, a handle is a server-minted token, open-by-path lives only inside the server). The
  three-process design (block server, FS server, client), why the block server waits on the
  completion interrupt, and the error boundary mapped once. Read AND write are now proven end to end
  on both ISAs, the write with a host-tool reopen of the image; the old "writes loop in the allocator
  commit" open item was stale and the note records the correction. Milestone 31 phase 2 completed the
  write path (`CREATE`, `TRUNCATE`), added the name check that was previously true only by the absence
  of a path walker, and sized the FS server's stack by measurement after a 528-byte overflow presented
  as a mystery 900-second test. Milestone 37 turned crash consistency from a claim into a measurement:
  a power cut at every one of 93 write points, the interrupted write torn at four offsets, and a
  device that lies about persistence, plus the controls that prove the injector bites (with the header
  ring's history removed, 92 of 93 fault points stop mounting) and the honest limit (a lying device is
  never survivable and never silent). Milestone 61 added the **verb table**: one row per opcode in
  `fs_proto::verb`, saying what a request's words mean and which rights the server demands, so the
  three caretakers that proxy this contract dispatch off the contract instead of off three
  hand-written matches, and a verb with no row is a compile error rather than a capability that is
  quietly missing. Milestone 57's write half added `mkfs`, the server's opposite (it creates a
  filesystem and never serves one), the vendor divergence that let it (the uuid becomes an argument,
  the way `ctime` already was) and the correction underneath: the *first* divergence taken for this
  could not have worked, because a `Header` a caller can build has nowhere to go when the write path
  is `pub(crate)`.
- [The directory capability](dir-capability.md): milestone 47's keystone: a directory stops being
  one authority and becomes a **six-rung rights ladder**, with `OPENDIR` handing back a directory
  capability rather than bytes. Why `DESCEND` earns its own rung (bundle it with reading and the
  *shape of the tree* decides how much authority a grant carries, which is ambient authority
  reintroduced by recursion), why attenuation is `parent & requested` by construction rather than a
  check anyone could forget, and why the refusal errno is part of the design (`ENOENT` for a naming
  right, `EROFS` for a mutating one, `EPERM` for `ENUMERATE`, which is the one rung where an empty
  listing would be a lie about the directory). The structural finding: the FS server's handle table
  is per *server*, so **the handle is the authority and the endpoint is the boundary**, which is why
  `fs_subtree_caretaker` exists and why, unlike `fs_file_caretaker`, it performs no rights checks at
  all. `RENAME`'s two atomicities stated apart (§42), the crash-atomic half measured at every fault
  point rather than asserted, and the startup ordering bug that one shared frame hides until a
  caretaker stages a request in it. Milestone 61's section on the verb table records how the three
  caretakers came to share a dispatch without sharing an attenuation, which is what keeps "this one
  checks nothing" true of the one that says so.
- [Removal needs a directory](rm.md): why `rm` is the first program granted a **directory** rather
  than a file (no per-file capability can express "take this name away", because a name lives in the
  directory that holds it), and why `-r` **widens the grant** rather than setting a flag: a program run
  without it holds no way to descend, so **its recursion is not disabled by a branch anybody has to get
  right**. Also `RMDIR` being empty-only, Unix's reporting checked against `rm(1)` rather than
  remembered, and why we need no special case for `/` where Unix ships one. Since 2026-08-17 it runs
  at the interactive prompt for a name one directory down, and the shape it still cannot be given is
  a grant on the root of the shell's own namespace.

- [Navigating with no global namespace](shell-navigation.md): milestone 47's commands: `cd`, `pwd`,
  `ls`, `mkdir`, `rm` as **builtins** (which retires the worry that a listing *program* would hold the
  power to read everything it lists). The three earned divergences and why each is forced rather than
  chosen: no absolute paths (the name cannot be *expressed*, so `InvalidFilename` and not
  `PermissionDenied`), `..` stopping at your root (not a check: the shell pops the stack of
  capabilities it descended through, and at the root there is nothing to pop), and `pwd` relative to
  that root. Why the cwd stops at the process boundary, and where in the code it does: a grant records
  the directory it was resolved in **as a value**, so a child cannot re-resolve a name and a later `cd`
  cannot change what an earlier grant meant. `rm` is **unlink and says so**, which cost a real
  implementation: RedoxFS frees a node the moment its last link goes, so the first version was a
  *revoke*, and registering an open node with the engine's usage table is what makes a holder keep
  reading. Revocation is not offered, and the reason is the per-server handle table. The headline, with
  the real shell binary on both ISAs: two shells rooted in two subtrees, each told nothing about which
  it holds, and neither can name the other's files.
- [Extended attributes](xattr.md): milestone 57's attribute layer, on the critical path to
  milestone 55's backup target because Samba stores Apple's Time Machine metadata as opaque byte
  strings and RedoxFS has none. Four verbs, three limits with a reason each (255-byte names are
  Linux's, and sixteen attributes per node is exactly what makes a listing fit one page, which is why
  `LISTXATTR` has no cursor). Why a **layer** above the engine rather than a fork of its on-disk
  format: reversibility, and the fact that a layer nothing can bypass is as authoritative as the
  filesystem, which is true here and false on Linux. The property only reachable inside the FS
  server: attributes key on the node, so **a rename carries them and nothing in the rename path
  knows they exist**, which AppleDouble sidecars get wrong. The three ways to leak a blob and what
  each costs, the `u32` type code nothing reads (carried so BFS-style indexing is not foreclosed),
  and a BUGS section naming the reserved name and the crash-atomicity claim that is inherited from
  the transaction boundary rather than separately measured. Milestone 61 added the section on the
  three caretakers forwarding these verbs, and on which of them refuses a write through a read-only
  grant (one does it itself; two leave it to the server, which is their design showing through).
- [Reading the backup from a MacBook or a Linux host](host-recovery.md): milestone 57's answer to
  "the board is dead, can I get my data?": `redoxfs_host ls`/`cat`/`extract`, no FUSE, no kernel
  extension, no root, identical on macOS and Linux. Why none of upstream's five binaries already did
  this (`redoxfs-ar` only writes), why the read paths must not write to the image (`cleanup` tidies,
  and `read_node`'s atime update fires only on files last read over an hour ago, so it passes every
  test on a fresh image and dirties the first real backup), and the operational rule: we are pinned
  at format version 8, a reader must match, so the tool or its exact source pin is stored **with**
  the backup. A backup readable only by software you no longer have is not a backup. Also: no
  filesystem-level encryption on this volume, so no key handling anywhere in the recovery path.
  Milestone 110 gave it a **device and a partition** (`--partition N`, `--partition-type GUID`,
  `partitions DEVICE`), which deleted the partition-slicing workaround from xtask, and corrected the
  premise on the way: the engine's header scan meant a partitioned disk read whole never failed, it
  quietly opened whichever filesystem lay in the first 256 MiB.
- [The GUID Partition Table](gpt.md): milestone 57 lane one (`crates/gpt`). The map that says where
  a filesystem starts: the protective MBR, the header, the entry array, the backup, and the four
  CRC-32s that make a GPT **a format that can tell you it is broken**. Why the crate does no I/O, the
  mixed-endian GUID trap, and why `last_lba` being inclusive is the off-by-one a casual test misses.
  Also what two independent real disks (`sgdisk` and macOS `diskutil`) taught us, including that
  **macOS writes no partition names at all**, and the clearest case yet of the enumerate-versus-prove
  rule: four exhaustive corruption sweeps, one of them 4.2 million cases, beside seven Kani harnesses
  for the claims that cannot be counted. Since 2026-08-03 it also covers **writing** a table on the
  target: the version-4 stamp the crate applies to bytes it did not generate, and why a partitioner
  reads every block before it writes one.
- [Block devices: what is attached, and what holding one means](block-devices.md): milestone 57's
  block-device lane, which is where `crates/gpt` stopped being wired to nothing. The guest reads a
  partition table **`sgdisk` wrote** off a virtio-blk device, backup half included, and the only real
  arithmetic (a GPT counts in 512-byte blocks, the block service moves 4096) lives host-tested in
  `gpt::span`. The design claim is the split: a **read-only roster page** says what drives are
  attached, an **endpoint** says you may read and write one of them, and the roster deliberately
  carries no capacity because a size is a fact about a device you hold. The negative control is what
  makes that a claim: the same program writes to the roster's exact address and dies. Also the three
  surprises, including a latent `user/link.ld` bug that only a program with no `.data` could hit.
  The **write half** (2026-08-03) is here too: a disk endpoint plus an entropy endpoint are jointly
  sufficient to partition and format a drive and separately neither is, proved by withholding each
  from the same binary and then reading the disk.
- [Prior art and reuse](prior-art.md): where to look before building (Redox, rCore, Tock,
  Hubris, seL4, Fuchsia) and the rule that decides build-vs-reuse: the reuse boundary is the
  TCB boundary. Inside it, always build; userspace components, actively prefer porting,
  because a confined foreign component is evidence for the milestone-23 thesis.
- [Deadlock](deadlock.md): the four Coffman conditions, and why breaking *any one* makes
  deadlock impossible. Every rule in our locking discipline is "pick a condition and destroy
  it." Also: Rust does not save you from this, and the reason why is worth knowing.
- [Locking](locking.md): why a plain spinlock in a kernel with interrupts is a
  *guaranteed* deadlock on a single core, the two orderings that are the whole point, and
  why "restore" is not the same as "enable".
- [Memory ordering, and the fences with no partner](memory-ordering.md): milestone 116's inventory
  of every fence and every ordered atomic outside test code, each adjudicated into a bug, a stated
  soundness argument, or dead code. The count and the two ways a grep gets it wrong. The structural
  answer to why there are so few (almost every happens-before edge in this kernel comes from the
  `SCHED` lock or a blocking IPC rendezvous, so it lives in a dependency where no grep can find it),
  and the one protocol that has no rendezvous under it, which is where the real bug was. Why the
  broad per-variable check **cannot** work, measured rather than argued: built, run, and it flagged
  the tree's best pair while missing its one genuine finding. The narrow check that ships instead,
  with the limitation named at the same volume as the feature. Also a corrected comment that claimed
  plain `write_volatile` store order was doing work it cannot do.
- [The L4 lessons, audited against this kernel](l4-lessons.md): Elphinstone and Heiser's 20-year
  retrospective renders a verdict on each original L4 decision, which makes it auditable rather than
  inspirational. 15 of 17 applied, one partial, two not, checked file by file. The misses are one
  cluster wearing three hats: no direct process switch, which forces a rendezvous to queue the
  receiver rather than switch to it, which is why every thread needs its own 28 KiB kernel stack.
  Also why milestone 132's 11.2 KiB measurement artifact was a fingerprint of that choice, and the
  finding that neither miss was ever decided anywhere in the tree.
- [How portable kernels are written](portability.md): what actually goes in `arch/` (a
  surprisingly short list), what can't be abstracted (the memory model), and why the second
  port should come early and be as alien as possible.
- [Where nife could actually run](target-hardware.md): the ISA is almost never the
  constraint. What decides bootability, why a Pi 4 is the next port, and why the port
  *after* it should probably be a UEFI/ACPI machine rather than another Device Tree board.
- [The aarch64 board for the seL4 comparison](aarch64-board-survey.md): milestone 25's leftover
  needs a real PMU, and the board has to be one sel4bench *really* runs on, read from seL4's own CI
  configs rather than the support matrix. The three evidence tiers, the candidate table with checked
  prices, why a used Jetson TX1 wins (it is the silicon under the only published aarch64 seL4
  numbers), and the honest port-cost and to-verify lists.
- [Porting to RISC-V](riscv-port.md): the second-architecture port (milestone 20), the real
  test of rule #1. The exact `arch/` boundary RISC-V must satisfy, the two HAL leaks it exposes
  (`Context` is aarch64-shaped in portable code; the `paging` crate encodes the aarch64 descriptor
  format), the RISC-V specifics (SBI, S-mode boot, Sv39, NS16550, PLIC/CLINT), and the incremental
  plan from "compiles for riscv64" to "the capability core runs on a second ISA".
- [The VisionFive 2: first silicon](visionfive2.md): milestone 16a's board facts, every one with a
  source. The four real differences from QEMU `virt` (DRAM base, the DW-8250 UART, the PLIC context
  map, the disabled S7 hart), the Image-header load path through vendor U-Boot, the microSD payload
  and `script/board-image`, the bench runbook with its failure-triage ladder, and the honest list of
  what only the bench can measure.
- [Scoping RISC-V / aarch64 parity](riscv-parity-scope.md): aarch64 is a strict superset once the
  port proved the capability core; this scopes the remaining gap (SMP, an in-kernel test run,
  virtio+DMA, the full boot/shell, benchmarks), what each proves, and the order to close them.
- [The RISC-V arch tests](riscv-arch-tests.md): `arch/aarch64` had 21 unit tests and `arch/riscv64`
  had none, so the properties that differ between the ISAs were asserted on one side only. Writing
  the twins found the timer running at 80 Hz while reporting 100. What translated, what has no
  analogue (and why RISC-V needs no `running_at_el1`), how each test was proved able to fail, and
  which three cannot fail on a machine that booted.
- [The CPU-model matrix](cpu-models.md): every RISC-V result this project had was taken on
  `-cpu rv64`, QEMU's maximalist model, while the board arriving is an RV64GC U74. Running the same
  suite against `sifive-u54`, the RVA profiles and `thead-c906` (211 tests, all five green), the
  preflight that proves `-cpu` is enforced rather than merely advertised, what the narrow models
  would have caught, and the one test written for the board that no CPU model can exercise.
- [The HVF leg](hvf-leg.md): the aarch64 suite on the physical Apple Silicon core, added to
  `script/gates` as its final step (and skipped loudly where HVF does not exist, so a Linux CI
  transcript cannot be misread as silicon coverage). What `--hvf` does and does not re-run, the
  measured cost against TCG, the exact behaviour of a semihosting trap nobody answers, the SMMU
  belief the machine overruled, and the two yield-count assertions a *fast* machine found for the
  same reason a slow one finds them.
- [Load-sensitive assertions](load-sensitive-assertions.md): the milestone 78 verdicts, in two
  rounds. Eight assertions failed pull requests that changed no executable code. Round one sorted
  them by the **direction** of the failure (a slow machine produces a deficit, never a surplus), so
  the three that fired on negative counts were measuring their neighbours rather than their subject.
  Round two sorted three more by their **window**: a tick counted outside the lock it was charged
  to, a probe's execution that placement never promised, a spinner sampled before it had had a turn.
  It also reverses round one's "left alone" verdict on the placement probe, whose 60 s wait turns
  out to be unreachable rather than merely late. What each was rescoped to, the two siblings checked
  and left, and the honest cost of the `<=` trade.
- [The instruction clock](instruction-clock.md): milestone 78's last piece, and the answer to the two
  claims no wall clock can make, because from inside the guest a slow handler and a descheduled
  emulator are the same observation. Under `-icount shift=0,sleep=off` virtual time advances by one
  nanosecond per instruction retired and by nothing else, so `script/icount` asserts that the timer
  fired at the deadline the kernel armed (on riscv64, that SBI was armed with the `DEADLINE` word at
  all), that the handler costs fewer than N instructions, and that no tick was missed. Why it is a
  boot mode rather than a flag on the test path, the measurement that says the reason is **not**
  speed, the calibration that refuses to measure without the flag, and what its 16- and
  100-instruction resolutions can and cannot see.
- [Scoping a PCIe transport](pcie-transport-scope.md): a PCI root complex (ECAM enumeration, BARs,
  virtio-pci capability parsing, INTx via the PLIC) so a virtio disk can be driven over PCIe, the
  transport QEMU's riscv `virt` and real hardware use. Portable (both boards are ECAM-generic); the
  virtqueue/DMA-confinement machinery is reused. Unblocks parity C.

## Build

- [LLVM](llvm.md): the thing that actually turns our Rust into aarch64. rustc is a
  *frontend*; it emits LLVM IR and hands off. Explains why we get an ARM backend, a
  cross-platform linker, and `llvm-objcopy` for free.
- [Linker scripts](linker-scripts.md): who decides what address your code lives at, why
  nobody zeroes our `.bss`, and where the stack comes from when there's no OS.
- [ELF](elf.md): the container the kernel ships in. Sections vs. segments, where the
  entry point lives, what QEMU actually does with `-kernel` (almost nothing), and what a
  magic number is (the `BadMagic` that caught the 19f archive fed to the ELF loader).
- [nifefs](nifefs.md): the boot archive, and the 2026-08-01 change that widened its names
  from 24 bytes to 32. What the wider entry cost (`MAX_FILES`, `DIR_BLOCKS`, and a kernel-stack
  charge that turned out to have been retired already), why the magic bumped this time when it
  did not last time, the three readers a format change has to reach, and the silent name
  truncation found on the way.
- [The boot protocol](boot-protocol.md): how QEMU decides whether you're a kernel or an
  anonymous blob, and the 64-byte arm64 Image header that is the entire difference. Why
  `text_offset` and the linker script must agree, and why the failure mode is silent.

---

## Still to write

Topics we've touched but not yet documented. Add as they come up:

- The GIC (interrupt controller)
- virtio
- [The SCHED lock inventory](sched-lock-inventory.md): what the one remaining scheduler lock protects, by temperature; milestone 17's denominator, gated on 88's curve and 80's method.
