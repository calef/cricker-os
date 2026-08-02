# Fuzz seeds

Inputs a fuzz target starts from, committed on purpose. See [notes/fuzzing.md](../../notes/fuzzing.md)
for the whole discipline; this file is the part a reader meets when they open the directory and find
almost nothing in it.

**Almost nothing is the point.** A fuzzer starting from `[]` spends its first minutes rediscovering
that a device tree begins `d0 0d fe ed`, which with a sixty-second CI budget means it never gets past
the magic check. So seeds matter. But the seeds this project needs mostly **already exist in the
tree**, committed for their own reasons and tested by their own tests:

| Target | Seeds from | What they are |
|---|---|---|
| `dtb_walk` | `crates/dtb/tests/fixtures/` | three real device trees, dumped from the boards we boot |
| `gpt_table` | `crates/gpt/tests/fixtures/` | two real disks, formatted by `sgdisk` and by Apple's Disk Utility |
| `elf_parse` | here | the file below, because nothing else in the tree is a small ELF |
| `crickerfs_roundtrip` | nothing | the input is a *structure*, not bytes; the fuzzer builds file sets from scratch and reaches the interesting shapes immediately |

`script/fuzz` passes those fixture directories to libFuzzer as read-only corpora. Copying them here
would make a second copy of bytes that already have one, and a second copy drifts.

## `elf_parse/minimal_rx.elf`

120 bytes: a 64-byte ELF64 header and one 56-byte `PT_LOAD` program header, read-execute, with the
entry point inside the segment. It is the smallest thing `elf::Elf::parse` accepts, which is exactly
what a seed should be: past every constant check and every validation refusal, so the fuzzer's
mutations land on the arithmetic instead of on the magic number.

**`e_machine` is `EM_AARCH64` (183)**, and that is host-dependent: `crates/elf` picks its expected
machine at compile time, so a riscv64 build would reject this seed. `crates/elf/tests/fuzz_seed.rs`
asserts both that the seed parses and that its machine matches the build, so the degradation is loud
rather than silent.

Regenerate it with:

```sh
python3 - <<'PY'
import struct
EHDR, PHDR = 64, 56
total = EHDR + PHDR
vaddr, memsz, entry = 0x4000_0000, 0x1000, 0x4000_0000

e = bytearray(EHDR)
e[0:4] = b'\x7fELF'
e[4], e[5], e[6] = 2, 1, 1             # ELFCLASS64, ELFDATA2LSB, EV_CURRENT
struct.pack_into('<H', e, 16, 2)       # e_type = ET_EXEC
struct.pack_into('<H', e, 18, 183)     # e_machine = EM_AARCH64 (243 for riscv)
struct.pack_into('<Q', e, 24, entry)
struct.pack_into('<Q', e, 32, EHDR)    # e_phoff
struct.pack_into('<H', e, 52, EHDR)    # e_ehsize
struct.pack_into('<H', e, 54, PHDR)    # e_phentsize
struct.pack_into('<H', e, 56, 1)       # e_phnum

p = bytearray(PHDR)
struct.pack_into('<I', p, 0, 1)        # p_type = PT_LOAD
struct.pack_into('<I', p, 4, 5)        # p_flags = PF_R | PF_X
struct.pack_into('<Q', p, 8, 0)        # p_offset
struct.pack_into('<Q', p, 16, vaddr)
struct.pack_into('<Q', p, 24, vaddr)   # p_paddr
struct.pack_into('<Q', p, 32, total)   # p_filesz
struct.pack_into('<Q', p, 40, memsz)
struct.pack_into('<Q', p, 48, 0x1000)  # p_align

open('fuzz/seeds/elf_parse/minimal_rx.elf', 'wb').write(bytes(e + p))
PY
```

## What does not go here

**The working corpus.** libFuzzer writes every input that reaches a new edge into
`fuzz/corpus/<target>/`, which grows without limit and is machine-specific. Gitignored.

**Crash artifacts.** When a target finds a crash, the input becomes a host test in the crate that
owns the bug, where it runs in milliseconds forever and where a reader meets it next to the code.
`crates/dtb/tests/hostile.rs` is the worked example. A hand-built blob with a docstring saying what
it attacks is worth more than a 7,642-byte file named after its SHA-1.
