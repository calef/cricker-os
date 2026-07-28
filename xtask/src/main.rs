//! Build orchestration for cricker-os.
//!
//! A normal Rust binary that runs on the *host*. Building a kernel means a custom
//! target, a linker script, and driving QEMU with the right flags, none of which fits
//! neatly into `cargo build`. This beats a Makefile because it's Rust and it composes.
//! See DECISIONS.md §7.
//!
//!     cargo xtask run      boot the kernel (the milestone tour), print to this terminal
//!     cargo xtask shell    boot straight to the interactive shell (add --hvf for the real core)
//!     cargo xtask test     host tests (milliseconds), then the kernel under QEMU
//!     cargo xtask gdb      boot paused, waiting for a debugger on :1234
//!     cargo xtask objdump  disassemble the kernel
//!     cargo xtask image    build the flat arm64 Image and dump its header
//!
//! Note that `run` and `test` do NOT invoke QEMU themselves. They just call cargo,
//! which invokes `scripts/qemu-runner.sh` via the runner setting in
//! `.cargo/config.toml`. That script is the single source of truth for how the kernel
//! gets booted, so there is exactly one place to get the QEMU flags wrong.

use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};

const TARGET: &str = "aarch64-unknown-none-softfloat";
const RUNNER: &str = "scripts/qemu-runner.sh";

/// The RISC-V target, for the second-architecture initrd (milestone 20). The kernel itself is built
/// and run through cargo + `scripts/qemu-runner-riscv.sh` directly, not this xtask; this const exists
/// only so `initrd-riscv` builds the userspace archive for the matching target.
const RISCV_TARGET: &str = "riscv64imac-unknown-none-elf";

/// Whether this run builds optimized binaries. Only `bench --release` sets it (a fair cross-OS
/// comparison wants an optimized kernel and userspace, not the debug default). Everything else stays
/// debug: faster builds, and the tests and the tour want debuginfo and cheap rebuilds.
static RELEASE: AtomicBool = AtomicBool::new(false);

/// `"release"` or `"debug"`: the cargo profile directory the built artifacts land in.
fn profile_dir() -> &'static str {
    if RELEASE.load(Ordering::Relaxed) {
        "release"
    } else {
        "debug"
    }
}

/// Run `cargo <args>`, adding `--release` when this is a release run. For the build commands whose
/// output profile must match `profile_dir()` (the kernel and user builds behind `bench --release`).
fn cargo_profiled(args: &[&str]) -> bool {
    let mut v = args.to_vec();
    if RELEASE.load(Ordering::Relaxed) {
        v.push("--release");
    }
    cargo(&v)
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();

    let ok = match cmd.as_str() {
        "build" => build(),
        "run" => {
            maybe_hvf();
            // Build the disk and the initrd first: the kernel boots with them, and `cargo run`
            // would not rebuild them on its own (the kernel does not depend on them in cargo).
            mkdisk() && user() && cargo(&["run", "-p", "kernel", "--target", TARGET])
        }
        "shell" => {
            // Boot straight to the interactive shell (the milestone tour compiled out).
            maybe_hvf();
            eprintln!(
                "--- booting cricker-os to an interactive shell (type `help`, Ctrl-C to quit) ---"
            );
            mkdisk()
                && user()
                && cargo(&[
                    "run",
                    "-p",
                    "kernel",
                    "--features",
                    "shell",
                    "--target",
                    TARGET,
                ])
        }
        "initboot" => {
            // Milestone 19d.2c: boot with userspace init as the boot path (it brings up the
            // console). Add --hvf for the real core.
            maybe_hvf();
            eprintln!("--- booting cricker-os via userspace init (Ctrl-C to quit) ---");
            mkdisk()
                && user()
                && cargo(&[
                    "run",
                    "-p",
                    "kernel",
                    "--features",
                    "initboot",
                    "--target",
                    TARGET,
                ])
        }
        "initrd-riscv" => initrd_riscv(),
        "test" => test(),
        "bench" => bench(),
        "gdb" => gdb(),
        "objdump" => objdump(),
        "image" => image(),
        other => {
            if !other.is_empty() {
                eprintln!("unknown command: {other}\n");
            }
            eprintln!(
                "usage: cargo xtask <build|run|shell|initboot|initrd-riscv|test|bench|gdb|objdump|image> [--hvf]"
            );
            eprintln!("       cargo xtask bench [--riscv] [--real] [--release] [--check] [--save]");
            return ExitCode::FAILURE;
        }
    };

    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn build() -> bool {
    // The user program and the disk image first: the kernel boots with the program as an initrd
    // and reads the disk over virtio, so both have to exist before it runs.
    mkdisk() && user() && cargo(&["build", "-p", "kernel", "--target", TARGET])
}

/// Build the userspace ELF that the kernel will load at milestone 7.
///
/// It is a **separate crate with its own linker script** (linked at `0x40_0000`, in the low half,
/// where `TTBR0` lives), so it cannot accidentally share anything with the kernel. And it stays
/// an **ELF**: the kernel's loader wants program headers, unlike the kernel itself, which QEMU
/// wants as a flat image. See notes/elf.md.
fn user() -> bool {
    cargo_profiled(&["build", "-p", "user", "--target", TARGET]) && mkinitrd()
}

/// Where the packed initrd archive is written.
fn initrd_path() -> String {
    workspace_root()
        .join("target/initrd.img")
        .display()
        .to_string()
}

/// Where the RISC-V initrd archive is written (milestone 20). Separate from the aarch64 one because
/// it holds riscv64 ELFs, not aarch64 ones.
fn riscv_initrd_path() -> String {
    workspace_root()
        .join("target/initrd-riscv.img")
        .display()
        .to_string()
}

/// **Build the RISC-V userspace archive** (milestone 20, the richer-initrd step). Compiles the two
/// portable programs the second architecture runs (`builder`, the minimal init, and `worker`, the
/// child it loads) for the riscv target, and packs them into a crickerfs archive: `builder` under
/// the name `init` (the entry the kernel loads first), `worker` under `worker` (the one init loads by
/// name). Point `CRICKER_INITRD` at the result and boot the riscv kernel, e.g.:
///
/// ```text
/// cargo xtask initrd-riscv
/// CRICKER_INITRD=target/initrd-riscv.img cargo run -p kernel --target riscv64imac-unknown-none-elf
/// ```
fn initrd_riscv() -> bool {
    // Only the portable bins: hello/console/input/shell are aarch64-wired and do not build here.
    if !run(
        "cargo",
        &[
            "build",
            "-p",
            "user",
            "--bin",
            "builder",
            "--bin",
            "worker",
            "--bin",
            "driver",
            "--bin",
            "elbench",
            "--bin",
            "coremark",
            "--bin",
            "sysinit",
            "--bin",
            "console",
            "--bin",
            "input",
            "--bin",
            "shell",
            "--bin",
            "blk",
            "--target",
            RISCV_TARGET,
        ],
    ) {
        return false;
    }

    let bin = |name: &str| {
        workspace_root()
            .join(format!("target/{RISCV_TARGET}/debug/{name}"))
            .display()
            .to_string()
    };
    // Read each bin's ELF into an owned buffer, then pack. The archive name comes first, the bin
    // name second: `builder` is packed as `init` (the entry the kernel loads); the rest keep their
    // names. `sysinit`/`console`/`input`/`shell` are the interactive-shell system (parity D).
    let entries: &[(&str, &str)] = &[
        ("init", "builder"),
        ("worker", "worker"),
        ("driver", "driver"),
        ("elbench", "elbench"),
        ("coremark", "coremark"),
        ("sysinit", "sysinit"),
        ("console", "console"),
        ("input", "input"),
        ("shell", "shell"),
        ("blk", "blk"),
    ];
    let mut blobs: Vec<(&str, Vec<u8>)> = Vec::new();
    for &(archive_name, bin_name) in entries {
        match std::fs::read(bin(bin_name)) {
            Ok(b) => blobs.push((archive_name, b)),
            Err(e) => {
                eprintln!("initrd-riscv: cannot read {}: {e}", bin(bin_name));
                return false;
            }
        }
    }
    let files: Vec<(&str, &[u8])> = blobs.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let size = crickerfs::image_size(&files);
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
        eprintln!("initrd-riscv: could not build the archive");
        return false;
    }
    if let Err(e) = std::fs::write(riscv_initrd_path(), &img) {
        eprintln!("initrd-riscv: could not write {}: {e}", riscv_initrd_path());
        return false;
    }
    eprintln!(
        "wrote {} ({size} bytes): init=builder, worker=worker",
        riscv_initrd_path()
    );
    true
}

/// Pack the built user ELF into the initrd archive the kernel hands init (milestone 19f).
///
/// The initrd is a **crickerfs image**, the same format the virtio disk uses, so one parser serves
/// both the RAM archive and the disk. It holds `init` (the `hello` binary, which the kernel loads
/// and init re-enters at its remaining roles) plus the distinct binaries lifted out of hello:
/// `worker` (19f.2) and `console` (19f.3). The kernel reads the `init` entry to boot; init loads the
/// rest by name. Generated, not checked in, exactly like the disk and the flat kernel image: a blob
/// in git is a blob nobody can review.
fn mkinitrd() -> bool {
    let hello = match std::fs::read(user_elf()) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", user_elf());
            return false;
        }
    };
    let worker = match std::fs::read(bin_elf("worker")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("worker"));
            return false;
        }
    };
    let console = match std::fs::read(bin_elf("console")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("console"));
            return false;
        }
    };
    let input = match std::fs::read(bin_elf("input")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("input"));
            return false;
        }
    };
    let shell = match std::fs::read(bin_elf("shell")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("shell"));
            return false;
        }
    };
    let coremark = match std::fs::read(bin_elf("coremark")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("coremark"));
            return false;
        }
    };
    let elbench = match std::fs::read(bin_elf("elbench")) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("mkinitrd: cannot read {}: {e}", bin_elf("elbench"));
            return false;
        }
    };
    // "init" is the hello binary (the kernel loads it, init re-enters it at its remaining roles);
    // "worker", "console", "input", "shell" are the split system binaries (19f.2-5), "coremark" is
    // the compute workload (19e), and "elbench" is the EL0 microbenchmark program (primitive suite).
    // init (and the bench boot) load each by name. All are entries in the one archive.
    let files: [(&str, &[u8]); 7] = [
        ("init", &hello),
        ("worker", &worker),
        ("console", &console),
        ("input", &input),
        ("shell", &shell),
        ("coremark", &coremark),
        ("elbench", &elbench),
    ];
    let size = crickerfs::image_size(&files);
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
        eprintln!("mkinitrd: could not build the initrd archive");
        return false;
    }
    if let Err(e) = std::fs::write(initrd_path(), &img) {
        eprintln!("mkinitrd: could not write {}: {e}", initrd_path());
        return false;
    }
    true
}

/// The packed initrd archive ([`initrd_path`]) is what `scripts/qemu-runner.sh` passes to QEMU as
/// `-initrd` (milestone 19f); the raw user ELF ([`user_elf`]) is only the input `mkinitrd` packs.
///
/// **Deliberately the same road Linux's initramfs travels**, now literally an archive like theirs.
/// QEMU loads the file into RAM and writes its address into `/chosen/linux,initrd-start` in the
/// device tree; the kernel finds it there (`memory::initrd_region`, built at milestone 3 for
/// exactly this). Nothing about the contents is known to the kernel at build time, which is the
/// entire point of milestone 7c.
///
/// If `--hvf` was passed, boot under Apple's Hypervisor.framework instead of TCG.
fn maybe_hvf() {
    if std::env::args().any(|a| a == "--hvf") {
        unsafe { std::env::set_var("CRICKER_ACCEL", "hvf") };
        eprintln!("--- on the real Apple Silicon core via Hypervisor.framework ---");
    }
}

/// Where the crickerfs disk image is written.
fn disk_path() -> String {
    workspace_root()
        .join("target/crickerfs.img")
        .display()
        .to_string()
}

/// The PCIe transport's copy of the disk image, a sibling of [`disk_path`]. Two files because
/// both transports are now attached **writable** (milestone 32's write path) and QEMU's image
/// locking refuses to attach one file to two devices once either attachment can write. The
/// runner derives this name from `CRICKER_DISK`, so the two stay in lockstep.
fn disk_pci_path() -> String {
    workspace_root()
        .join("target/crickerfs-pci.img")
        .display()
        .to_string()
}

/// Build the crickerfs disk images the virtio-blk driver will read and write.
///
/// **The disk is generated, not checked in**, the same way the flat kernel image is: a binary
/// blob in git is a blob nobody can review. The contents are a couple of tiny files, written
/// through the same `crickerfs::write_image` the userspace filesystem server reads back, so the
/// format has exactly one definition.
///
/// `scratch` is the write-path tests' one-block playground: the driver writes a pattern into its
/// block and reads it back, so nothing else on the disk is ever a write target. Regenerating the
/// images here is also what makes test runs independent: whatever a previous run wrote to
/// scratch is rebuilt to zeros.
fn mkdisk() -> bool {
    let files: [(&str, &[u8]); 3] = [
        (
            "motd",
            b"cricker-os: read from a virtio disk, by a driver at EL0.\n",
        ),
        (
            "readme",
            b"this file came off a real block device through a userspace driver.\n",
        ),
        ("scratch", &[0u8; 512]),
    ];
    let size = crickerfs::image_size(&files).max(64 * 1024); // pad to a friendly size
    let mut img = std::vec![0u8; size];
    if crickerfs::write_image(&files, &mut img).is_err() {
        eprintln!("mkdisk: could not build the image");
        return false;
    }
    // One identical image per transport; see disk_pci_path for why they cannot share a file.
    for path in [disk_path(), disk_pci_path()] {
        if let Err(e) = std::fs::write(&path, &img) {
            eprintln!("mkdisk: could not write {path}: {e}");
            return false;
        }
    }
    true
}

fn user_elf() -> String {
    // ABSOLUTE, and that is not fussiness.
    //
    // Cargo runs the runner script with the working directory set to the **package** dir for
    // `cargo test` and the workspace root for `cargo run`. A relative path therefore resolved
    // under `cargo run` and silently did not under `cargo test`, so the tests booted with no
    // initrd at all and the one that noticed was the one that panicked.
    workspace_root()
        .join(format!("target/{TARGET}/{}/hello", profile_dir()))
        .display()
        .to_string()
}

/// The ELF path of a named binary the `user` package builds beside `hello` (milestone 19f.2+):
/// `worker`, `console`, and so on. `mkinitrd` packs each into the archive under that same name.
fn bin_elf(name: &str) -> String {
    workspace_root()
        .join(format!("target/{TARGET}/{}/{name}", profile_dir()))
        .display()
        .to_string()
}

/// The repo root, from the *compile-time* location of this crate, so it does not depend on
/// whatever directory cargo happens to hand us.
fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has no parent directory")
        .to_path_buf()
}

/// Host tests first, then the kernel under QEMU.
///
/// The host crates (`dtb`, `frames`) hold the pure logic and run in *milliseconds* with no
/// emulator, so they fail fast and cheap. Only once they pass is it worth spending twenty
/// seconds booting QEMU. See DECISIONS.md §7.
fn test() -> bool {
    // Tests always run under TCG. They exit via semihosting, which QEMU only intercepts in the
    // TCG path; under HVF the `hlt #0xf000` traps to the guest and the harness hangs. TCG is also
    // the right place for reproducible tests: deterministic, and identical on any host.
    unsafe { std::env::remove_var("CRICKER_ACCEL") };
    eprintln!("--- host tests (pure logic, no emulator) ---");
    // Every host crate, not just two. `paging`, `heap` and `slab` each carry real tests and
    // were silently not being run here for four milestones.
    if !cargo(&[
        "test",
        "-p",
        "abi",
        "-p",
        "caps",
        "-p",
        "crickerfs",
        "-p",
        "dtb",
        "-p",
        "elf",
        "-p",
        "frames",
        "-p",
        "paging",
        "-p",
        "pci",
        "-p",
        "ipc",
        "-p",
        "slots",
        "-p",
        "intrusive",
        "-p",
        "asid",
        "-p",
        "regions",
        "-p",
        "abi",
    ]) {
        return false;
    }
    eprintln!();
    eprintln!("--- kernel tests, aarch64 (QEMU) ---");
    if !user() || !mkdisk() {
        return false;
    }
    if !cargo(&["test", "-p", "kernel", "--target", TARGET]) {
        return false;
    }

    // The same booted kernel test suite on the second architecture (parity workstream B). The
    // portable tests (scheduler, capabilities, revocation, memory, sync) run on RISC-V's real Sv39
    // kernel; the aarch64-specific ones (the userspace-exec suite, the SGI interrupt tests, and SMP)
    // are gated to aarch64. RISC-V exits via the sifive_test finisher, same harness. See
    // notes/riscv-parity-scope.md.
    eprintln!();
    eprintln!("--- kernel tests, riscv64 (QEMU) ---");
    // The riscv userspace tests (parity C) load programs from the initrd and read the disk, so
    // build the riscv archive and point the runner at IT, not at the aarch64 archive `cargo()`
    // exports: the riscv ELF loader must never be handed aarch64 ELFs. The disk is arch-neutral
    // (a crickerfs data image) and was built by mkdisk() above.
    if !initrd_riscv() {
        return false;
    }
    unsafe { std::env::set_var("CRICKER_INITRD", riscv_initrd_path()) };
    unsafe { std::env::set_var("CRICKER_DISK", disk_path()) };
    run("cargo", &["test", "-p", "kernel", "--target", RISCV_TARGET])
}

/// The microbenchmarks (milestone 21; design/roadmap.md §21).
///
/// Two instruments:
/// - default: TCG with `-icount`, where virtual time is a deterministic function of instructions
///   executed. Counts are exact and reproducible; `--check` diffs them against
///   `bench/baseline.txt` and fails on drift, `--save` rewrites the baseline (a deliberate act,
///   committed alongside whatever changed the numbers).
/// - `--real`: HVF, natively on the host core. Real caches and TLBs, statistical numbers,
///   reported in nanoseconds, never gating.
///
/// The bench kernel never exits on its own (semihosting does not work under HVF; see `test`).
/// We own the QEMU child, watch its output for `bench: done`, and kill it: one exit mechanism
/// for both accelerators.
fn bench() -> bool {
    let check = std::env::args().any(|a| a == "--check");
    let save = std::env::args().any(|a| a == "--save");
    // `--release` builds an optimized kernel and userspace, for a fair cross-OS comparison (the debug
    // default is fine for the icount gate, whose counts are path length, but not for magnitudes next
    // to release Linux). Release changes instruction counts, so it never runs under icount and never
    // gates: it implies `--real` (HVF magnitudes only).
    let release = std::env::args().any(|a| a == "--release");
    RELEASE.store(release, Ordering::Relaxed);
    let real = release || std::env::args().any(|a| a == "--real");
    if real && (check || save) {
        let why = if release { "--release" } else { "--real" };
        eprintln!("bench: {why} numbers are statistical and never gate; no --check/--save");
        return false;
    }

    // The second architecture. RISC-V has its own path (its own kernel target, runner, and initrd,
    // no disk, no HVF); everything else -- the icount instrument, the parsing, the table, the
    // baseline gate -- is shared through run_bench. See bench_riscv.
    if std::env::args().any(|a| a == "--riscv") {
        return bench_riscv(check, save);
    }

    if !mkdisk()
        || !user()
        || !cargo_profiled(&[
            "build",
            "-p",
            "kernel",
            "--features",
            "bench",
            "--target",
            TARGET,
        ])
    {
        return false;
    }

    // Run the kernel through the same runner script as everything else, with the accelerator
    // chosen by env and, for the deterministic instrument, icount pinning virtual time to the
    // instruction stream (sleep=off: virtual time never waits for the wall clock).
    let mut cmd = Command::new(RUNNER);
    cmd.arg(kernel_elf());
    if real {
        cmd.env("CRICKER_ACCEL", "hvf");
        eprintln!("--- bench: HVF, natively on the host core (statistical; medians matter) ---");
    } else {
        cmd.env_remove("CRICKER_ACCEL");
        cmd.args(["-icount", "shift=0,sleep=off"]);
        eprintln!("--- bench: TCG + icount (deterministic instruction-clocked counts) ---");
    }
    cmd.env("CRICKER_INITRD", initrd_path());
    cmd.env("CRICKER_DISK", disk_path());

    run_bench(
        cmd,
        real,
        check,
        save,
        workspace_root().join("bench/baseline.txt"),
    )
}

/// **The RISC-V benchmark path** (parity E's follow-up). Same primitive suite, same deterministic
/// icount instrument, on the second architecture, so the tick counts are directly comparable to the
/// aarch64 ones: both are the virtual timer advancing under `-icount`, which is instruction-clocked,
/// not wall-clock. No HVF (there is no RISC-V hypervisor on this host) and no disk (the bench boot
/// runs no virtio); it just needs the riscv initrd carrying `elbench` + `coremark`. Its baseline is a
/// separate file, since the counts differ by ISA. `cargo xtask bench --riscv [--check|--save]`.
fn bench_riscv(check: bool, save: bool) -> bool {
    if !initrd_riscv()
        || !run(
            "cargo",
            &[
                "build",
                "-p",
                "kernel",
                "--features",
                "bench",
                "--target",
                RISCV_TARGET,
            ],
        )
    {
        return false;
    }

    let mut cmd = Command::new("scripts/qemu-runner-riscv.sh");
    cmd.arg(format!("target/{RISCV_TARGET}/debug/kernel"));
    // icount pins virtual time (rdtime) to the instruction stream; sleep=off so it never waits on the
    // wall clock. This is what makes the riscv counts deterministic and comparable to aarch64's.
    cmd.args(["-icount", "shift=0,sleep=off"]);
    cmd.env("CRICKER_INITRD", riscv_initrd_path());
    // One hart: a primitive benchmark measures per-core cost. With more harts, a thread that waits
    // for a spawned child leaves its hart idling in `wfi`, and under `-icount` a `wfi` jumps virtual
    // time to the next timer tick, inflating the spawn primitives to timer-quantized nonsense. The
    // single-core costs are what compare to aarch64 anyway.
    cmd.env("CRICKER_SMP", "1");
    eprintln!(
        "--- bench: riscv64, single hart, TCG + icount (deterministic instruction counts) ---"
    );

    run_bench(
        cmd,
        false,
        check,
        save,
        workspace_root().join("bench/baseline-riscv.txt"),
    )
}

/// Run a bench kernel through `cmd`, read its `bench:` lines until `bench: done`, and report the
/// table (and, off the deterministic icount instrument, save or check against `baseline`). Shared by
/// the aarch64 and RISC-V bench paths so the parsing, the table, and the regression gate are one
/// implementation. `real` only chooses the "ns are fiction" footer.
fn run_bench(
    mut cmd: Command,
    real: bool,
    check: bool,
    save: bool,
    baseline_path: std::path::PathBuf,
) -> bool {
    cmd.stdout(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("bench: failed to start the runner: {e}");
            return false;
        }
    };

    // Read lines until the guest says it is done, then kill it: it is parked in wfi and will
    // never exit by itself (deliberately; see kernel/src/bench.rs).
    use std::io::BufRead;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::io::BufReader::new(stdout);
    let mut results: Vec<(String, u64, u64)> = Vec::new();
    let mut cntfrq: u64 = 0;
    let mut done = false;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Some(rest) = line.strip_prefix("bench: ") else {
            continue;
        };
        if rest == "done" {
            done = true;
            break;
        }
        let parts: Vec<&str> = rest.split_whitespace().collect();
        match parts.as_slice() {
            ["cntfrq", hz] => cntfrq = hz.parse().unwrap_or(0),
            [name, ticks, iters] => {
                if let (Ok(t), Ok(i)) = (ticks.parse(), iters.parse()) {
                    results.push((name.to_string(), t, i));
                }
            }
            _ => {}
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    if !done {
        eprintln!("bench: QEMU ended before printing `bench: done`; no results");
        return false;
    }

    // Report. icount counts are the regression currency; ns is computed for both instruments
    // (fictional under icount, real under HVF) because a human wants a magnitude to look at.
    eprintln!();
    eprintln!(
        "{:<14} {:>12} {:>8} {:>12} {:>10}",
        "benchmark", "ticks", "iters", "ticks/iter", "ns/iter"
    );
    for (name, ticks, iters) in &results {
        // `checked_div`, not `/`: a benchmark that reports zero iterations (a skip, or a future
        // diagnostic line) must not panic the whole harness after the run already happened.
        let per = ticks.checked_div(*iters).unwrap_or(0);
        let ns = (ticks * 1_000_000_000)
            .checked_div(cntfrq)
            .and_then(|v| v.checked_div(*iters))
            .unwrap_or(0);
        eprintln!("{name:<14} {ticks:>12} {iters:>8} {per:>12} {ns:>10}");
    }
    if !real {
        eprintln!("(TCG+icount: ticks are deterministic; ns are fiction. --real for magnitudes.)");
    }

    if save {
        let mut out = String::from(
            "# bench/baseline.txt: deterministic icount tick counts (cargo xtask bench --save).
             # Updating this file is a statement that a performance change is intended and
             # understood; do it in the commit that causes the change. Checked by --check, a coarse
             # 10% tripwire (icount counts drift across builds; see notes/benchmarks.md).
",
        );
        for (name, ticks, iters) in &results {
            out.push_str(&format!(
                "{name} {ticks} {iters}
"
            ));
        }
        if let Err(e) = std::fs::write(&baseline_path, out) {
            eprintln!("bench: cannot write {}: {e}", baseline_path.display());
            return false;
        }
        eprintln!("bench: baseline saved to {}", baseline_path.display());
        return true;
    }

    if check {
        let Ok(text) = std::fs::read_to_string(&baseline_path) else {
            eprintln!(
                "bench: no baseline at {} (run `cargo xtask bench --save` first)",
                baseline_path.display()
            );
            return false;
        };
        let mut ok = true;
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            let [name, base, _iters] = parts.as_slice() else {
                continue;
            };
            let base: u64 = base.parse().unwrap_or(0);
            let Some((_, cur, _)) = results.iter().find(|(n, _, _)| n == name) else {
                eprintln!("bench: CHECK FAIL {name}: in the baseline but not in this run");
                ok = false;
                continue;
            };
            // A COARSE tripwire: 10% either way, with a small absolute floor so tiny counts do not
            // false-alarm. Not 2%: adding unrelated *live* code shifts even untouched benchmarks by
            // several percent, non-uniformly, because the compiler remakes whole-crate inlining and
            // monomorphization decisions (measured: a new bench function moved yield_switch -7% while
            // ipc_rtt went +1.8%). So icount --check catches a gross regression, "you 3x'd IPC," not
            // a 3% one; --real medians, read by a human, are the fine signal. See notes/benchmarks.md.
            let slack = (base / 10).max(64);
            let (lo, hi) = (base.saturating_sub(slack), base + slack);
            if *cur < lo || *cur > hi {
                let delta = *cur as i64 - base as i64;
                eprintln!(
                    "bench: CHECK FAIL {name}: {cur} vs baseline {base} ({delta:+} ticks,                      allowed +-{slack})"
                );
                ok = false;
            }
        }
        if ok {
            eprintln!("bench: check passed (all within 10% of baseline; coarse tripwire)");
        } else {
            eprintln!();
            eprintln!(
                "bench: a benchmark moved. If intended, rerun with --save and commit the new                  baseline WITH the change that moved it."
            );
        }
        return ok;
    }

    true
}

/// Boot the kernel with QEMU frozen and a GDB stub listening.
///
/// `-s` opens the stub on :1234, `-S` holds the CPU before the first instruction.
/// The kernel ELF carries symbols and DWARF, so GDB shows Rust source lines rather
/// than raw addresses (notes/elf.md). Point GDB at the **ELF**, even though QEMU is
/// running the flat image: the image has no symbols, and the addresses match.
///
/// This is the tool that will save you at milestone 4, when the MMU comes on and
/// `println!` stops being an option.
fn gdb() -> bool {
    if !build() {
        return false;
    }

    let elf = kernel_elf();
    eprintln!("QEMU is paused, waiting for a debugger on localhost:1234.");
    eprintln!("In another terminal:");
    eprintln!();
    eprintln!("    gdb {elf}");
    eprintln!("    (gdb) target remote :1234");
    eprintln!("    (gdb) break kernel_main");
    eprintln!("    (gdb) continue");
    eprintln!();
    eprintln!("To watch boot.s set up the stack and zero .bss:");
    eprintln!();
    eprintln!("    (gdb) break _boot");
    eprintln!("    (gdb) layout asm");
    eprintln!("    (gdb) si          # step one instruction");
    eprintln!();

    run(RUNNER, &[&elf, "-s", "-S"])
}

fn objdump() -> bool {
    if !build() {
        return false;
    }
    match llvm_tool("llvm-objdump") {
        Some(tool) => run(
            &tool,
            &[
                "-d",
                "--no-show-raw-insn",
                "-M",
                "no-aliases",
                &kernel_elf(),
            ],
        ),
        None => false,
    }
}

/// Build the flat arm64 Image and show its 64-byte header.
///
/// Useful when the header is wrong, which is a failure mode with no diagnostics at
/// all: QEMU simply falls back to treating the file as an anonymous blob, boots it,
/// and hands you a zero in x0. See notes/boot-protocol.md.
fn image() -> bool {
    if !build() {
        return false;
    }
    let Some(objcopy) = llvm_tool("llvm-objcopy") else {
        return false;
    };

    let elf = kernel_elf();
    let img = format!("{elf}.img");
    if !run(&objcopy, &["-O", "binary", &elf, &img]) {
        return false;
    }

    match std::fs::read(&img) {
        Ok(bytes) if bytes.len() >= 64 => {
            let magic = u32::from_le_bytes(bytes[56..60].try_into().unwrap());
            let text_offset = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
            let image_size = u64::from_le_bytes(bytes[16..24].try_into().unwrap());

            eprintln!("{img}  ({} bytes)", bytes.len());
            eprintln!();
            eprintln!("  text_offset  {text_offset:#x}");
            eprintln!("  image_size   {image_size:#x}");
            eprintln!(
                "  magic        {magic:#010x}  {}",
                if magic == 0x644d5241 {
                    "ok (\"ARM\\x64\")"
                } else {
                    "WRONG - QEMU will not treat this as a kernel"
                }
            );
            magic == 0x644d5241
        }
        Ok(_) => {
            eprintln!("image is shorter than its own 64-byte header");
            false
        }
        Err(e) => {
            eprintln!("cannot read {img}: {e}");
            false
        }
    }
}

/// Locate an LLVM tool inside the rustup sysroot.
///
/// These ship with the `llvm-tools` component, which `rust-toolchain.toml` pins. We
/// do NOT use the `rust-objdump` / `rust-objcopy` wrappers, because those require a
/// separate `cargo install cargo-binutils` that nothing else in the project needs,
/// and its absence produces a confusing "command not found" rather than a real error.
fn llvm_tool(name: &str) -> Option<String> {
    let sysroot = capture("rustc", &["--print", "sysroot"])?;
    let verbose = capture("rustc", &["-vV"])?;
    let host = verbose
        .lines()
        .find_map(|l| l.strip_prefix("host: "))?
        .trim();

    let path = format!("{}/lib/rustlib/{host}/bin/{name}", sysroot.trim());
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        eprintln!("cannot find {name} at {path}");
        eprintln!("the llvm-tools rustup component should provide it (see rust-toolchain.toml)");
        None
    }
}

fn capture(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    String::from_utf8(out.stdout).ok()
}

fn kernel_elf() -> String {
    format!("target/{TARGET}/{}/kernel", profile_dir())
}

fn cargo(args: &[&str]) -> bool {
    // The runner needs to know where the initrd is. Set it for every cargo invocation; the
    // script ignores it when the file is not there (which is any build before `user` exists).
    unsafe { std::env::set_var("CRICKER_INITRD", initrd_path()) };
    unsafe { std::env::set_var("CRICKER_DISK", disk_path()) };

    run("cargo", args)
}

fn run(program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or_else(|e| {
            eprintln!("failed to run {program}: {e}");
            false
        })
}
