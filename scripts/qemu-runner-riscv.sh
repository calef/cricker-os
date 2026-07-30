#!/bin/sh
#
# The RISC-V QEMU runner (milestone 20). Cargo invokes this for `cargo run` and `cargo test` on the
# riscv64 target, appending the path to the ELF it just built.
#
# Simpler than the aarch64 runner: QEMU's `virt` machine boots a RISC-V ELF directly with `-kernel`,
# and `-bios default` runs OpenSBI, which initializes the machine in M-mode and hands our payload
# control in S-mode (hart id in a0, device-tree pointer in a1). There is no flat-Image / objcopy
# step, because RISC-V has no equivalent of the arm64 Image header that the aarch64 path needs.
#
# The kernel halts with `wfi` (arch::halt), so QEMU does not exit on its own. Bound any interactive
# run with scripts/qemu-bounded.sh, exactly as on aarch64 (see CLAUDE.md, "Never leave QEMU
# running"). See notes/riscv-port.md.

set -e

ELF="$1"
shift

# Four harts, matching aarch64's runner (parity workstream A). OpenSBI boots hart 0; the others sit
# in SBI HSM STOPPED state until the kernel starts them with sbi_hart_start (arch::psci_cpu_on). The
# NS16550 console is on the `virt` machine at 0x1000_0000; `-serial stdio` wires it to this terminal.
SMP="${CRICKER_SMP:-4}"

# The userspace program rides in as an initrd, exactly as on aarch64: QEMU loads the file into RAM
# and writes its address into /chosen/linux,initrd-start in the device tree, where memory::init reads
# it. Set CRICKER_INITRD to a riscv64 user ELF (or a crickerfs archive) to hand it to the kernel; the
# milestone-20 boot loads and runs it at U-mode. Unset, the kernel prints "no -initrd" and moves on.
INITRD=""
if [ -n "$CRICKER_INITRD" ]; then
    INITRD="-initrd $CRICKER_INITRD"
fi

# Attach the crickerfs image as a virtio-mmio block device (parity C), exactly as the aarch64 runner
# does: `if=none` + `-device virtio-blk-device` puts a block device in one of the `virt` machine's
# virtio-mmio slots (0x1000_1000..), which virtio::find_block_device probes. force-legacy=false picks
# modern virtio (version 2). Without a disk the kernel simply finds no block device and says so.
#
# A SET CRICKER_DISK naming a missing file is an error, not a silent no-op; see the same check in
# qemu-runner.sh for why (it very likely manufactured the false parity-C blocker).
DISK=""
if [ -n "$CRICKER_DISK" ] && [ ! -f "$CRICKER_DISK" ]; then
    echo "qemu-runner-riscv: CRICKER_DISK=$CRICKER_DISK does not exist (run mkdisk first)" >&2
    exit 1
fi
if [ -n "$CRICKER_DISK" ]; then
    # Two transports, two image files: virtio-mmio (hd0, the parity-C transport) and
    # virtio-blk-pci (hd1, the PCIe transport). Both are WRITABLE (milestone 32: the
    # write-capable block path), and QEMU's image locking refuses to open one file for two
    # devices once either can write, so mkdisk writes an identical sibling image for the PCI
    # side. A missing sibling is a stale build; fail loud, same rule as the main image.
    # disable-legacy=on makes the PCI function MODERN (device id 0x1042): without it QEMU offers a
    # transitional device (0x1001), whose legacy register layout we deliberately do not drive.
    #
    # iommu_platform=on puts the PCI disk BEHIND the RISC-V IOMMU (milestone 16b, the twin of the
    # aarch64 SMMU): the device emits IOVAs the IOMMU translates through the domain the kernel built,
    # and offers VIRTIO_F_ACCESS_PLATFORM so the driver negotiates it. Without it QEMU's virtio
    # device bypasses the IOMMU silently, and the confinement test fails loudly. The mmio disk has no
    # IOMMU in front of it and takes no such flag.
    PCI_DISK="${CRICKER_DISK%.img}-pci.img"
    if [ ! -f "$PCI_DISK" ]; then
        echo "qemu-runner-riscv: $PCI_DISK does not exist (run mkdisk first; it writes both images)" >&2
        exit 1
    fi
    # The RedoxFS image (milestone 32 phase 2), the SECOND mmio block device. Placed BEFORE the
    # crickerfs disk on the command line on purpose: QEMU's virt assigns virtio-mmio devices to
    # slots in REVERSE command-line order, and the kernel finds block devices by ascending slot, so
    # the crickerfs disk must be the LAST mmio device to keep slot 0 (find_block_device -> crickerfs,
    # the phase-1 tests), leaving RedoxFS at slot 1 (find_block_device_n(1) -> RedoxFS). Soft:
    # present only when the test flow built it. Created host-side by tools/redoxfs-host.
    REDOXFS_DISK="${CRICKER_DISK%.img}-redoxfs.img"
    REDOXFS_MMIO=""
    if [ -f "$REDOXFS_DISK" ]; then
        REDOXFS_MMIO="-drive file=$REDOXFS_DISK,if=none,format=raw,id=hd2 -device virtio-blk-device,drive=hd2"
    fi
    DISK="-global virtio-mmio.force-legacy=false $REDOXFS_MMIO -drive file=$CRICKER_DISK,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -drive file=$PCI_DISK,if=none,format=raw,id=hd1 -device virtio-blk-pci,drive=hd1,disable-legacy=on,iommu_platform=on"
fi

# A virtio-net NIC on QEMU user-mode (slirp) networking when CRICKER_NET is set (milestone 30), the
# twin of the aarch64 runner's block. slirp NATs the guest with a built-in DHCP server (10.0.2.0/24)
# and DNS resolver (10.0.2.3), and needs no host setup. Two NICs mirror the two disks: the mmio NIC
# (net0) has no IOMMU in front of it, the PCI NIC (net1) sits behind the RISC-V IOMMU
# (iommu_platform=on). guestfwd adds a deterministic TCP echo peer at 10.0.2.9:7777 (piped to
# /bin/cat) for the TCP round-trip gate; nothing outlives QEMU. See the aarch64 runner for detail.
GUESTFWD="guestfwd=tcp:10.0.2.9:7777-cmd:/bin/cat"
NET=""
if [ -n "$CRICKER_NET" ]; then
    NET="-netdev user,id=net0,$GUESTFWD -device virtio-net-device,netdev=net0 -netdev user,id=net1,$GUESTFWD -device virtio-net-pci,netdev=net1,disable-legacy=on,iommu_platform=on"
fi

# A virtio-gpu when CRICKER_GPU is set (milestone 29), the twin of the aarch64 runner's block. PCIe
# only (there is no mmio GPU on this machine either), modern (disable-legacy=on, device id 0x1050),
# and behind the RISC-V IOMMU (iommu_platform=on). That last flag matters more for the GPU than for
# the disk: a virtio-gpu's backing addresses ride in a device-level command payload rather than in a
# descriptor, so the transport's shadow-ring validator never sees them and the IOMMU is the only thing
# that bounds them. See notes/framebuffer-contract.md and the aarch64 runner.
GPU=""
if [ -n "$CRICKER_GPU" ]; then
    GPU="-device virtio-gpu-pci,disable-legacy=on,iommu_platform=on"
fi

# A QEMU monitor on a unix socket when CRICKER_GPU_MON names one (milestone 29), the twin of the
# aarch64 runner's block: `screendump` over it writes a PPM of the scanout even with -display none,
# which is how the scanout gets proven rather than only the framebuffer. The path must stay under the
# OS's 104-byte unix-socket limit, which is why xtask puts it in /tmp. See gpu_shot in xtask.
MON=""
if [ -n "$CRICKER_GPU_MON" ]; then
    MON="-monitor unix:$CRICKER_GPU_MON,server,nowait"
fi

# The RISC-V IOMMU (milestone 16b): the ratified v1.0.1 IOMMU as a PCI function (riscv-iommu-pci,
# Red Hat 1b36:0014) in front of the PCIe bus. Present on every boot for parity with the aarch64
# SMMU that is always on the machine; the kernel enumerates it, places its BAR, and brings it up
# (pci::init_iommu). Idle when no PCI disk is attached. Placed on the command line before the disk
# so it fronts the bus the virtio-blk-pci device joins.
IOMMU="-device riscv-iommu-pci"

exec qemu-system-riscv64 \
    -machine virt \
    -cpu rv64 \
    -smp "$SMP" \
    -m 128M \
    -bios default \
    -display none \
    -serial stdio \
    -kernel "$ELF" \
    $IOMMU \
    $INITRD \
    $DISK \
    $NET \
    $GPU \
    $MON \
    "$@"
