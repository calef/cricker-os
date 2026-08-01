/*
 * The swappable component's arithmetic, in C (milestone 23, DECISIONS §39 and §31).
 *
 * This is the whole of what `c_swappable` does differently from `rust_swappable`: one function,
   computing one
 * digest, in a language with no bounds checks and no borrow checker. It holds no capability and
 * makes no syscall, because it has never seen one -- the Rust shell around it holds every
 * capability and does every `svc`/`ecall` (DECISIONS §31, notes/c-seam.md).
 *
 * It exists to make the milestone's claim as strong as it goes: the replacement server that a
 * running client did not notice was not a rebuild of the same program, it was a different
 * implementation in a different language, and the client checked its answers.
 *
 * The definition must match `swap::digest` in user/src/swap.rs bit for bit. FNV-1a over the eight
 * little-endian bytes of the sequence number: deliberately trivial arithmetic, so that "the two
 * implementations agree" is a statement about the seam and not about anyone's cleverness.
 */

#include <stdint.h>

uint64_t cx_digest(uint64_t seq) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (int i = 0; i < 8; i++) {
        h ^= (seq >> (8 * i)) & 0xff;
        h *= 0x100000001b3ULL;
    }
    return h;
}
