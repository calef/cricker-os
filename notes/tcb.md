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

Decided at B.2: a **static pool**, a MAX_THREADS-sized array in BSS, a Tid's slot bits naming
its storage directly. That pool was always a scaffold: the B.2 note said "the pool upgrades to
retype-backed storage behind the table when init lands."

**Milestone 19c.2 landed that upgrade, and the static pool is gone.** Every `Thread` is now
**page-resident**: it lives at the start of one page, and the generational table stores a
pointer to it rather than indexing a BSS array. Kernel threads' TCB pages come from the kernel's
own budget (`kmem`, notes/kernel-budget.md); a user process (19c.3) retypes its TCB page from
its own untyped by the same mechanism, the page merely coming from a different budget. A page's
address never changes (direct-mapped, its region pinned), which supplies the pinning the `Box`
and then the pool both provided. The win over the pool: the kernel reserves no per-thread memory
it was not handed, the last corner of milestone 14's no-open-ended-spending thesis. Why this is
now worth doing when B.2 said retype earned nothing: B.2's premise was "the kernel is the only
payer," and 19c is exactly when that expires (init becomes a payer).

On the page-granularity worry the pool decision leaned on: a TCB is sub-page, so page-residency
"wastes" most of a page. That was a real reason to prefer the pool *while the kernel paid*, and
it is not one now, because the page is paid by whoever owns the thread (a user thread's by its
creator's untyped, a kernel thread's by `kmem`, both bounded budgets). Sub-page packing stays
declined for the same reason it was for endpoints (19a): one memory rule for the whole object
family, packing a later placement optimization behind the table, not a slab rebuilt in the
milestone that deleted the slab.
