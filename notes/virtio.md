# virtio-blk, driven from userspace

Milestone 9. A real block device, driven by an unprivileged process at EL0, reading a file off a
disk. It builds on everything: EL0 (7a), address spaces and shared mappings (7c/8), the capability
syscall surface (7d), IPC (7e), the userspace-driver pattern (8), and interrupt-as-message (9a).

## What virtio is, briefly

virtio is a standard for **paravirtualized** devices: the "hardware" is cooperating software (QEMU,
or a hypervisor) that agreed on an interface designed to be efficient across the guest/host
boundary, rather than emulating some real chip register-for-register. A virtio device is driven
through three things:

- **MMIO registers** for setup and notification (feature negotiation, queue addresses, "go").
- **A virtqueue** in shared memory: a descriptor table and two rings, through which the driver
  hands the device buffers and the device hands them back.
- **An interrupt**, raised when the device has finished with a buffer.

QEMU's `virt` machine exposes virtio over **virtio-mmio**: 32 device slots at `0x0a000000`, 0x200
bytes apart, with interrupts SPI 16..47 (INTID 48..79). We force **modern** virtio (version 2),
whose register interface gives separate physical addresses for the descriptor table and each ring.

## Who does what: the kernel enumerates, the driver operates

The kernel reads three standardized registers of each slot (magic, device-id, version) to find the
block device and route it to a driver. **That is bus enumeration** — the same thing firmware does
walking a PCI bus — and it is the smallest amount of virtio knowledge that lets the kernel say
"the block device is in slot 31, its interrupt is INTID 79." It does not set up a queue, negotiate
a feature, or move a byte. Everything else is the driver's, at EL0.

The kernel hands the driver, and nothing more:

- **the device's registers**, mapped as user device memory (`Flags::user_device`). A slot is not
  page-aligned, so the kernel maps the containing page and passes the in-page offset;
- **a DMA page**, mapped read/write, *and its physical address* (in a register), because
  descriptors speak physical addresses and a process only knows virtual ones;
- **an `Irq` capability** for INTID 79;
- **an endpoint** to report what it read.

## DMA: the one place a process needs a physical address

Everywhere else, a process deals only in virtual addresses; the kernel and the MMU keep it that
way on purpose. A device is the exception. The virtio device is *not* behind our MMU (there is no
IOMMU on QEMU `virt`), so when the driver puts a buffer address in a descriptor, it must be the
**physical** address the device will actually read and write. The driver cannot compute that from a
virtual address, so the kernel hands it the DMA region's physical base at spawn, and the driver
works out physical addresses as `dma_phys + offset`.

On real hardware there is a second concern: cache coherence between the CPU and a DMA-capable
device. QEMU's DMA is coherent, so we get away with just compiler/CPU ordering (`dmb ish`) around
publishing descriptors and reading results. A real driver would add cache maintenance. The note is
here so the gap is on the record.

## The virtqueue, and a read

The driver lays a descriptor table and two rings out in its DMA page and tells the device their
physical addresses. To read a block it builds a **three-descriptor chain**:

```
  desc[0]  header   { type=READ, sector }   device READS it     -> next desc[1]
  desc[1]  data     512 bytes               device WRITES it    -> next desc[2]
  desc[2]  status   1 byte                   device WRITES it    (end)
```

then publishes the head index in the **available ring**, bumps the ring's index, and pokes
`QueueNotify`. The device does the read, writes the data and a status byte, puts the head on the
**used ring**, and raises its interrupt. The driver waits for that interrupt (below), then checks
the status byte is 0 (OK) and reads the 512 bytes the device left in the buffer.

## The completion arrives as a message

The driver does not poll. It `WAIT`s on its `Irq` capability (9a). When the device finishes and
raises INTID 79, the kernel masks the line, turns it into a notification, and wakes the driver. The
driver quiets the device (reads its `InterruptStatus`, writes `InterruptACK`), then `ACK`s its `Irq`
capability to re-enable the line. This is the whole reason 9a came first: a userspace driver's
completion path *is* interrupt-as-message.

## The bug this milestone flushed out: a scheduler with no idle

The first time the driver `WAIT`ed for its interrupt with nothing else to run, the kernel
**panicked**: "every thread is blocked on IPC: a deadlock." It was wrong. A thread blocked on a
device interrupt is not deadlocked; it is waiting for an event that will arrive. The scheduler had
no **idle thread**, so a moment where every thread was legitimately waiting for I/O looked
identical to a genuine deadlock.

The fix is the idle thread every real kernel has: a thread whose entire body is `wfi`, kept out of
the ready queue and scheduled only when nothing else can run. It parks the CPU until an interrupt
makes something runnable. See notes on the scheduler.

## And a real race it also flushed out

While chasing the above, the test suite started failing **intermittently** with a lock-order
violation: `schedule()` taking the run-queue lock while it was already held. The cause was a
one-instruction window that had been latent since milestone 6:

> `schedule()`'s "nobody else is ready, keep running" path called
> `interrupts::restore(was_enabled)` **while still holding the scheduler lock**, then returned
> (which is what actually dropped the lock). Between the restore and the return, interrupts were
> **on** and the lock was **held**. A timer firing in that window re-entered `schedule()` and tried
> to take the lock again.

It was invisible until milestone 9 added enough scheduler churn (more threads, the idle thread, a
driver blocking and waking on interrupts) to hit the window. The fix: never restore interrupts or
return from inside the locked block. All paths now leave through one point, the lock drops, and
*then* interrupts are restored, once. Recorded here because it is exactly the kind of "cheap to
follow, expensive to retrofit" ordering rule DECISIONS §9 is about, and we got it wrong for three
milestones.

## What a driver bug costs, again

The virtio driver is a process. A wrong descriptor, a bad DMA address, a mis-parsed superblock:
each faults the driver alone (7a's `user_fault`), the kernel reports it on its debug UART, and the
machine keeps running. Milestone 8 made that true for the console; milestone 9 makes it true for
storage.

## What it prints

```
      a driver at EL0 read the file 'motd' off a virtio disk,
      through a crickerfs superblock it parsed itself,
      woken by the device's interrupt delivered as a message.
      the kernel issued no virtio command and touched no DMA.
```

## crickerfs

The filesystem is [`crates/crickerfs`](../crates/crickerfs/src/lib.rs): read-only, flat, fixed
everything, host-tested, with one definition of the format shared by the disk-building tool and the
reader. Block 0 is a superblock (magic, a small directory of name → start-block/length); files
follow, block-aligned. The driver reads block 0, walks the directory to find `motd`, and reads its
data block. It is deliberately the least thing that is still a real filesystem, in the same spirit
as `crates/elf`: milestone 9 is about drivers and block I/O, not filesystem design.
