# 60. Fuzzing complements the proofs, and the parsers are exactly where it wins

**Status: DECIDED.**

Milestone 42 put `cargo-fuzz` over the four parsers that read bytes we did not write: `dtb_walk`,
`elf_parse`, `gpt_table`, `crickerfs_roundtrip`. Those four are the tree's actual trust boundary;
everything else parses bytes this system wrote itself.

**It is not a weaker Kani, it is a different instrument, and the split is about bounds.** Kani proves
a property exhaustively over a *bounded* input; fuzzing searches an *unbounded* input space shallowly.
A parser's whole difficulty is unbounded length, which is precisely the bound Kani must fix to be
tractable, so the parsers were the gap the proofs could not cover rather than a place nobody had got
to yet. §46's rule that we write what is on the verification path is what put this logic in
host-testable crates, and that is why the fuzzers could reach it at all.

**The justification is empirical, not aesthetic: it found three real defects in its first sitting.**
Two panics in `dtb` on a hostile device tree, which is boot-path code parsing bytes the *firmware*
wrote; and `crickerfs` writing a name containing a NUL that could then never be read back, from the
one-file input `[("\0", [])]`, in under a minute. Each is fixed with a regression test beside the
fix, so the finding survives whether or not anyone reruns the fuzzer. **A found bug is a permanent
test, not a permanent fuzzing job.**

The CI job is a **time-boxed sweep on a fixed budget per target, not a regression tripwire.** It does
not prove a pull request introduced nothing, and `notes/fuzzing.md` says so plainly, including that
`gpt_table` barely reaches `check_backup`.

## BUGS

- **Coverage is stated, not measured.** The note records where each target reaches by reading the
  code, and a target whose corpus stops covering a branch will not say so.
- **The seeds are in the tree and the corpus is not**, so a long CI sweep starts from near scratch
  each time and rediscovers shallow ground before it reaches new ground.
