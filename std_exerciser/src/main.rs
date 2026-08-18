//! The std proof (milestone 27): an ordinary Rust program, no `no_std`, running on the native
//! capability ABI. Every line exercises a PAL surface: `println!` SENDs on the stdout endpoint
//! (slot 1), collections draw from the untyped budget (slot 0), `Instant` reads the virtual
//! counter, `SystemTime` reads the clock page slot 5 grants (milestone 51), `std::random` asks the
//! entropy service slot 6 grants (milestone 56), and `fs` returns honestly `Unsupported`.
//!
//! The two `#![feature]`s below are about **the APIs' stability upstream, not about this platform**:
//! `std::random` (rust-lang/rust#130703) and `std::fs::Dir` (rust-lang/rust#120426) are both still
//! unstable in Rust, so any program on any target that calls them opts in the same way. Everything
//! else here is stable Rust.
#![feature(dirfd, random)]
//!
//! **One binary, three behaviours, chosen by the authority it was granted.** A std program reaches
//! the network only if it holds the network, and the filesystem only if it holds a directory (no
//! ambient authority, DECISIONS §10, §25, §27). So it probes, and its grants decide:
//!   - **granted a directory** (the loader placed an FS-service endpoint in slot 4 and mapped the
//!     page it shares with the FS server): it opens the file the RedoxFS image ships, reads it
//!     through `std::fs`, and proves that a path trying to leave the granted directory is refused.
//!   - **granted the network** (a `Stack` endpoint and a frame untyped in slots 2 and 3): it runs a
//!     real UDP DNS query and a TCP echo round trip through `std::net` (milestone 27 phase two),
//!     the same net_stack socket contract the hand-written client uses. And if its stack also
//!     carries a **listen grant**, it serves: `TcpListener` binds the granted port, refuses the
//!     one nobody granted, and answers connections a host process opens into the guest (milestone
//!     64's inbound half). That is a fourth behaviour chosen by a fourth authority, on the same
//!     principle as the three above; the grant is `net_stack`'s spawn word, not anything this
//!     program can ask for.
//!   - **granted neither** (only the heap and stdout slots): both return `Unsupported`, and the
//!     program runs the phase-one transcript, proving the collections, timing, and the honest
//!     refusals.
//!
//! One binary keeps the initrd inside its nifefs directory limit (`MAX_FILES`, 31 entries when
//! this was written and 76 since 2026-08-01) while still proving all three. The kernel test suite spawns it three ways and checks each transcript
//! byte for byte, on both ISAs.

use std::collections::HashMap;
use std::fs::{Dir, File};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::random::{Rng, SystemRng};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use socket_proto::fixture;

fn main() {
    // Probe for a directory capability first: an `Unsupported` open means no FS-service endpoint in
    // slot 4, i.e. this process holds no directory and there is no ambient filesystem to fall back
    // on. Anything else means the filesystem IS granted, so a failure to open the file the image
    // ships is a real failure and must not be silently swallowed by falling through to the net.
    match File::open(fs_proto::fixture::MOTD_NAME) {
        Ok(f) => return fs_demo(f),
        Err(e) if e.kind() == ErrorKind::Unsupported => {}
        Err(e) => panic!("a directory capability was granted but the motd would not open: {e:?}"),
    }

    // Probe for the network by trying to open a UDP socket. A program not granted the `Stack`
    // endpoint and a frame untyped gets `Unsupported` here (the RETYPE of a shared frame fails
    // with no untyped in slot 3), which is the signal to run the offline transcript instead.
    match UdpSocket::bind("0.0.0.0:0") {
        Ok(sock) => net_demo(sock),
        Err(_) => offline_demo(),
    }
}

/// The phase-one transcript: collections on the untyped heap, `Instant`, and the honest refusal of
/// `fs` and `net`. Runs when the program was not granted the network.
fn offline_demo() {
    let t0 = Instant::now();

    println!("hello from std on nife");
    println!("os {}", std::env::consts::OS);

    // Vec: growth reallocations against the untyped-backed heap.
    let v: Vec<u64> = (0..10_000).map(|i| i * 3).collect();
    let sum: u64 = v.iter().sum();
    println!("vec sum {sum}");

    // String: heap bytes whose length the receiver checks.
    let mut s = String::new();
    for _ in 0..100 {
        s.push_str("nife ");
    }
    println!("string len {}", s.len());

    // HashMap: exercises the platform RandomState seed (sys/random) plus many small allocations.
    let mut m = HashMap::new();
    for k in 0u64..100 {
        m.insert(k, k * k);
    }
    println!("map lookup {}", m[&37]);

    // The honesty checks: the platform must refuse, not pretend.
    match std::fs::File::open("/init") {
        Err(e) if e.kind() == ErrorKind::Unsupported => println!("fs honestly unsupported"),
        other => println!("fs lied: {other:?}"),
    }
    match TcpStream::connect("127.0.0.1:80") {
        Err(e) if e.kind() == ErrorKind::Unsupported => println!("net honestly unsupported"),
        other => println!("net lied: {other:?}"),
    }

    // Instant: monotonic and advancing, but asserted rather than printed (a printed duration
    // would make the transcript nondeterministic).
    let t1 = Instant::now();
    assert!(t1 >= t0, "the virtual counter went backwards");
    assert!(
        t1.duration_since(t0).as_nanos() > 0,
        "no time passed across real work"
    );
    println!("instant monotonic ok");

    // **Wall-clock time** (milestone 51). This process was granted a clock: a `Frame` capability
    // naming the clock service's page in slot 5 and a read-only mapping of it, which is all
    // `SystemTime::now()` needs (the offset from the page, the monotonic counter from the ambient
    // register). Asserted rather than printed, because a real date is not a deterministic
    // transcript; what is checked is that it is inside the same sanity window the clock service
    // applies, which 1970-plus-uptime is not.
    let wall = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the wall clock is before the Unix epoch");
    assert!(
        wall >= NOT_BEFORE && wall < NOT_AFTER,
        "the wall clock reads {wall:?}, outside the plausible window",
    );
    // And the property the counter-plus-offset design buys: `SystemTime` and `Instant` are read
    // from the same counter, so the wall clock advancing does not mean the monotonic one jumped.
    let t2 = Instant::now();
    assert!(t2 >= t1, "the monotonic counter went backwards");
    println!("wall clock ok");

    // **Real entropy** (milestone 56). This process was granted one endpoint that means "you may
    // obtain randomness"; it names no device, and the entropy service on the other end is the only
    // thing that can read the virtio-rng. Asserted rather than printed, because random bytes are
    // the least deterministic transcript imaginable. Two 32-byte draws agreeing is a 2^-256 event
    // with a real source and a certainty with the counter-seeded stream this replaced, so the
    // comparison is what says the bytes came off a device.
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    SystemRng.fill_bytes(&mut a);
    SystemRng.fill_bytes(&mut b);
    assert_ne!(a, b, "two draws from std::random are identical");
    assert!(a.iter().any(|&x| x != 0), "a draw is all zeros");
    println!("entropy ok");

    // **The environment** (milestone 64, rank 4 of the measured gap list). Three separate claims,
    // and the first one is why the line exists at all.
    //
    // `vars()` must not panic. Before this milestone nife had no `sys::env` backend, so it fell
    // through to the unsupported one whose `env()` is `panic!`, and `std::env::vars()` aborted the
    // process. That compiled perfectly. Counting the iterator is what proves the call returned.
    //
    // It must be EMPTY, because nothing endows a nife process with variables and inventing some
    // would be exactly the ambient authority this system does not have.
    //
    // And `set_var` must actually take, because that is what `set_var` means everywhere else and a
    // library configured by its caller through the environment is an ordinary thing to do.
    assert_eq!(
        std::env::vars().count(),
        0,
        "a nife process started with variables it was never granted",
    );
    assert!(
        std::env::var("PATH").is_err(),
        "PATH resolved, so something is fabricating an ambient environment",
    );
    unsafe { std::env::set_var("NIFE_DEMO", "set-by-the-program") };
    assert_eq!(
        std::env::var("NIFE_DEMO").as_deref(),
        Ok("set-by-the-program"),
        "a variable this process set did not read back",
    );
    assert_eq!(
        std::env::vars().count(),
        1,
        "the variable this process set is not in its own listing",
    );
    unsafe { std::env::remove_var("NIFE_DEMO") };
    assert!(
        std::env::var("NIFE_DEMO").is_err(),
        "a removed variable still resolves",
    );
    println!("env ok");

    // **The path half of `std::env`, and `process::id`** (milestone 64, third pass). Every
    // assertion below used to be unreachable, because three of these calls did not refuse: they
    // were `panic!` in the shared fallbacks nife fell through to, so a program that asked was
    // killed. `std::env::temp_dir()`, `std::env::split_paths()` and `std::process::id()` all
    // compiled perfectly and aborted, which is `env::vars()`'s finding a third time.
    //
    // `process::id` is 0 because this system issues no process identifier at all, and 0 is the one
    // number no Unix assigns to a user process, so it cannot be mistaken for a real one.
    assert_eq!(
        std::process::id(),
        0,
        "a nife process reported a pid it was never issued",
    );
    // `temp_dir` is the directory this process holds, which is the only place a temporary file
    // could go, and `TMPDIR` steers it exactly as it does on Unix. That variable is the seam
    // milestone 47's namespace arrives through: nothing seeds it today, so the fallback answers.
    assert_eq!(
        std::env::temp_dir(),
        PathBuf::from("."),
        "temp_dir is not the granted directory",
    );
    unsafe { std::env::set_var("TMPDIR", "scratch") };
    assert_eq!(
        std::env::temp_dir(),
        PathBuf::from("scratch"),
        "TMPDIR did not steer temp_dir",
    );
    unsafe { std::env::remove_var("TMPDIR") };
    // `join_paths`/`split_paths` are pure string work with no platform in them, which is why the
    // old `panic!` was indefensible rather than merely unimplemented. Round-tripped, because a
    // splitter that agrees with nothing is worth less than no splitter.
    let joined = std::env::join_paths(["a", "b", "c"]).expect("joining three plain names failed");
    let split: Vec<PathBuf> = std::env::split_paths(&joined).collect();
    assert_eq!(
        split,
        [PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")],
        "a path list did not survive a join and a split",
    );
    assert!(
        std::env::join_paths(["a:b"]).is_err(),
        "a segment containing the separator was joined into a list that reads back as two",
    );
    // And the refusals that stay refusals, asserted so that a later lane cannot quietly turn one
    // into a fabricated answer. Each of these can say no in its own signature, and each needs a
    // namespace to resolve against, which is milestone 47's half of the `File::open` fork.
    assert!(
        std::env::current_dir().is_err(),
        "current_dir answered, so something is fabricating an ambient namespace",
    );
    assert!(
        std::env::current_exe().is_err(),
        "current_exe answered, but nothing tells a nife process the path it was loaded from",
    );
    assert!(
        std::env::home_dir().is_none(),
        "home_dir answered, but nobody gave this program a home",
    );
    println!("paths ok");
}

/// The same sanity window `clock_proto::policy` applies, restated here because a std program links
/// std and not the contract crate. 2026-01-01 and 2100-01-01.
const NOT_BEFORE: Duration = Duration::from_secs(1_767_225_600);
const NOT_AFTER: Duration = Duration::from_secs(4_102_444_800);

/// **The `std::fs` transcript** (milestone 27 phase two, the FS half): ordinary `File`, `Read`,
/// `read_to_string`, and `metadata`, all served by the RedoxFS FS server over the §27 contract, and
/// all reached through the one directory capability this process was granted.
///
/// The interesting half is the refusals. `File::open` takes a path, but this system has no global
/// namespace: a name means "under the directory I hold", so `/etc/passwd`, `../motd`, and
/// `sub/motd` are not things this process can express, and each is refused *before* a byte reaches
/// the server. The refusal is `InvalidFilename` (there is no such name here), never
/// `PermissionDenied`, because nothing checked a permission. A name that IS expressible but absent
/// is an ordinary `NotFound`, which is what makes the difference legible.
///
/// `motd` is already open when this runs: opening it was the probe that chose this branch.
fn fs_demo(mut motd: File) {
    println!("std fs on nife");

    // Bytes off a real RedoxFS image, through a confined FS server, reached with `Read` on an
    // ordinary `File`. Printed as well as asserted, so the kernel test compares the file's contents
    // byte for byte after they have crossed the whole stack.
    let mut bytes = Vec::new();
    motd.read_to_end(&mut bytes)
        .expect("reading the motd through std::fs failed");
    assert_eq!(
        bytes,
        fs_proto::fixture::MOTD,
        "std::fs read the wrong bytes off the image"
    );
    print!(
        "{}",
        String::from_utf8(bytes).expect("the motd is UTF-8 and ends in a newline")
    );
    drop(motd); // Drop CLOSEs the handle the server minted for us.

    // read_to_string reopens the same name and leans on the size hint the PAL answers with FSTAT.
    let text = std::fs::read_to_string(fs_proto::fixture::MOTD_NAME)
        .expect("read_to_string through std::fs failed");
    println!("read_to_string {}", text.len());

    let meta =
        std::fs::metadata(fs_proto::fixture::MOTD_NAME).expect("metadata through std::fs failed");
    assert!(meta.is_file(), "the motd is a regular file");
    println!("metadata len {}", meta.len());

    refused("/etc/passwd", "absolute");
    refused("../motd", "dotdot");
    // **`..` is refused at every position, not only the first** (milestone 122, where nested paths
    // stopped being refused as a class). This one names a file that really exists and that this
    // process really can reach through the root grant it holds, by a route the contract has no verb
    // for: a handle names a directory and nothing on the wire names its parent, so there is no
    // ascent to perform and no capability that would designate what it reached.
    refused(
        &format!(
            "{}/../{}/{}",
            fs_proto::fixture::tree::SUB,
            fs_proto::fixture::tree::OTHER,
            fs_proto::fixture::tree::SECRET
        ),
        "inner dotdot",
    );

    match File::open("definitely-not-here") {
        Err(e) if e.kind() == ErrorKind::NotFound => println!("missing not found"),
        other => panic!("a missing name did not read as NotFound: {other:?}"),
    }

    // **`std::fs::write` works now** (milestone 31 phase 2), and this is the assertion the CREATE and
    // TRUNCATE verbs exist for. It creates a name the image does not carry, so it exercises CREATE,
    // and it is deliberately written TWICE with a SHORTER payload the second time, which is the case
    // that used to be impossible to get right: without TRUNCATE the second write would leave the tail
    // of the first behind and the read-back would come up long. §27's four corrections all trace to
    // that one behaviour, so it is pinned here at the top level rather than only in a host test.
    let long = b"the first write, deliberately the longer of the two";
    let short = b"the second write, shorter";
    assert!(
        short.len() < long.len(),
        "the shorter write must be shorter"
    );

    std::fs::write("made-by-std", long).expect("fs::write could not create a file");
    std::fs::write("made-by-std", short).expect("fs::write could not rewrite a file");
    let back = std::fs::read("made-by-std").expect("reading back what fs::write wrote failed");
    assert_eq!(
        back, short,
        "a shorter fs::write must REPLACE the contents, not leave the old tail",
    );
    println!("write create ok");

    // `create_new` on a name that now exists is AlreadyExists, not Unsupported and not a silent
    // overwrite. That distinction is the reason CREATE refuses rather than opening.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("made-by-std")
    {
        Err(e) if e.kind() == ErrorKind::AlreadyExists => println!("create_new refused"),
        other => panic!("create_new over an existing name did not refuse: {other:?}"),
    }

    // And a created name is still bound by the directory capability: creating outside it is refused
    // the same way opening outside it is, so CREATE did not widen what a client can reach.
    for (path, what) in [("/tmp/escape", "absolute"), ("../escape", "dotdot")] {
        match std::fs::write(path, b"x") {
            Err(e) if e.kind() != ErrorKind::PermissionDenied => {
                println!("create refused {what}")
            }
            other => panic!("creating an un-nameable path was not refused: {other:?}"),
        }
    }

    // **The on-device write path, and a correction to the record.** notes/fs-server.md recorded
    // that an end-to-end write looped inside RedoxFS's allocator commit on bare metal, so the
    // milestone-32 client's test stayed read-only. With interrupt-driven block completion restored
    // it completes: this writes over a file the image ships (there is no create verb, so `scratch`
    // must already exist), reads it back, and the host tool re-reads the image after the run to
    // prove the bytes reached the disk rather than a cache.
    let mut scratch = std::fs::OpenOptions::new()
        .write(true)
        .open(fs_proto::fixture::SCRATCH_NAME)
        .expect("opening scratch for writing through std::fs failed");
    scratch
        .write_all(fs_proto::fixture::WRITE_PATTERN)
        .expect("writing scratch through std::fs failed");
    drop(scratch);
    let back = std::fs::read(fs_proto::fixture::SCRATCH_NAME)
        .expect("reading scratch back through std::fs failed");
    assert_eq!(
        back,
        fs_proto::fixture::WRITE_PATTERN,
        "the write did not read back"
    );
    println!("write readback ok");

    namespace_transcript();
    descent_transcript();

    println!("fs ok");
}

/// **The namespace verbs** (milestone 64): `create_dir`, `read_dir`, `rename`, `remove_file`,
/// `remove_dir`.
///
/// All five were `Unsupported` in this PAL until now, and none of them was waiting on the FS
/// contract: `MKDIR`, `OPENDIR`, `READDIR`, `RENAME`, `UNLINK` and `RMDIR` have been dispatched by
/// the server since milestones 47 and 48. What this function proves is the binding, end to end, on
/// a real RedoxFS image.
///
/// **It cleans up before it starts, not after**, and that is deliberate rather than tidy.
/// `NIFE_KEEP_REDOXFS=1` runs the suite against an image a previous boot wrote, which is a
/// supported mode (it is how the cross-boot write case is reached), so this has to be idempotent
/// over an image that already carries what a previous run made. Cleaning up first also means the
/// directory's contents are known by the time `read_dir` looks at them.
fn namespace_transcript() {
    const DIR: &str = "made-dir-by-std";
    const RENAMED: &str = "renamed-by-std";

    // Cleanup. `NotFound` is the expected answer on a fresh image and is not a failure; anything
    // else is, because it would mean the verb itself is broken rather than the name absent.
    for (name, removed) in [(RENAMED, false), (DIR, true)] {
        let r = if removed {
            std::fs::remove_dir(name)
        } else {
            std::fs::remove_file(name)
        };
        match r {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            other => panic!("cleaning up {name} failed: {other:?}"),
        }
    }

    std::fs::create_dir(DIR).expect("create_dir failed");
    println!("mkdir ok");

    // **The listing, and the one fact only a listing can give you.** Before this verb bound, a
    // program holding a directory capability could open a name it already knew and nothing else;
    // it could not find out what was there. Two assertions: the fixture's own name is present (so
    // the listing reaches real image contents), and the directory just created comes back marked
    // as a directory (so `dirent`'s IS_DIR crosses the wire).
    let mut saw_motd = false;
    let mut saw_dir = false;
    for entry in std::fs::read_dir(".").expect("read_dir of the granted directory failed") {
        let entry = entry.expect("a directory entry did not decode");
        let name = entry.file_name();
        if name == fs_proto::fixture::MOTD_NAME {
            saw_motd = true;
            assert!(
                entry.file_type().expect("file_type failed").is_file(),
                "the motd came back marked as a directory",
            );
        }
        if name == DIR {
            saw_dir = true;
            assert!(
                entry.file_type().expect("file_type failed").is_dir(),
                "a directory came back marked as a file",
            );
        }
    }
    assert!(saw_motd, "read_dir did not list the fixture's own file");
    assert!(saw_dir, "read_dir did not list the directory just created");
    println!("read_dir ok");

    // A subdirectory is reached by *descending*: `OPENDIR` mints a capability to it and the
    // enumeration runs through that, which is why this is a different code path from the line
    // above and worth its own assertion. A directory just made is empty; there are no `.` and
    // `..` entries on this contract, because they would be names for things no capability
    // designates.
    let n = std::fs::read_dir(DIR)
        .expect("read_dir of a subdirectory failed")
        .count();
    assert_eq!(n, 0, "a directory just created was not empty");
    println!("read_dir descend ok");

    // **Neither remove verb will remove the kind the other one is for**, which is the whole safety
    // property behind having two. A single "remove whatever you find" is what makes `rm -r`
    // dangerous, and this contract does not offer it at any opcode.
    match std::fs::remove_file(DIR) {
        Err(e) if e.kind() == ErrorKind::IsADirectory => println!("unlink refused a directory"),
        other => panic!("remove_file on a directory was not refused: {other:?}"),
    }
    match std::fs::remove_dir("made-by-std") {
        Err(e) if e.kind() == ErrorKind::NotADirectory => println!("rmdir refused a file"),
        other => panic!("remove_dir on a file was not refused: {other:?}"),
    }

    // **Rename, which matters out of proportion to how often crates name it.** Write-a-temp-then-
    // rename is how a careful program replaces a file, and it needs the destination to appear with
    // the source's contents and the source's name to be gone in the same step. Both halves are
    // asserted, because a rename that copied would pass the first one.
    std::fs::rename("made-by-std", RENAMED).expect("rename failed");
    let back = std::fs::read(RENAMED).expect("reading the renamed file failed");
    assert_eq!(
        back, b"the second write, shorter",
        "the renamed file did not carry the source's contents",
    );
    match File::open("made-by-std") {
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        other => panic!("the source name survived a rename: {other:?}"),
    }
    println!("rename ok");

    std::fs::remove_file(RENAMED).expect("remove_file failed");
    assert!(
        !std::fs::exists(RENAMED).expect("exists failed after remove_file"),
        "a removed name still resolves",
    );
    println!("unlink ok");

    // **A directory can be stat'd, which sounds minor and is not.** `OPEN` refuses a directory, so
    // `Path::is_dir()` used to be false for every directory that exists, and `create_dir_all` was
    // not idempotent: it recovers from `AlreadyExists` by asking whether the name is a directory
    // already, and got told no. Both are asserted here, and `create_dir_all` over the name that
    // already exists is the one that would have failed.
    assert!(
        std::path::Path::new(DIR).is_dir(),
        "a directory did not answer is_dir",
    );
    assert!(
        !std::path::Path::new(fs_proto::fixture::MOTD_NAME).is_dir(),
        "a file answered is_dir",
    );
    assert!(
        std::path::Path::new(".").is_dir(),
        "the granted directory itself did not answer is_dir",
    );
    std::fs::create_dir_all(DIR).expect("create_dir_all over an existing directory failed");
    println!("is_dir ok");

    std::fs::remove_dir(DIR).expect("remove_dir failed");
    println!("rmdir ok");

    set_len_and_copy();
}

/// **`File::set_len` and `fs::copy`** (milestone 64, ranks 8 and 26), both of which the PAL refused
/// until this milestone and neither of which needed anything from the contract that was not there.
///
/// `set_len` is `TRUNCATE` with the size in the second word, which is the same message `File::open`
/// has been sending with a 0 in it since milestone 31. Both directions are checked, because
/// `ftruncate` grows as well as shrinks and a binding that only shrank would pass a shrink-only test.
///
/// `copy` needs no verb at all: it is an open, a read/write loop and two closes. What it proves that
/// a length check would not is that the destination gets the *source's bytes* rather than a file of
/// the right size, which is the way a copy loop with a bad offset fails.
fn set_len_and_copy() {
    const ORIGINAL: &str = "set-len-subject";
    const DUPLICATE: &str = "copy-destination";

    std::fs::write(ORIGINAL, b"0123456789").expect("write failed");

    // Shrink. The bytes past the new end must be gone, not merely unreported.
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(ORIGINAL)
        .expect("reopening for set_len failed");
    f.set_len(4).expect("set_len shrink failed");
    drop(f);
    assert_eq!(
        std::fs::read(ORIGINAL).expect("read after shrink failed"),
        b"0123",
        "set_len shrank the reported size without discarding the tail",
    );

    // Grow. POSIX extends with zeroes, and the contract's TRUNCATE promises the same, so the four
    // bytes must survive and the new tail must be NUL rather than whatever was there before.
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(ORIGINAL)
        .expect("reopening for set_len grow failed");
    f.set_len(8).expect("set_len grow failed");
    drop(f);
    assert_eq!(
        std::fs::read(ORIGINAL).expect("read after grow failed"),
        b"0123\0\0\0\0",
        "set_len grew the file with something other than zeroes",
    );
    println!("set_len ok");

    let n = std::fs::copy(ORIGINAL, DUPLICATE).expect("copy failed");
    assert_eq!(n, 8, "copy reported the wrong byte count");
    assert_eq!(
        std::fs::read(DUPLICATE).expect("read of the copy failed"),
        b"0123\0\0\0\0",
        "the copy does not hold the source's bytes",
    );
    // The source survives a copy. Trivially true and worth asserting once, because the way to
    // implement `copy` wrongly on a contract with a RENAME verb is to reach for the wrong one.
    assert!(
        std::fs::exists(ORIGINAL).expect("exists on the copy source failed"),
        "the source did not survive a copy",
    );
    println!("copy ok");

    std::fs::remove_file(ORIGINAL).expect("cleanup of the set_len subject failed");
    std::fs::remove_file(DUPLICATE).expect("cleanup of the copy destination failed");
}

/// **Descent** (milestone 122): a nested path is a chain of `OPENDIR`s, and a directory is a thing a
/// program can hold.
///
/// The sharp bug this closes is the one a walker hits on its first step. `read_dir` has worked since
/// milestone 64, and every entry it hands back carries a `path()`; one level down that path is two
/// components, and two components were refused. **A std program could list a subdirectory and then
/// not open what it had just been told was in there**, which is most of what filesystem software
/// does.
///
/// Nothing here widens the grant, and the fixture is chosen so that can be seen rather than
/// asserted. `sub/deeper/leaf` is two descents inside the directory this process holds, and each hop
/// is an `OPENDIR` whose rights are the last hop's intersected with what was asked for.
/// `sub/../other/secret` is a file in a *sibling* of `sub` that this process can reach perfectly
/// well by naming `other/secret`, and it is refused through the `..` route, because the refusal is
/// about there being no ascent rather than about what is at the end.
fn descent_transcript() {
    use fs_proto::fixture::tree;

    // One descent, and the bytes are the fixture's own, so this is the whole stack again (disk,
    // block server, FS server, contract, PAL) with a directory capability minted in the middle.
    let inner = std::fs::read(format!("{}/{}", tree::SUB, tree::INNER))
        .expect("a file one directory down would not open");
    assert_eq!(
        inner,
        tree::INNER_BODY,
        "the descent read the wrong file's bytes"
    );
    println!("descend read ok");

    // Two descents. `deeper` exists in the fixture precisely so that a second one has somewhere to
    // go, and so that `dir::DESCEND` has something to withhold.
    let leaf = std::fs::read(format!("{}/{}/{}", tree::SUB, tree::DEEPER, tree::LEAF))
        .expect("a file two directories down would not open");
    assert_eq!(
        leaf,
        tree::LEAF_BODY,
        "the second descent read the wrong file"
    );
    println!("descend twice ok");

    // **The tree this program owns**, built before anything lists it. It cannot be the fixture's
    // `sub`: milestone 47's directory-capability attacker is granted exactly that directory and
    // writes into it, so its contents depend on which tests ran first in this boot. `sub/inner` and
    // `sub/deeper/leaf` above are safe to read because the post-run host check pins them; the
    // *listing* of `sub` is not a fact this program can assert.
    //
    // Building it also exercises the walk on the way in: `create_dir` on a nested name is a walked
    // `MKDIR`, and `write` to one is a walked `CREATE`.
    const TREE: &str = "walked-by-std";
    match std::fs::remove_dir_all(TREE) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        other => panic!("cleaning up {TREE} failed: {other:?}"),
    }
    std::fs::create_dir(TREE).expect("create_dir failed");
    std::fs::create_dir(format!("{TREE}/{}", tree::DEEPER)).expect("a nested create_dir failed");
    std::fs::write(format!("{TREE}/top"), tree::LEAF_BODY).expect("a nested write failed");

    // **The pair that was broken.** List a subdirectory, then open every file the listing named,
    // through the `path()` the listing itself handed back. One level down that path is two
    // components, and two components used to be refused, so a program could be told what was there
    // and then not reach it. Every filesystem walker is built out of exactly this pair.
    let mut opened = 0;
    let mut dirs = 0;
    for entry in std::fs::read_dir(TREE).expect("read_dir of a subdirectory failed") {
        let entry = entry.expect("a directory entry did not decode");
        if entry.file_type().expect("file_type failed").is_dir() {
            dirs += 1;
            continue;
        }
        let bytes = std::fs::read(entry.path())
            .expect("a file read_dir listed would not open by the path read_dir gave");
        assert_eq!(
            bytes,
            tree::LEAF_BODY,
            "an entry opened by its listed path read the wrong bytes",
        );
        opened += 1;
    }
    assert_eq!(opened, 1, "the tree this program made holds one file");
    assert_eq!(dirs, 1, "the tree this program made holds one directory");
    println!("walk entry ok");

    // A name in the middle of a path has to be a directory. The server answers `ENOTDIR` for an
    // `OPENDIR` of a file, and that is a fact about the name rather than about the capability, so it
    // does not read as a refusal.
    match File::open(format!("{}/{}", fs_proto::fixture::MOTD_NAME, tree::INNER)) {
        Err(e) if e.kind() == ErrorKind::NotADirectory => println!("through a file refused"),
        other => panic!("a path walking through a file was not refused: {other:?}"),
    }

    // **`std::fs::Dir`**: std's own `openat`-shaped API, holding a capability this process was
    // handed. This is the half of milestone 122 that matters most, because it is the interface that
    // makes the system's model visible to the software running on it: the name `inner` is resolved
    // against a directory the program is *holding*, and there is no path composition anywhere in it.
    let dir = Dir::open(tree::SUB).expect("Dir::open of a subdirectory failed");
    let mut f = dir
        .open_file(tree::INNER)
        .expect("Dir::open_file under a held directory failed");
    let mut held = Vec::new();
    f.read_to_end(&mut held)
        .expect("reading through a held directory failed");
    assert_eq!(
        held,
        tree::INNER_BODY,
        "the held directory opened the wrong file"
    );
    drop(f);

    // A held directory is bound exactly as the granted one is: `..` names nothing through it either.
    match dir.open_file(format!("../{}", fs_proto::fixture::MOTD_NAME)) {
        Err(e) if e.kind() == ErrorKind::InvalidFilename => {}
        other => panic!("`..` through a held directory was not refused: {other:?}"),
    }

    // `Dir::open(".")` is the granted directory itself and costs no message at all, because it is
    // what the endpoint is bound to rather than a name inside anything.
    let granted = Dir::open(".").expect("Dir::open of the granted directory failed");
    let mut motd = granted
        .open_file(fs_proto::fixture::MOTD_NAME)
        .expect("the granted directory would not open its own file");
    let mut bytes = Vec::new();
    motd.read_to_end(&mut bytes).expect("read failed");
    assert_eq!(
        bytes,
        fs_proto::fixture::MOTD,
        "the granted directory read the wrong file"
    );
    println!("dir handle ok");

    // **A move between two directories**, which is one atomic message rather than a copy and an
    // unlink: `RENAME` carries a handle on each side and both of them are now walked to.
    std::fs::rename(
        format!("{TREE}/top"),
        format!("{TREE}/{}/{}", tree::DEEPER, tree::LEAF),
    )
    .expect("renaming between two directories failed");
    assert!(
        !std::fs::exists(format!("{TREE}/top")).expect("exists after rename failed"),
        "the source name survived a rename",
    );
    assert_eq!(
        std::fs::read(format!("{TREE}/{}/{}", tree::DEEPER, tree::LEAF))
            .expect("the renamed file would not open"),
        tree::LEAF_BODY,
        "the rename moved a name without its bytes",
    );
    println!("rename across ok");

    // **`remove_dir_all` needed no nife code at all.** std's generic recursion is written in terms
    // of `read_dir`, `remove_file` and `remove_dir` on paths it composes with `DirEntry::path`, so
    // it started working the moment those paths resolved. It was `Unsupported` here for two
    // milestones with a note explaining why, which is the fourth refusal in this PAL to have
    // outlived its own reason.
    std::fs::remove_dir_all(TREE).expect("remove_dir_all failed");
    assert!(
        !std::fs::exists(TREE).expect("exists after remove_dir_all failed"),
        "remove_dir_all left the tree behind",
    );
    println!("remove_dir_all ok");
}

/// Assert that a path is refused as un-nameable, and say which case it was.
fn refused(path: &str, label: &str) {
    match File::open(path) {
        Err(e) if e.kind() == ErrorKind::InvalidFilename => println!("{label} refused"),
        other => panic!("{path} was not refused as un-nameable: {other:?}"),
    }
}

/// The guestfwd echo peer the test runners attach. Both of this program's network fixtures now live
/// inside libslirp (this one and the TFTP server `udp_ok` uses), so the transcript is offline and
/// deterministic: nothing it depends on can be dropped by somebody else's router.
const ECHO_PEER: &str = "10.0.2.9:7777";

/// The networked transcript (milestone 27 phase two): a UDP round trip and a TCP echo round
/// trip, reached only through `std::net`. The program never sees a capability, a socket id, or a
/// shared frame; it writes to a socket and reads from it, the way any Rust program does. Runs when
/// the program holds the network. `sock` is the already-bound UDP socket the probe opened.
fn net_demo(sock: UdpSocket) {
    println!("std net on nife");

    // **The inbound half is chosen by authority, exactly as this program's three top-level
    // branches are** (milestone 64). A listening port is a grant `net_stack` was spawned with, so
    // asking for one is the probe: granted means serve, and `PermissionDenied` means this stack was
    // never told which ports it may serve. Neither answer is a fallback, and the refusal is
    // *printed* rather than absorbed, so a run that silently lost the grant is a diff in the
    // pinned transcript rather than a program quietly doing less than it was asked to.
    match TcpListener::bind(("0.0.0.0", fixture::LISTEN_PORT)) {
        Ok(listener) => {
            // **The granted run serves and stops**, rather than also doing the outbound work. Each
            // of the two boots this binary gets on the network proves one thing, because a boot is
            // the expensive unit here: a net test spends minutes in `net_stack`'s userspace smoltcp
            // poll, and the outbound half is already proven by the run that is refused the port.
            drop(sock);
            return inbound_demo(&listener);
        }
        Err(e) if e.kind() == ErrorKind::PermissionDenied => println!("listen refused"),
        Err(e) => panic!("the listen probe failed for a reason that is not a refusal: {e:?}"),
    }

    // Assertions rather than printed status keep the transcript byte-stable: a failure faults (the
    // panic path), which the kernel test sees as a missing line and a timeout, not a wrong answer.
    assert!(udp_ok(&sock), "the UDP round trip through std::net failed");
    println!("udp ok");

    // The UDP socket is held (by ref) across the TCP exchange so the two use distinct socket ids,
    // and thus distinct net_stack local ports: net_stack derives a socket's local port from its id, so a TCP
    // connect that reused a just-closed UDP socket's id would reuse its port against slirp and can
    // stall (notes/std.md, the reuse finding). Keeping both open sidesteps it cleanly.
    assert!(
        tcp_echo_ok(),
        "the TCP echo round trip through std::net failed"
    );
    println!("tcp echo ok");
    drop(sock);
}

/// **The inbound transcript** (milestone 64): this program is a *server*, through `std::net` and
/// nothing else.
///
/// Everything else here is the guest as a client. This is the mirror, and it is what a file
/// service is made of: bind a port the spawn granted, accept a connection a **host** process opened
/// through QEMU's `hostfwd`, read the request, compose an answer, and do it again on the same
/// listener. Nothing in this function names a capability, a socket id, or a shared frame.
///
/// Three claims, in the order they are printed, and the first two are the ones that matter most:
///
/// - **`denied refused`**: a port outside the grant is `PermissionDenied`. If this program could
///   bind a port nothing granted it, the whole inbound authority would be decoration, and that is
///   the one failure on this path that would not show up as a missing line somewhere else.
/// - **`in use refused`**: the granted port, asked for twice, is `AddrInUse`. Exclusivity is what
///   makes a port a grantable thing rather than a number, and this is what enforcing it feels like
///   through `std::io::ErrorKind`.
/// - **`served N`**: `ROUNDS` connections, one after another, on one listener. The second is the
///   load-bearing one: a listener that goes deaf after one connection would pass a one-round gate
///   and is exactly what milestone 55's Samba-shaped workload cannot use.
///
/// The host's half of this is `xtask`'s inbound prober, which requires its own bytes back and fails
/// the leg if the guest never answered. Neither side alone is the gate.
fn inbound_demo(listener: &TcpListener) {
    println!("listen ok");

    match TcpListener::bind(("0.0.0.0", fixture::DENIED_PORT)) {
        Err(e) if e.kind() == ErrorKind::PermissionDenied => println!("denied refused"),
        Ok(_) => panic!("bound a port this program was never granted"),
        Err(e) => {
            panic!("a port outside the grant failed, but not as a refusal of authority: {e:?}")
        }
    }

    match TcpListener::bind(("0.0.0.0", fixture::LISTEN_PORT)) {
        Err(e) if e.kind() == ErrorKind::AddrInUse => println!("in use refused"),
        Ok(_) => panic!("two listeners bound one port"),
        Err(e) => panic!("a second bind of a held port failed, but not as a port collision: {e:?}"),
    }

    for round in 0..fixture::ROUNDS {
        serve_one_inbound(listener, round);
    }
    println!("served {}", fixture::ROUNDS);
}

/// Accept one inbound connection, check what the host sent, answer it, and close.
///
/// `read_exact` rather than one `read`: a segment boundary is the host's business and a server that
/// assumed one read per request would be asserting something about slirp. Dropping the stream is
/// the close, and `net_stack` drains the handshake inside it, so the answer is on the wire before
/// this returns.
fn serve_one_inbound(listener: &TcpListener, round: usize) {
    let (mut conn, _peer) = listener
        .accept()
        .unwrap_or_else(|e| panic!("round {round}: nobody connected: {e:?}"));

    let mut got = vec![0u8; fixture::IN_MSG.len()];
    conn.read_exact(&mut got)
        .unwrap_or_else(|e| panic!("round {round}: reading the request failed: {e:?}"));
    assert_eq!(
        got,
        fixture::IN_MSG,
        "round {round}: something connected and said something else",
    );

    conn.write_all(fixture::OUT_MSG)
        .unwrap_or_else(|e| panic!("round {round}: answering failed: {e:?}"));
}

/// **The gating UDP round trip: slirp's own TFTP server**, the `std::net` twin of `socket_test_client`'s
/// `udp_tftp`. libslirp implements TFTP internally (enabled by `tftp=` on the netdev), so this
/// request and its reply never leave the emulator.
///
/// This used to be a DNS A-record query for `example.com` at 10.0.2.3:53, which is *not* a resolver:
/// libslirp NATs anything sent there to the HOST's nameserver, so the test silently depended on the
/// developer's DNS answering at that instant and flaked at roughly 2.5% per query. That was fixed for
/// the hand-built `socket_test_client` gate and **missed here**, which is why this twin went on flaking after the
/// fix landed; it cost a riscv leg on 2026-07-29. The lesson is worth the sentence: a fix applied to
/// one of two call sites of the same hazard is half a fix, and the surviving half is harder to find
/// because the record says the problem is solved.
///
/// What it proves is what the DNS version was there to prove about *our* code and nothing about the
/// host: a program holding no capability and no socket id sends a datagram through `std::net` to an
/// address it chooses and reads the reply back. Send a read request (opcode 1, `octet` mode) for the
/// fixture the runners planted, and require the first data packet: opcode 3, block 1, the fixture's
/// bytes exactly. The fixture is one short block, so the whole file arrives in that packet.
///
/// The name and body must match what `scripts/qemu-runner-*.sh` writes into `target/tftp`.
fn udp_ok(sock: &UdpSocket) -> bool {
    const TFTP_SERVER: &str = "10.0.2.2:69";
    const TFTP_NAME: &[u8] = b"nife";
    const TFTP_BODY: &[u8] = b"nife-tftp!";

    if sock.connect(TFTP_SERVER).is_err() {
        return false;
    }

    // RRQ: { u16 opcode = 1 } filename 0 "octet" 0
    let mut rrq = vec![0x00, 0x01];
    rrq.extend_from_slice(TFTP_NAME);
    rrq.push(0x00);
    rrq.extend_from_slice(b"octet");
    rrq.push(0x00);
    if sock.send(&rrq).is_err() {
        return false;
    }

    let mut buf = [0u8; 512];
    let Ok(n) = sock.recv(&mut buf) else {
        return false;
    };
    if n < 4 + TFTP_BODY.len() {
        return false;
    }
    // An ERROR packet (opcode 5) here means the fixture is missing: see the runners.
    let opcode = u16::from_be_bytes([buf[0], buf[1]]);
    let block = u16::from_be_bytes([buf[2], buf[3]]);
    if opcode != 3 || block != 1 || &buf[4..4 + TFTP_BODY.len()] != TFTP_BODY {
        return false;
    }

    // ACK block 1, ending the transfer properly rather than leaving the server retransmitting DATA
    // at a socket we are about to drop. Failing to be acknowledged is not this test's business, so
    // the send's result is deliberately ignored.
    let _ = sock.send(&[0x00, 0x04, 0x00, 0x01]);
    true
}

/// Connect to the echo peer over TCP, send a payload, and read the echo back whole.
fn tcp_echo_ok() -> bool {
    const MSG: &[u8] = b"nife-std-net!";
    let Ok(mut stream) = TcpStream::connect(ECHO_PEER) else {
        return false;
    };
    if stream.write_all(MSG).is_err() {
        return false;
    }
    let mut got = Vec::new();
    let mut buf = [0u8; 64];
    while got.len() < MSG.len() {
        match stream.read(&mut buf) {
            Ok(0) => break, // peer closed
            Ok(k) => got.extend_from_slice(&buf[..k]),
            Err(_) => return false,
        }
    }
    got == MSG
}
