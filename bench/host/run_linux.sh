#!/bin/sh
# Same-hardware Linux run of the primitive suite (milestone 25). Cross-compiles the static Linux
# bench (rust-lld, no C toolchain), downloads an Alpine aarch64 kernel (generated, not checked in,
# like the disk and initrd), builds a one-file initramfs, and boots it under QEMU-HVF on the SAME
# M-series core and the SAME virtualization tier as cricker-os. Prints the null-syscall and IPC
# round-trip medians, then the guest powers itself off.
#
# Needs: rustup target add aarch64-unknown-linux-musl; qemu-system-aarch64; network (once, for the
# kernel). Run: sh bench/host/run_linux.sh
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
WORK=${WORK:-/tmp/cricker-hostbench}
mkdir -p "$WORK"

KERNEL="$WORK/vmlinuz-virt"
if [ ! -f "$KERNEL" ]; then
    echo "fetching an aarch64 Linux kernel..."
    curl -sSL -o "$KERNEL" \
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/vmlinuz-virt"
fi

rustc -O --target aarch64-unknown-linux-musl -C target-feature=+crt-static \
    -C linker=rust-lld -C link-self-contained=yes "$HERE/linux_all.rs" -o "$WORK/init"

rm -rf "$WORK/iroot" && mkdir "$WORK/iroot" && cp "$WORK/init" "$WORK/iroot/init"
( cd "$WORK/iroot" && find . | cpio -o -H newc 2>/dev/null ) > "$WORK/initramfs.cpio"

"$ROOT/scripts/qemu-bounded.sh" 40 \
    qemu-system-aarch64 -M virt,accel=hvf,gic-version=2 -cpu host -m 1024 \
    -kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" \
    -append "console=ttyAMA0 rdinit=/init panic=1 quiet loglevel=0" \
    -display none -serial stdio 2>&1 | grep -E '^linux'
