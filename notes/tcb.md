# The TCB (Thread Control Block)

*(Written during milestone 14 phase B, when "where do TCBs live" became a decision.)*

## What it is

The kernel's bookkeeping record for one thread: our `Thread` struct in `kernel/src/thread.rs`.
Milestone 1 defined a thread as "a stack plus a set of register values"; the TCB is where the
kernel keeps everything needed to *manage* that thread while it is not running. Ours holds: the
id (a generational name, notes/generational-names.md), the state, the `context` pointer naming
where the registers are saved on the thread's own stack, ownership of that stack, the address
space, the capability table (cspace), the IPC mailbox, the intrusive queue link
(notes/intrusive-queues.md), and the `on_cpu`/`wake_pending` switch-out flags. "Spawn allocates
a TCB" means allocating this struct. seL4 uses the same name for the same object.

## The acronym collision, so nobody trips on it

**TCB also means Trusted Computing Base** (DECISIONS §14: "a small, machine-checked trusted
core"), which is unrelated. Both senses appear in this project's documents. In milestone 14 and
scheduler contexts, TCB is the thread struct; in §14 and verification contexts, it is the
trusted core. Expand the term when there is any doubt.

## Where TCBs live (the phase B.2 decision)

Decided and built: a **static pool**, a MAX_THREADS-sized array in BSS, with the generational
table's slot `i` being pool slot `i`, so a Tid's slot bits name the TCB's storage directly (the
table's `slot_of` is the lookup, covered by the same proofs as `get`). The address of a slot
never changes, which supplies the pinning the per-thread `Box` used to provide. Spawn writes the
new `Thread` into its slot in place; the reaper drops it in place; nothing on the thread
lifecycle allocates.

The alternative was retyping TCB memory from a kernel-owned untyped region, seL4's shape. The
contrast that decided it: both are a fixed chunk divided into TCB-sized slots; the difference is
*who owns the chunk and who decides to spend it*. seL4's retype exists so **userspace** pays for
kernel objects out of its own budget, which is what makes kernel-memory exhaustion structurally
impossible there. We do not have that ownership boundary yet (no syscall creates threads), so
the retype's only customer would be the kernel: the pool wearing a ledger. Worse, TCBs are
sub-page, so honest retype needs either a page per TCB or sub-page occupancy tracking, which is
the slab rebuilt under another name in the milestone that deletes the slab. The pool is the same
machine behavior with less machinery, and it upgrades to retype-backed storage behind the table
the day the init task (milestone 19) makes the ownership boundary real.
