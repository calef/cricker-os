# The UART

**U**niversal **A**synchronous **R**eceiver/**T**ransmitter.

Hardware with exactly one job: **convert between parallel and serial.**

The CPU thinks in bytes: eight bits at once, in parallel. A wire carries one bit at a time.
The UART sits between them. Write a byte to one of its registers and it shifts those eight
bits out onto a single wire, one after another. At the far end another UART catches them one
at a time and reassembles a byte.

That is the whole device.

## The interesting word is "Asynchronous"

**There is no clock wire.** Sender and receiver never share a clock signal. (This is what
separates UART from SPI and I²C, which both have a dedicated clock line.)

So how does the receiver know when a bit starts?

They **agree on the speed in advance** (the baud rate) and each keeps its own local clock.
The line idles high. To send a byte the sender pulls it low for exactly one bit-time: the
**start bit**. That falling edge is the only synchronization that ever happens. The receiver
sees the edge and then samples the line at the agreed interval, eight times. One or two
**stop bits** (line back high) guarantee a known idle before the next start bit.

```
idle   start │ d0  d1  d2  d3  d4  d5  d6  d7 │ stop   idle
─────┐       ┌───┐       ┌───────┐   ┌───────────────────
     └───────┘   └───────┘       └───┘
     ^
     the only sync in the entire protocol
```

Two consequences:

**Baud mismatch produces garbage.** The receiver samples at the wrong instants and reads
bits out of the middle of transitions. Those mojibake characters on a serial console are
*always* this.

**You pay 10 bits to send 8.** Start + 8 data + stop. So 115200 baud is ~11,520 bytes/sec,
not 14,400.

**8N1** (8 data bits, No parity, 1 stop bit) is the near-universal configuration, and it's
what our `init()` sets up.

## Our code, line by line

From `kernel/src/drivers/pl011.rs`:

**`DR`, the data register.** `r.DR.set(byte as u32)`. Write a byte here and the UART shifts
it out. That one line is the entire act of printing a character.

**`FR`, the flag register, and its `TXFF` bit.** "Transmit FIFO Full." The UART holds a
small hardware queue (16 entries on a PL011) so the CPU can dump several bytes and walk away
rather than waiting for each to physically shift out.

That matters more than it sounds. **At 115200 baud one byte takes ~87 microseconds to
transmit.** In CPU terms that is an eternity. Without the FIFO the processor would spend
nearly all its time standing around.

`write_byte` spins until there's room:

```rust
while r.FR.is_set(FR::TXFF) {
    core::hint::spin_loop();
}
r.DR.set(byte as u32);
```

That is a **polled** driver: the CPU busy-waits. Simplest thing that works, and fine for a
boot console.

> TODO (milestone 5): once we have interrupts, the UART can tell us when it has room instead
> of us asking.

**`IBRD` / `FBRD`, the baud divisors.** The UART is fed a clock and you divide it down to get
the bit rate: integer part and fractional part.

```
divisor = 48_000_000 / (16 * 115_200) = 26.0416...
IBRD = 26,  FBRD = round(0.0416... * 64) = 3
```

**QEMU ignores both of these completely.** There is no wire and no timing being simulated;
QEMU takes the byte and puts it on your terminal instantly. We set them anyway because a real
PL011 needs them, and the Raspberry Pi will.

## Why the UART is every kernel's first device

Because it needs nothing.

No memory allocation. No interrupts. No DMA. No driver framework. No MMU. Write to one
address, a character appears. It works before there is a heap, before there is a page table,
before there is a scheduler.

Which means it is **the one thing that still works when everything else is broken.** That is
precisely the situation you are in when you need it most. A kernel panic on an otherwise
wedged machine still reaches the serial console, because the serial console is the last thing
standing.

Every kernel on earth learns to do this first, for that reason.

## Trivia that bites later

**PL011** is ARM's UART design (part of their PrimeCell peripheral family). The classic PC one
is the **16550**. Different registers, same idea.

**The Pi has two UARTs**: a real PL011 and a cut-down "mini UART." Which one lands on the GPIO
pins depends on config, and getting this wrong is a classic first-port frustration. See
[target-hardware.md](target-hardware.md).

**Voltage levels are a separate layer from the protocol.** A Pi's GPIO speaks 3.3V TTL. The
old DB9 port on a 1990s PC used RS-232, which swings ±12V *and inverts the signal*. Wire one
straight to the other and you destroy the Pi. The ~$10 USB-TTL cable is a level converter,
which is why you buy one instead of soldering a DB9.

---

*Add to this file as new serial concepts come up.*
