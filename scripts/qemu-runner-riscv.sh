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
    PCI_DISK="${CRICKER_DISK%.img}-pci.img"
    if [ ! -f "$PCI_DISK" ]; then
        echo "qemu-runner-riscv: $PCI_DISK does not exist (run mkdisk first; it writes both images)" >&2
        exit 1
    fi
    DISK="-global virtio-mmio.force-legacy=false -drive file=$CRICKER_DISK,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -drive file=$PCI_DISK,if=none,format=raw,id=hd1 -device virtio-blk-pci,drive=hd1,disable-legacy=on"
fi

exec qemu-system-riscv64 \
    -machine virt \
    -cpu rv64 \
    -smp "$SMP" \
    -m 128M \
    -bios default \
    -display none \
    -serial stdio \
    -kernel "$ELF" \
    $INITRD \
    $DISK \
    "$@"
