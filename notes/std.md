# Rust `std` on the native ABI

*(Milestone 27. The first wall an application hits on cricker-os was "no std": you could write a
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

A std program is an ordinary cricker-os ELF (notes/abi.md §3): entered at `_start`, linked at
`0x40_0000`, cspace populated by its parent. std's runtime contract needs two things, and the ABI's
out-of-band convention (notes/abi.md §4) grants them at fixed slots:

- **slot 0: an untyped budget.** The global allocator draws heap pages from it lazily via
  `untyped::MAP`, one page per invoke, at `0x4000_0000`. This is the same untyped-backed heap the
  `allocdemo` workload proved (`crates/uheap` algorithm, host-tested), restated inside std because
  std cannot depend on an out-of-tree crate.
- **slot 1: an endpoint with WRITE.** `stdout` and `stderr` SEND here, 16 bytes per message (w0 =
  byte count, w1|w2 = the bytes, little-endian). std's own `LineWriter` batches user writes; the
  receiver reassembles.

Three more slots exist, and a program holds each only if it was *given* the thing behind it
(milestone 27 phase two, the `std::net` and `std::fs` bindings below):

- **slot 2: a `Stack` endpoint with WRITE.** `std::net` speaks netd's socket contract over it.
- **slot 3: an untyped budget** the net PAL mints each socket's shared frame from.
- **slot 4: an FS-service endpoint with WRITE**, which *is* a directory capability, plus the page it
  shares with the FS server mapped at `0x1100_0000`. `std::fs` speaks the §27 file contract over it.

A program that never allocates, prints, opens a socket, or opens a file never touches the slots it
does not use. The absence of slots 2 and 3 is exactly what "no ambient network" feels like from
inside a process, and the absence of slot 4 is "no ambient filesystem": each returns `Unsupported`
because there is no capability to reach, not because the code was compiled out. A program can hold
one and not the other, so the slots do not fill contiguously; notes/abi.md §4 records how the
kernel-side wiring places slot 4 while leaving 2 and 3 empty, and why the gap matters.

## The PAL surface, and what each piece binds to

The backend lives in `patches/std-cricker/overlay/std/src/sys/` and is materialized into a patched
std by `cargo xtask std-src`. Each file binds one std concept to the ABI:

| std concept | cricker binding |
|---|---|
| `GlobalAlloc` | `untyped::MAP` from slot 0, grow-on-demand (`sys/alloc/cricker`) |
| `stdout` / `stderr` | `endpoint::SEND` on slot 1 (`sys/stdio/cricker.rs`) |
| `Instant`, `SystemTime` | the virtual counter, `CNTVCT_EL0` / `rdtime` (`sys/time/cricker.rs`) |
| `panic!` | print, then `brk`/`ebreak`: a fault the kernel attributes. No unwinding. |
| `thread::spawn` | `Unsupported` in phase one; `sleep`/`yield` are real |
| `net` (`TcpStream`, outbound `UdpSocket`) | netd's socket contract on slots 2/3 (`sys/net/connection/cricker.rs`), or `Unsupported` when not granted |
| `fs` (`File`, `metadata`, `read`/`write`) | the FS service's file contract on slot 4 (`sys/fs/cricker.rs`), or `Unsupported` when no directory was granted |
| `HashMap` seed | splitmix64 from the counter (`sys/random/cricker.rs`), **not** cryptographic |
| `std::env::consts::OS` | `"cricker"` (patched into `env_consts.rs`) |

The syscall glue (`sys/pal/cricker/rt.rs`) is a deliberate twin of `crates/user_rt`: the same
`svc`/`ecall` wrappers, restated because std cannot depend on the crate. The ABI **constants** are
not restated: `abi.rs` is generated verbatim from `crates/abi` by `std-src`, so the numbers cannot
drift. Likewise `uheap.rs` from `crates/uheap` (the host-tested heap algorithm is the only heap
algorithm), `netproto.rs` from `user/src/netproto.rs`, and `fsproto.rs` from `crates/fs_proto`: every
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
3. **Patch that copy**: drop in the overlay PAL files, generate `abi.rs`/`uheap.rs`, and insert a
   `target_os = "cricker"` arm into std's `cfg_select!` dispatchers (pal, alloc, stdio, random,
   thread, time, io/error, thread_local storage and guard) plus `env_consts` and the
   `restricted_std` chain in std's `build.rs`.
4. **Link it** as the `cricker-dev` toolchain (`rustup toolchain link`).

`cargo xtask user-std` then builds the `hellostd` demo for both custom targets against it. The build
sets `RUSTUP_TOOLCHAIN=cricker-dev` explicitly rather than `+cricker-dev`, because the cargo proxy
that launched xtask already exports `RUSTUP_TOOLCHAIN=nightly`, which would override a `+` selector
and silently build std from the *unpatched* sysroot.

`std-src` is idempotent: a stamp of all inputs (the toolchain version, the ABI/heap crates, the
target specs, every overlay file, and a patch-logic version) guards the rebuild, so a warm farm and
its build-std cache survive across runs and only a PAL change forces std to recompile.

### The target specs

`targets/{aarch64,riscv64}-unknown-cricker.json`, built with `-Zbuild-std` and `-Zjson-target-spec`.
The load-bearing fields:

- `"os": "cricker"` selects our `sys` backend through every dispatcher.
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

`sys/net/connection/cricker.rs` binds std's `TcpStream` and outbound `UdpSocket` to netd's socket
contract (DECISIONS §25, notes/net.md, `user/src/netproto.rs`). The PAL is a **client** of the
frozen contract, nothing more: it holds the `Stack` endpoint (slot 2) and a frame untyped (slot 3),
and for each socket it mints a shared `Frame`, maps it, delegates it to netd (`SEND_CAP`,
`OP_ATTACH_FRAME`), and then drives the socket with `CALL`s carrying a socket id. Control words ride
the message; bytes sit in the shared frame. This is the exact path the hand-written `netcli` client
walks, reached through std's blocking API instead.

The wire constants are not restated: `netproto.rs` is generated verbatim from `user/src/netproto.rs`
into `sys/pal/cricker/netproto.rs` by `std-src`, the same anti-drift discipline as `abi.rs` and
`uheap.rs`. If the contract changes, the PAL's numbers change with it, because there is one source.

What binds, and how it maps to the contract:

- **`TcpStream::{connect, read, write, ...}`** -> `OP_OPEN_TCP`, `OP_CONNECT`, `OP_RECV`, `OP_SEND`,
  `OP_CLOSE` (on `Drop`). `read` blocks in netd until data arrives (a blocked `RECV`), the blocking
  semantics std's default API wants. A short `read` keeps the segment's tail in a per-socket residual
  buffer, so a stream never drops bytes.
- **`UdpSocket::{bind, connect, send, recv, send_to, recv_from}`** -> `OP_OPEN_UDP`, `OP_SENDTO`,
  `OP_RECV`. UDP `connect` only fixes a default peer (no contract call, matching Unix). `bind`'s
  local address is validated but not honored: netd assigns an ephemeral local port.
- **Errors map by meaning, no errno.** A refused TCP connect is `ConnectionRefused`; a netd timeout
  on `RECV` is `TimedOut`; a datagram larger than the frame is `InvalidInput`; an IPv6 address is
  `Unsupported` (netd is IPv4-only). A `CALL` on an empty `Stack` slot (no network granted) reads
  back negative and becomes `Unsupported`, the same answer a program with no net grants gets.

The concurrency model is the contract's: single-threaded, one synchronous exchange at a time. A
program can hold up to `MAX_SOCKETS` (4) sockets at once and interleave them, but there is only ever
one operation in flight, which is all a single-threaded process can do anyway.

**A finding, recorded honestly.** netd derives a socket's local port from its socket id
(`LOCAL_PORT_BASE + sid`), so an id is not an ephemeral port that rotates; reopening a just-closed id
reuses its exact local port. Against QEMU's slirp, a TCP connect that reuses a port whose previous
flow has not cleared stalls (the SYN's answer never comes, and netd blocks in its bounded poll on
the NIC interrupt). The PAL softens this by handing out ids round-robin, so consecutive opens prefer
different ids and ports, but a program that churns through more than `MAX_SOCKETS` sockets quickly
can still hit a reused port. The real fix is netd assigning ephemeral local ports independent of the
socket id, which is a **contract-side change reported up, not a client workaround**. The demo
sidesteps it by keeping its UDP and TCP sockets on distinct ids at once.

## `std::fs` over the FS-service contract (milestone 27 phase two)

`sys/fs/cricker.rs` binds std's `File` to the FS server's file contract (DECISIONS §27,
notes/fs-server.md, `crates/fs_proto`). Like the net PAL it is a **client** of a frozen contract and
nothing more, and like the net PAL its wire constants are generated verbatim (`fs_proto` becomes
`sys/pal/cricker/fsproto.rs` by `std-src`), so the PAL's numbers cannot drift from the server's.

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
- **A nested path is refused**, and the message points at milestone 31: a subdirectory needs its own
  directory capability, which the contract does not yet grant.
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

**A wart of the contract, recorded.** The wire's error space (a negated errno) overlaps the kernel's
invoke-error space (-1..-8), so `-2` is both `ENOENT` and `WrongObject`, and `-5` is both `EIO` and
`BadMethod`. It is harmless in practice: only `-1` and `-3` are read as "you hold no such
capability", and neither `EPERM` nor `ESRCH` is in the FS server's vocabulary, while `-2` is left to
the errno mapping so a missing file reads as `NotFound`. The clean fix is a tag or an offset in the
reply word, which is a contract change (`fs_proto`, the FS server, and `fsclient`), reported up
rather than papered over here.

### What binds, and what stays Unsupported

Bound: `File::open` (`OPEN`), `read`/`read_to_end`/`read_to_string` (`READ`), `write`/`write_all`
(`WRITE`), `seek`/`stream_position`, `metadata`/`len` and `File::size` (`FSTAT`), close on `Drop`
(`CLOSE`), and `std::fs::{metadata, read, exists}` built from open + fstat + close. The file position
lives on the client side because the contract's read and write are both explicitly positional, so
there is no cursor in the server to get out of step with, and a seek costs no message at all except
`SeekFrom::End`.

Unsupported, each because **no verb in the contract backs it**, not because the code is missing:

- **Creating a file** (`File::create`, `OpenOptions::create_new`, and `create(true)` on a name that
  is not there) and **truncating one** (`OpenOptions::truncate`, `File::set_len`). This is the one
  gap with real user-visible reach: `std::fs::write(path, data)` opens with create + truncate, so it
  is `Unsupported` by construction, and writing goes through
  `OpenOptions::new().write(true).open(name)` on a file the image already carries. Adding `CREATE`
  and `TRUNCATE` verbs is possible (RedoxFS's `create_node` is not std-gated; §27's "never create
  on-device" is about creating a *filesystem*, which needs uuid and getrandom, not a file), but it
  widens the contract, so it is a decision to take deliberately rather than a hole to plug.
- **Directory iteration** (`read_dir`), `mkdir`, `unlink`, `rename`, `rmdir`, `remove_dir_all`,
  `canonicalize`, `hard_link`, symlinks and `read_link`, `copy`.
- **Permissions and file times.** The server keeps an mtime (a write advances it) but no verb reports
  one, and there is no wall clock to interpret it against anyway. `Permissions::readonly` is honestly
  `false`: authority here is a capability, not a mode bit.
- **File locks** and `File::try_lock`.
- **`File::duplicate`.** A handle is a token the server minted for one session; copying the number
  would forge a second owner of the same handle, including its close.
- **`fsync`/`datasync` succeed rather than refuse**, and that is honest rather than a shrug: nothing
  is buffered on the client side, and the server commits a RedoxFS transaction per write (that is
  what makes a kill mid-write recoverable), so a returned write is already durable.

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
- **`fs` is bound, with the gaps listed above.** A program granted no directory capability still gets
  `Unsupported` from all of it, and the offline demo checks exactly that: same binary, no slot 4, and
  `File::open` refuses with `ErrorKind::Unsupported` rather than pretending there is an empty
  filesystem to look in.
- **`net` is bound, but with recorded gaps.** `TcpStream` and outbound `UdpSocket` work; the honest
  Unsupported list is `TcpListener` (no LISTEN/accept verb in the contract), non-blocking mode and
  read/write timeouts (the contract is blocking-only, no poll verb), DNS via `lookup_host` (no
  resolver rides the contract, so `ToSocketAddrs` handles numeric addresses only, and a program that
  wants DNS does it as a plain UDP query, as the demo does), IPv6 (netd is IPv4-only), and `peek` /
  socket duplication / multicast join-leave (no contract verb backs them). `UdpSocket::recv_from`
  reports the connected peer or the last send destination as the datagram source, because the
  contract's `RECV` does not carry it; that is correct for the request/response pattern the demo
  uses and recorded here for anything that assumes otherwise. Advisory knobs (`set_nodelay`,
  `set_ttl`, keepalive, broadcast, multicast options) accept and return plausible values rather than
  fail; they change nothing on the wire.
- **`SystemTime` is monotonic-since-boot, not wall-clock.** No RTC, no NTP, so "system time" honestly
  measures "since this machine came up". Differencing two `SystemTime`s gives a correct duration;
  reading a calendar date gives 1970 plus uptime, which is the truth available.
- **`std::random` is not cryptographic.** splitmix64 seeded from the virtual counter: fine for
  `HashMap`'s seeds and `sort_unstable`'s pivots, predictable to anyone who can guess boot-relative
  time. Never for keys or tokens. A real entropy story (a virtio-rng service) would replace the file.
- **stdout and stderr share one endpoint**, so they interleave by 16-byte chunk. One endpoint is what
  the contract grants today; milestone 28's terminal contract owns fixing it.
- **The `std-src` patches are string-anchored to the pinned nightly's std internals.** A rustc bump
  that reshapes a `cfg_select!` dispatcher fails loudly in `std_patch_dispatch` ("anchor not found"),
  which is the intended tripwire: re-point the anchor, do not paper over it. `rust-toolchain.toml`
  pins the channel; the coupling is the price of build-std against a std we do not fork.

## The proof

`user-std/src/main.rs` is an ordinary Rust program, no `no_std`, no attributes, no `unsafe`. It is
**one binary with three behaviours, chosen by the authority it was granted**: on start it probes for
a directory capability (`File::open` on the fixture name) and then for the network (a single
`UdpSocket::bind`), and the results branch it.

- **Granted a directory** (slot 4 and the shared page, alongside a running FS service): the open
  succeeds, and the program reads the file with `Read` and again with `read_to_string`, stats it,
  overwrites the image's `scratch` file and reads it back, and gets refused on `/etc/passwd`,
  `../motd`, and `sub/motd`. The kernel test
  `std_fs_reads_a_file_through_a_granted_directory_capability` spawns it this way.
- **Granted the network** (slots 2 and 3, alongside a running netd): the bind succeeds, and the
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
and the stdout endpoint. One binary also keeps the initrd under its 15-file crickerfs directory limit.
`cargo xtask test` builds the demo for both targets first, so both initrds carry it; both test legs
attach a virtio-net NIC (`CRICKER_NET`) with the guestfwd echo peer and the RedoxFS image as the
second disk.

**One boot has one FS service**, because the block server owns the RedoxFS device: a second wiring
would put a second driver on the same virtio slot and re-bind its interrupt. So `fs_service`
remembers what it wired, and the hand-written client's test and the `std::fs` test share one
instance; whichever runs first receives the two readiness sentinels (each is sent once) and the other
sees `None` and skips those assertions. That keeps the two tests order-independent, which matters
because nothing guarantees which of them the harness runs first.
