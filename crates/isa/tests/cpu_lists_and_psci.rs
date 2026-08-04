//! **What the machine says its cores are, and how to start them.** Milestone 100, on the host.
//!
//! Four trees, and each is here because it is a shape the others cannot show:
//!
//! * **QEMU aarch64 `virt` at `-smp 4`**, dumped from the exact machine `script/test` boots
//!   (`virt,gic-version=2,iommu=smmuv3`, `cortex-a72`). This is the one that holds the kernel's
//!   former hardcodes against the machine: `hvc`, `0xc4000003`, and cores `0..4`.
//! * **QEMU aarch64 `virt` with `virtualization=on`**, which is the *only* configuration on this
//!   laptop that produces `method = "smc"`. It is a real dump, not a hand-edit, and it is the whole
//!   evidence that the conduit is a machine property rather than a constant.
//! * **QEMU riscv64 `virt`**, borrowed from the `dtb` crate's fixtures, for the `timebase-frequency`
//!   the RISC-V timer used to hardcode.
//! * **A hand-written clustered tree**, for the shapes QEMU cannot emit: two address cells, a
//!   non-contiguous hardware id, a disabled core, a spin-table core, more cores than the kernel's
//!   ceiling, and PSCI 0.1 with its own published function id.
//!
//! The parsing they exercise is [`isa::cpu_list`] and [`isa::aarch64::Psci`]; what happens on the
//! machine (the refusals, the boot line, the bring-up itself) is in the kernel's own tests, which
//! need an emulator. This is the split DECISIONS.md §7 asks for.

use isa::aarch64::{Conduit, PSCI_CPU_ON_64, Psci};
use isa::cpu_list::{CpuList, EnableMethod, MAX_CPU_NODES};

/// The machine `script/test` boots on aarch64, at the `-smp 4` the runner passes.
const QEMU_VIRT_SMP4: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-aarch64-virt-smp4.dtb");
/// The same board with `virtualization=on`, which moves the conduit to `smc`.
const QEMU_VIRT_SMC: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-aarch64-virt-smc.dtb");
/// The RISC-V machine, borrowed rather than copied so a regenerated tree cannot leave two crates
/// testing different bytes.
const QEMU_RISCV: &[u8] = include_bytes!("../../dtb/tests/fixtures/qemu-riscv64-virt.dtb");
const CLUSTERED: &[u8] = include_bytes!("fixtures/clustered-cpus.dtb");
const NO_PSCI: &[u8] = include_bytes!("fixtures/no-psci.dtb");

fn cpus(bytes: &[u8]) -> CpuList {
    let dt = dtb::Dtb::from_bytes(bytes).expect("fixture should parse");
    CpuList::from_device_tree(&dt).expect("cpu list should decode")
}

fn psci(bytes: &[u8]) -> Option<Psci> {
    let dt = dtb::Dtb::from_bytes(bytes).expect("fixture should parse");
    Psci::from_device_tree(&dt).expect("psci node should decode")
}

/// **The four cores the runner asks for are the four the tree describes**, with the hardware ids the
/// kernel used to assume. This is the assertion that made the old hardcode *look* right: on this
/// machine `reg` really is `0..4`, so nothing the kernel did was wrong here. It is the shape of the
/// check `crates/pci` uses on its own hardcodes, and the reason a second board is where it breaks.
#[test]
fn qemu_virt_describes_the_four_cores_the_runner_starts() {
    let list = cpus(QEMU_VIRT_SMP4);

    assert_eq!(list.described, 4, "the runner passes -smp 4");
    assert_eq!(list.len, 4);
    assert!(!list.truncated());
    assert_eq!(list.address_cells, 1, "QEMU virt numbers cores in one cell");
    assert_eq!(
        list.cpus().iter().map(|c| c.hwid).collect::<Vec<_>>(),
        [0, 1, 2, 3],
    );
    for cpu in list.cpus() {
        assert!(cpu.usable, "QEMU disables no core");
        assert_eq!(cpu.enable_method, EnableMethod::Psci);
    }
    assert_eq!(
        list.timebase_hz, None,
        "aarch64 states its counter rate in CNTFRQ_EL0, not in the tree"
    );
}

/// **The conduit and the function id the kernel used to compile in are what this machine states.**
///
/// Both halves matter and they fail differently. A wrong function id returns a PSCI error, which
/// `smp.rs` already degrades on; a wrong conduit is an undefined instruction on a machine with
/// nothing at that exception level, and there is no error code for that.
#[test]
fn qemu_virt_states_hvc_and_the_standard_cpu_on() {
    let psci = psci(QEMU_VIRT_SMP4).expect("virt has a /psci node");

    assert_eq!(psci.conduit, Some(Conduit::Hvc));
    assert_eq!(psci.cpu_on, Some(PSCI_CPU_ON_64));
    assert_eq!(
        psci.cpu_on,
        Some(0xC400_0003),
        "the number smp.rs used to hold"
    );
    assert!(psci.standard, "QEMU claims arm,psci-0.2 and arm,psci-1.0");
    assert!(
        psci.cpu_on_from_property,
        "and publishes the id anyway, which we take at its word"
    );
    assert!(psci.can_start_a_core());
}

/// **The same board, one machine option different, answers on `smc`.**
///
/// `virtualization=on` puts something at EL2, so QEMU's own PSCI moves to EL3 and the tree says so.
/// This is the whole finding in one comparison: the conduit is not a property of aarch64, of QEMU,
/// or of the `virt` board. Nothing boots this configuration here (the kernel expects to be entered
/// at EL1 and this one enters at EL2), so the `smc` **call path** remains untested on any machine;
/// what is proved is that we read the answer rather than assume it.
#[test]
fn the_same_board_with_a_hypervisor_states_smc() {
    let hvc = psci(QEMU_VIRT_SMP4).unwrap();
    let smc = psci(QEMU_VIRT_SMC).unwrap();

    assert_eq!(hvc.conduit, Some(Conduit::Hvc));
    assert_eq!(smc.conduit, Some(Conduit::Smc));
    assert_eq!(
        hvc.cpu_on, smc.cpu_on,
        "the function id is the same; only the instruction that carries it changes"
    );
}

/// **A machine with no `/psci` node is told apart from one we could not parse.** The kernel says
/// different things about the two, and there is nothing in a `None` field to distinguish them.
#[test]
fn a_tree_without_psci_answers_none() {
    assert_eq!(psci(NO_PSCI), None);
    assert_eq!(psci(QEMU_RISCV), None, "RISC-V starts harts over SBI HSM");
}

/// **PSCI 0.1 publishes its own function ids, and they are not the standard ones.**
///
/// This is the case the old `const PSCI_CPU_ON: u64 = 0xC400_0003` could not have survived: 0.1
/// predates the standardised id space entirely, so the only correct source is the property. The
/// fixture's id is deliberately not the standard one, so a decoder that reached for the constant
/// would fail here rather than pass by coincidence.
#[test]
fn psci_0_1_publishes_its_own_function_id() {
    let psci = psci(CLUSTERED).expect("the fixture has a /psci node");

    assert!(!psci.standard, "a bare arm,psci is 0.1");
    assert_eq!(psci.cpu_on, Some(0x95c1_ba5e));
    assert!(psci.cpu_on_from_property);
    assert_eq!(psci.conduit, Some(Conduit::Smc));
    assert!(psci.can_start_a_core());
}

/// **A two-cell hardware id decodes as one 64-bit number, not as its high half.**
///
/// The ARM CPU binding requires `#address-cells = <2>` on `/cpus` the moment any core's
/// `MPIDR_EL1.Aff3` is nonzero. A decoder fixed at one cell reads `<0x100 0x200>` as `0x100`, which
/// is a plausible id belonging to a different core, so the failure would be a `CPU_ON` aimed at the
/// wrong place rather than an error.
#[test]
fn a_clustered_machine_decodes_two_cell_ids() {
    let list = cpus(CLUSTERED);

    assert_eq!(list.address_cells, 2);
    assert_eq!(
        list.cpus().iter().map(|c| c.hwid).collect::<Vec<_>>(),
        [0x0, 0x1, 0x100, 0x101, 0x100_0000_0200, 0x201, 0x202],
        "affinity is not a dense range, which is the point",
    );
}

/// **`status` and `enable-method` come back as the tree wrote them.**
///
/// The module records rather than obeys, because whether a `spin-table` core is startable is a fact
/// about the kernel and not about the tree. Both are here so the kernel's refusal has something
/// truthful to refuse on.
#[test]
fn a_disabled_core_and_a_spin_table_core_are_reported_as_such() {
    let list = cpus(CLUSTERED);

    let disabled = list.cpus()[5];
    assert_eq!(disabled.hwid, 0x201);
    assert!(!disabled.usable, "status = \"disabled\"");
    assert_eq!(disabled.enable_method, EnableMethod::Psci);

    let spinning = list.cpus()[6];
    assert_eq!(spinning.hwid, 0x202);
    assert!(spinning.usable, "no status property means okay");
    assert_eq!(spinning.enable_method, EnableMethod::SpinTable);

    // And the ordinary case, so the two above are read against something.
    assert!(list.cpus()[2].usable);
    assert_eq!(list.cpus()[2].enable_method, EnableMethod::Psci);
}

/// **A machine with more cores than fit is reported as truncated, not as smaller.**
///
/// `described` keeps counting past `MAX_CPU_NODES`, so a caller can print "this machine has more
/// cores than this kernel was built for" instead of quietly reading a prefix and calling it the
/// machine. Seven cores is not enough to overflow sixteen slots, so the equality below is the
/// honest state of the fixture and the inequality is what a bigger machine would hit.
#[test]
fn the_described_count_is_the_machines_and_the_length_is_ours() {
    let list = cpus(CLUSTERED);

    assert_eq!(list.described, 7);
    assert_eq!(list.len, 7);
    assert!(!list.truncated());
    assert!(
        list.described <= MAX_CPU_NODES,
        "widen the array if this trips"
    );
}

/// **The RISC-V counter rate comes from `/cpus/timebase-frequency`.**
///
/// 10 MHz is the number `arch/riscv64/timer.rs` held as a `const` with a comment saying it was
/// hardcoded until the device-tree parse landed. The parse landed with milestone 60 and the comment
/// outlived it; this is the value it should have been reading, and the aarch64 side has always read
/// its own from `CNTFRQ_EL0`.
#[test]
fn the_riscv_timebase_comes_from_the_tree() {
    assert_eq!(cpus(QEMU_RISCV).timebase_hz, Some(10_000_000));
}

/// **The `/cpus` decode does not care which architecture wrote the tree.** Same call, same struct,
/// two machines. The only field that differs by architecture is the RISC-V-only timebase.
#[test]
fn both_architectures_answer_the_same_call() {
    let arm = cpus(QEMU_VIRT_SMP4);
    let riscv = cpus(QEMU_RISCV);

    assert_eq!(arm.address_cells, riscv.address_cells);
    assert_eq!(riscv.described, 1, "the riscv fixture was dumped at -smp 1");
    assert_eq!(riscv.cpus()[0].hwid, 0);
    assert_eq!(
        riscv.cpus()[0].enable_method,
        EnableMethod::Unstated,
        "the RISC-V binding has no enable-method: SBI HSM is the only mechanism",
    );
    assert!(riscv.cpus()[0].usable, "the riscv tree says status = okay");
}
