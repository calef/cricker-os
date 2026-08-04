# 21. The terminal is a userspace component, and the kernel is out of the shell business (milestone 28)

**Status: AMENDED.** (one sentence superseded by milestone 41's deletion, recorded in place.)

**Decided and built 2026-07-28.** Milestone 28 put the tty line discipline in userspace as a
swappable component (`line_editor`), sitting on plain endpoints between the input/console drivers and
applications. Three things here are decisions, and the reason each gets recorded rather than left
in code:

- **The terminal protocol is a userspace protocol, not kernel ABI.** The opcodes
  (`OP_WRITE`/`OP_READLINE`/`OP_BYTES`), the read flags, and the shared-page convention live in
  `line_editor::proto` and are written up in [notes/terminal-contract.md](../../notes/terminal-contract.md).
  Every request is an endpoint `CALL` served through `RECV_CAP` and answered through the one-shot
  Reply capability (§12); the kernel routes the words without reading them. **No new syscall and no
  new kernel method were added.** This is the §4 boundary held on purpose: a whole tty layer landed
  as userspace composition, not as syscall surface.

- **The kernel is retired as the interactive system's builder.** The aarch64 kernel-wired
  `shell_service` (the pre-19d.2c path) cannot host a shell that speaks the terminal contract, so
  every aarch64 interactive build (the milestone tour, `--features shell`, `--features initboot`)
  now hands off to userspace init through `boot_via_init`, the way RISC-V's `--features shell`
  already hands off to the portable `system_initializer`. `shell_service` was kept as dead code for reference at
  the time, and **milestone 41 deleted it outright on 2026-07-30**, along with `input_service`. That
  supersedes the sentence this one replaces, and the reasoning is the project's existing rule rather
  than a new one: the heap and slab crates were deleted the same way on 2026-07-27, because *the git
  history preserves the work and a demonstrator's tree should hold what it ships* (notes/heap.md).
  Nothing was lost that this decision had not already replaced: the capability milestone 10 delivered,
  a shell at EL0 spawning processes on command, is exactly what userspace init does now, and doing it
  in userspace is the thesis rather than a consolation.

  **One honest caveat on that claim**, since it is the sort of thing that decays: no test in the suite
  boots the interactive shell, so "the capability still exists" rests on the hand-validated boot path
  rather than on the gate. Milestone 31's phase 3 is that one item, and it should gate that boot
  before anything else leans on it.
  This completes the §15 / 19d.2c direction ("userspace init is the boot path") for the
  interactive system on both architectures; the reasoning and the deadlock-freedom argument are in
  [notes/line-discipline.md](../../notes/line-discipline.md).

- **`^C` (interrupting the foreground process) is deferred as a design fork.** The terminal
  detects the interrupt and the contract carries `FLAG_INTERRUPTED`, but *routing* the interrupt to
  a running foreground process is a capability-routing question whose answer will not be Unix
  signals, and it is not built. The problem, candidate mechanisms, prior art (seL4, Fuchsia, Plan
  9), and a recommendation are in [design/interrupt-routing.md](../interrupt-routing.md), for
  the architect to settle before code.

The engine was **built, not ported**, against the §14 default for userspace, because `noline`
blocks on a cursor-position report a piped line never answers and is a per-read readline rather
than an always-on discipline, and `embedded-cli` is the application's altitude. The full accounting
is in [notes/line-discipline.md](../../notes/line-discipline.md).
