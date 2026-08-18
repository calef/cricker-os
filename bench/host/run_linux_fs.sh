#!/bin/sh
# Same-hardware, same-tier Linux/ext4 run of milestone 38's filesystem throughput phases.
#
# It boots the SAME Alpine kernel run_linux.sh uses, on the SAME `virt,accel=hvf` machine with the
# SAME `-cpu host`, `-m 256M`, `-smp 4` and the SAME `virtio-blk-device` on a raw host image file
# that scripts/qemu-runner-aarch64.sh gives nife's own bench boot. That matching is the whole point:
# the two filesystems then differ in the filesystem and in nothing under it.
#
# Needs: rustup target add aarch64-unknown-linux-musl; qemu-system-aarch64; network (once, for the
# kernel); and podman, which calef already runs. Podman is here because macOS ships neither an ext4
# formatter nor an unsquashfs, both of which this needs and both of which are one `apk add` away
# inside a container; `brew install e2fsprogs squashfs` would serve as well.
#
# BUGS: the scratch image is built under the repository's own `target/` rather than in $TMPDIR,
# because the podman machine only shares paths under the user's home directory and /tmp is not one
# of them. If you point WORK elsewhere, point it somewhere podman can see.
#
# Run: sh bench/host/run_linux_fs.sh
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
WORK=${WORK:-$ROOT/target/fsbench}
mkdir -p "$WORK"

KERNEL="$WORK/vmlinuz-virt"
if [ ! -f "$KERNEL" ]; then
    echo "fetching an aarch64 Linux kernel..."
    curl -sSL -o "$KERNEL" \
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/vmlinuz-virt"
fi

# **The Alpine `virt` kernel does not have ext4 built in**, which is the one thing about this that
# is not obvious: it is a netboot kernel and keeps its filesystems in `modloop-virt`, a squashfs.
# `mount("ext4")` from a one-file initramfs therefore answers ENODEV, which is what the first
# version of this script did. So the five modules ext4 needs are lifted out of that squashfs here
# and loaded by the bench itself (`finit_module`), which keeps the guest a single binary and keeps
# the boot free of a second disk.
MODLOOP="$WORK/modloop-virt"
if [ ! -f "$MODLOOP" ]; then
    echo "fetching Alpine's module squashfs..."
    curl -sSL -o "$MODLOOP" \
        "https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/aarch64/netboot/modloop-virt"
fi

# **The container runs once, not once per run.** It makes a *pristine* ext4 image and unpacks the
# modules; each run then starts from a byte-for-byte copy of that image, which is both faster and a
# stricter reset than re-running mkfs would be. macOS has no mke2fs and no unsquashfs and both live
# one `apk add` away, which is why podman is here at all; `brew install e2fsprogs squashfs` would do
# the same job.
PRISTINE="$WORK/ext4.pristine"
IMG="$WORK/ext4.img"
if [ ! -f "$PRISTINE" ] || [ ! -f "$WORK/mods/ext4.ko" ]; then
    rm -f "$PRISTINE"
    dd if=/dev/zero of="$PRISTINE" bs=1m count=64 2>/dev/null
    rm -rf "$WORK/mods" && mkdir -p "$WORK/mods"
    if command -v podman >/dev/null 2>&1; then
        podman run --rm -v "$WORK:/w" docker.io/library/alpine:latest sh -c '
            apk add --no-cache e2fsprogs squashfs-tools >/dev/null 2>&1
            mkfs.ext4 -q -F /w/ext4.pristine
            unsquashfs -d /m -f /w/modloop-virt >/dev/null 2>&1
            for m in drivers/virtio/virtio_mmio drivers/block/virtio_blk \
                     lib/crc16 crypto/crc32c_generic fs/mbcache fs/jbd2/jbd2 fs/ext4/ext4; do
                cp "/m/modules/"*"/kernel/$m.ko" /w/mods/ 2>/dev/null || true
            done'
    else
        echo "no podman: cannot make an ext4 filesystem or unpack the ext4 module" >&2
        exit 1
    fi
    [ -f "$WORK/mods/ext4.ko" ] || { echo "did not find ext4.ko in the modloop" >&2; exit 1; }
fi

# A fresh filesystem every run. Fresh matters: the sequential-write phase is the file's creation,
# and a filesystem that already holds last run's copy is not being asked the same question. The
# raw-device phase also scribbles over the first megabyte, so a reused image would not even mount.
if [ -z "$KEEP_EXT4" ] || [ ! -f "$IMG" ]; then
    rm -f "$IMG" && cp "$PRISTINE" "$IMG"
fi

rustc -O --edition 2021 --target aarch64-unknown-linux-musl -C target-feature=+crt-static \
    -C linker=rust-lld -C link-self-contained=yes "$HERE/linux_fs.rs" -o "$WORK/init"

rm -rf "$WORK/iroot" && mkdir "$WORK/iroot" && cp "$WORK/init" "$WORK/iroot/init"
mkdir "$WORK/iroot/mods" && cp "$WORK"/mods/*.ko "$WORK/iroot/mods/"
( cd "$WORK/iroot" && find . | cpio -o -H newc 2>/dev/null ) > "$WORK/initramfs.cpio"

# `scripts/qemu-bounded.sh`, not `timeout(1)` (which macOS does not have) and never `perl -e alarm`
# (QEMU installs its own SIGALRM handler and swallows it; CLAUDE.md records the eleven leaked
# emulators that taught us).
"$ROOT/scripts/qemu-bounded.sh" 300 \
    qemu-system-aarch64 -M virt,accel=hvf,gic-version=2 -cpu host -m 256M -smp 4 \
    -kernel "$KERNEL" -initrd "$WORK/initramfs.cpio" \
    -append "console=ttyAMA0 rdinit=/init panic=1 quiet loglevel=0" \
    -global virtio-mmio.force-legacy=false \
    -drive file="$IMG",if=none,format=raw,id=hd0 -device virtio-blk-device,drive=hd0 \
    -display none -serial stdio 2>&1 | grep -Ev '^\[|^$'
