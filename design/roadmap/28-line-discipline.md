# 28. A solid terminal: the line discipline as a component

**Status: BUILT.**

**In brief.** Line editing, history, ANSI in/out, control characters, and a written terminal contract, as a **swappable userspace component** between the input/console drivers and applications; Ctrl-C as a capability-routed interrupt to the foreground process, not a Unix signal. **Built, §21**: `line_editor` on both ISAs, a sans-IO engine (20 host tests), the contract in notes/terminal-contract.md, `shell_service` retired for userspace init; Ctrl-C routing **built** (two-tier, DECISIONS §24 amendment): a shared-flag cooperative tier and an `Untyped::DESTROY` forcible tier, shell-held, proven on both ISAs with `heeder`/`spinner`; the shell learns of `^C` through `line_editor`'s `OP_INTRCOUNT`

**Why it matters.** a terminal with real behavior is a far better "instance one" for milestone 23's live component replacement than the raw echo loop, and 27's stdio semantics need a terminal that has semantics. Serial, deliberately; the display terminal is 29, and they must not be confused

**Built 2026-07-28; see DECISIONS §21, notes/line-discipline.md, notes/terminal-contract.md, and
design/interrupt-routing.md (the Ctrl-C fork, decided in DECISIONS §24 and now built per its
implementation amendment: shell-held two-tier routing, proven on both ISAs).**

**Deliverable.** The layer Unix calls the tty line discipline, as a swappable userspace component
between the input/console drivers and applications: line editing (backspace, cursor keys,
kill/yank), history, ANSI escape parsing in and out, control characters, and a written contract
for what a terminal owes a program. The interesting design is **Ctrl-C**: interrupting the
foreground process is a capability-routing question (who holds the right to interrupt whom), and
this project's answer will not be Unix signals; that answer is the milestone's kernel-adjacent
substance. Serial, deliberately: the terminal emulator stays on the host end of the wire.

**Why.** Milestone 23's flagship line ("the console hot-swap is instance one") deserves a
component with real behavior, and 27's stdio semantics (line buffering, `read_line`) need a
terminal that has semantics. Pure userspace on machinery that all exists; could land any time.

**Prior art and reuse.** Userspace, outside the TCB: the rule says actively prefer porting.
`noline` (a no_std readline) and `embedded-cli` are live candidates for the editing core; the
component contract and the interrupt routing are ours. Read the Unix tty layer as the
mistake-catalog (its tangle is famous) and Plan 9 (editing pushed to the client) as the
counter-design. **Effort: 1 lane** (measured: it took one).
