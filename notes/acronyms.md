# Acronyms

Everything this project has thrown at you, expanded. If a term has a note of its own, it's
linked.

## Interrupts

| | Expands to | |
|---|---|---|
| **IRQ** | **Interrupt ReQuest** | The name is the wire. A device *requests* an interrupt by raising a signal line; the CPU notices between two instructions and diverts. One of the four exception kinds. |
| **FIQ** | **Fast Interrupt ReQuest** | A second, higher-priority line. ARM gave it *banked* registers so the handler could start without saving as much. Now mostly the secure world's. We treat it as fatal. |
| **GIC** | **Generic Interrupt Controller** | The CPU has **one** IRQ line; a machine has hundreds of sources. The GIC decides which is asserting, whether it's allowed through, which core gets it, and in what order. "Generic" because ARM standardized *one* controller for all ARM SoCs — before it, every board needed a different interrupt driver. See [interrupts.md](interrupts.md). |
| **GICD** | GIC **Distributor** | One per machine. Which core gets what, and what's enabled at all. |
| **GICC** | GIC **CPU interface** | One per core, *banked* (every core sees its own at the same address). |
| **PMR** | **Priority Mask Register** | A threshold: deliver only interrupts whose priority is **strictly less** than this. And lower value = higher priority, so `0xff` = everything, `0` = nothing. |
| **IAR** | **Interrupt Acknowledge Register** | Reading it **takes** the interrupt. It has a side effect. |
| **EOIR** | **End Of Interrupt Register** | "I'm done." Until it's written, no further interrupt of equal or lower priority is delivered. |
| **INTID** | **INTerrupt ID** | The number naming a source. 1023 means *spurious*. |
| **SGI** | **Software Generated Interrupt** | 0–15. One core kicking another. SMP bringup, TLB shootdown. |
| **PPI** | **Private Peripheral Interrupt** | 16–31. Per-core. **The timer is one**, and has to be. |
| **SPI** | **Shared Peripheral Interrupt** | 32+. The UART, the disk. Any core may take them. |
| **NMI** | **Non-Maskable Interrupt** | An interrupt you cannot turn off. aarch64 has no true one; Linux fakes it with PMR. |

> ⚠ **SPI collides.** *Shared Peripheral Interrupt* (here) and *Serial Peripheral Interface*
> (a bus, like I²C) are completely unrelated and both common in embedded. Context is the only
> disambiguator.

**Two levels of masking**, and they do different jobs:

| | Where | Granularity |
|---|---|---|
| `DAIF.I` | the **CPU** | all-or-nothing. What `IrqSafeMutex` uses. |
| `GICC_PMR` | the **GIC** | selective, by priority. What Linux's "pseudo-NMI" uses to keep a watchdog alive inside a critical section. |

## aarch64: the CPU

| | Expands to | |
|---|---|---|
| **ISA** | **Instruction Set Architecture** | The contract between software and silicon. [aarch64.md](aarch64.md) |
| **EL0–EL3** | **Exception Level** | The privilege model. EL0 = user, **EL1 = us**, EL2 = hypervisor, EL3 = secure firmware. |
| **PC / SP / LR** | Program Counter / Stack Pointer / Link Register | `LR` is `x30`. [registers.md](registers.md), [stack.md](stack.md) |
| **DAIF** | **D**ebug, **A**bort (SError), **I**RQ, **F**IQ | The four interrupt mask bits. `1` = masked, which is backwards from how you read a flag called `I`. |
| **AAPCS64** | **Procedure Call Standard for the Arm Architecture**, 64-bit | The calling convention. `x0`–`x7` are arguments, `x0` is the return value, `x19`–`x28` are callee-saved. It is why `extern "C"` exists on `kernel_main`. |
| **SVC** | **Supervisor Call** | The syscall instruction. Milestone 7. |
| **BRK** | Breakpoint | A deliberate trap. `ELR` points **at** it, not past it. |
| **WFI / WFE** | **Wait For Interrupt** / **Wait For Event** | Idle the core. **Use `wfi`**: QEMU sleeps the host thread on it and merely spins on `wfe`. [qemu.md](qemu.md) |
| **DSB / DMB / ISB** | **Data Synchronization / Data Memory / Instruction Synchronization Barrier** | Memory ordering. `ISB` is what makes a system-register write take effect for the *next* instruction. |
| **PSCI** | **Power State Coordination Interface** | The standard way to start and stop cores. How SMP bringup will work. |
| **SMP** | **Symmetric MultiProcessing** | More than one core, all equal. DECISIONS §6: not yet. |
| **LSE** | **Large System Extensions** | ARMv8.1 single-instruction atomics (`CAS`, `LDADD`). Our Cortex-A72 lacks them. [design/fat-binaries.md](../design/fat-binaries.md) |

## aarch64: system registers

The naming is systematic once you see it: `<thing>_EL<level>`.

| | Expands to | |
|---|---|---|
| **SCTLR** | **System Control Register** | The master switch. Bit 0 is "is the MMU on?" |
| **VBAR** | **Vector Base Address Register** | Where the exception vector table lives. [exceptions.md](exceptions.md) |
| **ESR** | **Exception Syndrome Register** | *What went wrong.* Bits 31:26 are the Exception Class. **Meaningless for an IRQ** — it describes a *synchronous* exception. |
| **FAR** | **Fault Address Register** | *Which address* faulted. Only meaningful for aborts. |
| **ELR** | **Exception Link Register** | Where the interrupted code resumes. `eret` reloads `PC` from it. |
| **SPSR** | **Saved Program Status Register** | The processor state at the moment of the exception, **including the exception level**. Which is how `eret` drops to EL0. |
| **TTBR0/1** | **Translation Table Base Register** | The page tables. **TTBR0 = userspace, TTBR1 = the kernel.** [higher-half.md](higher-half.md) |
| **TCR** | **Translation Control Register** | How to walk them. Address size, granule, which TTBRs are live. |
| **MAIR** | **Memory Attribute Indirection Register** | Eight slots saying what "memory type N" *means*. A descriptor says "look up slot N"; this says what N is. |
| **MPIDR** | **MultiProcessor Affinity Register** | Which core am I? `boot.s` reads it to park cores 1..n. |
| **CNTFRQ** | **CouNTer FReQuency** | How fast the counter ticks. Set by firmware. |
| **CNTPCT** | **CouNTer Physical CounT** | The counter itself. Monotonic. **This is what `Instant` is made of.** |
| **CNTP_CVAL** | Counter Physical **Compare VALue** | An **absolute** deadline. Use this. |
| **CNTP_TVAL** | Counter Physical **Timer VALue** | A **relative** countdown. A trap: re-arming with it in the handler makes the clock run slow, permanently. [interrupts.md](interrupts.md) |

## Memory

| | Expands to | |
|---|---|---|
| **MMU** | **Memory Management Unit** | Translates virtual → physical, per page, in hardware. [mmu.md](mmu.md) |
| **TLB** | **Translation Lookaside Buffer** | The CPU's cache of translations. **Change a mapping without invalidating it and the CPU keeps using the old one** — memory reads back as the previous owner's data. [page-tables.md](page-tables.md) |
| **BBM** | **Break-Before-Make** | Valid → **invalid** → invalidate → valid. Changing a valid descriptor straight to a different valid one can raise a TLB conflict abort. |
| **ASID** | **Address Space ID** | Tags TLB entries with which process they belong to, so a context switch needn't flush the whole TLB. Milestone 7. |
| **AF** | **Access Flag** | Descriptor bit 10. **Forget it and the first access to the page faults.** The single most common aarch64 paging bug. |
| **AP** | **Access Permissions** | Descriptor bits 7:6. Bit 7 = read-only, bit 6 = userspace may touch it. |
| **PXN / UXN** | **Privileged / Unprivileged eXecute Never** | Two separate bits. PXN on user pages is not paranoia: without it, a kernel bug that jumps into a user page runs **user-controlled instructions at EL1**. |
| **SH** | **SHareability** | How far cache coherency must extend. |
| **W^X** | **Write XOR eXecute** | No page is both writable and executable. It's how a buffer overflow becomes code execution. Enforced by construction: there is no `Flags::writable_and_executable()`. |
| **MMIO** | **Memory-Mapped I/O** | Talking to a device by reading and writing magic addresses. **Must be mapped as device memory**, or the CPU may cache it, reorder it, merge writes, and *speculatively read it* — and reading a FIFO register **consumes the byte**. |
| **DMA** | **Direct Memory Access** | A device reading and writing RAM without the CPU. It uses **physical** addresses, with no MMU to hide a scattered buffer, which is why we need `alloc_contiguous`. Milestone 8. |
| **SLUB** | the Linux slab allocator | Objects of one size per cache, so a freed object is immediately reusable and **coalescing becomes unnecessary rather than fast**. [heap.md](heap.md) |

## Files, tools, boot

| | Expands to | |
|---|---|---|
| **ELF** | **Executable and Linkable Format** | The container. Two indexes over the same bytes: *sections* for the linker, *segments* for the loader. [elf.md](elf.md) |
| **DTB / FDT** | **Device Tree Blob** / **Flattened Device Tree** | The machine describing itself. **Everything in it is big-endian.** [device-tree.md](device-tree.md) |
| **UART** | **Universal Asynchronous Receiver/Transmitter** | The serial port. "Asynchronous" means **there is no clock wire**. [uart.md](uart.md) |
| **PL011** | ARM **PrimeCell** part 011 | The specific UART design. The classic PC one is the 16550. |
| **QEMU** | **Quick EMUlator** | A computer made of software. [qemu.md](qemu.md) |
| **LLVM** | originally *Low Level Virtual Machine*; **officially nothing now** | It isn't a virtual machine and never really was. rustc is a *frontend* that emits LLVM IR. [llvm.md](llvm.md) |
| **IR** | **Intermediate Representation** | The universal middle format that turns M×N compilers into M+N. |
| **ABI** | **Application Binary Interface** | The contract between compiled things: where arguments go, how the stack is laid out. |
| **API** | Application *Programming* Interface | The source-level one. The distinction matters: you can change an API and keep the ABI, and vice versa. |

---

*Add to this file whenever a new one shows up.*
