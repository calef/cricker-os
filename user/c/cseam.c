/*
 * cseam.c -- the foreign component (milestone 36, DECISIONS §30).
 *
 * Memory-unsafe C, compiled by bare-metal clang, linked into a Rust user_rt shell
 * (user/src/cshim.rs) and confined by the kernel like any other process. It is the
 * smallest thing that can prove the seam: one honest function that does real work
 * over a granted buffer, and two functions that misbehave on purpose.
 *
 * THE RULES THIS FILE LIVES UNDER, and they are enforced by what is absent here:
 *
 *   - It makes NO syscalls. There is no `svc`, no `ecall`, no inline asm, and no
 *     declaration of anything that would trap. Everything it can do to the outside
 *     world it does through the pointer it was handed.
 *   - It holds NO capabilities. A capability is a slot number in a cspace, and this
 *     file never sees one. It could not name one if it wanted to.
 *   - It gets plain buffers. `unsigned char *` and a length, nothing else.
 *
 * That is what makes the confinement claim tight: the worst this file can do is
 * corrupt memory inside a grant the Rust shell already had, and it cannot widen the
 * kernel's syscall surface by so much as one method, because it cannot reach it.
 *
 * The five libc symbols below are the whole libc. They are defined in Rust, in
 * cshim.rs; there is no libc on this target, and nothing behind these needs POSIX
 * semantics. See notes/c-seam.md for how that list was discovered (by letting the
 * linker fail and reading the error, not by guessing).
 */

#include <stddef.h>
#include <stdint.h>

void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
size_t strlen(const char *s);
void *malloc(size_t n);
void free(void *p);

/*
 * The grant's layout. Mirrored in user/src/cshim.rs and user/src/cseam.rs; the two
 * sides agree by convention, because a C ABI cannot express a shared struct without
 * one of the two languages generating it, and generating bindings for one page of
 * bytes would be more machinery than the agreement is worth. The comment is the
 * contract, the same way every cricker-os program's capability slots are.
 *
 *   [0        .. 2048)   input:  a NUL-terminated ASCII string, written by the shell's parent
 *   [2048     .. 2052)   output: the checksum, little-endian
 *   [2052     .. len)    output: the transformed string, NUL-terminated
 */
#define CSEAM_IN_OFF 0u
#define CSEAM_OUT_OFF 2048u

/* The byte the misbehaving functions write. Chosen so it differs from the witness
 * patterns at every offset they target, so "the witness is unchanged" cannot be true
 * by coincidence. */
#define CSEAM_MARK 0xC0

/* How far past the end of the grant `cseam_wild` reaches: one page, so it lands beyond
 * the read-only witness page that sits immediately after the grant and hits a virtual
 * address this process has no mapping for at all. */
#define CSEAM_WILD_SKIP 4096u

/*
 * The honest work: uppercase the input string, checksum it, write both back.
 *
 * Deliberately shaped to need the whole libc list. `strlen` finds the input, `malloc`
 * gets a scratch copy (so the transform is not done in place, which is what a real
 * component would do when the input must survive), `memcpy` moves it, `memset` clears
 * the output area before writing it, `free` returns the scratch. The heap those two
 * come from is the process's own untyped budget (milestone 27), so a leak in this file
 * exhausts this process and nothing else.
 *
 * It also range-checks before writing, which is the point of contrast with the two
 * functions below: C *can* be written correctly, and this one is. The confinement is
 * what makes it not matter whether it was.
 */
uint32_t cseam_transform(unsigned char *grant, size_t len)
{
    char *in = (char *)grant + CSEAM_IN_OFF;
    unsigned char *out = grant + CSEAM_OUT_OFF;
    size_t n = strlen(in);
    char *tmp;
    uint32_t h;
    size_t i;

    if (len <= CSEAM_OUT_OFF || CSEAM_OUT_OFF + 4 + n + 1 > len) {
        return 0;
    }

    tmp = (char *)malloc(n + 1);
    if (tmp == NULL) {
        return 0;
    }
    memcpy(tmp, in, n + 1);
    for (i = 0; i < n; i++) {
        if (tmp[i] >= 'a' && tmp[i] <= 'z') {
            tmp[i] = (char)(tmp[i] - 32);
        }
    }

    /* FNV-1a, 32-bit. The Rust side recomputes this independently over the same input,
     * so a C function that returned a constant would be caught. */
    h = 2166136261u;
    for (i = 0; i < n; i++) {
        h ^= (uint32_t)(unsigned char)tmp[i];
        h *= 16777619u;
    }

    memset(out, 0, len - CSEAM_OUT_OFF);
    out[0] = (unsigned char)(h & 0xff);
    out[1] = (unsigned char)((h >> 8) & 0xff);
    out[2] = (unsigned char)((h >> 16) & 0xff);
    out[3] = (unsigned char)((h >> 24) & 0xff);
    memcpy(out + 4, tmp, n + 1);
    free(tmp);
    return h;
}

/*
 * The bug, version one: the classic off-by-one. One byte past the end of the buffer.
 *
 * The first store is legal and lands inside the grant, which is what proves this
 * function's stores work at all. The second is one past the end. In a monolithic
 * kernel this is a kernel memory corruption; here it is a page-permission fault in an
 * unprivileged process, because the page immediately after the grant is mapped
 * read-only into this address space and read/write into somebody else's.
 *
 * `volatile` on the store so no optimizer can decide that undefined behaviour licenses
 * deleting it. The whole test depends on the store actually being attempted.
 */
void cseam_overrun(unsigned char *grant, size_t len)
{
    volatile unsigned char *p;

    grant[0] = CSEAM_MARK;
    p = grant + len;
    *p = CSEAM_MARK;
}

/*
 * The bug, version two: a wild pointer, a whole page past the end.
 *
 * Different from the overrun in the one way that matters for the confinement proof:
 * the address it targets has no mapping in this process at all, so the fault is a
 * translation fault rather than a permission fault, and the witness page that lives at
 * that same virtual address *in another process* is what proves a virtual address means
 * nothing outside the address space that owns it.
 */
void cseam_wild(unsigned char *grant, size_t len)
{
    volatile unsigned char *p;

    grant[0] = CSEAM_MARK;
    p = grant + len + CSEAM_WILD_SKIP;
    *p = CSEAM_MARK;
}
