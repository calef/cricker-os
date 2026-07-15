# Per-process resource quotas

The security audit's one remaining userspace-reachable attack was **resource exhaustion**: a
process that spawns children without limit, or spawns children that block forever, makes the
kernel allocate a `Thread`, a 16 KiB kernel stack, and an address space per child, with no bound.
Untyped memory (milestone 11) bounds a process's *pages*, but not the kernel objects behind a
spawn. This is the bound for those.

## The idea, and why it needs no bookkeeping

A spawner is given a **quota**: at most N children alive at once. The clever part is *when* a slot
comes back. It is not a timer, not a scan, not a reference count the kernel has to maintain. The
slot is a value that lives **inside the child's `Thread`**, and Rust's ownership returns it at
exactly the right moment:

```rust
pub struct QuotaToken(&'static AtomicU32);
impl Drop for QuotaToken {
    fn drop(&mut self) { self.0.fetch_add(1, Ordering::Relaxed); }
}
```

Reserving a slot is one atomic decrement (a compare-exchange loop that never dips below zero). The
`QuotaToken` holding that reservation is a field of the spawned `Thread`. When the reaper drops a
finished thread, the token drops with it and the slot returns. So:

- A child that **exits** frees its slot the instant it is reaped. A well-behaved workload never
  touches the limit: the shell ran ten `run` commands back to back under a budget of eight, with
  zero refusals, because each worker exited and returned its slot before the next was asked for.
- A child that **blocks forever** (on an endpoint nobody drains, an interrupt that never fires)
  keeps holding its slot, which is exactly right — it is still consuming a thread, a stack, and an
  address space. The budget counts *live* children, and a leaked child is a live child.

This is the same ownership-does-the-work pattern the `KernelStack` and `AddressSpace` already use
(the reaper's `drop` frees them). The quota just adds one more thing that rides the thread's
lifetime.

## What it bounds

`sched::spawn_with_quota(&BUDGET, closure)` returns `None` when the budget is spent **or** the
kernel is out of memory — the caller cannot tell the two apart and does not need to. The shell's
process service uses a budget of eight (`shell_service::SPAWN_QUOTA`) and, on `None`, reports "could
not spawn a process" to the shell rather than panicking (the audit's other spawn finding). So a
spawn flood, or a pile of workers that block and never exit, is capped at eight live children:
eight threads, eight stacks, eight address spaces, and no more. Kernel memory from spawning is
bounded, per spawner, for the first time.

## What it is not

It is a **per-spawner** quota, and today there is one spawner (the shell's service), so it is a
per-process quota for the process that matters. Generalizing to many spawners is a table of
counters instead of one static, not a new idea. And it bounds *spawns*; it does not bound every
kind of kernel object a future syscall might create. The complete answer is still the
untyped-kernel-objects model (a process's entire kernel footprint drawn from its own untyped, as
seL4 does), where a `Thread` and its stack are retyped from untyped exactly as pages are in
milestone 11. This quota is the pragmatic bound that closes the reachable exhaustion vector now;
untyped kernel objects are the principled version, still ahead.
