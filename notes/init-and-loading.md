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

## The initrd is an archive now (milestone 19f.1)

Through 19d/19e the initrd *was* one ELF: the kernel parsed the whole blob as the init program, and
init, to load a child, parsed that same blob again (children were roles of the one binary). 19f
turns the blob into a **crickerfs archive**, the same named-file format the virtio disk uses, so one
parser serves both the RAM archive and the disk. `cargo xtask` packs it (`mkinitrd`); it holds one
entry today, `init`.

Two readers changed, each in its own domain:

- The **kernel** (`spawn_init`) reads the superblock, looks up the `"init"` entry, and loads *that*
  as the ELF. This is the same honest residue as before ("something has to load the first program"),
  now naming that program through a fixed archive index instead of assuming it sits at offset 0. The
  kernel gains a crickerfs read, which is proportionate to the ELF parse it already does for init and
  is bounded (a 512-byte superblock, count capped at 15). The milestone tour and the kernel-wired
  demos load a program the same way, through `user::program("init")`.
- **init** (`hello.rs` `program()`) parses `INITRD_VA` as a crickerfs archive and looks up a program
  by name, rather than treating the whole blob as one ELF.

Why the archive rides *beside* the kernel (handed in via the device tree) rather than baked into
init's own image: this is the QNX-IFS / old-Linux-initrd lineage, and it keeps the "delivery, not a
rebuild" property. You can swap userspace without rebuilding the kernel. seL4, our nearest relative,
instead embeds a cpio of the app ELFs into its root task; we chose the delivered-archive side so the
RAM boot and the eventual disk boot share one parser.

## The first distinct binary: the worker (milestone 19f.2)

The worker is the first program that is **its own binary**, not a role of `hello`. It lives in
`user/src/worker.rs`: its own `_start`, its own panic handler, ~30 lines, and not one line of hello's
code. It shares the `user` package's `link.ld` (so it links at `0x40_0000` like hello), which is not
a conflict because each program runs in its own address space. `mkinitrd` packs it as a second
archive entry, `"worker"`, beside `"init"`.

Every consumer that used to spawn "a role-6 worker of hello" now loads `"worker"` by name and starts
it with `x0 = 0` (a standalone binary needs no role selector) and the input in `x1`:

- init's `init_worker` and the initboot spawn service (`hello.rs`), for `run <n>`.
- the kernel-side `shell_service` (the pre-initboot interactive shell), same `run <n>`.

Removing the worker from `hello` is what proved the split was real: it broke every one of those call
sites (a role-6 spawn fell through to hello's default arm and *faulted*), and fixing each to load
`"worker"` is the migration. The hello binary no longer contains a worker at all. Two headless tests
pin it: `a_spawned_worker_process_computes_and_reports` (kernel spawns the worker binary, gets 81)
and `init_builds_a_worker_and_passes_it_an_argument` (init loads it by name, gets 49).

The tiny syscall runtime in `worker.rs` (`invoke`/`send`/`exit`) is duplicated from hello on purpose.
When 19f.3 splits the next binary, that second copy is the signal to lift a shared user-runtime
crate, with the requirements known rather than guessed (DECISIONS: don't build the abstraction before
the requirements are).

## What is not here yet

Console, input, and shell are still roles of `hello`. 19f.3+ migrates them into their own binaries
the same way the worker went, which is where "console server as its own binary" lands. The
kernel-side service wiring that the milestone tour still uses is retired by the `initboot` path,
which builds those services inside init.
