//! **coremark**: a CoreMark-derived compute benchmark, in portable `no_std` Rust (milestone 19e).
//!
//! A faithful-in-spirit port of EEMBC's CoreMark: the same three work items that make a CoreMark
//! iteration (a linked-list sort, a small-matrix multiply-accumulate, and a state machine over a
//! byte buffer), each folded into a CRC so the compiler cannot optimize the work away and a run
//! self-validates. It is **not** an EEMBC-certified CoreMark: the certified score requires the
//! unmodified reference C. This is a Rust reimplementation whose value is that the *identical source*
//! compiles for nife, macOS, and Linux, so the compute comparison across those three is one
//! program on three OSs (notes/benchmarks.md).
//!
//! `run(iters)` does `iters` core iterations over a fixed, seeded dataset and returns a CRC. Same
//! `iters` always yields the same CRC (a test pins it), which is the correctness check; the *score*
//! is `iters` divided by the wall-clock the caller measures around it. The dataset is a few kilobytes
//! (CoreMark's small-memory profile), so it stays cache-resident and fits a small userspace stack.
//!
//! # Examples
//!
//! How a caller scores a machine, and the two things about this crate that make the score mean
//! anything. The CRC is the run's self-validation: it is the same on nife, macOS and Linux, so a
//! comparison across the three is a comparison of *time* rather than of two different computations.
//!
//! ```
//! use std::time::Instant;
//!
//! use coremark::{PINNED_CRC_64, PINNED_ITERS, run};
//!
//! let start = Instant::now();
//! let crc = run(PINNED_ITERS);
//! let seconds = start.elapsed().as_secs_f64();
//!
//! // The correctness check comes first: a run whose CRC is wrong has not measured the benchmark,
//! // whatever its time says.
//! assert_eq!(crc, PINNED_CRC_64);
//!
//! // And the score, which is not in this crate because the clock is not either.
//! let score = f64::from(PINNED_ITERS) / seconds;
//! assert!(score > 0.0);
//! ```
//!
//! The CRC is what stops the optimiser deleting the work, so the property worth asserting is that it
//! **depends on the iteration count**. A compiler that hoisted any of the three work items out of the
//! loop would give the same answer for every count:
//!
//! ```
//! use coremark::run;
//!
//! assert_eq!(run(50), run(50)); // deterministic in the count
//! assert_ne!(run(50), run(51)); // and the work actually accumulates
//! ```
//!
//! Name: ratified 2026-08-01 (calef, milestone 63) as a proper noun: EEMBC's industry benchmark.
//! Recorded there as explicitly not a counter-example to the agent-noun rule that made `elbench`
//! into `os_primitives_benchmarker`.

#![no_std]

/// One byte through a CRC16 (CoreMark's polynomial and bit order). Every work item folds its result
/// here; the dependency chain is what stops the optimizer from deleting the computation.
fn crc8(mut crc: u16, mut data: u8) -> u16 {
    for _ in 0..8 {
        let x16 = ((data & 1) as u16) ^ (crc & 1);
        data >>= 1;
        if x16 == 1 {
            crc ^= 0x4002;
            crc >>= 1;
            crc |= 0x8000;
        } else {
            crc >>= 1;
            crc &= 0x7fff;
        }
    }
    crc
}

/// One 16-bit value through the CRC, low byte first (CoreMark's `crcu16`).
fn crc16(crc: u16, val: u16) -> u16 {
    let crc = crc8(crc, val as u8);
    crc8(crc, (val >> 8) as u8)
}

const LIST_SIZE: usize = 100;
const MATRIX_DIM: usize = 16;
const FSM_LEN: usize = 256;

/// A deterministic 16-bit generator (xorshift), seeded per run so a run is reproducible.
struct Rng(u32);
impl Rng {
    fn next16(&mut self) -> u16 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x & 0xffff) as u16
    }
}

/// **Work item 1: list processing.** Fill a list, sort it by a derived key with an in-place
/// insertion sort (each pass touches memory the way CoreMark's list walk does), then fold the
/// sorted data into the CRC. Insertion sort is O(n^2), which is the point: it is real, data-
/// dependent work whose result the CRC depends on.
fn list_work(crc: u16, rng: &mut Rng) -> u16 {
    let mut data = [0u16; LIST_SIZE];
    for d in data.iter_mut() {
        *d = rng.next16();
    }
    // Sort by the low byte (the "key"), ties broken by the value, exactly a stable insertion sort.
    for i in 1..LIST_SIZE {
        let v = data[i];
        let key = v & 0xff;
        let mut j = i;
        while j > 0
            && ((data[j - 1] & 0xff) > key || ((data[j - 1] & 0xff) == key && data[j - 1] > v))
        {
            data[j] = data[j - 1];
            j -= 1;
        }
        data[j] = v;
    }
    let mut c = crc;
    for &d in data.iter() {
        c = crc16(c, d);
    }
    c
}

/// **Work item 2: matrix multiply-accumulate.** Build two NxN matrices, compute `C = A*B` with a
/// scaling shift (CoreMark's `matrix_mul_matrix_bitextract` shape), and fold C into the CRC. This is
/// the arithmetic-heavy, cache-friendly item.
fn matrix_work(crc: u16, rng: &mut Rng) -> u16 {
    let mut a = [[0i32; MATRIX_DIM]; MATRIX_DIM];
    let mut b = [[0i32; MATRIX_DIM]; MATRIX_DIM];
    for (row_a, row_b) in a.iter_mut().zip(b.iter_mut()) {
        for (va, vb) in row_a.iter_mut().zip(row_b.iter_mut()) {
            *va = (rng.next16() as i16 as i32) & 0xff;
            *vb = (rng.next16() as i16 as i32) & 0xff;
        }
    }
    let mut c = crc;
    // Index-based on purpose: `b[k][j]` is a column access (j fixed, k striding), which the iterator
    // form cannot express, and the triple `i,j,k` loop is the readable shape of a matrix multiply.
    #[allow(clippy::needless_range_loop)]
    for i in 0..MATRIX_DIM {
        for j in 0..MATRIX_DIM {
            let mut acc: i32 = 0;
            for k in 0..MATRIX_DIM {
                // The bit-extract CoreMark does: multiply, then fold two bit-slices back in, so the
                // result depends on more than the low bits and the work is not a trivial dot product.
                let p = a[i][k] * b[k][j];
                acc += (p >> 2) & 0x3fff;
            }
            c = crc16(c, (acc & 0xffff) as u16);
        }
    }
    c
}

/// **Work item 3: state machine.** Generate a buffer of number-like bytes and run CoreMark's
/// classifier over it (start / integer / float / scientific / invalid), counting the state at each
/// byte and folding the counts into the CRC. Branch-heavy, data-dependent control flow.
fn fsm_work(crc: u16, rng: &mut Rng) -> u16 {
    // A byte alphabet weighted toward digits, signs, dots, and exponents, so the classifier moves
    // through its states rather than sitting in INVALID.
    const ALPHABET: &[u8; 16] = b"0123456789+-.eE ";
    let mut buf = [0u8; FSM_LEN];
    for x in buf.iter_mut() {
        *x = ALPHABET[(rng.next16() as usize) & 15];
    }

    let mut counts = [0u32; 5]; // one per state
    let mut state: usize = 0; // START
    for &ch in buf.iter() {
        state = match state {
            // START
            0 => match ch {
                b'0'..=b'9' => 1, // -> INT
                b'+' | b'-' | b'.' => 1,
                _ => 4, // -> INVALID
            },
            // INT
            1 => match ch {
                b'0'..=b'9' => 1,
                b'.' => 2,        // -> FLOAT
                b'e' | b'E' => 3, // -> SCI
                _ => 0,           // -> START (delimiter)
            },
            // FLOAT
            2 => match ch {
                b'0'..=b'9' => 2,
                b'e' | b'E' => 3,
                _ => 0,
            },
            // SCI
            3 => match ch {
                b'0'..=b'9' => 3,
                b'+' | b'-' => 3,
                _ => 0,
            },
            // INVALID
            _ => match ch {
                b' ' => 0,
                _ => 4,
            },
        };
        counts[state] += 1;
    }
    let mut c = crc;
    for &n in counts.iter() {
        c = crc16(c, (n & 0xffff) as u16);
        c = crc16(c, (n >> 16) as u16);
    }
    c
}

/// One core iteration: all three work items over a freshly seeded dataset, folded into `crc`. Each
/// iteration reseeds from the running CRC, so iterations are not independent and none can be hoisted.
fn iteration(crc: u16) -> u16 {
    let mut rng = Rng(0x1000_0000 ^ (crc as u32).wrapping_mul(0x9E37_79B1) | 1);
    let c = list_work(crc, &mut rng);
    let c = matrix_work(c, &mut rng);
    fsm_work(c, &mut rng)
}

/// Run `iters` core iterations and return the final CRC. Deterministic in `iters`: the same count
/// always yields the same CRC, which is the run's self-validation. The score is `iters / seconds`,
/// where the caller measures the seconds around this call.
pub fn run(iters: u32) -> u16 {
    let mut crc: u16 = 0;
    for _ in 0..iters {
        crc = iteration(crc);
    }
    crc
}

/// The CRC of `run(64)`, pinned so every OS and every build agrees. Verified by the host test below
/// and re-asserted by the nife workload; a mismatch anywhere means the compute diverged.
pub const PINNED_CRC_64: u16 = 0x7954;

/// The iteration count the nife workload runs, and the value `PINNED_CRC_64` is the checksum
/// of. Kept beside the pin so the two never drift apart.
pub const PINNED_ITERS: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    /// The CRC is deterministic in the iteration count: the correctness anchor. If a refactor
    /// changes the computation, this value changes and the test fails, which is exactly what a
    /// benchmark's self-check is for. (The value itself is arbitrary; it is pinned, not derived.)
    #[test]
    fn a_run_is_deterministic() {
        let a = run(50);
        let b = run(50);
        assert_eq!(a, b, "same iteration count must give the same CRC");
        // More iterations must change the CRC (the work actually accumulates).
        assert_ne!(
            run(50),
            run(51),
            "the CRC must depend on the iteration count"
        );
    }

    /// Pin the CRC for a fixed small run, so a cross-OS port (or an accidental logic change) is
    /// caught. The kernel workload asserts this same value, so all three OSs agree bit for bit.
    #[test]
    fn the_pinned_run_matches() {
        assert_eq!(run(64), PINNED_CRC_64);
    }
}
