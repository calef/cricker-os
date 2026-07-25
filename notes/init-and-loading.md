# init, and loading a program from userspace

*(Milestone 19d. `kernel/src/user.rs` `spawn_init`, and the `init`/`child` roles and `build_child`
loader in `user/src/hello.rs`.)*

## The one thing 19d moves, and why it matters

Until 19d, when a program ran on cricker-os the **kernel** read its file and set it up: it parsed
the ELF (the standard "here is a program: code here, data there, start at this address" format),
copied the pieces into memory, and started it. That parser lived inside the kernel.

Parsing a program file means processing bytes an attacker may have crafted, and a bug in a parser
is where exploits live. A bug in a parser *inside the kernel* is the worst kind: it compromises
the trusted core the whole §14 thesis rests on. So 19d moves the parser **out**, into an ordinary
confined program where a parser bug is just that program's problem, confined by the same
capability walls as any workload.

That program is **init**: the first program the kernel starts, whose job is to start the others.

## What still loads init (the honest residue)

Something has to load the *first* program, so the kernel keeps exactly enough loader for one: it
`spawn_init`s init and nothing else. init loads every *other* program. "The kernel loads exactly
one program" is not a slogan we rounded up to; it is literally one call site. (19d.2 removes the
kernel's other loaders, the ones that wire up the console and shell services today, by moving that
wiring into init.)

## How init loads a child (the loader, in userspace, through the verbs)

init is handed three things by `spawn_init`: a building **untyped** budget (slot 0), a **report**
endpoint (slot 1, with `GRANT` so it can endow a child), and the whole **initrd mapped read-only**
at `INITRD_VA` so it can read the ELF. Its length arrives in `x1`.

`build_child` then does, entirely through the milestone-19 granular verbs, what the kernel's
`map_segments` used to do privileged:

1. `RETYPE_OBJ(ASPACE)` — a fresh address space out of init's budget.
2. For each ELF segment, page by page: `RETYPE` a frame, `frame::MAP` it read/write into init's
   *own* scratch window to fill it (zero it — free `.bss` — then copy the segment's bytes), then
   `MAP_INTO` the child at the segment's own virtual address with the segment's permissions
   (executable code via the `MAP_CODE` mode 19d added). Then `cap_delete` the frame cap so the
   16-slot cspace recycles the slot: a loader retypes hundreds of frames, so slot recycling is
   why `SYS_CAP_DELETE` exists.
3. A stack frame, mapped read/write.
4. `RETYPE_OBJ(TCB)`, `CAP_INSERT` the report endpoint as the child's slot 0, `CONFIGURE` (entry
   from the ELF, the child's stack top, the aspace), `START`. `START` carries the child's first
   three registers: `x0` is the role (which of the multi-role binary this instance is), and `x1`,
   `x2` are data the child needs before it can run. See "The argument to START" below.

The child is a second instance of the same multi-role binary, entered at the `CHILD` role. It
SENDs one word through the capability init granted it, and exits. Receiving that word is the proof:
init parsed a real ELF and produced a running thread, and the kernel never looked at the child's
bytes.

## The argument to START (milestone 19e)

Through 19d, `START` handed the child exactly one word, its role in `x0`, and that was enough:
every child was pure code selected by identity. A *worker* breaks that. It computes `n*n`, and `n`
has to reach it before it runs. So 19e widened `START` to carry `x0`, `x1`, `x2`, the way a
function call carries arguments, and the loader passes the role in `x0` and the input in `x1`.

The plumbing is one value carried through the whole thread-creation path. `START`'s three
arguments land in `Thread::start_args` (`kernel/src/thread.rs`); `arm_for_start` writes them into
the faked switch frame as `x21/x22/x23`; the EL0 trampoline (`context.s`
`user_entry_trampoline`) moves those into `x0/x1/x2` right before dropping to the child's entry.
The child sees them as the arguments to its `_start(x0, x1, x2)`.

init's spawn service (the shell's `run <n>`) is the payoff: the shell SENDs `n`, init builds a
worker endowed with a result endpoint and `START`s it with `n` in `x1`, the worker squares it and
SENDs the answer straight to the endpoint the shell is waiting on. init only builds the pipe; it
never sees the number. The kernel test `init_builds_a_worker_and_passes_it_an_argument` proves the
argument survives the crossing: the worker reports `n*n`, not `n` and not garbage.

## Two hardware details a userspace loader must respect

- **The instruction cache is not coherent with the data cache** (aarch64). init writes a child's
  code as ordinary data; the CPU's instruction fetcher has never heard of those bytes. So when
  `MAP_INTO` maps a page executable, the *kernel* makes it coherent (clean to the point of
  unification, invalidate the I-cache) for that physical page. Without it, the child fetches
  whatever was in the frame before the program was written into it. This is the same
  `sync_icache` the kernel's own loader always did; 19d just moved *when* it happens.
- **W^X across two address spaces.** init keeps a code frame writable in its own scratch window
  while the child maps it executable. A trusted loader mapping pages writable to fill them is the
  standard shape (seL4's does the same); the child's mapping is never writable, so the child
  cannot rewrite its own code.

## What is not here yet

The service migration (console, shell, the demo wiring in `user.rs`) still runs kernel-side;
19d.2 moves it into init and retires the kernel's other loaders. And every program here is a
*role* of one multi-tool binary (`hello`) selected by `x0`: init, the child, the console server
are the same ELF loaded more than once. A real system has **distinct binaries per program**,
each exactly its one job. That is not "the same mechanism with different bytes" as this note first
claimed: it needs a **program-delivery mechanism** we do not have yet, because today the kernel
hands init a single initrd blob. Delivering several named programs wants either a bundled archive
(Linux-style initramfs, indexed by name) or loading from the `crickerfs` filesystem on the disk
(programs as files, `exec`'d by path). That is milestone 19f (design/init-and-granular-spawn.md),
and it is where "console server as its own binary" actually lands.
