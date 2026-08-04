# 2. Primary target: QEMU `virt`, Raspberry Pi as a later port

**Status: DECIDED.**

The QEMU `virt` machine has a PL011 UART, a GIC interrupt controller, and virtio
devices, all well-specified. Boots in a second, debuggable with GDB, scriptable in
tests.

The Raspberry Pi port is a deliberate later milestone, not an afterthought. It is the
moment the hardware abstraction layer gets tested for real, and it will reveal exactly
which assumptions were secretly QEMU-shaped.
