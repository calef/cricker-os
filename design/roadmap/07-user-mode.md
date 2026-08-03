# 7. User mode: EL0, capabilities, the ELF loader, and IPC

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). The densest citation target in the tree (79
references when the backfill was written), and the one early milestone whose outcome is a
decision as much as code: DECISIONS §10, capability-based and microkernel-shaped, recorded in
`491f23d` (2026-07-14) at the decision point §8 had deliberately parked. The original plan row
called this "the actual OS boundary. Decision point," and the deferral held: the decision was
made before any of milestone 7 was written, not hacked in mid-syscall.

The build then landed as lettered commits, same day:

- `a34d002` **7a**: EL0. "The machine now runs code it does not trust," two `svc` round trips
  ("one proves we left; two prove we came back"), and a hostile program killed for touching
  kernel memory while the kernel survived.
- `0c7793d` **7c**: the ELF loader; the kernel runs a binary it has never seen, delivered by
  initrd the way Linux's initramfs is, parsed by a host-testable crate so forging a malicious
  binary for a test is eleven lines.
- `ec6f1b5` **7d**: three syscalls: `exit`, `yield`, `invoke`. No open, no read, no write, no
  fork; the same binary spawned with an empty capability table cannot print one byte.
- `54596b6` **7e**: IPC as synchronous rendezvous, three words in registers, memory never touched
  on the way.

An honest gap: there is no commit titled 7b, and no surviving record of what the lettering
reserved it for; the capability table and address-space work it plausibly named arrived inside
7c and 7d. The history shows the letters, not the plan behind them.
