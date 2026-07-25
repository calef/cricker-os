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
   from the ELF, the child's stack top, the aspace), `START` with `x0` = the child's role.

The child is a second instance of the same multi-role binary, entered at the `CHILD` role. It
SENDs one word through the capability init granted it, and exits. Receiving that word is the proof:
init parsed a real ELF and produced a running thread, and the kernel never looked at the child's
bytes.

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
19d.2 moves it into init and retires the kernel's other loaders. And the child here is loaded but
init still copies the *whole* binary (one `.text` for all roles); a real system would load
distinct programs, which is the same mechanism with different bytes.
