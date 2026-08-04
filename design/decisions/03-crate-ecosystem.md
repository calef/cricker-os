# 3. Use the crate ecosystem

`aarch64-cpu` for system-register access, `tock-registers` for typed MMIO. Not
hand-rolled `asm!` and raw volatile pointer writes.

Time goes to kernel concepts (memory, scheduling, syscalls, filesystems), not to
debugging typos in ARM system-register encodings that a crate would have caught at
compile time.
