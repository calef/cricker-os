# 9. A virtio-blk driver at EL0, and an interrupt becomes a message

**Status: BUILT.**

Backfilled 2026-08-03 from history (milestone 76). Two commits on 2026-07-14:

- `0eb187c` **9a**: an interrupt becomes a message, "the foundational piece of milestone 9, and
  the thing a userspace driver cannot live without." A routed INTID is masked at the GIC the
  instant it fires (the line is level-triggered; leave it enabled and it storms), delivered as a
  notification, and re-enabled when the driver ACKs its Irq capability. The kernel's half does
  nothing device-specific.
- `7a10c7b` **9**: "a driver at EL0 read the file 'motd' off a virtio disk, through a nifefs
  superblock it parsed itself, woken by the device's interrupt delivered as a message. The kernel
  issued no virtio command and touched no DMA." The kernel's remaining role is bus enumeration,
  three standardized registers per virtio-mmio slot.

Plan against outcome, honestly: the revised row (`491f23d`) promised "virtio-blk in userspace + a
filesystem server." The driver landed; a separate filesystem server did not exist at this
milestone. The driver parsed nifefs itself, and a filesystem behind its own capability server
is milestone 32's work, eighteen days and a real filesystem later.
