#!/bin/sh
#
# The QEMU runner. Cargo invokes this for `cargo run` and `cargo test`, appending the
# path to the ELF it just built.
#
# Why this exists rather than a plain `qemu-system-aarch64 ... -kernel` line in
# .cargo/config.toml: QEMU only follows the **Linux arm64 boot protocol** (and
# therefore only hands us a device tree pointer in x0) for a flat `Image`. Given an
# ELF, it takes a bare-metal path instead and populates no registers at all.
#
# So we strip the ELF down to a flat binary. The arm64 Image header lives at byte 0
# of it (kernel/src/arch/aarch64/image_header.s), which is what makes QEMU recognize
# the blob as a kernel.
#
# Tests boot through exactly the same path as `cargo xtask run` does, deliberately.
# A test harness that exercises a different boot path than the real thing is testing
# a fiction.
#
# See notes/boot-protocol.md.

set -e

ELF="$1"
shift

# llvm-objcopy ships with the `llvm-tools` rustup component, which rust-toolchain.toml
# pins. We resolve it out of the sysroot rather than expecting it on PATH, because
# `rust-objcopy` needs a separate `cargo install cargo-binutils` and we'd rather not
# add a setup step that fails confusingly six months from now.
SYSROOT="$(rustc --print sysroot)"
HOST="$(rustc -vV | awk '/^host:/{print $2}')"
OBJCOPY="$SYSROOT/lib/rustlib/$HOST/bin/llvm-objcopy"

if [ ! -x "$OBJCOPY" ]; then
    echo "qemu-runner: cannot find llvm-objcopy at $OBJCOPY" >&2
    echo "qemu-runner: is the llvm-tools component installed? (rust-toolchain.toml pins it)" >&2
    exit 1
fi

IMG="$ELF.img"
"$OBJCOPY" -O binary "$ELF" "$IMG"

# The userspace program rides in as an initrd, exactly the way Linux gets its initramfs: QEMU
# loads the file into RAM and writes the address into /chosen/linux,initrd-start in the device
# tree it generates. The kernel finds it there. Nothing about the binary is compiled into the
# kernel. See notes/elf.md and kernel/src/memory.rs.
INITRD=""
if [ -n "$CRICKER_INITRD" ] && [ -f "$CRICKER_INITRD" ]; then
    INITRD="-initrd $CRICKER_INITRD"
fi

# Attach the crickerfs image as a virtio-blk device. `if=none` + `-device virtio-blk-device`
# gives us a virtio-mmio block device on the `virt` machine, which is what the userspace driver
# probes for and reads. Without a disk, the kernel simply finds no block device and says so.
#
# A SET CRICKER_DISK naming a missing file is an error, not a silent no-op. The old behaviour
# (quietly booting diskless) had the kernel truthfully reporting "no block device", which reads
# like a machine fact when it is actually a build-order mistake; it very likely produced the
# false "riscv virt has no mmio disk" record in notes/riscv-parity-scope.md.
DISK=""
if [ -n "$CRICKER_DISK" ] && [ ! -f "$CRICKER_DISK" ]; then
    echo "qemu-runner: CRICKER_DISK=$CRICKER_DISK does not exist (run mkdisk first)" >&2
    exit 1
fi
if [ -n "$CRICKER_DISK" ]; then
    # force-legacy=false selects MODERN virtio-mmio (version 2), whose split register interface
    # (separate physical addresses for the descriptor table and the two rings) is the current
    # design and the one worth learning. Without it QEMU gives legacy (version 1), a different
    # and older queue layout.
    #
    # Both transports are attached WRITABLE (milestone 32: the write-capable block path), which
    # is why there are two image files rather than one attached twice: QEMU's image locking
    # refuses to open one file for two devices once either can write. mkdisk writes the sibling
    # alongside the main image with identical contents; missing sibling = stale build, fail loud
    # (the readonly-era silent-degradation lesson, see the CRICKER_DISK check above).
    #
    # iommu_platform=on is what puts the PCI disk BEHIND the SMMU (milestone 16b): the device then
    # emits IOVAs the SMMU translates through the domain the kernel built, and offers
    # VIRTIO_F_ACCESS_PLATFORM so the driver knows it. WITHOUT this flag QEMU's virtio device
    # bypasses the SMMU silently, and the confinement test (which asserts the hardware faults an
    # out-of-region DMA) fails loudly rather than passing on a fiction. The mmio disk (hd0) has no
    # IOMMU in front of it on this machine, so it takes no such flag.
    PCI_DISK="${CRICKER_DISK%.img}-pci.img"
    if [ ! -f "$PCI_DISK" ]; then
        echo "qemu-runner: $PCI_DISK does not exist (run mkdisk first; it writes both images)" >&2
        exit 1
    fi
    # The RedoxFS image (milestone 32 phase 2), the SECOND mmio block device. It is placed on the
    # command line BEFORE the crickerfs disk on purpose: QEMU's `virt` assigns virtio-mmio devices
    # to slots in REVERSE command-line order (the last -device gets the lowest-address slot), and
    # the kernel finds block devices by ascending slot. So the crickerfs disk must be the LAST mmio
    # device to keep slot 0 (find_block_device -> crickerfs, the phase-1 driver tests), which leaves
    # the RedoxFS disk at slot 1 (find_block_device_n(1) -> RedoxFS, the FS server's block server).
    # Getting this backwards silently hands the phase-1 tests the wrong disk; that is exactly the
    # bug this ordering fixes. Soft: present only when the test flow built it (cargo xtask test),
    # absent for a plain interactive boot, which just skips the FS-server test. Created host-side by
    # tools/redoxfs-host; the server only ever opens it.
    REDOXFS_DISK="${CRICKER_DISK%.img}-redoxfs.img"
    REDOXFS_MMIO=""
    if [ -f "$REDOXFS_DISK" ]; then
        REDOXFS_MMIO="-drive file=$REDOXFS_DISK,if=none,format=raw,id=hd2 -device virtio-blk-device,drive=hd2"
    fi
    DISK="-global virtio-mmio.force-legacy=false $REDOXFS_MMIO -drive file=$CRICKER_DISK,if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 -drive file=$PCI_DISK,if=none,format=raw,id=hd1 -device virtio-blk-pci,drive=hd1,disable-legacy=on,iommu_platform=on"
fi

# Attach a virtio-net NIC on QEMU user-mode (slirp) networking when CRICKER_NET is set (milestone
# 30). slirp NATs the guest with a built-in DHCP server (10.0.2.0/24, gateway 10.0.2.2) and DNS
# resolver (10.0.2.3), and needs no host setup, so the net tests run with zero privilege. Two NICs
# mirror the two disks: the mmio NIC (net0) has no IOMMU in front of it, the PCI NIC (net1) sits
# behind the SMMU (iommu_platform=on), the same hardware confinement the PCI disk gets. There is no
# image file to fail loud on here; the manufactured-fact hazard (CRICKER_NET set but no NIC
# enumerated) is caught by the net test, which asserts a NIC is present rather than skipping.
#
# guestfwd adds a deterministic TCP echo peer at 10.0.2.9:7777 inside slirp: a connection to it is
# piped to a fresh `/bin/cat`, so the TCP round-trip gate (connect, send, recv the echo, close) runs
# with zero host setup and nothing outlives QEMU. Verified against QEMU 11.0.2. Each slirp instance
# is its own network, so both NICs can use the same virtual address without conflict.
GUESTFWD="guestfwd=tcp:10.0.2.9:7777-cmd:/bin/cat"
NET=""
if [ -n "$CRICKER_NET" ]; then
    NET="-netdev user,id=net0,$GUESTFWD -device virtio-net-device,netdev=net0 -netdev user,id=net1,$GUESTFWD -device virtio-net-pci,netdev=net1,disable-legacy=on,iommu_platform=on"
fi

# shellcheck disable=SC2086  # $INITRD, $DISK and $NET are deliberately word-split or empty
# CPU and accelerator.
#
# By default we run under TCG (QEMU translates every aarch64 instruction), with an emulated
# cortex-a72. That is deterministic and runs identically on any host, which is what the test
# harness wants.
#
# Set CRICKER_ACCEL=hvf to run under Apple's Hypervisor.framework instead: HVF puts the kernel on
# the real Apple Silicon core at guest EL1, using the hardware virtualization the chip already
# has. The coincidence that makes this a flag and not a port is that the host and the guest are the
# same ISA (aarch64). Two consequences:
#
#   - HVF runs the PHYSICAL core, so `-cpu host` is mandatory; you cannot ask for an emulated a72.
#   - gic-version is PINNED to 2, so a future QEMU default cannot swap in a GICv3 our driver does
#     not speak. QEMU emulates the GIC either way (Apple cores use their own AIC natively) and
#     injects interrupts through HVF, so the MMIO GICv2 driver keeps working.
if [ "$CRICKER_ACCEL" = "hvf" ]; then
    MACHINE="virt,accel=hvf,gic-version=2"
    CPU="host"
else
    # iommu=smmuv3 puts an SMMUv3 in front of the PCIe root complex (milestone 16b). It is on the
    # TCG path only, on purpose: the IOMMU is a correctness feature proven by the test suite (which
    # runs under TCG), not a benchmark axis, and smmuv3 emulation alongside HVF acceleration is the
    # fragile combination. The device tree then carries an `smmuv3@...` node (memory::smmu_region
    # finds it) and an identity iommu-map for the bus. A plain boot without a PCI disk still gets the
    # SMMU; it just has nothing to confine.
    MACHINE="virt,gic-version=2,iommu=smmuv3"
    CPU="cortex-a72"
fi

# Number of cores. Four by default, matching cpu::MAX_CPUS and the SMP tests (§11). QEMU brings
# up core 0 running; the kernel starts the rest itself via PSCI CPU_ON (see smp.rs).
SMP="${CRICKER_SMP:-4}"

exec qemu-system-aarch64 \
    -machine "$MACHINE" \
    -cpu "$CPU" \
    -smp "$SMP" \
    -display none \
    -serial stdio \
    -semihosting \
    -kernel "$IMG" \
    $INITRD \
    $DISK \
    $NET \
    "$@"
