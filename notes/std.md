# Rust `std` on the native ABI

*(Milestone 27. The first wall an application hits on nife was "no std": you could write a
`no_std` binary against `crates/user_rt`, and nothing else. This milestone makes ordinary Rust,
`Vec` and `String` and `println!` and `Instant`, compile and run on the capability ABI. See
DECISIONS.md §22 for the decision and why; notes/abi.md for the ABI it binds to.)*

The shape is **Hermit's, not Redox's**. Hermit implements std's platform layer directly on a
non-POSIX unikernel ABI; Redox writes a POSIX C library (relibc) first and puts std on top of that.
We took the native road: there is no errno, no fd table, no `open`, no `fork` under our `sys`
backend, because the OS does not have them and std does not actually need them to run a workload
that stays off files and sockets. That is the whole point of having done the native ABI first
(DECISIONS §14, §15): std widens "runs real workloads" from hand-built `no_std` binaries to most of
crates.io, without smuggling in the POSIX assumptions the ABI deliberately excludes.

## What a std program is given

A std program is an ordinary nife ELF (notes/abi.md §3): entered at `_start`, linked at
`0x40_0000`, cspace populated by its parent. std's runtime contract needs two things, and the ABI's
out-of-band convention (notes/abi.md §4) grants them at fixed slots:

- **slot 0: an untyped budget.** The global allocator draws heap pages from it lazily via
  `untyped::MAP`, one page per invoke, at `0x4000_0000`. This is the same untyped-backed heap the
  `allocator_exerciser` workload proved (`crates/user_heap` algorithm, host-tested), restated inside std because
  std cannot depend on an out-of-tree crate.
- **slot 1: an endpoint with WRITE.** `stdout` and `stderr` SEND here, 16 bytes per message (w0 =
  byte count, w1|w2 = the bytes, little-endian). std's own `LineWriter` batches user writes; the
  receiver reassembles.

Three more slots exist, and a program holds each only if it was *given* the thing behind it
(milestone 27 phase two, the `std::net` and `std::fs` bindings below):

- **slot 2: a `Stack` endpoint with WRITE.** `std::net` speaks net_stack's socket contract over it.
- **slot 3: an untyped budget** the net PAL mints each socket's shared frame from.
- **slot 4: an FS-service endpoint with WRITE**, which *is* a directory capability, plus the page it
  shares with the FS server mapped at `0x1100_0000`. `std::fs` speaks the §27 file contract over it.
- **slot 5: a `Frame` capability naming the clock page**, with `READ`, plus a read-only mapping of it
  at `0x1200_0000`. `SystemTime::now()` is the offset it finds there plus the ambient counter
  (milestone 51, §43).
- **slot 6: the entropy service's request endpoint**, with WRITE (milestone 56, §44). It means "you
  may obtain randomness" and names no device; there is no mapping alongside it, because randomness is
  obtained by asking rather than by reading. `std::random::SystemRng` is a `CALL` on it.

A program that never allocates, prints, opens a socket, or opens a file never touches the slots it
does not use. The absence of slots 2 and 3 is exactly what "no ambient network" feels like from
inside a process, and the absence of slot 4 is "no ambient filesystem": each returns `Unsupported`
because there is no capability to reach, not because the code was compiled out. A program can hold
one and not the other, so the slots do not fill contiguously; notes/abi.md §4 records how the
kernel-side wiring places slot 4 while leaving 2 and 3 empty, and why the gap matters.

## The PAL surface, and what each piece binds to

The backend lives in `patches/std-nife/overlay/std/src/sys/` and is materialized into a patched
std by `cargo xtask std-src`. Each file binds one std concept to the ABI:

| std concept | nife binding |
|---|---|
| `GlobalAlloc` | `untyped::MAP` from slot 0, grow-on-demand (`sys/alloc/nife`) |
| `stdout` / `stderr` | `endpoint::SEND` on slot 1 (`sys/stdio/nife.rs`) |
| `Instant`, `SystemTime` | the virtual counter, `CNTVCT_EL0` / `rdtime` (`sys/time/nife.rs`) |
| `panic!` | print, then `brk`/`ebreak`: a fault the kernel attributes. No unwinding. |
| `thread::spawn` | `Unsupported` in phase one; `sleep`/`yield` are real |
| `net` (`TcpStream`, outbound `UdpSocket`) | net_stack's socket contract on slots 2/3 (`sys/net/connection/nife.rs`), or `Unsupported` when not granted |
| `fs` (`File`, `metadata`, `read`/`write`) | the FS service's file contract on slot 4 (`sys/fs/nife.rs`), or `Unsupported` when no directory was granted |
| `std::random::SystemRng` | the entropy service's endpoint on slot 6 (`sys/random/nife.rs`), or a **panic** when not granted |
| `HashMap` seed | the same service when granted; splitmix64 from the counter when not, and labelled |
| `std::env::consts::OS` | `"nife"` (patched into `env_consts.rs`) |
| `std::env::var` / `vars` / `set_var` | a **process-local table, empty at start** (`sys/env/nife.rs`); nothing endows a nife process with variables |
| `std::env::temp_dir` / `split_paths` / `join_paths` | `TMPDIR` or `.`, and a `:`-separated list (`sys/paths/nife.rs`); `current_dir`, `current_exe`, `chdir` refuse and `home_dir` is `None` |
| `std::process::id` | `0`, because this system issues no process identifier (`sys/process/nife.rs`); everything else in `std::process` refuses |

The syscall glue (`sys/pal/nife/rt.rs`) is a deliberate twin of `crates/user_rt`: the same
`svc`/`ecall` wrappers, restated because std cannot depend on the crate. The ABI **constants** are
not restated: `abi.rs` is generated verbatim from `crates/abi` by `std-src`, so the numbers cannot
drift. Likewise `user_heap.rs` from `crates/user_heap` (the host-tested heap algorithm is the only heap
algorithm), `netproto.rs` from `crates/socket_proto/src/lib.rs`, and `fsproto.rs` from `crates/fs_proto`: every
wire format the PAL speaks has exactly one definition, and it lives with the server that answers it.

## The toolchain: build-std against a patched rust-src

There is no crate to adopt; the deliverable IS the PAL, plus the machinery to build it. Rust's
`-Zbuild-std` compiles std from source, and it finds that source in the sysroot of the rustc it
invokes. So a **patched std means a toolchain whose sysroot is patched**. `cargo xtask std-src`
builds one:

1. **Hardlink-clone the real nightly** (`cp -al` of `bin` and `lib`). Blocks are shared, so the
   clone costs almost no disk. rustc resolves *this* directory as its sysroot (it derives the
   sysroot from the location of `librustc_driver`, which the clone puts inside the farm; a symlink
   farm does not work, because the symlink resolves back to the real toolchain, which was the first
   thing tried and measured).
2. **Replace the `src` subtree with a real copy** (independent inodes), so patching it never
   touches the shared rustup toolchain.
3. **Patch that copy**: drop in the overlay PAL files, generate `abi.rs`/`user_heap.rs`, and insert a
   `target_os = "nife"` arm into std's `cfg_select!` dispatchers (pal, alloc, stdio, random,
   thread, time, io/error, thread_local storage and guard) plus `env_consts` and the
   `restricted_std` chain in std's `build.rs`.
4. **Link it** as the `nife-dev` toolchain (`rustup toolchain link`).

`cargo xtask std-exerciser` then builds the `std_exerciser` demo for both custom targets against it. The build
sets `RUSTUP_TOOLCHAIN=nife-dev` explicitly rather than `+nife-dev`, because the cargo proxy
that launched xtask already exports `RUSTUP_TOOLCHAIN=nightly`, which would override a `+` selector
and silently build std from the *unpatched* sysroot.

`std-src` is idempotent: a stamp of all inputs (the toolchain version, the ABI/heap crates, the
target specs, every overlay file, and a patch-logic version) guards the rebuild, so a warm farm and
its build-std cache survive across runs and only a PAL change forces std to recompile.

### `nife-dev` is global to the machine, and the stamp does not guard it

**The farm is per-worktree; the name is not.** `rustup toolchain link` writes one symlink under
`$RUSTUP_HOME/toolchains` for the whole user account, so `nife-dev` means whichever worktree ran
`std-src` last, while every build downstream resolves std through that name rather than through a
path. Two agent lanes gating at once therefore contend for it, and the loser does not fail: it
compiles against a farm inside somebody else's worktree.

**A warm stamp used to be enough to skip the link entirely**, which is what made the failure silent.
The stamp answers *is this worktree's farm built*, and nothing was asking *does `nife-dev` still
mean it*. On 2026-08-18 lane `55-durability` relinked mid-run and lane `64-more`'s `std_exerciser`
built against 55's farm; it was caught by a person reading the `Compiling std` path out of the build
output, and nothing else would have caught it. AGENTS.md had warned about this shape in prose since
2026-08-01 and the warning is rung four, which is exactly as much as it turned out to be worth.

`std_src` now verifies the link on the warm path and **relinks, loudly, when it points elsewhere**.
Relink rather than refuse, because the lane calling it is about to build and needs the name to mean
its own farm; taking the link is what every lane already does by design. What changed is that the
theft is deliberate and printed, so a foreign `Compiling std` path cannot happen without a line
above it naming who took what. It also fixes the dangling case AGENTS.md describes, where a pruned
worktree left `nife-dev` pointing at nothing and unrelated builds failed far from the cause with
`override toolchain 'nife-dev' is not installed`.

**This does not make concurrent lanes safe, and must not be read that way.** It makes the loss
visible and self-healing at the next call. A lane whose build is already in flight when another
relinks still loses; the honest fix is a per-worktree toolchain name, which nobody has priced.

### The target specs

`targets/{aarch64,riscv64}-unknown-nife.json`, built with `-Zbuild-std` and `-Zjson-target-spec`.
The load-bearing fields:

- `"os": "nife"` selects our `sys` backend through every dispatcher.
- `"panic-strategy": "abort"` means unwinding machinery is never even linked; `panic!` prints and
  faults.
- `"singlethread": true` turns off `target_has_threads`, so std uses its `no_threads` sync
  primitives and single-`static` TLS. This is honest for phase one (one thread of execution per
  process, `thread::spawn` is `Unsupported`); it flips off when real threads arrive.
- softfloat (aarch64 `-neon`, riscv `lp64`) matches EL0/U-mode with no FP save area, the same
  choice the `no_std` `user` crate makes.

The build also passes `-Zbuild-std-features=compiler-builtins-mem` to supply `memcpy`/`memset` for
the bare target.

## `std::net` over the socket contract (milestone 27 phase two)

`sys/net/connection/nife.rs` binds std's `TcpStream` and outbound `UdpSocket` to net_stack's socket
contract (DECISIONS §25, notes/net.md, `crates/socket_proto/src/lib.rs`). The PAL is a **client** of the
frozen contract, nothing more: it holds the `Stack` endpoint (slot 2) and a frame untyped (slot 3),
and for each socket it mints a shared `Frame`, maps it, delegates it to net_stack (`SEND_CAP`,
`OP_ATTACH_FRAME`), and then drives the socket with `CALL`s carrying a socket id. Control words ride
the message; bytes sit in the shared frame. This is the exact path the hand-written `socket_test_client` client
walks, reached through std's blocking API instead.

The wire constants are not restated: `netproto.rs` is generated verbatim from `crates/socket_proto/src/lib.rs`
into `sys/pal/nife/netproto.rs` by `std-src`, the same anti-drift discipline as `abi.rs` and
`user_heap.rs`. If the contract changes, the PAL's numbers change with it, because there is one source.

What binds, and how it maps to the contract:

- **`TcpStream::{connect, read, write, ...}`** -> `OP_OPEN_TCP`, `OP_CONNECT`, `OP_RECV`, `OP_SEND`,
  `OP_CLOSE` (on `Drop`). `read` blocks in net_stack until data arrives (a blocked `RECV`), the blocking
  semantics std's default API wants. A short `read` keeps the segment's tail in a per-socket residual
  buffer, so a stream never drops bytes.
- **`UdpSocket::{bind, connect, send, recv, send_to, recv_from}`** -> `OP_OPEN_UDP`, `OP_SENDTO`,
  `OP_RECV`. UDP `connect` only fixes a default peer (no contract call, matching Unix). `bind`'s
  local address is validated but not honored: net_stack assigns an ephemeral local port.
- **Errors map by meaning, no errno.** A refused TCP connect is `ConnectionRefused`; a net_stack timeout
  on `RECV` is `TimedOut`; a datagram larger than the frame is `InvalidInput`; an IPv6 address is
  `Unsupported` (net_stack is IPv4-only). A `CALL` on an empty `Stack` slot (no network granted) reads
  back negative and becomes `Unsupported`, the same answer a program with no net grants gets.

The concurrency model is the contract's: single-threaded, one synchronous exchange at a time. A
program can hold up to `MAX_SOCKETS` (4) sockets at once and interleave them, but there is only ever
one operation in flight, which is all a single-threaded process can do anyway.

**A finding, recorded honestly.** net_stack derives a socket's local port from its socket id
(`LOCAL_PORT_BASE + sid`), so an id is not an ephemeral port that rotates; reopening a just-closed id
reuses its exact local port. Against QEMU's slirp, a TCP connect that reuses a port whose previous
flow has not cleared stalls (the SYN's answer never comes, and net_stack blocks in its bounded poll on
the NIC interrupt). The PAL softens this by handing out ids round-robin, so consecutive opens prefer
different ids and ports, but a program that churns through more than `MAX_SOCKETS` sockets quickly
can still hit a reused port. The real fix is net_stack assigning ephemeral local ports independent of the
socket id, which is a **contract-side change reported up, not a client workaround**. The demo
sidesteps it by keeping its UDP and TCP sockets on distinct ids at once.

## `std::fs` over the FS-service contract (milestone 27 phase two)

`sys/fs/nife.rs` binds std's `File` to the FS server's file contract (DECISIONS §27,
notes/fs-server.md, `crates/fs_proto`). Like the net PAL it is a **client** of a frozen contract and
nothing more, and like the net PAL its wire constants are generated verbatim (`fs_proto` becomes
`sys/pal/nife/fsproto.rs` by `std-src`), so the PAL's numbers cannot drift from the server's.

### The interesting part: `File::open` takes a path, and there is no global namespace

This is the design question the binding had to answer, and the answer is not a compromise. Per §27,
open-by-path exists **only inside the FS server**, resolved relative to the one directory node the
client's endpoint is bound to. So the honest mapping is:

> a std program holds a **directory capability** (slot 4), and `File::open("motd")` means *"motd,
> under the directory I was granted"*, not *"motd somewhere in a global filesystem"*.

Four behaviours follow, and each is enforced on the client side, before a byte reaches the wire. The
server enforces the same rule again (it resolves one component in its bound directory and nothing
else); doing it here as well is not redundant, it is what turns a would-be escape into a legible
`io::Error` instead of an `ENOENT` that reads like a missing file.

- **An absolute path is refused.** `/etc/passwd` names nothing: this process holds a directory
  capability, not a filesystem root.
- **Any `..` is refused.** It would leave the granted directory, and no capability designates what is
  out there.
- **A nested path is a chain of descents** (milestone 122). `File::open("a/b/c")` is `OPENDIR a`,
  `OPENDIR b`, then `OPEN c` against `b`'s handle. It used to be refused outright. The grant is
  exactly as tight as it was: every hop resolves under the capability this process holds, rights only
  ever narrow, and there are no symlinks to escape through. What changed is only whether the program
  has to spell the descent itself.
- **A name that IS expressible but absent is an ordinary `NotFound`**, which is what makes the three
  refusals above meaningfully different from "no such file".

**The refusal is `ErrorKind::InvalidFilename`, deliberately not `PermissionDenied`.** Nothing
consulted a permission; there is no name here for what was asked, because no capability designates
it. Mapping a capability refusal onto `PermissionDenied` would be a Unix EPERM fiction, and this
whole milestone exists to avoid smuggling POSIX assumptions into std (the §22 reasoning). `NotFound`
was the other candidate (a sandbox commonly reports ENOENT for paths outside its namespace) and was
rejected for conflating the two cases a program actually wants to tell apart.

### Detecting "no filesystem" without touching the shared page

A program that was not granted a directory has **no shared page mapped**, so a probe that wrote a
name into it would fault instead of returning an error. The probe therefore has to carry no payload,
and it is an `FSTAT` on a handle number the server's table can never contain:

- **no capability in the slot:** the kernel refuses the invoke itself and answers with one of its own
  small negatives (`NoSuchSlot` -1, `WrongObject` -2, `NotPermitted` -3).
- **a real server:** it answers `-EBADF` (-9) for the impossible handle, which is a *reply*, so a
  filesystem is reachable.

The answer is cached, because a cspace slot's contents are fixed at spawn on this ABI.

**A wart of the contract, and this paragraph used to be wrong about it.** The wire's error space (a
negated errno) overlaps the kernel's invoke-error space (-1..-8), so `-2` is both `ENOENT` and
`WrongObject`, `-5` is both `EIO` and `BadMethod`, and **`-1` is both `EPERM` and `NoSuchSlot`**.
This note said the overlap was harmless because neither `EPERM` nor `ESRCH` is in the FS server's
vocabulary; `EPERM` has been since milestone 47 and nobody noticed for four milestones. Milestone 122
resolved `-1` in favour of the server (see the descent section below): every entry point checks
reachability first, so a kernel `NoSuchSlot` cannot follow a reply. The cost of that choice is that a
**revoked** FS endpoint now reads as `PermissionDenied` rather than `Unsupported`, which is a trade
made deliberately, because `EPERM` is reachable every day and revoking the FS endpoint is milestone
108's open question. `-3` still reads as the kernel's, because `ESRCH` really is not in the server's
vocabulary. The clean fix is a tag or an offset in the reply word, which is a contract change
(`fs_proto`, the FS server, and `fs_test_client`), reported up rather than papered over here.

### What binds, and what stays Unsupported

Bound: `File::open` (`OPEN`), `read`/`read_to_end`/`read_to_string` (`READ`), `write`/`write_all`
(`WRITE`), `seek`/`stream_position`, `metadata`/`len` and `File::size` (`FSTAT`), close on `Drop`
(`CLOSE`), and `std::fs::{metadata, read, exists}` built from open + fstat + close. The file position
lives on the client side because the contract's read and write are both explicitly positional, so
there is no cursor in the server to get out of step with, and a seek costs no message at all except
`SeekFrom::End`.

**Also bound since milestone 31 phase 2** (this used to be the head of the Unsupported list):
`File::create`, `OpenOptions::create_new`, `create(true)`, and `OpenOptions::truncate`, backed by the
contract's new `CREATE` and `TRUNCATE` verbs. **So `std::fs::write` works.**

Two things about that are worth keeping. The order in `File::open` is POSIX's and it is load-bearing:
open, then create only if the open reported `NotFound` and the caller asked for it, then truncate
after a successful open of an existing file. `std::fs::write` is `create(true).truncate(true)`, so
getting the order wrong leaves the old tail behind on precisely the path that exists to *replace* a
file's contents, which is the confusion DECISIONS §27 was corrected four times over. And the previous
refusal was the right call at the time for a reason worth remembering: without `TRUNCATE`, a
`std::fs::write` would have half-worked, and a write that half-works reads as a write that failed.
`create_new` over a name that exists closes the handle the probing open minted before returning
`AlreadyExists`, because the error path is the one nobody exercises and therefore the one that leaks.

**And bound since milestone 64's second pass:** `File::set_len` and `fs::copy`. `set_len` is the same
`TRUNCATE` message `File::open` has been sending since 31, with the requested size in the second word
instead of a 0; it grows as well as shrinks, because the contract's verb is `ftruncate` in both
directions and a binding that only shrank would pass a shrink-only test. `copy` is backed by no verb
at all and needs none. Neither was waiting on the contract, which is the third time this milestone
has found a refusal that outlived its own reason; see notes/crates-io-on-nife.md.

**A per-file grant needs no std API at all**, which is the payoff of having bound the PAL to a
capability contract rather than to a namespace. A program handed a narrowed file capability (§27's
caretaker, `user/src/fs_file_caretaker.rs`) is an ordinary `std::fs` client: the one granted name
opens, every other name is an ordinary `NotFound`, and a write through a read-only grant surfaces as
`ErrorKind::ReadOnlyFilesystem`. Nothing in the PAL knows whether slot 4 leads to a directory or to
one file, and it does not need to.

**And bound since milestone 64: the namespace verbs.** `read_dir`, `create_dir`, `remove_file`,
`remove_dir` and `rename`, on `OPENDIR`, `READDIR`, `MKDIR`, `UNLINK`, `RMDIR` and `RENAME`.

**None of these was waiting on the contract**, which is the part worth recording rather than the
feature. The FS server had been dispatching all six since milestones 47 and 48, while this note and
the PAL's own comments went on saying "no verb in the contract backs it". A refusal outlived its
reason by two milestones, and nothing caught it, because a refusal that is correct-looking reads the
same as a refusal that is correct. Milestone 64 found it by asking fifty crates.io crates what they
actually needed (notes/crates-io-on-nife.md), which put `create_dir` and `read_dir` near the top
of real demand.

Four behaviours are worth knowing before you use them:

- **`read_dir(".")` lists the granted directory itself** and costs no `OPENDIR`; the handle is
  `fs::ROOT`. `read_dir("sub")` descends first (`OPENDIR` mints a capability to `sub`, the
  enumeration runs through it, and it is closed afterwards), because there is no other way to name
  what is inside `sub`. There are no `.` and `..` entries: they would be names for things no
  capability designates.
- **The listing is drained whole at `read_dir` time**, not streamed. std hands the caller an
  iterator they may hold across arbitrary work including opening the files they just listed, and
  the listing arrives through the one page every other operation also locks. So a huge directory
  costs a `Vec` of its names, and the listing is a snapshot: an entry removed before the caller
  reaches it opens as `NotFound`. That is the ordinary readdir caveat rather than something this
  choice introduced.
- **Neither remove verb removes the kind the other one is for.** `remove_file` on a directory is
  `IsADirectory` and `remove_dir` on a file is `NotADirectory`, and `remove_dir` takes only an empty
  one (`DirectoryNotEmpty` otherwise). A single "remove whatever you find" is what makes `rm -r`
  dangerous and this contract does not offer it at any opcode.
- **`EROFS` from any of them maps to `ErrorKind::ReadOnlyFilesystem`, and it is a capability
  answer**, not a mode bit: the directory capability this process holds does not carry the right
  that verb needs (§47's ladder). It is the one error in the map that is about *you* rather than
  about the name.
- **`metadata` answers for a directory now, at no extra message.** `OPEN` refuses one with
  `EISDIR`, and that refusal *is* the answer to "what kind of thing is this name", so it is read as
  one rather than propagated. `Path::is_dir()` used to be false for every directory that exists,
  which meant `std::fs::create_dir_all` was not idempotent: it recovers from `AlreadyExists` by
  asking whether the name is already a directory and got told no. `metadata(".")` answers without a
  message at all, because the granted directory is what the endpoint is bound to rather than a name
  inside it. The size reported is 0 and is a placeholder; `modified`/`accessed`/`created` still
  refuse, so nothing here invents a fact the contract does not carry.

**Over-asking for rights is a refusal, not an attenuation**, and it is the trap in this half of the
PAL. `OPENDIR` and `MKDIR` carry the rights the caller wants on the child, and the server answers
`EPERM` when the intersection with the parent's comes up *short of the request* (§47's monotonicity
is the intersection; the refusal is the server telling the truth about it). A PAL cannot know what
its own capability carries, so it asks for the minimum the operation needs: `ENUMERATE` to
enumerate, and nothing at all for `create_dir`, which closes the handle it gets back. The first
version of this binding asked for `dir::ALL` and would have worked through every test in the suite,
because they all grant the root's full rights, and failed through every narrowed one.

Still Unsupported, and now genuinely because **no verb in the contract backs it**:

- `canonicalize`, `hard_link`, symlinks and `read_link`. **`copy` is bound** (milestone 64's second
  pass) and needed no verb: it is an open, a read/write loop and two closes, both names under the
  granted directory. **`remove_dir_all` is bound since milestone 122** and needed no code either;
  see below.
- **Permissions and file times.** The server keeps an mtime (a write advances it) but no verb
  reports one. The second half of that reason is now stale and is recorded as such: there **is** a
  wall clock to interpret a timestamp against since milestone 51, so what stands between
  `File::metadata().modified()` and an answer is a missing contract verb and nothing else.
  `Permissions::readonly` is honestly `false`: authority here is a capability, not a mode bit.
- **File locks** and `File::try_lock`.
- **`File::duplicate`.** A handle is a token the server minted for one session; copying the number
  would forge a second owner of the same handle, including its close.
- **`fsync`/`datasync` succeed rather than refuse**, and that is honest rather than a shrug: nothing
  is buffered on the client side, and the server commits a RedoxFS transaction per write (that is
  what makes a kill mid-write recoverable), so a returned write is already durable.

### Descent: a nested path, and a directory a program can hold (milestone 122)

Until this milestone the PAL called `OPENDIR` in exactly one place, inside `read_dir`, against
`fs::ROOT`, and then let the handle go. Nothing in `std` held a directory, so a name had to be one
component and a nested path was refused as a class.

The consequence was sharp and is worth stating before the fix, because it is the shape of the bug
rather than its size. `read_dir(".")` yields `./name` and feeding that back to `File::open` works;
one level down an entry's `path()` is `sub/name`, which is two components, which was refused. **A
`std` program could list a subdirectory and could not open what it had just been told was in there.**
That pair is what every filesystem walker is built out of, so `walkdir` and `ignore` built and could
not walk.

Two answers, and they are not alternatives. The milestone's block argues both and this is what
landed.

#### The walk: `File::open("a/b/c")` is `OPENDIR a`, `OPENDIR b`, `OPEN c`

Every name-taking verb goes through one function now (`walk` in `sys/fs/nife.rs`), which resolves a
path to **the directory its last name lives in**, plus that name. `open`, `create`, `mkdir`,
`unlink`, `rmdir`, `rename` and `readdir` all use it, so the shape is the same everywhere and there
is one place to be wrong.

**The rights it asks for are the design**, and they look like a widening until you do the
arithmetic. Each hop asks for `DESCEND | needs`, where `needs` is what the *final* verb requires on
the directory it lands in (`fs_proto::verb::TABLE` is the list; `OPEN`'s "READ or WRITE" is the one
row a caller has to resolve from its own `OpenOptions`). Carrying `needs` down the whole chain rather
than asking for it only at the end costs nothing that was ever available, because a child's rights
are its parent's *intersected* with the request: a right an ancestor lacks is a right no descendant
of it could have had either. So the walk asks for exactly the maximum the grant could give at the
depth it is going to, and no more.

**`..` is refused at every position, not only the first**, and that is what makes the walk safe by
construction rather than by checking. There is no verb for an ascent and no capability that would
designate what one reached: a handle names a directory and nothing on the wire names its parent.
`cap-primitives` solves this same problem on Unix and has to work far harder, because there it is
defending against a hostile namespace rather than standing on one that cannot express the attack.

**At most two handles are open at once.** Each hop drops the one before it, which closes it, so a
path's depth costs round trips rather than handle-table slots. The milestone block forecast one per
level and was wrong in the cheap direction.

#### `std::fs::Dir`: the object, and the surprise that it already existed

The other answer is that a program should be able to **hold** a directory and open one name under
it, which is this system's actual model and what `cap-std` binds to. The surprise was that this is
not an interface anyone here had to invent: **`std::fs::Dir` exists upstream** behind
`#![feature(dirfd)]` (rust-lang/rust#120426), with `open`, `open_file`, `open_file_with`,
`metadata`, `remove_file` and `rename`, and it *is* `openat`.

nife was getting std's generic fallback, which stores a `canonicalize`d path. `canonicalize` is
`Unsupported` here, so **`Dir::open` on this system failed at its first call**, and the std type most
aligned with what this OS *is* was the one type it could not offer. It is now a held `OPENDIR`
handle: `Dir::open(".")` is the granted directory and costs no message at all, and anything else is
one attenuated descent per component.

**Why a `Dir` asks for everything its parent carries, when everything else here asks for the
minimum.** The rule in the rest of this PAL is to ask for exactly what the verb needs, because
over-asking is `EPERM` rather than attenuation and a client cannot read its own capability. A held
directory has no single verb to be the minimum of: it is an object, and what will be asked of it
later is not knowable when it is minted. So it asks for the projection of the authority the process
already holds onto one directory inside it, which cannot widen anything (`parent & requested`) and is
exactly what `openat(dirfd, name, O_DIRECTORY)` hands back on Unix.

Knowing what the parent carries is the awkward part, because **the contract has no way to say
"attenuate to whatever you have"** and no verb reporting a handle's rights. The common case is one
message: ask for `dir::ALL`, and a full-rights grant answers with the handle. A narrowed grant
answers `EPERM`, and the PAL then finds out what is there by asking for one right at a time, six
messages, at most once per `Dir::open` because every hop after the first descends from a parent whose
rights are now known. **That probe is a workaround, not a design.** The fix is a sentinel in
`OPENDIR`'s rights word meaning "the parent's, whatever they are", which is a wire-format change and
therefore not a lane's to make.

#### `remove_dir_all` needed no code

It had been refused with a note saying the recursion has to descend, a nested path is refused, and
the loop therefore belongs where it can hold a directory capability per level, which is
`user/src/rm.rs`. The second half of that was right and is now this module's business, because the
walk holds one per level. std's own generic implementation is written entirely in terms of
`read_dir`, `remove_file` and `remove_dir` on paths it composes with `DirEntry::path`, so switching
one re-export was the whole change.

That is the **fourth** refusal in this PAL found to have outlived its own reason, after milestone
64's three. Each looked correct while it was wrong, which is the property that makes this class hard:
a refusal that is correct-looking reads exactly like a refusal that is correct.

#### Two live bugs the walk found

Both were in the tree before this milestone and neither could show against the granted directory,
because nothing is *requested* there: `fs::ROOT` carries whatever the endpoint carries.

- **A created file was unwritable one directory down.** A file handle carries
  `parent & (READ | WRITE)` (`create_file_at`, the same arithmetic as `open_file_at`), so a descent
  that asked only for `CREATE` hands back a directory handle with no `WRITE` in it, which mints a
  file nobody can write. `std::fs::write("a/b")` created the file and then failed `EROFS` on its own
  first write. The fix is that `create_handle` asks for `CREATE` *and* what the caller will do with
  the file.
- **`dir::EPERM` was reaching std programs as `Unsupported`.** The PAL read a `-1` reply as the
  kernel refusing the invoke, on the recorded ground that `EPERM` is not in the FS server's
  vocabulary. **It has been since milestone 47**, where it is what a directory capability answers
  when a verb's right is withheld and when a descent asks for more than the parent holds. So the one
  reply meaning "this capability does not carry that right" arrived as "this platform cannot do
  that", which is §42's silent degradation and would have made a narrowed grant indistinguishable
  from no grant at all. It is now `ErrorKind::PermissionDenied`, which is not the EPERM fiction the
  path refusals avoid: those are the client saying no capability designates that name, where nothing
  consulted a permission; this is the server stating a fact about authority. What makes `-1`
  unambiguous is that every entry point checks reachability first, so a server has already answered
  by the time a request is issued.

#### What is proven, and what is not

`std_exerciser`'s fs transcript walks all of it on both ISAs: a file one directory down, a file two
down, a subdirectory listed and every file in it opened **through the `path()` the listing handed
back**, a path refused for walking through a file, `std::fs::Dir` opening a file under a held
directory and refusing `..` through it, a rename between two directories in one message, and
`remove_dir_all` over a tree the program builds.

**The rights discipline is exercised only under a full-rights grant**, and that is the honest gap.
Every std test grants the mount root, so a walk that over-asked would pass all of them, which is
precisely the trap this PAL records nearly falling into once already. A std program spawned on an
`fs_subtree_caretaker` endpoint is the test that would close it.

### The write path: a correction to the record

notes/fs-server.md and §27 recorded an open item, that an end-to-end write "loops inside RedoxFS's
allocator commit on bare metal even on a pristine image". **It does not.** Driven through `std::fs`,
a write to the file the image ships completes on both ISAs, reads back through the server, and, the
part a cache cannot fake, reads back byte for byte when the host tool reopens the image afterwards
with the pinned engine. That check is in the gate (`redoxfs_check_after_run` compares `scratch`
against the fixture, and `mkredoxfs` rewrites it to a placeholder before every run, so the check
passing means this run's guest write landed).

The likely reason is the fix/irq-delivery change of 2026-07-29, which put the block server back on
the completion interrupt instead of polling the used ring, the same correction that note had already
made for the read path. Stated as likely rather than proven: what was measured is that the write
completes, not why the poll path did not.

## Honest caveats (what is Unsupported, and why)

- **`thread::spawn` returns `Unsupported`.** The kernel has everything it needs (retype a TCB,
  configure it, start it); what does not exist yet is the std-side plumbing that makes the result
  safe: a TLS story, park/unpark on a kernel primitive, join. Phase one ships without it rather than
  shipping it wrong. The sync primitives are std's single-threaded `no_threads` implementations, and
  the allocator's spinlock is uncontended today but stays correct under future preemption.
- **The environment is empty, and `set_var` is real** (milestone 64, `sys/env/nife.rs`). A nife
  process inherits no variables, because there is nothing to inherit from: what a process holds is
  what it was granted, and a variable is not a capability. So `env::var("HOME")` is `None` because
  nobody gave this program a home, and `env::vars()` yields nothing. What a program sets on itself
  reads back, because that is what `set_var` means on every platform and nothing about it leaves the
  process. When milestone 47's namespace gives a program an endowment, it seeds this table; the shape
  does not change.

  **This module exists because `env::vars()` used to abort the process.** Without a nife backend, std
  fell through to `sys::env::unsupported`, whose `env()` is `panic!("not supported on this
  platform")`. `getenv` was already answering `None` honestly, so nothing in the type system, the
  build, or a gap list built from `Unsupported` counts said a word; the program simply died. It is
  the same lesson as `Path::is_dir()` returning `false` for every directory: the dangerous refusal is
  not the one that says `Unsupported`, it is the one that answers.
- **The path half of `std::env` answers where it must and refuses where it can** (milestone 64,
  `sys/paths/nife.rs`), and the split between those two is the whole design. `temp_dir` returns
  `TMPDIR` if the program set one and `.` otherwise, because `PathBuf` carries no error and
  something has to be named; `.` is `sys/fs/nife.rs`'s own answer restated (*"./motd is motd: the
  current directory IS the granted one"*), so a temporary file goes where every other file goes,
  which is the only directory the process has authority over. `split_paths` and `join_paths` are
  ordinary string work over `:`. Meanwhile `current_dir`, `current_exe` and `chdir` return
  `Unsupported` and `home_dir` is `None`: each of them **can** say no, and each needs a namespace to
  resolve against, which is milestone 47's unbuilt half.

  **Two of these used to abort the process**, which is why the file exists at all. nife had no
  `paths` backend, and the shared fallback's `temp_dir()` is `panic!("no filesystem on this
  platform")` while its `split_paths()` is `panic!("unsupported")`. So `tempfile::NamedTempFile::new()`
  died inside `std::env::temp_dir` before it ever reached `tempfile`'s own "not supported" arm, and
  notes/crates-io-on-nife.md had recorded the wrong one of those two as the failure for a fortnight.
  It is `env::vars()`'s lesson a second time, and the rule that falls out of it is short enough to
  remember: **fix the ones that abort, leave the ones that refuse.**
- **`std::process::id()` is `0` and the rest of `std::process` refuses** (milestone 64,
  `sys/process/nife.rs`). This was the third `panic!` of the same sweep: `getpid` in the shared
  fallback is `panic!("no pids on this platform")`. There is no process identifier here to report,
  the syscall surface issues none, and `u32` cannot say "there is none", so `0` is the answer,
  chosen because no Unix assigns it to a user process. Every reachable call site in the fifty
  measured dependency closures is a *fork* check (`gix-tempfile` compares an owning pid so cleanup
  runs only in the creating process); nife has no `fork`, so a constant is what makes those
  comparisons right rather than merely quiet. A scheme that wanted cross-process uniqueness from a
  pid would be broken here with nothing reporting it, and a real per-process identity is a
  syscall-surface decision rather than a PAL one.
- **`std::process::exit` is a real exit, and the code it carries is dropped on the floor**
  (milestone 64). It was the fourth `panic!`-shaped finding of the same sweep and the worst of them:
  `sys/exit.rs`'s `_ =>` arm is `crate::intrinsics::abort()`, so a nife program calling
  `std::process::exit(0)` compiled perfectly and then executed `brk`. The kernel takes that as a
  fault, reports it on the console, and delivers `EVENT_FAULT` with a pc and a faulting address to
  the process's supervisor (§26). **A clean exit arrived as a crash**, and this is the way almost
  every CLI-shaped program ends.

  Nothing had noticed because the two ways a Rust program ends took different exits and only one was
  wired: `_start` calls the PAL's `rt::exit` on `main`'s return value directly, and
  `std::process::exit` is the *only* caller of `sys::exit::exit` in the whole of std. Both now reach
  the same `SYS_EXIT`.

  **The code is still discarded, and that is the kernel's, not the PAL's.** `sched::exit()` is
  `depart(abi::fault::EVENT_EXIT, 0, 0)`: the §26 message carries the event and the tid, and the
  two remaining words are a pc and a fault address that a clean exit has nothing to put in. So a
  supervisor can tell exit from crash and cannot tell `exit(0)` from `exit(1)`. Widening that is a
  wire-format change to a message two programs agree on, which is the expensive category rather
  than the cheap one; it is not something a PAL arm can decide. Until it happens, a nife program
  that wants to report *why* it stopped says so on an endpoint it holds, the way every other
  result travels here.
- **`fs` is bound, with the gaps listed above.** A program granted no directory capability still gets
  `Unsupported` from all of it, and the offline demo checks exactly that: same binary, no slot 4, and
  `File::open` refuses with `ErrorKind::Unsupported` rather than pretending there is an empty
  filesystem to look in.
- **`net` is bound, but with recorded gaps.** `TcpStream` and outbound `UdpSocket` work; the honest
  Unsupported list is `TcpListener` (the contract grew LISTEN and ACCEPT in milestone 107, so this
  is now a gap in the PAL rather than in the contract; see notes/net.md for what binding it takes),
  non-blocking mode and
  read/write timeouts (the contract is blocking-only, no poll verb), DNS via `lookup_host` (no
  resolver rides the contract, so `ToSocketAddrs` handles numeric addresses only, and a program that
  wants DNS does it as a plain UDP query, as the demo does), IPv6 (net_stack is IPv4-only), and `peek` /
  socket duplication / multicast join-leave (no contract verb backs them). `UdpSocket::recv_from`
  reports the connected peer or the last send destination as the datagram source, because the
  contract's `RECV` does not carry it; that is correct for the request/response pattern the demo
  uses and recorded here for anything that assumes otherwise. Advisory knobs (`set_nodelay`,
  `set_ttl`, keepalive, broadcast, multicast options) accept and return plausible values rather than
  fail; they change nothing on the wire.
- **`SystemTime` is real wall-clock time when the program was granted a clock** (milestone 51,
  DECISIONS §43, notes/clock.md). It used to be the monotonic counter offset from `UNIX_EPOCH`, so
  the machine reported **1970 plus uptime** and nothing in the interface said so; that is gone.
  A std program's wall-clock authority is **slot 5** (a `Frame` capability naming the clock page,
  with `READ`) plus a read-only mapping of that page at `rt::CLOCK_PAGE`, and `SystemTime::now()` is
  the offset it finds there plus the ambient monotonic counter: two loads and an add, no server
  round trip, and nothing the program can write. `Instant` is untouched and cannot be perturbed by a
  clock adjustment, by construction.

  **A program granted no clock, or running on a machine that does not know the time, gets a panic
  from `SystemTime::now()`,** naming which of the two it was. This is the honest limit rather than a
  clean win: `SystemTime::now()` has no error channel, so the only loud refusal available is a
  panic, and std has no way to represent "I do not know", which means a program cannot ask whether
  it *can* ask. The `Unsupported` shape `fs` and `net` use is not available here. Anything that
  needs to check first reads `clock_proto::state` off the page directly, which is what a `no_std`
  component does. The alternative considered and rejected was returning a frozen `UNIX_EPOCH`, which
  is still reporting 1970 and is exactly the confusion §42 forbids.
- **`std::random` is a granted capability, and refuses loudly without it** (milestone 56, §44). It
  used to be splitmix64 seeded from the virtual counter, predictable to anyone who could guess
  boot-relative time; that file has been replaced rather than patched. `SystemRng` is now a `CALL` on
  **slot 6**, answered by a userspace entropy service that is the only thing that can read the
  virtio-rng device. A program granted no entropy capability gets a **panic**, for exactly the reason
  `SystemTime::now()` does: `fill_bytes` has no error channel, and quietly substituting a predictable
  stream is the lie the milestone exists to remove.

  **`HashMap`'s seed is the one caller that still degrades**, to the same splitmix64 stream, and that
  is deliberate: its promise is DoS resistance for a hash table rather than cryptographic strength,
  std's own `unsupported` backend degrades that same function, and a `HashMap` in a program nobody
  granted entropy must still work. Nothing in the file lets the weak path reach `SystemRng`. See
  notes/entropy.md for what the bytes are and, under QEMU, are not.
- **stdout and stderr share one endpoint**, so they interleave by 16-byte chunk. One endpoint is what
  the contract grants today; milestone 28's terminal contract owns fixing it.
- **The `std-src` patches are string-anchored to the pinned nightly's std internals.** A rustc bump
  that reshapes a `cfg_select!` dispatcher fails loudly in `std_patch_dispatch` ("anchor not found"),
  which is the intended tripwire: re-point the anchor, do not paper over it. `rust-toolchain.toml`
  pins the channel; the coupling is the price of build-std against a std we do not fork.

## What still ends a nife process

*(Milestone 64, fourth pass, 2026-08-18.)*

**The dangerous std call is not the one that returns `Unsupported`. It is the one that compiles and
then kills you.** Four have been found so far, each by a different accident:

| call | what it was | found by |
|---|---|---|
| `std::env::vars()` | `panic!("not supported on this platform")` | working the ranked gap list and noticing a *neighbour* |
| `std::env::temp_dir()` | `panic!("no filesystem on this platform")` | reading every module the PAL falls through |
| `std::env::split_paths()` | `panic!("unsupported")` | the same reading |
| `std::process::id()` | `panic!("no pids on this platform")` | the same reading |
| `std::process::exit()` | `crate::intrinsics::abort()` | `cargo xtask std-aborts` |

**None of them could appear on a gap list**, because notes/crates-io-on-nife.md's list is built from
PAL functions that answer `Unsupported`, and a function that ends the process never answers. And the
fifth could not be found by the method that found the middle three either: `sys/exit.rs` is not a
`sys/<module>/mod.rs` backend, it is one file with a `cfg_select!` inside a function, so "read every
module the PAL falls through" walks straight past it however carefully somebody does it.

So the reading became a check.

```sh
cargo xtask std-aborts       # on its own, against whatever the farm last built
script/test                  # runs it too: `std-exerciser` ends with it
```

### What it does

It asks the compiler which `library/std/src/sys/**` sources it **actually compiled** for the nife
targets, by unioning every `library/std/src/sys/` path out of cargo's own dep-info under
`std_exerciser/target/`, and greps exactly those for bodies that end a process: `panic!`,
`unimplemented!`, `todo!`, `rtabort!`, `intrinsics::abort()`, `panic_nounwind`. Comment lines are
skipped, which is not fussiness: this tree's PAL files discuss the panics they replaced at length,
and a check that could not tell a fix from its own explanation would have been useless on the day it
was written.

What it finds is compared against `ABORTS_ACCEPTED` in `xtask/src/main.rs`, **which carries the
reason for every entry**. A new one fails the build with the file, the line, and the two things it
can be. Today there are 26 across 79 compiled sources, in three groups:

- **unreachable on nife** (nine): a body behind a `cfg` this target does not satisfy. `cfg_select!`
  keeps every arm's source in the file, so they are read but not compiled into anything reachable.
- **no answer exists** (nine): single-threaded, so the call can only deadlock or end, and upstream
  chose to end. `Condvar::wait` and `Once::wait` are the two that matter, and they stay open until
  milestone 64's `thread::spawn` fork is decided rather than being fixable by a PAL arm.
- **ours, and deliberate** (three files' worth): the clock and entropy refusals, where ending the
  process is the honest report because the call has no error channel and inventing a value would be
  the lie §42 forbids.

### EXAMPLES

Adding a `panic!` to a compiled fallback and running the check:

```
$ cargo xtask std-aborts
std-aborts: a std source compiled for nife ends the process somewhere new:
  sys/net/hostname/unsupported.rs:7: pub fn invented() -> ! { panic!("no hostname on this platform") }
```

and a clean tree:

```
$ cargo xtask std-aborts
std-aborts: 26 process-ending bodies across 79 compiled std sources, all accounted for
```

The fix the check prompted, read off the binary rather than off the source, which is the form of
evidence this milestone is short of:

```
$ llvm-objdump -d --demangle std_exerciser/target/aarch64-unknown-nife/release/std_exerciser
000000000040c290 <std::process::exit>:
  40c290: stp  x30, x19, [sp, #-0x10]!
  40c294: mov  w19, w0
  40c298: bl   0x40eaf0 <std::rt::cleanup>      ; flush stdout, announce end of stream
  40c29c: sxtw x0, w19                          ; the exit code
  40c2a0: mov  x8, xzr                          ; SYS_EXIT
  40c2a4: svc  #0

000000000040c2b0 <std::process::abort>:
  40c2b0: brk  #0                               ; still a fault, which is what abort MEANS
```

`exit` used to be the second of those two. That is the whole bug in six instructions: the same
`brk`, under the name of the call that is supposed to be the clean one.

### BUGS

- **It covers `sys/` and nothing else, on purpose, and that is a real gap.** `sys` *is* std's
  platform layer: a panic under it says "this platform has nothing to offer", while a panic in
  `path.rs` or `thread/scoped.rs` says "you called this wrong" and says it identically on Linux. The
  first version swept all of std, found about forty of the second kind and none of the first, and
  would have been abandoned within a week. The cost of the narrowing is that **portable std code
  which is only reachable on a platform this thin is invisible here** (a `LazyLock` poisoned by an
  earlier panic, say), and finding those still needs somebody reading.
- **An accepted entry matches a substring of a line, not a line number.** That is what stops a
  nightly's blank line from rewriting the list, and it means one entry can bless two sites when the
  same text appears twice in a file. `sys/exit.rs`'s `crate::intrinsics::abort()` is exactly that
  case: the UEFI arm's last resort and the `_ =>` arm nife used to take are the same string, so
  after this milestone one accepted entry covers a line nobody reaches and a line nobody takes.
  **Three entries are deliberately blanket**, matching bare `panic!(` in `sys/random/nife.rs`,
  `sys/time/nife.rs` and `sys/pal/nife/clockproto.rs`: those are the PAL's own files, where every
  panic is one this project wrote on purpose and a new one arrives through review rather than
  through a nightly. A blanket entry over a file we do not own would be the wrong trade.
- **It proves reachability of a *body*, never of a *call*.** A body compiled into the reachable set
  might still be dead. The check deliberately does not try to decide that, because deciding it is
  reading the call sites, which is the work it exists to prompt rather than to replace.
- **It needs a build.** The dep-info only exists after `cargo xtask std-exerciser`, which is why the
  check runs at the end of that step rather than in `script/lint`. Run against a stale farm it
  reports the stale farm, honestly and uselessly.
- **`std-aborts` is a provisional name** (milestone 64, 2026-08-18). Names are calef's; this one is
  not ratified.

## The proof

`std_exerciser/src/main.rs` is an ordinary Rust program, no `no_std`, no `unsafe`, and two
`#![feature]` gates that are both about **an API's stability upstream rather than about this
platform**: `std::random` (rust-lang/rust#130703) and, since milestone 122, `std::fs::Dir`
(rust-lang/rust#120426). A program on any target calling those opts in the same way. It is
**one binary with three behaviours, chosen by the authority it was granted**: on start it probes for
a directory capability (`File::open` on the fixture name) and then for the network (a single
`UdpSocket::bind`), and the results branch it.

- **Granted a directory** (slot 4 and the shared page, alongside a running FS service): the open
  succeeds, and the program reads the file with `Read` and again with `read_to_string`, stats it,
  overwrites the image's `scratch` file and reads it back, and gets refused on `/etc/passwd`,
  `../motd`, and `sub/motd`. Since milestone 31 phase 2 it also **creates** a name the image does not
  carry with `std::fs::write`, writes it a second time with a *shorter* payload, and asserts the
  read-back equals the shorter one: without `TRUNCATE` that second write would leave the first one's
  tail behind, which is the whole of §27's four-times-corrected bug pinned at the top level rather
  than only in a host test. `create_new` over the name it just made is `AlreadyExists`, and creating
  `/tmp/escape` or `../escape` is refused exactly as opening them is, so `CREATE` did not widen what a
  client can reach. Since milestone 64 it then walks the **namespace** verbs: makes a directory,
  lists the granted directory and finds both the fixture (a file) and the directory it just made
  (marked as one), descends into that directory and finds it empty, gets refused unlinking a
  directory and rmdir-ing a file, renames the file it created and asserts the *source name is gone*
  as well as the destination's contents, then removes both. Since milestone 122 it then **descends**:
  it reads a file one directory down and another two down, builds a small tree of its own and lists
  it, opens every file that listing named through the `path()` the listing handed back (the pair that
  used to break), gets refused a path that walks through a file, opens a file under a held
  `std::fs::Dir` and gets refused `..` through it, renames between two directories in one message,
  and removes the tree with `remove_dir_all`. **The tree it lists is one it built**, deliberately,
  and not the fixture's `sub`: milestone 47's directory-capability attacker is granted exactly that
  directory and writes into it, so `sub`'s contents depend on which tests ran first in this boot, and
  the first version of this transcript asserted them and failed. `sub/inner` and `sub/deeper/leaf`
  are safe to *read* because the post-run host check pins them. It cleans up before it starts rather
  than after, because `NIFE_KEEP_REDOXFS=1` runs the suite against an image a previous boot
  wrote. The kernel test `std_fs_reads_a_file_through_a_granted_directory_capability` spawns it this
  way.
- **Granted the network** (slots 2 and 3, alongside a running net_stack): the bind succeeds, and the
  program does a real UDP DNS query to slirp's resolver and a TCP echo round trip to slirp's
  guestfwd peer, both through `std::net` and both asserted. The kernel test
  `std_net_runs_over_the_socket_contract` spawns it this way.
- **Granted neither** (only slots 0 and 1): both probes return `Unsupported`, and the program runs
  the offline transcript, exercising `Vec` (10,000-element collect against the untyped heap),
  `String`, `HashMap` (the random seed), `Instant` (asserted monotonic and advancing), and the
  honesty of `fs` and `net`. The kernel test `std_tests::a_whole_std_program_runs_on_the_native_abi`
  spawns it this way.

The same binary doing three things by its grants alone is the point of "no ambient authority": the
code never chose to have a network or a filesystem, its cspace did. All three tests reassemble the
byte stream off the endpoint and compare it byte for byte, on **both** ISAs out of each arch's own
initrd (the parity gate, DECISIONS §19). The fs transcript splices the file's own bytes into the
expected buffer from the shared fixture, so that one comparison covers the whole path: disk,
DMA-confined block server, FS server running an engine we did not write, the file contract, the PAL,
and the stdout endpoint. One binary also keeps the initrd inside its nifefs directory limit
(`nifefs::MAX_FILES`, 31 entries when this was written and 76 since 2026-08-01).
`cargo xtask test` builds the demo for both targets first, so both initrds carry it; both test legs
attach a virtio-net NIC (`NIFE_NET`) with the guestfwd echo peer and the RedoxFS image as the
second disk.

**One boot has one FS service**, because the block server owns the RedoxFS device: a second wiring
would put a second driver on the same virtio slot and re-bind its interrupt. So `fs_service`
remembers what it wired, and the hand-written client's test and the `std::fs` test share one
instance; whichever runs first receives the two readiness sentinels (each is sent once) and the other
sees `None` and skips those assertions. That keeps the two tests order-independent, which matters
because nothing guarantees which of them the harness runs first.
