//! **`grant_plan`: the grant-expression logic of the capability shell** (milestone 31, phase 1).
//!
//! This crate is the pure logic behind the shell's one idea: on a nife command line,
//! *designation is authorization* (Mark Miller's principle). Naming a resource in a command is
//! how you grant it; a program that names nothing gets nothing beyond the report channel every
//! spawn carries. There is no ambient authority to fall back on, so the failure when a program
//! needs something the command did not grant is legible ("you hold no such capability"), not a
//! Unix-flavored EPERM.
//!
//! It performs no IO and makes no syscalls. It turns a line of bytes into a decision: a
//! [`Command`], and for a program invocation an [`Endowment`] (exactly what to grant the child) or a
//! typed [`Refusal`] the shell prints at the prompt. That split is DECISIONS §7 applied, the same
//! shape as `line_editor`: the parsing and the manifest checking are host-tested in milliseconds, and
//! only the wiring (the shell and init that carry the caps) needs QEMU. See
//! notes/grant-expression.md and notes/program-manifest.md.
//!
//! # The grammar, after milestone 47 took two words out of it
//!
//! ```text
//! <prog> [-abc] [--mem N] [token ...]
//! caps [command]
//! time <command>
//! help
//! echo <text>
//! ```
//!
//! `caps` and `time` are **prefix words**: their operand is a whole command line, operators
//! included, which is why neither is a program (a program takes tokens) and why both keep the tail
//! unsplit for the shell to act on.
//!
//! Short options arrived with `rm` (milestone 47's rmdir lane), and they are checked against the
//! program's [`Manifest`] at the prompt rather than passed along unread, because one of them can
//! **widen a grant**: `rm -r logs` hands over the subtree that `rm logs` does not.
//!
//! There is no `run` verb and no `file:` designator, and both removals are milestone 47's
//! (roadmap, "`file:` and `run` are not earned"). A bare program name spawns it, because a shell
//! where builtins are bare words and programs need a prefix makes the user learn which class a
//! command is in before they can type it. A bare token in a file position designates the file,
//! because `file:` marked the half of the grant that was already visible (*which* file) and said
//! nothing about the half that matters (read or write), which lives in the [`Manifest`] by design.
//!
//! # The three moving parts
//!
//! - [`parse`] tokenizes a command line into a [`Command`]. A first word that is not a builtin is a
//!   program invocation, and everything after it is the `--mem N` budget grant plus **unclassified
//!   positional tokens**: the parser deliberately does not decide which token is the argument and
//!   which is the file, because that is the manifest's job.
//! - A [`Manifest`] is a program's declared endowment: does it take an argument, does it require
//!   (or forbid) a memory grant, may it be granted a file and in which direction, does it report
//!   back. [`Prog::manifest`] is the static table.
//! - [`plan`] checks a parsed invocation against the named program's manifest, **placing the
//!   positional tokens into the slots the manifest declares**, and yields an [`Endowment`] or a
//!   [`Refusal`]. A mismatch is caught here, at the prompt, before anything is spawned, which is
//!   milestone 23's component contract in embryo.
//!
//! # The wire half
//!
//! [`spawnproto`] is the word layout for the shell-to-init spawn protocol, the capability-shell
//! analogue of `line_editor::proto`. It is a userspace protocol (DECISIONS §21's shape): the kernel
//! routes the words and never reads them.
//!
//! Name: ratified 2026-08-01 (calef, milestone 63), replacing `capsh`. Refused `capsh` (Linux's
//! libcap ships `capsh(1)`, a capability shell wrapper, so a reader arriving from Linux would
//! assume ours is that tool); `designation`, `designate` and `designator` together (the user
//! designates by typing a name, and this crate's work starts after that, so all three put it in a
//! role it does not hold); and the grant synonyms `endow`, `award`, `confer`, `bestow`, `allot` and
//! `furnish` (grant is already this tree's word and a synonym is a decoder ring, and `endow` is
//! additionally taken by `supervision_proto::ChildEndowment`). Deliberately not named for `swish`: seven
//! things use it, so naming it for one consumer would repeat `dwarden`'s defect.

#![no_std]
// milestone 68's ratchet is workspace-wide (§107); this crate opts out until its 22-item
// worklist (notes/doc-coverage.md) is burned down.
#![allow(missing_docs)]

pub mod expand;
pub mod jobframe;
pub mod line;
pub mod nav;
pub mod spawnproto;
pub mod word;

use expand::{Expansion, Name, NameSet};
use line::{Sink, Source};

/// A program the shell can spawn. The set is small and closed in phase 1; each variant carries a
/// static [`Manifest`] and a stable wire id for [`spawnproto`].
///
/// This is deliberately an enum and not a string lookup at the grant boundary: the shell resolves
/// a typed program once, and everything downstream (the manifest check, the wire id init decodes)
/// speaks the type, not the name. A name that does not resolve is [`Refusal::NoSuchProgram`], the
/// "there is nothing there to name" shape of no-ambient-authority applied to programs themselves.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prog {
    /// Squares its integer argument and reports the answer. Needs no memory grant.
    Worker,
    /// Spends a granted untyped budget: maps pages until the budget is exhausted and reports how
    /// many it got. The program that makes `--mem` *real* rather than parsed-and-ignored: the
    /// number it reports is the authority the command line handed it.
    Budgeter,
    /// A long-running job that *heeds* the cooperative interrupt: it works forever, polling its
    /// interrupt flag between work units, and on `^C` cleans up and exits (DECISIONS §24). The
    /// cooperative tier made visible: the first `^C` stops it gracefully.
    Heeder,
    /// A runaway that ignores the interrupt entirely: a tight loop that never checks its flag. Only
    /// the forcible tier (the shell tearing its region down) ends it. The case the cooperative tier
    /// cannot reach, and the reason the second `^C` exists.
    Spinner,
    /// Print the wall-clock time (milestone 51, `user/src/date.rs`). It takes nothing from the
    /// command line: no argument, no memory, no file. **Its whole authority is a read-only mapping
    /// of the clock page, which init endows and this shell cannot**, and that asymmetry is why
    /// [`Manifest::clock`] exists: the grant is real, it is just not something a person designates.
    /// The interactive boot starts a clock service and hands init the page, so `date` at the prompt
    /// prints a time; on a machine whose RTC the service did not believe it prints "the time is
    /// unknown: the machine has no clock it believes", which is the other true sentence.
    Date,
    /// **Remove a name, and with `-r` the tree under it** (milestone 47, `user/src/rm.rs`).
    ///
    /// A **program, not a builtin**, and that is Unix's shape rather than a divergence from it.
    /// `cd`, `pwd` and `ls` are builtins here because the shell is rebinding what it already holds;
    /// `rm -r` is a destructive loop, not a rebinding. A builtin would run with the shell's **entire
    /// endowment**, while a program takes an explicit attenuated grant, so `caps rm -r logs` prints
    /// the subtree at risk before anything happens and a bug in the recursion can only reach what it
    /// was handed. See [`DirSpec`].
    Rm,
    /// **Count what arrives on its input** (milestone 50, `user/src/wc.rs`): lines, words and
    /// bytes, printed as one line of text.
    ///
    /// The first program that declares [`InputSpec::Required`], and the reason that spec exists.
    /// `wc` with nothing feeding it would block on a receive forever, and the shell can see that at
    /// the prompt: the manifest says it reads a stream, so a line that gives it none is
    /// [`Refusal::InputRequired`] before anything is spawned. Unix cannot make that check, because
    /// there fd 0 always exists and "nobody is ever going to write to it" is not a property of the
    /// command line.
    ///
    /// Its output is [`OutputSpec::Bytes`] because it has to be: `wc`'s answer is text, so `echo a
    /// | wc | wc` composes, and a program that reported a number in a register could not be on the
    /// left of a pipe at all.
    ///
    /// And because it declares an input and declares no file, it is also the program that gives the
    /// **input operand** its meaning: `wc report.txt` is `wc < report.txt` with the operator left
    /// out, resolved by [`plan_against_with`] into a [`line::Source::File`]. See that function for
    /// why what the child holds is narrower than a per-file capability rather than the same thing.
    Wc,
    /// **Render markdown for a terminal** (milestone 40, `user/src/doc.rs`, notes/manual.md).
    ///
    /// The same manifest as [`Prog::Wc`]: a stream in, a stream out, and nothing else. `doc
    /// notes/glob.md` reads like Unix's `man` and is not: the name is a designation the *shell*
    /// resolves against the directory it holds, and what arrives at the program is bytes. A viewer
    /// that opened the page it renders would be a viewer that could open any page.
    ///
    /// **Provisional name.**
    Doc,
    /// **List the processes in the supervision domain it was spawned into** (milestone 126,
    /// `user/src/ps.rs`, notes/process-view.md).
    ///
    /// The reason [`Manifest::domain`] exists, and the same asymmetry [`Prog::Date`] made for the
    /// clock: the grant is real and it is not something a person designates on the line. There is
    /// no `/proc` here to name and no pid space to scan, so what `ps` can see is decided entirely
    /// by which supervision endpoint init put in its cspace, and `caps ps` prints that.
    Ps,
    /// **Name the members of that same domain that match, and do nothing to them** (milestone 126,
    /// `user/src/pgrep.rs`, notes/process-view.md).
    ///
    /// [`Prog::Ps`]'s manifest exactly, down to the field, and that is the declaration doing the
    /// work rather than a coincidence. On Unix `pgrep` and `pkill` are one lookup with two endings,
    /// so a program that can find a process can end it. Here the two manifests being identical is
    /// the readable form of **a domain names its members and does not act on them** (calef,
    /// 2026-08-17): the finding program is granted no authority the listing program lacks, and there
    /// is no `Prog::Pkill` for it to be compared against, because a tid is a name and no method
    /// turns one into a capability.
    ///
    /// `ArgSpec::Forbidden` is [`Prog::Date`]'s deliberate under-declaration, for the same reason
    /// and with the same consequence: `pgrep` reads a state mask out of a register the shell cannot
    /// set, so at this prompt it takes the default and names every member. Nothing in this system
    /// delivers *bytes* from a command line to a program, so a pattern is not something the line can
    /// carry until [`ArgSpec`] grows the positional arity milestone 47 deferred; see `crates/pgrep`'s
    /// `BUGS`.
    Pgrep,
}

/// The number of programs [`Prog::id`] can name, which is the size of the table init indexes with
/// it. Init's array is `[Option<&Elf>; COUNT]`, so adding a variant without widening the array is
/// an out-of-bounds panic in init rather than a compile error; the constant is here so both inits
/// can be written against one number.
pub const PROG_COUNT: usize = 10;

impl Prog {
    /// Resolve a program by the name typed on the command line.
    ///
    /// Builtins are matched first by [`parse`], so a program named `help`, `echo`, `caps` or `time`
    /// would be unreachable. The program namespace must not contain any of those names.
    pub fn from_name(name: &[u8]) -> Option<Prog> {
        match name {
            b"worker" => Some(Prog::Worker),
            b"budgeter" => Some(Prog::Budgeter),
            b"heeder" => Some(Prog::Heeder),
            b"spinner" => Some(Prog::Spinner),
            b"date" => Some(Prog::Date),
            // `rm` stopped being a builtin in milestone 47's rmdir lane, which is what makes this
            // line reachable: a builtin would have shadowed it, because `parse` matches those
            // first.
            b"rm" => Some(Prog::Rm),
            b"wc" => Some(Prog::Wc),
            b"doc" => Some(Prog::Doc),
            b"ps" => Some(Prog::Ps),
            b"pgrep" => Some(Prog::Pgrep),
            _ => None,
        }
    }

    /// The name init loads it by in the initrd (nifefs), and the shell prints.
    pub fn name(self) -> &'static str {
        match self {
            Prog::Worker => "worker",
            Prog::Budgeter => "budgeter",
            Prog::Heeder => "heeder",
            Prog::Spinner => "spinner",
            Prog::Date => "date",
            Prog::Rm => "rm",
            Prog::Wc => "wc",
            Prog::Doc => "doc",
            Prog::Ps => "ps",
            Prog::Pgrep => "pgrep",
        }
    }

    /// The stable wire id the shell sends and init decodes ([`spawnproto`]).
    pub fn id(self) -> u64 {
        match self {
            Prog::Worker => 0,
            Prog::Budgeter => 1,
            Prog::Heeder => 2,
            Prog::Spinner => 3,
            Prog::Date => 4,
            Prog::Rm => 5,
            Prog::Wc => 6,
            Prog::Doc => 7,
            Prog::Ps => 8,
            Prog::Pgrep => 9,
        }
    }

    /// The inverse of [`id`](Prog::id): init turns the wire id back into a program.
    pub fn from_id(id: u64) -> Option<Prog> {
        match id {
            0 => Some(Prog::Worker),
            1 => Some(Prog::Budgeter),
            2 => Some(Prog::Heeder),
            3 => Some(Prog::Spinner),
            4 => Some(Prog::Date),
            5 => Some(Prog::Rm),
            6 => Some(Prog::Wc),
            7 => Some(Prog::Doc),
            8 => Some(Prog::Ps),
            9 => Some(Prog::Pgrep),
            _ => None,
        }
    }

    /// The program's declared endowment: what the shell must (and must not) grant it.
    pub fn manifest(self) -> Manifest {
        match self {
            Prog::Worker => Manifest {
                arg: ArgSpec::Required,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                // A worker answers with a number in a register, which is older than the sink
                // contract and still the right shape for one integer. It is also why `worker 9 >
                // out.txt` is refused: there are no bytes to put in the file.
                output: OutputSpec::Words,
                input: InputSpec::Forbidden,
                reports: true,
                // A worker finishes in one step; there is no long computation to interrupt, so it
                // is granted no interrupt channel. The shell waits for its result and no ^C tier
                // applies (DECISIONS §24).
                interruptible: false,
                clock: false,
                domain: false,
            },
            Prog::Budgeter => Manifest {
                arg: ArgSpec::Forbidden,
                // A budget between 1 and 64 pages. The lower bound makes "budgeter with no --mem"
                // a refusal (it exists to spend memory); the upper bound is a sanity ceiling the
                // shell's own budget can actually back.
                mem: MemSpec::Required { min: 1, max: 64 },
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                // The page count it managed to map, in a register. Same reason as worker's.
                output: OutputSpec::Words,
                input: InputSpec::Forbidden,
                reports: true,
                interruptible: false,
                clock: false,
                domain: false,
            },
            // The two interrupt demonstrators. Both run until interrupted, take no argument and no
            // memory grant, and report through the shared job frame rather than the result endpoint
            // (so `reports` is false: they hold no result cap). `interruptible` is what makes the
            // shell wire the two-tier ^C path and hold the region for a forcible teardown.
            Prog::Heeder => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                // It reports through the shared job frame and holds no output capability at all,
                // which is a third thing a slot can be and the reason `OutputSpec` has three
                // variants: `Silent` is not "bytes nobody reads", it is no slot.
                output: OutputSpec::Silent,
                input: InputSpec::Forbidden,
                reports: false,
                interruptible: true,
                clock: false,
                domain: false,
            },
            Prog::Spinner => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                output: OutputSpec::Silent,
                input: InputSpec::Forbidden,
                reports: false,
                interruptible: true,
                clock: false,
                domain: false,
            },
            // `date` declares an empty grant expression, and that is the interesting part: its
            // authority (a read-only mapping of the clock page) is not something the command line
            // can hand over, so there is nothing to type and nothing to forbid getting wrong.
            // `FileSpec::Forbidden` and `MemSpec::Forbidden` are the load-bearing half: a clock
            // reader has no use for either, so naming one is refused rather than granted-and-
            // ignored. It reports through the shared result endpoint, one framed line of text.
            //
            // `ArgSpec::Forbidden` is a **deliberate under-declaration**: `date` reads three
            // registers (format, UTC offset, provenance), and a manifest that could express that
            // needs positional arity, which milestone 47 explicitly defers. So the shell spawns it
            // with the defaults (`Human`, UTC, no provenance line), which is the `date` a person
            // typing `date` wants; the selectors arrive when `ArgSpec` grows.
            Prog::Date => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                // **`date` was speaking the sink contract before the contract existed**, because
                // its hand-rolled framing is bit for bit a `BYTES` message (notes/sink-protocol.md
                // on why `OP_BYTES` is zero). So it is the first program that can be piped, and it
                // needed one change to be one: an end-of-stream message, without which a reader
                // waits forever on a producer that has already exited.
                //
                // **And the first program to declare a second stream** (DECISIONS §67), because it
                // is the program whose loss motivated the decision: "the time is unknown" is a
                // complaint about the run and it travelled on the same endpoint as the answer, so
                // `date > when.txt` on a clockless machine wrote it into the file. Declaring the
                // second output is what lets `2>` name where it goes, and what makes the default
                // (the shell prints it) a different place from where the answer went.
                output: OutputSpec::BytesAndDiagnostics {
                    slot: DIAGNOSTICS_SLOT,
                },
                input: InputSpec::Forbidden,
                reports: true,
                interruptible: false,
                // **The one program in this table that declares a clock**, and the reason the field
                // exists. Nothing on the command line designates it, so init reads this to decide
                // which children get the read-only mapping (milestone 51's wiring, notes/date.md).
                clock: true,
                domain: false,
            },
            // **The first program endowed a directory**, and the first with options. It takes no
            // integer and no memory: what it needs is the authority to take a name out of the
            // directory that holds it, which is a capability and not a number.
            //
            // `subtree_flag: Some(b'r')` is the whole of "`rm -r` hands over more than `rm`". A
            // grant without it cannot descend or enumerate, so the recursion is not merely disabled
            // by a branch in the program: the capability to walk was never handed over, and a `rm`
            // that tried anyway would be told there is no such name.
            Prog::Rm => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Required {
                    subtree_flag: Some(b'r'),
                },
                // In `rmopt`'s bit order, which is what that module pins.
                flags: b"rfv",
                // `-v` prints names, and it prints them the way `date` prints a time. Piping `rm
                // -rv logs | wc` is therefore expressible; whether it is a good idea is the user's
                // business and not the manifest's.
                output: OutputSpec::Bytes,
                input: InputSpec::Forbidden,
                reports: true,
                interruptible: false,
                clock: false,
                domain: false,
            },
            // **The consumer**, and the only program that declares an input. Everything else about
            // it is empty: no argument, no memory, no file, no directory, no options. What it does
            // is entirely decided by what is in its input slot, which is the point of having it.
            Prog::Wc => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                output: OutputSpec::Bytes,
                // **The barrier**, and the only one in this tree. `wc` prints nothing until end of
                // stream, which is what lets this shell feed it and read it back with the one wait
                // point a process has. See [`InputSpec::Required::writes_while_reading`]: a filter
                // that answered as it read could not be run this way at all.
                input: InputSpec::Required {
                    writes_while_reading: false,
                },
                reports: true,
                interruptible: false,
                clock: false,
                domain: false,
            },
            // **The viewer**, whose manifest is "a stream in, a stream out" like `wc`'s, and handed
            // bytes like every other stage. The one place it parts from `wc` is the field milestone
            // 50 added: `wc` absorbs the whole stream before it emits (`writes_while_reading:
            // false`), while `doc` renders as it reads and so writes while it is still reading. That
            // difference is not decoration. It is exactly why `doc` can deadlock a rendezvous
            // pipeline where `wc` never does (notes/manual.md's BUGS section), and the planner can
            // only account for it if the manifest declares it. No memory grant, because the renderer
            // never allocates.
            Prog::Doc => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                output: OutputSpec::Bytes,
                input: InputSpec::Required {
                    writes_while_reading: true,
                },
                reports: true,
                interruptible: false,
                clock: false,
                domain: false,
            },
            // **`ps`: a stream out, a domain in, and nothing else** (milestone 126).
            //
            // Every designated grant is `Forbidden`, which is the row worth reading: there is no
            // file, no directory, no memory and no argument, so nothing a person could type widens
            // what this program sees. Its whole authority is `domain`, which init endows and the
            // command line cannot name, exactly as `date`'s clock is.
            //
            // `OutputSpec::BytesAndDiagnostics` because a refusal and a listing must not travel on
            // the same stream: `ps > running.txt` on a domain it was not granted has to leave the
            // file empty and put the reason on the terminal, which is the whole of DECISIONS §67
            // applied to a monitor. `InputSpec::Forbidden` because a snapshot reads nothing.
            Prog::Ps => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                output: OutputSpec::BytesAndDiagnostics {
                    slot: DIAGNOSTICS_SLOT,
                },
                input: InputSpec::Forbidden,
                reports: true,
                interruptible: false,
                clock: false,
                domain: true,
            },
            // **`pgrep`: `ps`'s manifest, field for field, and the sameness is the claim.**
            //
            // A reader comparing these two blocks should find nothing to compare, because the
            // program that *finds* a process is granted exactly what the program that *lists* them
            // is: one supervision endpoint, `ENUMERATE`, and no way to name anything outside the
            // domain. On Unix `pgrep` and `pkill` differ only in what they do with the answer, and
            // there is no such pair here: a tid is a name, and killing a live child is
            // `Untyped::DESTROY` on its region, held by whoever spawned it (DECISIONS §24).
            //
            // `ArgSpec::Forbidden` is `date`'s deliberate under-declaration, not an oversight.
            // `pgrep` reads a state mask out of the integer-argument register, and a *pattern* is
            // bytes, which nothing in this system carries from a command line to a program. So the
            // prompt sends the default and `pgrep` names every member; see `crates/pgrep`'s `BUGS`
            // for why that is a property of the boundary rather than of this program.
            Prog::Pgrep => Manifest {
                arg: ArgSpec::Forbidden,
                mem: MemSpec::Forbidden,
                file: FileSpec::Forbidden,
                dir: DirSpec::Forbidden,
                flags: NO_FLAGS,
                output: OutputSpec::BytesAndDiagnostics {
                    slot: DIAGNOSTICS_SLOT,
                },
                input: InputSpec::Forbidden,
                reports: true,
                interruptible: false,
                clock: false,
                domain: true,
            },
        }
    }
}

/// **What a program's output slot carries.** Declared per program because this system has two
/// conventions for "a program said something" and they are not interchangeable; see
/// [`Manifest::output`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputSpec {
    /// Nothing. The program holds no output capability (the interrupt demonstrators report through
    /// a shared frame). Redirecting it is [`Refusal::NotAByteStream`], and so is piping it, because
    /// there is no slot to substitute.
    Silent,
    /// One or more raw `u64` answers on the result endpoint, read by the shell and rendered by it.
    /// Older than the sink contract and still right for an integer; not redirectable.
    Words,
    /// The sink contract (`crates/sink_proto`): self-framing byte messages ending in `OP_EOF`. The
    /// only output that `>` and `|` can substitute, because it is the only one whose meaning does
    /// not depend on who is reading it.
    Bytes,
    /// [`Bytes`](OutputSpec::Bytes) **and a second byte stream** for this program's own
    /// diagnostics, in the cspace slot named here (DECISIONS §67).
    ///
    /// This is what `2>` binds to, and the slot number is why it is a declaration rather than
    /// Unix's fd 2 with a capability underneath. Nothing in this system agrees in advance that any
    /// number means anything; the program says where its second stream goes, `caps` prints it, and a
    /// program that says nothing has no second stream to name.
    ///
    /// **The slot is high and out of the way on purpose**, which is `abi::fault::FAULT_EP_SLOT`'s
    /// reasoning applied a second time: a child's ordinary grants fill from zero upward and how many
    /// there are depends on the wiring (`date` gets a clock from init and none from the guest test
    /// harness), so a low number would move under the program and it would probe the wrong slot. A
    /// number nothing else can reach is the same number in every wiring.
    BytesAndDiagnostics {
        /// The cspace slot the diagnostic endpoint is inserted into.
        slot: u64,
    },
}

impl OutputSpec {
    /// **Whether `>` and `|` have anything to substitute**, which is what the operators' check asks.
    /// A declared second stream does not change the answer: the first stream is still the sink
    /// contract, and a redirection still names where *it* goes.
    pub fn is_byte_stream(self) -> bool {
        matches!(
            self,
            OutputSpec::Bytes | OutputSpec::BytesAndDiagnostics { .. }
        )
    }

    /// The cspace slot this program's declared second stream lands in, or `None` for a program that
    /// declares none. `Some` is the whole of what makes `2>` legal on a command line.
    pub fn diagnostics_slot(self) -> Option<u64> {
        match self {
            OutputSpec::BytesAndDiagnostics { slot } => Some(slot),
            _ => None,
        }
    }
}

/// **Whether a program reads an input stream**, milestone 50's `<` and the right-hand end of `|`.
///
/// The asymmetry with [`OutputSpec`] is deliberate and it is a fact about the two directions.
/// Output has three shapes because the system grew two of them before the sink contract existed.
/// Input has one shape, because nothing read a stream at all until this milestone: the source
/// contract is the sink contract received rather than sent, and there was never a chance for a
/// second convention to establish itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputSpec {
    /// The program reads nothing. Giving it an input is [`Refusal::InputForbidden`] rather than a
    /// capability it would never receive on, because a slot nobody reads is authority that moved
    /// for no reason.
    Forbidden,
    /// The program reads an input stream and cannot do anything without one. A line that gives it
    /// none is [`Refusal::InputRequired`] **at the prompt**, which is a check Unix cannot make.
    Required {
        /// **Whether this program's output may begin before its input has ended.**
        ///
        /// `wc` counts to end of stream and then prints one line, so it is `false`. A renderer or a
        /// filter that transforms as it reads is `true`, and the difference is not a performance
        /// note: it decides whether **this shell** can run the program at all.
        ///
        /// A sink message is a rendezvous `SEND` and a process has exactly one wait point (no
        /// select, no poll, no timed receive: DECISIONS §51's fork is still open, and
        /// design/roadmap/106 is NOT-STARTED). So a shell that is feeding a stage cannot also be
        /// receiving from it, and a stage that writes while it reads blocks against a shell that is
        /// still writing. Both stop, and the prompt never comes back.
        ///
        /// A program that declares `false` is a **barrier**: it absorbs the whole stream before it
        /// answers, which lets the shell finish feeding before anything comes back. One barrier
        /// anywhere in a chain is enough, which is why `doc page.md | wc` runs and `doc page.md`
        /// does not. See [`check_chain`] and notes/pipes.md.
        writes_while_reading: bool,
    },
}

/// A program that takes no short options. Spelled once so a manifest reads as a declaration rather
/// than as an empty literal repeated six times.
pub const NO_FLAGS: &[u8] = b"";

/// **The cspace slot a declared diagnostic stream lands in** (DECISIONS §67), for the programs that
/// declare one at all.
///
/// It is a constant here rather than a number each manifest picks, and that is not a retreat to an
/// ambient convention: what the manifest declares is **whether** there is a second stream, and the
/// slot is a placement detail every declarer may as well share. What matters is that a program that
/// declares nothing has no slot and no stream, which is where the model differs from fd 2.
///
/// **Eight**, which is above every ordinary grant (a child's are inserted from zero and the most
/// any manifest asks for is four) and below `abi::fault::FAULT_EP_SLOT` at fifteen. Out-of-the-way
/// is load-bearing: how many low slots a child gets depends on the wiring, so a low number would
/// move under the program. `abi::tcb::CAP_INSERT`'s explicit target is what puts it there, and it
/// exists already, for the fault endpoint, for exactly this reason.
pub const DIAGNOSTICS_SLOT: u64 = 8;

/// **The cspace slot a process-domain capability lands in** (milestone 126), for the programs that
/// declare [`Manifest::domain`].
///
/// Placed rather than taken from the next free slot, for exactly the reason above: how many low
/// slots a child gets depends on what else the line granted it, and `ps` reads a fixed number. Seven
/// sits below [`DIAGNOSTICS_SLOT`] and above every ordinary grant, so the two named slots read as one
/// small block a person can hold in their head.
///
/// A number is not a right. What it means to hold this slot is decided by the capability init puts
/// in it, and a program spawned without the declaration holds an empty slot here and is refused by
/// the kernel rather than shown an empty domain.
pub const DOMAIN_SLOT: u64 = 7;

/// A program's expectation about the integer argument (`worker 9`'s `9`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ArgSpec {
    /// The program consumes an argument; omitting it is [`Refusal::ArgRequired`].
    Required,
    /// The program takes no argument; supplying one is [`Refusal::ArgForbidden`].
    Forbidden,
}

/// A program's expectation about a memory grant (`--mem N`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemSpec {
    /// The program takes no memory grant; supplying one is [`Refusal::MemForbidden`].
    Forbidden,
    /// The program must be granted a budget in `[min, max]` pages. Omitting it is
    /// [`Refusal::MemRequired`]; out of range is [`Refusal::MemOutOfRange`].
    Required { min: u64, max: u64 },
}

/// A program's expectation about a **file grant**, milestone 31 phase 2.
///
/// **The manifest declares the direction; the command line designates the file.** That split is the
/// SHILL shape and it is deliberate: a program knows whether it needs to write (that is a property of
/// what it does), while *which* file is the human's business and belongs on the command line. So
/// `wc report.txt` reads and `tee report.txt` writes, with no flag either way, and the authority is
/// still exactly what the line says because the program's half is fixed and published.
///
/// This enum is also what decides that a bare token *is* a file at all. Milestone 47 removed the
/// `file:` prefix on the finding that it announced the visible half of the grant and was silent on
/// the half that matters: `worker 5 extra` is refused because worker's manifest says `Forbidden`,
/// not because of any prefix. The manifest was doing all the work.
///
/// One file, not a list. A program that needs two files needs a manifest that says so, and that is a
/// later widening (`Required { count }`) rather than something to leave ambiguous now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileSpec {
    /// The program takes no file; naming one is [`Refusal::FileForbidden`].
    Forbidden,
    /// The program is granted exactly one file, `writable` or not. Omitting it is
    /// [`Refusal::FileRequired`].
    Required { writable: bool },
}

/// A program's expectation about a **directory grant**, milestone 47's `rm -r`.
///
/// [`FileSpec`] narrows a directory capability down to one file. This is the rung above: the program
/// is granted a capability to **a whole directory**, because what it does cannot be expressed as one
/// file. `rm` is the case that forced it: removing a name is an operation on the *directory that
/// holds it*, and no per-file capability carries the authority to do it.
///
/// # The grant is the directory the name is in, and that is wider than the name
///
/// `rm logs/old` hands `rm` a capability to `logs`, not to `old`, because that is what the
/// filesystem contract can express: `UNLINK` resolves a name under a directory handle. So the
/// program could remove any name in that directory, and it removes the one it was told. That is a
/// real over-grant, it is declared rather than hidden, and it is the same gap globbing closes: the
/// answer is a directory capability attenuated to a **name set** (the caretaker `fs_file_caretaker`
/// already is for one name), which is a later lane. Until then the honest statement is that `rm` is
/// granted the directory, and `caps rm -r logs` prints exactly that before anything happens.
///
/// # Why the rights are not a mask here
///
/// The fields say what the program will *do*, not what the wire calls it. `fs_proto::dir`'s
/// six-rung ladder is the filesystem contract's vocabulary, and translating into it belongs to the
/// shell, which holds both contracts; keeping it out of `grant_plan` is [`MAX_FILE_NAME`]'s rule, that a
/// command line can be checked without linking the filesystem.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DirSpec {
    /// The program takes no directory.
    Forbidden,
    /// The program is granted a capability to the directory that holds what the command names,
    /// carrying the authority to **take names out of it** and nothing else.
    ///
    /// `subtree_flag`, when the option it names is on the command line, widens that to walking and
    /// listing what is underneath, which is what a recursive removal needs at every level. The
    /// widening is the point rather than a convenience: **typing `-r` visibly hands over more**, and
    /// a program run without it holds a capability that cannot even see a subdirectory, let alone
    /// descend into one.
    Required { subtree_flag: Option<u8> },
}

/// A program's SHILL-style manifest: the endowment it declares it expects. The shell checks the
/// command's grants against this at spawn, so a mismatch is a refusal at the prompt rather than a
/// mystery hang inside a program that did not get what it needed. See notes/program-manifest.md.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Manifest {
    pub arg: ArgSpec,
    pub mem: MemSpec,
    /// A per-file grant, milestone 31 phase 2. See [`FileSpec`] for why the direction lives here
    /// and the name lives on the command line.
    pub file: FileSpec,
    /// A per-**directory** grant, milestone 47's `rm -r`. See [`DirSpec`]: the program is handed a
    /// capability to the directory holding what the command names, because what it does (taking a
    /// name out) is an operation on a directory and not on a file.
    pub dir: DirSpec,
    /// **The short options this program accepts**, as their letters, in the order their bits are
    /// numbered ([`Endowment::flags`]). Empty for a program that takes none, which is every program
    /// but `rm` today.
    ///
    /// The manifest owns this for the same reason it owns the file's direction: which options exist
    /// is a property of what the program does, and the shell must be able to refuse `-q` at the
    /// prompt rather than pass a letter along for the program to ignore. An option is **not**
    /// authority by itself, which is why the parser may collect letters before it knows the
    /// program; what makes one matter here is that `subtree_flag` lets one *widen a grant*, and
    /// that widening is checked and printed before anything is spawned.
    pub flags: &'static [u8],
    /// **What this program's output slot carries** (milestone 50). A slot is a capability, but what
    /// travels over it is a convention, and `>` and `|` only make sense for one of them: a program
    /// that answers with a number in a register cannot have a file put behind it, because a file
    /// sink would receive a word it has no way to read as bytes.
    ///
    /// Declaring it is what lets `worker 5 > out.txt` be [`Refusal::NotAByteStream`] at the prompt
    /// instead of a file full of nothing. Unix has no equivalent because on Unix every program's
    /// stdout is bytes by construction; here the register fastpath is real and older than the sink
    /// contract, so the two conventions coexist and the manifest is where they are told apart.
    pub output: OutputSpec,
    /// **Whether this program reads an input stream** (milestone 50). See [`InputSpec`]: a program
    /// that declares [`InputSpec::Required`] and is given nothing would block forever on a receive,
    /// and that is a mistake the shell can catch at the prompt.
    pub input: InputSpec,
    /// Endowed with the shared result endpoint (so it can report back). Every phase-1 program
    /// reports; the field exists so a program that does not can drop the channel it never uses.
    pub reports: bool,
    /// Granted a per-job interrupt channel so `^C` can reach it (DECISIONS §24). A
    /// long-running or interactive program declares this and the shell wires the two-tier interrupt
    /// path for it; a program that finishes in one step (worker) declares `false` and is simply
    /// waited on. "Granted by default to interactive programs" is expressed here, per program.
    pub interruptible: bool,
    /// **Endowed a read-only mapping of the wall clock** (milestone 51's wiring).
    ///
    /// The odd one out in this struct, and deliberately: every other field is about something the
    /// command line can designate, and this is about something it cannot. A clock is not a name a
    /// person types, so there is no token to place and no refusal to write; what the manifest is for
    /// here is telling **init** which children to endow, and telling a person reading `caps date`
    /// that the authority exists at all.
    ///
    /// It is a *read-only* mapping and nothing else, which is the whole of DECISIONS §43's split
    /// made concrete: a program declaring this can read the time and has no object through which it
    /// could set or propose one. See notes/date.md.
    pub clock: bool,
    /// **Endowed a view over the supervision domain it is spawned into** (milestone 126,
    /// notes/process-view.md).
    ///
    /// [`clock`](Manifest::clock)'s twin, and for the same reason: a process domain is not a name a
    /// person types, so there is no token to place and no refusal to write. What this field does is
    /// tell **init** which children to endow, and tell a person reading `caps ps` that the authority
    /// exists and how wide it is.
    ///
    /// It lands in [`DOMAIN_SLOT`] carrying `READ`, which is what `abi::endpoint::SURVEY` takes. A
    /// program that does not declare it holds nothing there, and the kernel refuses its survey
    /// loudly instead of answering with an empty list: **that difference is the whole feature**, and
    /// `ps` prints the two apart.
    pub domain: bool,
}

/// A parsed command line. The shell dispatches on this; only [`Command::Run`] carries a grant
/// expression that must be planned against a manifest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command<'a> {
    /// An empty line. Reprompt.
    Empty,
    /// `help`.
    Help,
    /// `echo <text>`: print the rest of the line verbatim.
    Echo(&'a [u8]),
    /// `caps`: print this shell's own endowment. `caps <command>` (a tail) previews what that
    /// command would grant; the tail is carried for the shell to re-parse. The tail is the command
    /// you would have typed, unprefixed, which is the whole point of the spelling: what you inspect
    /// and what you run cannot drift apart.
    Caps(&'a [u8]),
    /// `time <command>`: run that command exactly as typed and say how long it took.
    ///
    /// **The second prefix word, and it carries a whole command line for [`Caps`](Command::Caps)'s
    /// reason**: `time date | wc` must time the pipeline, so the tail keeps its operators and the
    /// shell re-dispatches it down the path it would have taken untimed. What you time is what you
    /// run, the same way `caps` inspects what you would run.
    ///
    /// **It grants nothing.** The tail's endowment is the tail's; the *timing* is done with the
    /// shell's own clock, so a child holding no clock at all can still be timed and no authority
    /// moves because the word is on the line. That is what makes it a prefix word rather than a
    /// program: a `time` program would have to be handed the thing it timed.
    Time(&'a [u8]),
    /// `xargs <command>`: run that command once per **batch** of what its pattern matched
    /// (milestone 109). The name is provisional.
    ///
    /// **The third prefix word, and it is a prefix word for the same reason the other two are**: its
    /// operand is a whole command line, and what it changes is how the shell *runs* the line rather
    /// than anything any program does. It grants nothing itself, which is the test [`Command::Time`] states:
    /// a batching *program* would have to be handed the union of every batch, and there is no
    /// carrier for that union. That is the premise of the whole milestone, so a program in the
    /// middle could only hold the **directory**, which is milestone 47's rejected
    /// "directory plus a name list" answer wearing a different hat.
    ///
    /// Unix can make `xargs` a program precisely because its `xargs` holds *nothing*: names in a
    /// pipe are text, and `rm`'s authority comes from its uid. Here a name in a pipe is still text,
    /// so a `find | xargs rm` analogue would spawn an `rm` holding no authority at all. The pipe
    /// cannot carry the thing that has to be batched.
    Xargs(&'a [u8]),
    /// `cd [path]`: rebind the shell's position inside the directory capability it already holds.
    /// **A builtin, in the same category as `caps`**: it spawns nothing, grants nothing, and confers
    /// no new authority, because a working directory *is* a capability the shell holds used as the
    /// default base for resolving names (see [`nav`]).
    ///
    /// An empty operand means **your root**. There is no `HOME` here, and the root of your namespace
    /// is the one distinguished place you have: it is exactly what you were granted.
    Cd(&'a [u8]),
    /// `pwd`: print where you are, **relative to your root** ([`nav::Cwd::render`]). Naming anything
    /// above it would imply a namespace that does not exist.
    Pwd,
    /// `ls [path]`: enumerate a directory you can reach, the cwd by default. A builtin rather than a
    /// program, which retires the worry that a listing program would be over-granted: it would have
    /// to hold the power to read everything it lists.
    Ls(&'a [u8]),
    /// `mkdir <path>`: make a directory and, in the same verb, obtain a capability to it
    /// (`fs_proto::fs::MKDIR` is descend-with-creation). Needs `CREATE` **and** `DESCEND`.
    Mkdir(&'a [u8]),
    /// `apropos <term>`: **name the installed pages that mention a word** (milestone 40 phase 2).
    /// The operand is one word, folded the way the host builder folded the text it indexed.
    ///
    /// **A builtin, in [`Ls`](Command::Ls)'s category rather than [`Prog::Rm`]'s**, and the
    /// argument is the one that milestone's own note reached: search *is* enumeration, the shell
    /// already holds enumeration over what it can see, and a searching *program* would have to be
    /// handed a capability to the whole documentation store in order to read every shard in it.
    /// That is a wider grant than the answer needs and a new principal holding it, for a command
    /// that moves no authority at all.
    ///
    /// **What it produces is names, never capabilities.** A result is a store location a person can
    /// then type at `doc`, and *that* line is where the grant happens, resolved against the
    /// directory the shell holds exactly as any other designation is. So search cannot widen what
    /// its caller could already reach, which is the property `doc notes/ipc-naming.md` rests on.
    Apropos(&'a [u8]),
    /// A program invocation: `<prog> [--mem N] [token ...]`. Named `Run` for the act of running a
    /// program, not for a verb on the line; milestone 47 deleted the verb. A first word that is not
    /// a builtin lands here even when no such program exists, and [`plan`] answers
    /// [`Refusal::NoSuchProgram`], which is the "there is nothing there to name" shape of
    /// no-ambient-authority applied to programs.
    Run(RunSpec<'a>),
}

/// The most positional tokens a [`RunSpec`] keeps. Two would cover every manifest shape today (an
/// argument and a file); the headroom exists so a third token is *refused* rather than dropped
/// before anything looks at it.
pub const MAX_POSITIONALS: usize = 4;

/// The parsed form of a program invocation, before the manifest check. [`plan`] turns it into an
/// [`Endowment`] or a [`Refusal`].
///
/// **The positional tokens are deliberately unclassified here.** The parser knows the shape of a
/// token, not what it means: which one is the integer argument and which is the file is a question
/// only the program's [`Manifest`] can answer, and milestone 47 removed the `file:` prefix precisely
/// because the manifest was already answering it. Keeping them in order and letting [`plan_against`]
/// place them is what makes that true in the code rather than only in the prose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RunSpec<'a> {
    /// The program name as typed (may not resolve).
    pub prog: &'a [u8],
    /// `--mem N`, if given. Recognized before or after the program name, because with the `run`
    /// verb gone the line reads `budgeter --mem 16` the way every other command line does.
    pub mem: Option<u64>,
    /// The positional tokens after the program name, in order, filled prefix in `pos[..npos]`.
    pos: [&'a [u8]; MAX_POSITIONALS],
    npos: usize,
    /// The short-option letters typed, in order, filled prefix in `opts[..nopts]`. Collected without
    /// knowing the program, because **an option letter is not authority**: what it may do is widen a
    /// grant, and that is decided in [`plan_against`] against the manifest, at the prompt, before
    /// anything is spawned. A letter no manifest declares is [`Refusal::NoSuchOption`] there.
    opts: [u8; MAX_FLAGS],
    nopts: usize,
    /// Which of [`positionals`](RunSpec::positionals) were quoted, in the same order. A quoted word
    /// is **never a pattern**, whatever bytes are in it, which is the one authority-visible thing
    /// quoting does: `rm "*.txt"` designates one name and `rm *.txt` designates a set.
    posq: [bool; MAX_POSITIONALS],
    /// A token nothing can place: a flag-shaped token that is not `--mem`, or one past
    /// [`MAX_POSITIONALS`]. Kept so the shell refuses it rather than silently ignoring authority the
    /// user thought they were granting. A **flag** that fell through to the file position would be
    /// the one way a typo could turn into a capability transfer, so it never reaches that position.
    pub unexpected: Option<&'a [u8]>,
    /// **The line's quotes do not make sense** ([`Refusal::UnclosedQuote`] or
    /// [`Refusal::PartlyQuoted`]), carried rather than returned because [`parse_run`] has no error
    /// channel and because this is a fact about the *line*, not about any program.
    ///
    /// [`plan_against_with`] answers it **before** it resolves the program, which is the one place
    /// the ordering matters: a line whose words cannot be read has no reliable first word either, so
    /// "no such program" would be a true sentence about a name nobody typed.
    pub misquoted: Option<Refusal>,
}

impl<'a> RunSpec<'a> {
    /// The positional tokens, in the order typed. [`plan_against`] places them into the slots the
    /// manifest declares; nothing else should interpret them.
    pub fn positionals(&self) -> &[&'a [u8]] {
        &self.pos[..self.npos]
    }

    /// The short-option letters typed, in order. Meaningless until a manifest says which exist.
    pub fn options(&self) -> &[u8] {
        &self.opts[..self.nopts]
    }

    /// **Whether positional `i` was quoted**, which is the whole of what quoting decides about
    /// authority: a quoted word designates itself and is never expanded, so `rm "*.txt"` hands over
    /// one name where `rm *.txt` hands over a set.
    ///
    /// `false` for an index past the end, because a word that is not there was not quoted.
    pub fn quoted(&self, i: usize) -> bool {
        i < self.npos && self.posq[i]
    }
}

/// The most short-option letters one invocation may carry (`rm -rfv` is three). A ceiling rather
/// than a limit anybody meets, and past it the line is [`Refusal::Unexpected`] rather than quietly
/// short of an option the user typed.
pub const MAX_FLAGS: usize = 8;

/// What a valid `run` resolves to: exactly the authority to hand the child, and nothing else.
/// Reading this is reading the whole endowment, which is §14's "one literal tells you a process's
/// authority" made concrete.
///
/// **It borrows nothing from the command line**, and that stopped being an accident with milestone
/// 47's globbing lane: a name a pattern produced comes out of a directory listing rather than out of
/// the line, so the grant has to own it. What falls out is the rule the rest of a grant already
/// followed by hand ([`FileGrant::dir`] is a value, not a pointer at the shell): once an endowment
/// is planned, nothing that happens afterwards can change what it means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endowment {
    pub prog: Prog,
    /// The integer argument to start it with (0 when the program takes none).
    pub arg: u64,
    /// Pages of untyped to split from the shell's own budget and grant (0 = none).
    pub mem_pages: u64,
    /// The one file to narrow a directory capability down to, and the direction, or `None`.
    /// Delivered as an endpoint served by a file caretaker (`user/src/fs_file_caretaker.rs`), so
    /// what the child ends up holding designates this name and nothing else.
    pub file: Option<FileGrant>,
    /// The one directory to narrow down to, and the **names** in it the program is to act on, or
    /// `None`. Delivered as an endpoint served by a caretaker, so what the child ends up holding
    /// reaches that directory and nothing above or beside it: `user/src/fs_subtree_caretaker.rs`
    /// for a set of one, `user/src/fs_nameset_caretaker.rs` for the set a pattern matched.
    pub dir: Option<DirGrant>,
    /// **The short options that were on the line**, as a bitmask: bit `i` is set when the manifest's
    /// `flags[i]` was typed. Numbered by position in the manifest rather than by letter, so nothing
    /// here has to know what any option means; the program and the manifest agree on the order, and
    /// a host test pins them (`grant_plan::rmopt`).
    pub flags: u64,
    /// **What goes in the child's output slot** (milestone 50): the shell's own result endpoint,
    /// the pipe to the next stage, or a file sink. Reading this line is reading everything `>` and
    /// `|` did, and it is one field because they are one mechanism.
    pub sink: line::Sink,
    /// **What goes in the child's input slot**: nothing, the pipe from the previous stage, or a
    /// file streamed out over the same contract. [`sink`](Endowment::sink)'s mirror.
    pub source: line::Source,
    /// **Where this program's declared second stream goes** (milestone 50's `2>`, DECISIONS §67).
    /// [`line::Diagnostics::None`] for the programs that declare none, which is all of them but
    /// `date`; the shell mints and delegates a second endpoint only for the rest.
    pub diagnostics: line::Diagnostics,
    /// Grant the shared result endpoint.
    pub reports: bool,
    /// Wire the two-tier `^C` path for this job (mirrors the manifest). When true the shell mints
    /// the per-job interrupt channel and runs the escalation policy while the job is foreground.
    pub interruptible: bool,
    /// **Whether this stage may write before it has read to the end** (mirrors
    /// [`InputSpec::Required::writes_while_reading`]; `false` for a program that reads nothing,
    /// which cannot be fed and so cannot be the thing that blocks).
    ///
    /// It is carried on the endowment rather than looked up from `prog.manifest()` because
    /// [`check_chain`] asks about a **whole planned line** and because [`plan_against_with`] can be
    /// given an explicit manifest. That is what keeps the check live, tested logic rather than a
    /// branch nothing reaches: no shipped program declares `true` yet, and the host tests plan one
    /// that does, exactly as `FileSpec::Required` is exercised.
    pub writes_while_reading: bool,
}

/// One resolved per-file grant: the directory it was resolved against, the name the command
/// designated, and the direction the program's manifest declared. Those three are the whole
/// authority; `fs_proto::grant` packs the name and the direction into the file caretaker's start
/// arguments, and the directory is which capability the caretaker is handed to narrow.
///
/// # The cwd stops at the process boundary, and this value is where that is true
///
/// `wc report.txt` resolves the name against the shell's position **at the moment the grant is
/// made**, and this is that moment: [`plan_against`] walks any leading path once, here, and records
/// where it landed. The child receives a capability to that one file. It has no cwd, inherits
/// nothing, and cannot re-resolve anything, which is the point: a child that could re-resolve a name
/// later would have ambient authority smuggled in through a string.
///
/// That [`dir`](FileGrant::dir) is a **value** rather than a reference to the shell's position is
/// what makes it structural instead of a promise. A `cd` after this grant is planned cannot change
/// what the grant means, because there is nothing here that points at the shell any more.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FileGrant {
    /// The directory the name was resolved in, relative to the shell's root, fixed at plan time.
    pub dir: nav::Cwd,
    /// The final component. Always a single component: a path was resolved into `dir`, not passed
    /// on, because the FS contract takes one component per request and no server here walks a path.
    ///
    /// Owned rather than borrowed from the line, because a pattern that matched exactly one file is
    /// a legitimate way to designate one and that name came from a directory listing.
    pub name: Name,
    pub writable: bool,
}

/// One resolved **directory** grant (milestone 47's `rm -r`, generalized by its globbing lane): the
/// directory the child is handed a capability to, the **set of names** in it the program was told to
/// act on, and whether that capability must reach below the directory or only name what is directly
/// in it.
///
/// It is [`FileGrant`]'s shape one rung up, and the same rule holds for the same reason: `dir` is
/// **where the operand's leading path landed, fixed at plan time**, as a value rather than a pointer
/// at the shell, so a later `cd` cannot change what an already-planned grant means. The child holds
/// a capability to that directory, has no cwd, and cannot re-resolve anything.
///
/// # Why the name is a set
///
/// `rm old.txt` designates one name and `rm *.txt` designates five hundred, and the roadmap's four
/// candidate answers to what the second grants have one survivor: **a directory capability
/// attenuated to a name set**. Five hundred file capabilities exhausts capability slots; the
/// directory plus a name list over-grants catastrophically (the program could touch anything in that
/// directory, which is the thing this model refuses); making `rm` a builtin dodges the question and
/// costs `rm` as a program.
///
/// So [`names`](DirGrant::names) is a set, a literal operand is the set of one, and the
/// generalization is smaller than it looks: `fs_file_caretaker` already serves a namespace of
/// exactly one name, and `user/src/fs_nameset_caretaker.rs` serves the same protocol over a wider
/// one. **Nothing new in the kernel.**
///
/// **These three together are the whole authority**, which is what makes `caps rm -r logs` worth
/// typing: the names at risk are printed before anything happens, and a bug in the program's
/// recursion can only reach what this designates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DirGrant {
    /// The directory the capability designates, relative to the shell's root, fixed at plan time.
    pub dir: nav::Cwd,
    /// The names in it the program is to act on: exactly one for a literal operand, the matched set
    /// for a pattern. Each is a single component, for [`FileGrant`]'s reason: a path was resolved
    /// into `dir` rather than passed on.
    pub names: NameSet,
    /// The capability must reach **below** this directory (walk into it and list it), because the
    /// program was given the option its manifest declares for that. False means it may take names
    /// out of this one directory and cannot even see what is under it.
    pub subtree: bool,
}

/// **`rm`'s options**, by the bit they occupy in [`Endowment::flags`], which is their position in
/// [`Prog::Rm`]'s manifest. Named here so the shell, the program and the tests share one definition
/// rather than three copies of an ordering; `rm_options_are_numbered_by_their_position` pins them.
pub mod rmopt {
    /// `-r`: recurse. It is also `rm`'s `subtree_flag`, so typing it visibly widens the grant.
    pub const RECURSIVE: u64 = 1 << 0;
    /// `-f`: ignore a name that is not there, and do not let its absence change the exit status.
    /// It is **not** "ignore errors": a removal that fails for any other reason still reports.
    pub const FORCE: u64 = 1 << 1;
    /// `-v`: say what was removed. It exists because the default says nothing.
    pub const VERBOSE: u64 = 1 << 2;
}

/// **What the shell itself holds**, which is what decides whether a designation can be backed at all.
///
/// This is why it is a parameter rather than a constant. "You hold no such capability" must be a
/// statement about the shell's actual cspace, not a hardcoded era: the same command line is a
/// refusal in a shell that was granted no directory and a real grant in one that was, and neither
/// the parser nor the manifest can tell them apart. Phase 1 hardcoded the refusal, which was true
/// then and would have quietly become a lie.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Holdings {
    /// The shell holds a directory capability it can narrow into a per-file grant.
    pub dir: bool,
    /// **Where in that capability it currently is**, which is the other half of what it holds: a
    /// working directory is a directory capability used as the default base for resolving names, so
    /// it belongs here beside the fact that there is one. Defaults to the root, which is also the
    /// only position a shell holding no directory can be in.
    pub cwd: nav::Cwd,
}

/// Why an invocation was refused, decided at the prompt before any spawn. Each variant maps to one
/// legible line; [`Refusal::message`] is the fixed half, host-tested so the wording cannot drift.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// The named program does not exist. Also where a mistyped builtin lands, since with the `run`
    /// verb gone the first word is either a builtin or a program name.
    NoSuchProgram,
    /// The command designated a resource this shell holds no capability for: a file named at a
    /// program that takes one, in a shell that was never granted a directory to narrow. This is the
    /// refusal the milestone is about: not "permission denied" but "there is nothing you hold that
    /// could grant this."
    NoSuchCapability(CapKind),
    /// The program takes no file, but a token was left over that nothing else could be. The
    /// milestone's inversion cuts both ways: a name the program has no use for is authority the user
    /// did not mean to move, so it is refused rather than granted-and-ignored. **This is the refusal
    /// the `file:` prefix was wrongly credited with**: `worker 5 extra` stops here because worker's
    /// manifest says `FileSpec::Forbidden`, which is what refused it before the prefix went away.
    FileForbidden,
    /// The program is endowed a file and the command named none. Naming it on the line is the only
    /// way it can get one, so this is caught at the prompt rather than as an empty slot at runtime.
    FileRequired,
    /// The program is endowed a **directory** and the command named nothing in one. `rm` with no
    /// operand is this: there is no default, because a default would be a grant nobody typed.
    PathRequired,
    /// A short option this program does not declare. Refused at the prompt rather than handed over
    /// for the program to ignore, and it is the same rule as an unplaceable token: **the line means
    /// exactly what it says or it does not run.** It matters more than it looks here, because an
    /// option can widen a grant (`rm -r`), so a shell that silently dropped one would be a shell
    /// whose printed preview and actual transfer could disagree.
    NoSuchOption,
    /// The named file is not something this shell can express: empty, a component longer than the
    /// two argument words a grant's name rides in (`fs_proto::grant::MAX_NAME`), deeper than the
    /// shell tracks, or a path that designates a directory rather than a file.
    FileNotNameable,
    /// The program takes no memory grant, but `--mem` was given.
    MemForbidden,
    /// The program requires a memory grant, but none was given.
    MemRequired,
    /// `--mem N` was outside the program's declared range.
    MemOutOfRange { min: u64, max: u64 },
    /// The program requires an integer argument, but none was given.
    ArgRequired,
    /// The program takes no argument, but one was given.
    ArgForbidden,
    /// **A pattern matched no name here** (milestone 47's globbing lane). zsh's default rather than
    /// bash's pass-the-pattern-through, and the model forces it: the expansion *is* the grant, so an
    /// empty expansion is an empty grant and running the command would be running it with an
    /// authority nobody named. See [`expand::Expander::finish`].
    NoMatch,
    /// **A pattern matched more names than one grant can carry.** Unix's `ARG_MAX` with a more
    /// honest cause: not a buffer that ran out but a capability the child could neither hold nor be
    /// shown. Loud at the prompt with nothing spawned, because a glob that quietly granted a prefix
    /// of what it matched is the worst outcome this mechanism has.
    TooManyNames,
    /// A name a pattern matched cannot travel in a grant (longer than
    /// [`expand::MAX_NAME`]). Refused whole rather than dropped from the set, for
    /// [`Refusal::TooManyNames`]'s reason.
    MatchNotNameable,
    /// A pattern somewhere other than a token's **last** component (`*/report.txt`). Selecting
    /// directories to walk is an authority question, not a matching one, so it is not something a
    /// string matcher gets to answer (notes/glob.md).
    PatternInPath,
    /// A pattern reached the grant planner with no expansion behind it. It is a fact about the
    /// shell rather than about the line: expanding needs `ENUMERATE` on the directory, and a shell
    /// that holds none cannot say what the pattern designates. Never a silent grant of a name with a
    /// `*` in it, which is a name no directory here has.
    Unexpanded,
    /// A pattern matched more than one name at a program that is granted exactly one **file**. The
    /// count is the designation, so this is a fact about the line rather than a policy.
    AmbiguousFile,
    // ---- milestone 50's operators. Every one of these is decided before anything is spawned,
    // which matters more here than elsewhere: a pipeline that half-starts leaves a process blocked
    // on a channel nobody will ever write to.
    /// **`>` or `<` with nothing after it.** A redirection is a designation, and there is nothing
    /// designated.
    NoRedirectTarget,
    /// **One stage, two destinations** (`date > a > b`), or two inputs. Neither "the last wins" nor
    /// "the first wins" is a rule anybody should have to remember.
    RedirectRepeated,
    /// **A redirection somewhere a pipeline already answers the question.** `a > f | b` says where
    /// `a`'s bytes go and then pipes them somewhere else. Refused rather than resolved by
    /// precedence.
    RedirectMidPipeline,
    /// **A word after a redirection's name** (`< f wc`). The spelling this shell takes is `wc < f`.
    /// See notes/pipes.md: refusing the other order is what keeps a stage's text a slice of the
    /// line in a shell with no allocator.
    WordAfterRedirect,
    /// **A pipeline stage with no command in it** (`| wc`, `a |`).
    ///
    /// **This listed `a || b` until milestone 67 and no longer can**: `||` is a connector now, and
    /// the shell splits on it before [`line::split`] ever sees the line, so that spelling is two
    /// commands rather than a pipeline with a hole in it. A `||` still reaches this function in a
    /// direct call, which is why the sequence splitter has to run first rather than merely usually
    /// running first.
    EmptyStage,
    /// **More stages than one line may carry** ([`line::MAX_STAGES`]). A ceiling on processes and
    /// endpoints, not on a buffer.
    TooManyStages,
    /// **A pattern where a redirection's name goes.** A redirection designates one destination and
    /// a pattern designates a set, so even a pattern that matched exactly one name would make what
    /// the line writes to depend on what is in the directory.
    PatternInRedirect,
    /// **This program's output is not a byte stream**, so there is nothing for `>` or `|` to
    /// substitute. See [`OutputSpec`]: `worker 9` answers with a number in a register, and a file
    /// sink handed that word would write nothing legible into the file.
    NotAByteStream,
    /// **This program declares no second output stream**, and the line put a `2>` on it (DECISIONS
    /// §67). The truthful refusal the decision names: `2>` binds to something the manifest offers,
    /// and a program whose diagnostics ride its one output in-band has nothing for it to bind to.
    /// It is not "you may not do that"; it is "there is no second stream here to name", which is
    /// [`NoSuchProgram`](Refusal::NoSuchProgram)'s shape applied to a stream.
    NoDiagnosticStream,
    /// **This program reads no input**, and the line gave it one. Authority that moved for no
    /// reason, refused for the same reason an unplaceable token is.
    InputForbidden,
    /// **This program reads an input stream and the line gave it none.** `wc` on its own would
    /// block on a receive forever; the manifest says so, so the prompt can. **The refusal Unix
    /// cannot produce**, because there fd 0 always exists.
    InputRequired,
    /// **This shell would be both the writer and the reader of the line, and it can only wait on
    /// one thing.** Every stage declares that it writes while it reads
    /// ([`InputSpec::Required::writes_while_reading`]), so nothing in the chain absorbs the stream,
    /// and the shell's feed would block against a stage that is blocked writing back.
    ///
    /// It is a fact about **this shell** rather than about the program, which is why the fix is to
    /// put a reader after it (`| wc`) rather than to change anything the program does. See
    /// [`check_chain`] and notes/pipes.md: with no select, no poll and no timed receive, one wait
    /// point per process is the whole of the reason.
    NoReaderButThisShell,
    /// **A builtin in a pipeline stage that the shell cannot supply.** `cd | wc` is the shape: a
    /// builtin rebinds what the shell holds and produces no stream. The producing builtins (`echo`,
    /// `ls`, `pwd`) are not this, because the shell writes their bytes into the pipe itself.
    NotAPipelineStage,
    /// A token no slot in the manifest can hold, and that no other refusal reads truer for: a
    /// second integer at a program that takes one, a flag that is not `--mem`, a name past the one
    /// file a program is granted. Refused rather than ignored, so authority the user thought they
    /// were granting never silently evaporates.
    Unexpected,
    // ---- milestone 67's quoting and sequencing. Both are facts about the *line*, decided before
    // any manifest is consulted, which is why they sit at the end rather than beside the grant
    // refusals above.
    /// **A quote that never closes** (`wc 'my notes.txt`). The line is refused whole rather than
    /// run to the last complete word, because a line missing its end designates something nobody
    /// typed. See [`word`].
    UnclosedQuote,
    /// **A quote that wraps part of a word** (`a"b"`, `'it''s'`). Every token here is a slice of
    /// the line you typed, so two quoted pieces cannot be joined into one word; the alternative is
    /// a byte buffer this shell has no allocator for. Refused rather than misread, because both
    /// readings (join them, or take the quotes literally) would be silently wrong. See [`word`].
    PartlyQuoted,
    /// **A connector with nothing on one side of it** (`&& date`, `date &&`). The mirror of
    /// [`EmptyStage`](Refusal::EmptyStage) one level out: `&&` asks about a command that ran, and
    /// there is no command.
    EmptySegment,
    /// **More commands on one line than the shell will sequence.** A ceiling like
    /// [`TooManyStages`](Refusal::TooManyStages), and a line that wants more is better written as
    /// several lines than silently truncated.
    TooManySegments,
}

/// The kind of capability a command designated but the shell cannot back. Phase 1 has one; the
/// enum exists so milestone 32 (files) and later device/endpoint grants slot in without changing
/// the refusal's shape.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CapKind {
    /// A file or directory, designated by naming it. Arrives with milestone 32's FS server.
    File,
}

impl Refusal {
    /// The fixed, program-independent half of the refusal line. The shell prefixes the program
    /// name or the offending token where a variant needs it. The strings are the deliverable's
    /// voice: a refusal that reads like the capability model, not like errno.
    pub fn message(self) -> &'static str {
        match self {
            // The hint is not decoration. With `run` gone, a mistyped builtin and a mistyped
            // program name arrive at the same refusal, so this line has to be able to point at
            // both halves of what the prompt understands.
            Refusal::NoSuchProgram => "no such program (try 'help' for the builtins)",
            Refusal::NoSuchCapability(CapKind::File) => {
                "you hold no such capability: this shell was granted no directory to narrow"
            }
            // "drop the name", in the same voice as "drop the --mem": the token would have been a
            // file, and this program is declared to take none.
            Refusal::FileForbidden => "takes no file; drop the name",
            Refusal::FileRequired => "is granted one file; name it on the line",
            Refusal::PathRequired => "is granted the directory holding what you name; name it",
            Refusal::NoSuchOption => "no such option (this shell will not pass one along unread)",
            Refusal::FileNotNameable => {
                "that is not a name this shell can grant: at most 16 bytes a component, 8 deep"
            }
            // Each of these says what the *pattern* designates, because that is the grant. None of
            // them can be phrased as a permission, and none should be: nothing was denied, the
            // command either named something grantable or it did not.
            Refusal::NoMatch => "no name here matches that pattern, so there is nothing to grant",
            Refusal::TooManyNames => {
                "that pattern matched more names than one grant can carry (at most 8)"
            }
            Refusal::MatchNotNameable => {
                "a name it matched cannot travel in a grant: at most 16 bytes"
            }
            Refusal::PatternInPath => {
                "a pattern names files, not directories to walk: only the last component may be one"
            }
            Refusal::Unexpanded => {
                "there is nothing here to expand that pattern against: this shell holds no directory"
            }
            Refusal::AmbiguousFile => {
                "is granted one file, and that pattern matched more than one name"
            }
            Refusal::MemForbidden => "takes no memory grant; drop the --mem",
            Refusal::MemRequired => "needs a memory grant; add --mem <pages>",
            Refusal::MemOutOfRange { .. } => "memory grant is out of the range it declares",
            Refusal::ArgRequired => "needs an integer argument",
            Refusal::ArgForbidden => "takes no argument",
            Refusal::Unexpected => {
                "unexpected argument (this shell will not grant what it cannot place)"
            }
            // The operators' refusals. Each says what the line designates rather than what was
            // denied, in the same voice as the grant refusals above: nothing here consults a
            // permission, and a pipeline that could not be built is a line that did not name one.
            Refusal::NoRedirectTarget => "a redirection names a file; this one names nothing",
            Refusal::RedirectRepeated => {
                "one output and one input to a stage: this line names two of one of them"
            }
            Refusal::RedirectMidPipeline => {
                "the pipeline already says where that goes: only the first stage takes input and \
                 only the last is redirected"
            }
            Refusal::WordAfterRedirect => {
                "the redirection goes last: write 'wc < report.txt', not '< report.txt wc'"
            }
            Refusal::EmptyStage => "a pipeline stage with no command in it",
            Refusal::TooManyStages => "that is more stages than one line carries (at most 4)",
            Refusal::PatternInRedirect => {
                "a redirection names one file, and a pattern designates a set"
            }
            Refusal::NotAByteStream => {
                "does not write a byte stream, so there is nothing for > or | to redirect"
            }
            Refusal::NoDiagnosticStream => {
                "declares no second output, so there is nothing for 2> to name (its diagnostics \
                 ride its output)"
            }
            Refusal::InputForbidden => "reads no input; there is no slot for those bytes to go in",
            Refusal::InputRequired => {
                "reads an input stream: name a file, redirect with '<', or pipe into it"
            }
            Refusal::NotAPipelineStage => {
                "is a builtin that produces no stream, so it cannot be a stage of a pipeline"
            }
            // Phrased about the line rather than about the program, because it is true of neither
            // one alone: the program is fine and the shell is fine, and what cannot be done is this
            // shell feeding it and reading it at the same time.
            Refusal::NoReaderButThisShell => {
                "writes while it reads, and this shell can only wait on one thing at a time: give \
                 it a reader that is not this shell, as in '| wc'"
            }
            // Milestone 67's. Each names what the line is missing rather than what it did wrong,
            // and the quoting pair say *why* the shape is the shape: a token here is a slice of
            // what you typed, so there is nothing to join pieces into.
            Refusal::UnclosedQuote => "a quote here is never closed, so the line ends mid-name",
            Refusal::PartlyQuoted => {
                "quote a whole word: this shell hands a name over as the bytes you typed, so it \
                 cannot join pieces of one"
            }
            Refusal::EmptySegment => "a connector with no command on one side of it",
            Refusal::TooManySegments => "that is more commands than one line sequences (at most 8)",
        }
    }
}

/// Split a command line into whitespace-separated tokens, writing them into `out` and returning
/// the filled prefix. A tiny `no_std` tokenizer: no allocation, bounded by `out.len()`. Tokens
/// past `out.len()` are dropped (a command line with more than a handful of tokens is a mistake,
/// not a workload).
///
/// **Whitespace inside quotes is not a separator** (milestone 67). The quotes stay on the token,
/// because this function's job is where a token ends and [`word::read`]'s is what it means; a
/// caller that wants the name rather than the spelling asks for it. That split is what lets
/// [`parse_run`] refuse `a"b"` in one place instead of every scanner having to.
pub fn tokenize<'a, 'b>(line: &'a [u8], out: &'b mut [&'a [u8]]) -> &'b [&'a [u8]] {
    let mut n = 0;
    let mut i = 0;
    let mut c = word::Cursor::new();
    while i < line.len() && n < out.len() {
        // A whitespace byte can never open or close a quote, so the cursor's state cannot change
        // while it is being skipped and this loop does not have to step it.
        while i < line.len() && !c.open() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < line.len() && !(!c.open() && line[i].is_ascii_whitespace()) {
            c.step(line[i]);
            i += 1;
        }
        if i > start {
            out[n] = &line[start..i];
            n += 1;
        }
    }
    &out[..n]
}

/// Parse a whole command line into a [`Command`]. Pure and allocation-free.
///
/// The grammar is small: the first token selects the command, and a first token that is not a
/// builtin is a **program name**, because milestone 47 deleted the `run` verb. `echo` keeps the
/// rest of the line verbatim (so `echo   two  spaces` prints its spaces); everything else works on
/// tokens.
///
/// Builtins win over programs, which is why [`Prog::from_name`] must never learn a name that is
/// also a builtin. A handful of reserved words is the whole cost of a shell where every command is
/// typed the same way.
///
/// **Two of them are prefix words**, `caps` and `time`, and they are the ones that keep the rest of
/// the line intact instead of tokenizing it: what they operate on is a command line, not an operand.
pub fn parse(line: &[u8]) -> Command<'_> {
    let trimmed = trim(line);
    if trimmed.is_empty() {
        return Command::Empty;
    }
    let (first, rest) = split_first_word(trimmed);
    // **A quoted builtin name still names the builtin**, because quoting decides what a word *is*
    // and this match is about what the word says. A first word whose quotes do not make sense falls
    // through to [`parse_run`], which is the one place a quoting refusal is raised; raising it here
    // as well would be a second copy of the same sentence.
    let first = match word::read(first) {
        Ok(w) => w.text,
        Err(_) => return Command::Run(parse_run(trimmed)),
    };
    match first {
        b"help" => Command::Help,
        b"echo" => Command::Echo(rest),
        b"caps" => Command::Caps(rest),
        // The second prefix word (milestone 86). Like `caps`, its operand is a whole command line
        // rather than a token, so `rest` is kept unsplit and the shell re-dispatches it.
        b"time" => Command::Time(rest),
        // The third (milestone 109), and the same shape for the same reason: batching is about how
        // the shell hands a designation over, so its operand is the line it batches.
        b"xargs" => Command::Xargs(rest),
        // The navigation builtins (milestone 47). They take a path operand rather than tokens,
        // because a path is one designation however many slashes it has, and `trim` is all the
        // classification they need.
        b"cd" => Command::Cd(trim(rest)),
        b"pwd" => Command::Pwd,
        b"ls" => Command::Ls(trim(rest)),
        b"mkdir" => Command::Mkdir(trim(rest)),
        // **Search the documentation store** (milestone 40 phase 2). A builtin for exactly
        // [`Command::Ls`]'s reason, stated there: a search *is* an enumeration, and a searching
        // program would have to be handed the store's directory in order to read every shard in it.
        // The operand is a word rather than a path, so `trim` is all the classification it needs.
        b"apropos" => Command::Apropos(trim(rest)),
        // **`rm` is deliberately not here.** It was a builtin in the commands lane and is a program
        // now (milestone 47's rmdir lane): a builtin runs with the shell's whole endowment, and a
        // destructive loop should take an explicit attenuated grant instead. Since builtins are
        // matched first, deleting the line is the entire change at this level; `Prog::from_name`
        // picks the word up.
        // Not a builtin, so it is a program invocation, and the whole line (name included) is the
        // grant expression. Whether the program exists is `plan`'s question, not the parser's.
        _ => Command::Run(parse_run(trimmed)),
    }
}

/// Parse a whole program invocation (the line, starting at the program name) into a [`RunSpec`].
///
/// It recognizes `--mem N` in any position and takes the first other token as the program name;
/// every token after that is kept **in order and unclassified**, because deciding which one is the
/// argument and which is the file needs the manifest (see [`plan_against`]). The one thing it does
/// classify is a flag: a token starting with `-` that is not `--mem` is refused rather than allowed
/// to fall into the file position, which is the single way a typo could otherwise become a
/// capability transfer.
pub fn parse_run(line: &[u8]) -> RunSpec<'_> {
    let mut toks: [&[u8]; 16] = [b""; 16];
    let toks = tokenize(line, &mut toks);

    let mut mem = None;
    let mut prog: &[u8] = b"";
    let mut pos = [&b""[..]; MAX_POSITIONALS];
    let mut posq = [false; MAX_POSITIONALS];
    let mut npos = 0;
    let mut opts = [0u8; MAX_FLAGS];
    let mut nopts = 0;
    let mut unexpected = None;
    let mut misquoted = None;
    let mut have_prog = false;

    let mut i = 0;
    while i < toks.len() {
        // **The quotes come off before anything is classified** (milestone 67), because what a
        // token *is* is what it says: `"--mem"` is a name and `--mem` is an option, and deciding
        // that after the flag check would make a quoted name into a flag.
        let (t, quoted) = match word::read(toks[i]) {
            Ok(w) => (w.text, w.quoted),
            Err(r) => {
                if misquoted.is_none() {
                    misquoted = Some(r);
                }
                i += 1;
                continue;
            }
        };
        if !quoted && t == b"--mem" {
            // The next token is the page count. A missing or non-numeric value leaves mem None,
            // which the plan turns into MemRequired for a program that needs it.
            if i + 1 < toks.len() {
                // Unquoted first, so `--mem "16"` is the same grant as `--mem 16`. Quoting decides
                // what a word is and this one has to be a number either way.
                mem = word::read(toks[i + 1]).ok().and_then(|w| parse_u64(w.text));
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // A quoted token is never an option, which is the same rule one line up: `rm "-r"` names a
        // file called `-r`, and reading it as the flag that widens a directory grant into a subtree
        // walk would be the loudest possible version of a typo becoming a capability transfer.
        if !quoted && t.first() == Some(&b'-') {
            // A cluster of short options (`-r`, `-rf`), kept for the manifest to check. Anything
            // else shaped like a flag is refused: it never becomes a program name and never reaches
            // the file position, because an unknown flag silently designating a file is exactly the
            // typo-into-a-grant the old `file:` prefix was there to prevent, and this is the part
            // of that job worth keeping.
            let letters = &t[1..];
            let short = !letters.is_empty() && letters.iter().all(u8::is_ascii_alphabetic);
            if short && nopts + letters.len() <= MAX_FLAGS {
                for &c in letters {
                    opts[nopts] = c;
                    nopts += 1;
                }
            } else if unexpected.is_none() {
                unexpected = Some(t);
            }
        } else if !have_prog {
            prog = t;
            have_prog = true;
        } else if npos < MAX_POSITIONALS {
            pos[npos] = t;
            posq[npos] = quoted;
            npos += 1;
        } else if unexpected.is_none() {
            unexpected = Some(t);
        }
        i += 1;
    }

    RunSpec {
        prog,
        mem,
        pos,
        posq,
        npos,
        opts,
        nopts,
        unexpected,
        misquoted,
    }
}

/// Check a parsed invocation against the program's manifest and produce the exact [`Endowment`] to
/// grant, or the [`Refusal`] to print. The whole authority decision lives here: after this returns
/// `Ok`, the shell grants precisely what the `Endowment` names and the child can reach nothing
/// else.
///
/// The program is resolved first, because "there is no such program" is a fact about the system and
/// everything else is a fact about a program that exists.
/// **`expanded` is why the shell expands before it plans.** A pattern designates the names it
/// matched, so the endowment *is* the set, and the planner must see the set rather than the pattern.
/// [`Expansion::none`] is the answer for a line with no pattern on it, which is every line the shell
/// could plan before milestone 47's globbing lane.
pub fn plan(run: &RunSpec<'_>, holds: Holdings, expanded: Expansion) -> Result<Endowment, Refusal> {
    plan_stage(run, holds, expanded, Streams::default())
}

/// **What the operators decided for this stage**: which capability its output slot gets and which
/// its input slot gets. [`line::split`] and the shell between them produce it; [`plan_stage`] checks
/// it against the program's manifest.
///
/// It is a pair rather than two arguments because the two are one question asked in two directions,
/// and because a stage in the middle of a pipeline has both.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Streams {
    pub sink: Sink,
    pub source: Source,
    /// **The file a `2>` named, and what it does to what is in it**, or `None` for a line with no
    /// `2>` on it. It is the *question* the line asked; [`Endowment::diagnostics`] is the answer,
    /// because whether there is a second stream at all is the manifest's to say.
    pub diagnostics: Option<(FileGrant, line::Mode)>,
}

/// [`plan`], with the operators' answer folded in.
///
/// The stream check comes **after** the grant check and not before, deliberately. A line can be
/// wrong about both (`worker > out.txt` names no integer *and* has no bytes to write), and the
/// grant refusal is the one that reads truer: what a program is endowed is a bigger fact about the
/// line than where its output was going to go.
pub fn plan_stage(
    run: &RunSpec<'_>,
    holds: Holdings,
    expanded: Expansion,
    streams: Streams,
) -> Result<Endowment, Refusal> {
    // Ahead of resolving the program, for the reason [`RunSpec::misquoted`] states: a line whose
    // words cannot be read has no reliable first word either.
    if let Some(r) = run.misquoted {
        return Err(r);
    }
    let prog = Prog::from_name(run.prog).ok_or(Refusal::NoSuchProgram)?;
    plan_against_with(run, prog, prog.manifest(), holds, expanded, streams)
}

/// [`plan`]'s second half, against an **explicit** manifest rather than the static table.
///
/// Split out for two reasons, and the first is immediate: it is the only way to exercise a manifest
/// shape no shipped program declares yet (a program endowed a file), so `FileSpec::Required` is live,
/// tested logic instead of a branch nothing reaches. The second is milestone 23, where a manifest
/// travels *with* a component rather than living in this table, and the composer checks a program it
/// did not write. That is the same call with a different source of the manifest.
///
/// **This is where the positional tokens acquire meaning**, and the order is the order they are
/// typed in: the integer argument first if the manifest declares one, then the file if it declares
/// one. A token past the last declared slot is unplaceable, and unplaceable is a refusal.
///
/// # What globbing changed here, and why it had to
///
/// The slots used to be filled by splitting a slice, which was enough while a token was a name. A
/// token can now be a **pattern**, and a pattern designates the set the shell expanded it into, so
/// the placement is by **index** and the [`Expansion`] is keyed to that index. The alternative was
/// to let the planner infer which slot the expansion belonged to, which would have been the parser
/// classifying tokens again, one layer down and less visibly.
///
/// Two guards fall out of that, and both exist so authority cannot move silently:
///
/// - a pattern with no expansion behind it is [`Refusal::Unexpanded`], never a grant of a name with
///   a `*` in it;
/// - a token with **no** magic never consults the expansion at all, so a name that was typed always
///   designates itself.
pub fn plan_against(
    run: &RunSpec<'_>,
    prog: Prog,
    m: Manifest,
    holds: Holdings,
    expanded: Expansion,
) -> Result<Endowment, Refusal> {
    plan_against_with(run, prog, m, holds, expanded, Streams::default())
}

/// [`plan_against`], with the operators' answer. The one that does the work; the other two are the
/// spellings without a pipeline on the line.
pub fn plan_against_with(
    run: &RunSpec<'_>,
    prog: Prog,
    m: Manifest,
    holds: Holdings,
    expanded: Expansion,
    streams: Streams,
) -> Result<Endowment, Refusal> {
    // **A line whose quotes do not make sense is answered before anything else** (milestone 67),
    // because every other refusal below is a sentence about a word, and this is the one that says
    // the words could not be read. Ordering it after "no such program" would name a program the
    // user did not type.
    if let Some(r) = run.misquoted {
        return Err(r);
    }

    // A flag nothing knows, or a token past what any manifest could hold. Nothing about this
    // program's declaration can rescue it, so it is answered before the slots are filled.
    if run.unexpected.is_some() {
        return Err(Refusal::Unexpected);
    }

    // The options, before any grant is placed: a letter this program does not declare is a refusal
    // at the prompt, never a letter passed along for the program to ignore. It comes first because
    // one of them may *widen the directory grant* below, so the line has to be known-good before
    // anything is decided about how much authority it moves.
    let mut flags = 0u64;
    for &letter in run.options() {
        let bit = m
            .flags
            .iter()
            .position(|&d| d == letter)
            .ok_or(Refusal::NoSuchOption)?;
        flags |= 1 << bit;
    }

    let pos = run.positionals();
    let mut next = 0usize;

    // The argument takes the first positional, and it must be an integer: `worker eight` is a
    // missing argument, not a file named "eight", because worker's manifest declares no file.
    //
    // A pattern can never land here by accident: every magic byte (`*`, `?`, `[`, `\`) is a
    // non-digit, so `parse_u64` refuses a pattern before the expansion is ever consulted, and
    // `worker *` is "needs an integer argument" rather than a grant of anything.
    let arg = match m.arg {
        ArgSpec::Required => {
            let first = pos.get(next).ok_or(Refusal::ArgRequired)?;
            let v = parse_u64(first).ok_or(Refusal::ArgRequired)?;
            next += 1;
            v
        }
        ArgSpec::Forbidden => 0,
    };

    // The file grant takes the next positional. The name is the whole designation; the direction
    // comes from the manifest, which is why `wc report.txt` reads and `tee report.txt` writes
    // without the human typing a mode.
    let file = match m.file {
        FileSpec::Forbidden => None,
        FileSpec::Required { writable } => {
            let token = *pos.get(next).ok_or(Refusal::FileRequired)?;
            let at = next;
            next += 1;
            // What the shell holds decides whether the designation can be backed at all, and it
            // wins over the name's shape: "there is nothing I hold that could grant this" is a
            // bigger fact than "and that name is too long".
            if !holds.dir {
                return Err(Refusal::NoSuchCapability(CapKind::File));
            }
            let (dir, names) = designate(token, at, holds.cwd, expanded)?;
            // One file, so a pattern that matched more than one has designated something this
            // program cannot be handed. The count is the designation; nothing was denied.
            let name = names.only().ok_or(Refusal::AmbiguousFile)?;
            Some(FileGrant {
                dir,
                name: Name::new(name).ok_or(Refusal::FileNotNameable)?,
                writable,
            })
        }
    };

    // The directory grant takes the next positional, and it is resolved exactly as a file grant is:
    // the leading path is walked here, once, against where the shell is standing now, and the
    // result is recorded as a value. The child is handed a capability to the directory the names
    // are **in**, plus the names, because taking a name away is an operation on the directory that
    // holds it and no per-file capability can express it. See [`DirSpec`].
    let dir = match m.dir {
        DirSpec::Forbidden => None,
        DirSpec::Required { subtree_flag } => {
            let token = *pos.get(next).ok_or(Refusal::PathRequired)?;
            let at = next;
            next += 1;
            if !holds.dir {
                return Err(Refusal::NoSuchCapability(CapKind::File));
            }
            let (dir, names) = designate(token, at, holds.cwd, expanded)?;
            // Typing the recursion option is what widens the capability from "take names out of
            // this directory" to "walk what is under it". A program run without it holds no way to
            // descend, so its recursion is not disabled by a branch anybody has to get right.
            let subtree = match subtree_flag {
                Some(letter) => match m.flags.iter().position(|&d| d == letter) {
                    Some(bit) => flags & (1 << bit) != 0,
                    // A manifest naming an option it does not declare is a bug in the manifest, and
                    // the safe reading is the narrow one: no widening.
                    None => false,
                },
                None => false,
            };
            Some(DirGrant {
                dir,
                names,
                subtree,
            })
        }
    };

    // **The input operand: `wc report.txt` is `wc < report.txt` with the operator left out.**
    //
    // This is milestone 47's move applied to the other direction. The `file:` prefix came out on the
    // finding that the manifest was doing all the work, and it is doing all of it here too: a
    // program that declares `InputSpec::Required` reads a stream and nothing else, so a bare name in
    // the position after its declared grants can only be the thing feeding it. There is nothing for
    // the parser to classify and no ambiguity to resolve, because a manifest declaring an input
    // never also declares a file (a program that wanted both would need positional arity, which is
    // the same widening `ArgSpec` is waiting on).
    //
    // **What the child ends up holding is narrower than a file capability**, and that is worth being
    // exact about rather than claiming the headline. The shell opens the name and streams it, so the
    // program is handed an endpoint carrying the bytes of that one file: it cannot seek, re-read,
    // stat, or name a second thing, and it cannot tell a file from a pipe from a builtin. Typing the
    // name is still what moves the authority (`wc` alone is refused at the prompt, before anything
    // is spawned), which is designation-is-authorization; what it is *not* is the per-file capability
    // `FileSpec::Required` describes, because `wc` deliberately does not speak the filesystem
    // contract at all (milestone 50: "a `wc` that could open the file it counts would be a `wc` that
    // could open any file").
    //
    // An explicit `<` wins, because it is the same designation said out loud: a line carrying both
    // has named its input twice, and the operand then has nowhere to go and is unplaceable below.
    let mut streams = streams;
    if matches!(m.input, InputSpec::Required { .. })
        && matches!(streams.source, Source::None)
        && let Some(&token) = pos.get(next)
    {
        let at = next;
        next += 1;
        // What the shell holds decides whether the designation can be backed at all, and it wins
        // over the name's shape, exactly as it does for a file grant.
        if !holds.dir {
            return Err(Refusal::NoSuchCapability(CapKind::File));
        }
        let (dir, names) = designate(token, at, holds.cwd, expanded)?;
        let name = names.only().ok_or(Refusal::AmbiguousFile)?;
        streams.source = Source::File(FileGrant {
            dir,
            name: Name::new(name).ok_or(Refusal::FileNotNameable)?,
            // A reader reads. The direction is the manifest's here as everywhere else, and
            // `InputSpec` has only the one.
            writable: false,
        });
    }

    // Anything left designates a slot this program does not have.
    if let Some(&extra) = pos.get(next) {
        return Err(unplaceable(extra, m));
    }

    // The memory grant. A flag, not a position, so it is checked last: it cannot be confused with
    // anything and it should not shadow a refusal about a resource.
    let mem_pages = match (m.mem, run.mem) {
        (MemSpec::Forbidden, None) => 0,
        (MemSpec::Forbidden, Some(_)) => return Err(Refusal::MemForbidden),
        (MemSpec::Required { .. }, None) => return Err(Refusal::MemRequired),
        (MemSpec::Required { min, max }, Some(n)) => {
            if n < min || n > max {
                return Err(Refusal::MemOutOfRange { min, max });
            }
            n
        }
    };

    let diagnostics = check_streams(m, streams)?;

    Ok(Endowment {
        prog,
        arg,
        mem_pages,
        file,
        dir,
        flags,
        sink: streams.sink,
        source: streams.source,
        diagnostics,
        reports: m.reports,
        interruptible: m.interruptible,
        writes_while_reading: matches!(
            m.input,
            InputSpec::Required {
                writes_while_reading: true
            }
        ),
    })
}

/// **Resolve a redirection's name into the grant that backs it** (milestone 50).
///
/// `> report.txt` and `< report.txt` designate a file the same way an operand does: the leading
/// path is walked against where the shell stands **now**, once, and the result is a value. So this
/// is `designate` with the pattern case taken out, because [`line::split`] has already refused a
/// pattern here.
///
/// `writable` comes from the operator rather than from a manifest, and that is the one place this
/// shell reads a direction off the line. It is not the exception it looks like: the program is not
/// being granted a file at all. It is being handed a **sink**, which appends and can do nothing
/// else, and the direction is a property of which end of the pipe the shell is building.
pub fn redirect_target(
    token: &[u8],
    holds: Holdings,
    writable: bool,
) -> Result<FileGrant, Refusal> {
    if !holds.dir {
        return Err(Refusal::NoSuchCapability(CapKind::File));
    }
    let (dir, names) = designate(token, 0, holds.cwd, Expansion::none())?;
    let name = names.only().ok_or(Refusal::FileNotNameable)?;
    Ok(FileGrant {
        dir,
        name: Name::new(name).ok_or(Refusal::FileNotNameable)?,
        writable,
    })
}

/// **Whether this program can be wired the way the line asks**, which is the whole of the operators'
/// manifest check.
///
/// Two questions, and they are not symmetric because the two directions are not. **Output**: has
/// this program got bytes to redirect at all? A `Words` program answers with a number in a register
/// and a `Silent` one holds no output capability, so in both cases there is nothing for `>` or `|`
/// to substitute and the honest answer is that the line asked for something that does not exist.
/// **Input**: a program that reads a stream and is handed none blocks forever, and a program that
/// reads nothing and is handed one has been given authority for no reason. Both are refusals, and
/// the first one is the one Unix cannot make.
///
/// **Output is now two questions**, and the second is `2>`'s (DECISIONS §67): the manifest says
/// whether this program has a second stream at all, so a `2>` aimed at one that does not is
/// [`Refusal::NoDiagnosticStream`] at the prompt with nothing spawned. What comes back is where the
/// second stream goes, which is `None` for a program that declares none, the shell's own printing
/// for one that does, and a file when the line named one.
fn check_streams(m: Manifest, streams: Streams) -> Result<line::Diagnostics, Refusal> {
    if !matches!(streams.sink, Sink::Report) && !m.output.is_byte_stream() {
        return Err(Refusal::NotAByteStream);
    }
    // A declared second stream exists whether or not the line mentions it; `2>` only decides where
    // it goes. That is the whole shape of the decision: the separation is the program's declaration
    // and the operator is a destination for something already separate.
    let diagnostics = match (m.output.diagnostics_slot(), streams.diagnostics) {
        (None, None) => line::Diagnostics::None,
        (None, Some(_)) => return Err(Refusal::NoDiagnosticStream),
        (Some(_), None) => line::Diagnostics::Printed,
        (Some(_), Some((g, mode))) => line::Diagnostics::File(g, mode),
    };
    match (m.input, streams.source) {
        (InputSpec::Required { .. }, Source::None) => Err(Refusal::InputRequired),
        (InputSpec::Forbidden, Source::None) => Ok(diagnostics),
        (InputSpec::Forbidden, _) => Err(Refusal::InputForbidden),
        (InputSpec::Required { .. }, _) => Ok(diagnostics),
    }
}

/// **Whether this shell can be at both ends of the chain it just planned**, which is the one
/// question the operators cannot answer stage by stage.
///
/// `shell_feeds_head` is true when the bytes going into the first stage come from *this process*: a
/// `<`, an input operand (`wc report.txt`), or a producing builtin at the head (`ls | wc`). When it
/// is false there is a separate process on the far end of the head's input and none of this applies.
///
/// # The constraint, which is the kernel's and not the shell's
///
/// A process has **one wait point**. `SEND` blocks until a receiver takes the message, `RECV` blocks
/// until one arrives, and there is no select, no poll and no timed wait (design/roadmap/106 is
/// NOT-STARTED and is a kernel-surface fork). So a shell that is feeding a chain cannot also be
/// receiving from it, and the two blocked processes have nothing that could wake either.
///
/// **No interleaving schedule fixes this**, which is worth stating because it is the first thing
/// anybody reaches for. Alternating one send with one receive deadlocks whenever the stage reads
/// twice before it writes; alternating the other way deadlocks whenever it writes twice before it
/// reads. The shell cannot know which, because the whole point of the sink contract is that neither
/// end knows anything about the other.
///
/// # What makes a line runnable anyway: one barrier
///
/// A stage that declares [`InputSpec::Required::writes_while_reading`] `false` absorbs the whole
/// stream before it answers. One of those anywhere in the chain is enough: everything upstream of it
/// can stream freely, the shell's feed completes, and only then does anything travel back. So
/// `doc page.md | wc` runs and `doc page.md` is refused, with `| wc` being the fix the message names.
///
/// A line whose head is a builtin and which spawns nothing at all (`ls > out.txt`) is not a chain
/// and is always `Ok`: there is no second process to deadlock against.
pub fn check_chain(shell_feeds_head: bool, plans: &[Option<Endowment>]) -> Result<(), Refusal> {
    if !shell_feeds_head {
        return Ok(());
    }
    let mut spawned = false;
    for e in plans.iter().flatten() {
        spawned = true;
        if !e.writes_while_reading {
            return Ok(());
        }
    }
    if spawned {
        return Err(Refusal::NoReaderButThisShell);
    }
    Ok(())
}

/// **What one operand token designates**: the directory its leading path landed in, and the set of
/// names in that directory.
///
/// This is where "resolve at grant time" and "the expansion is the grant" meet, and both halves are
/// values rather than pointers. The leading path is walked here, once, against where the shell is
/// standing *now*; the final component is either the name typed or the set the shell expanded it
/// into. Nothing downstream re-resolves anything, so a `cd` after this line cannot change what an
/// already-planned grant means, and neither can a file appearing in the directory afterwards.
fn designate(
    token: &[u8],
    at: usize,
    cwd: nav::Cwd,
    expanded: Expansion,
) -> Result<(nav::Cwd, NameSet), Refusal> {
    // Refuses a pattern anywhere but the last component before anything else looks at the token,
    // because `a*/b` is not a smaller question than `a*`, it is a different one (notes/glob.md).
    let magic = expand::magic_component(token)?;
    let parsed = nav::path(token).map_err(nav_refusal)?;
    let (lead, last) = parsed
        .split_last_component()
        .ok_or(Refusal::FileNotNameable)?;
    // **An absolute token is resolved from the holder's own root**, not from where it is standing,
    // which is the one thing the steps cannot say for themselves. Everything downstream already
    // re-walks a recorded position from the root (`swish::open_at`), so rooting it here is the
    // whole of what the syntax needed.
    let mut dir = if parsed.from_root() {
        nav::Cwd::root()
    } else {
        cwd
    };
    dir.apply(lead).map_err(nav_refusal)?;

    let names = if magic {
        // The pattern's whole meaning is the set, so a planner with no set has nothing to grant and
        // must not fall back on the pattern's own text.
        let set = expanded.for_positional(at).ok_or(Refusal::Unexpanded)?;
        if set.is_empty() {
            return Err(Refusal::NoMatch);
        }
        set
    } else {
        // A name that was typed always designates itself. The expansion is not consulted, so a
        // caller cannot substitute a set for a literal and have the grant differ from the line.
        //
        // Its type bit reads as "not a directory" because nothing enumerated it; see the BUGS note
        // in notes/glob-grant.md. Only a set the shell expanded carries types it observed.
        NameSet::one(last, false).ok_or(Refusal::FileNotNameable)?
    };
    Ok((dir, names))
}

/// A navigation refusal, in the vocabulary the prompt prints for a *grant*.
///
/// Every navigation refusal now reads as a fact about the token's shape, which it did not before
/// 2026-08-18: `nav::Refused::Absolute` had its own line here because "there is no namespace to
/// root a path in" was a statement about the model rather than about the token. A leading `/` roots
/// in the holder's own namespace now (`nav::Path::from_root`), so the case is gone rather than
/// re-worded.
pub(crate) fn nav_refusal(_r: nav::Refused) -> Refusal {
    Refusal::FileNotNameable
}

/// Which refusal to print for a token no declared slot can hold.
///
/// The manifest decides that the token is unplaceable; the token's *shape* only decides which true
/// sentence reads closest to what the user meant. An integer at a program that takes no integer is
/// "takes no argument"; a name at a program that takes no file is "takes no file"; anything else is
/// the generic unexpected-argument line.
fn unplaceable(tok: &[u8], m: Manifest) -> Refusal {
    let numeric = parse_u64(tok).is_some();
    if numeric && matches!(m.arg, ArgSpec::Forbidden) {
        Refusal::ArgForbidden
    } else if !numeric
        && matches!(m.file, FileSpec::Forbidden)
        && matches!(m.dir, DirSpec::Forbidden)
    {
        // Only when the program can be handed **neither** a file nor a directory does "takes no
        // file" read true. A second name at `rm` is a name it has nowhere to put, which is the
        // generic line: one operand is what a manifest granting one directory can express.
        Refusal::FileForbidden
    } else {
        Refusal::Unexpected
    }
}

/// The longest file name a per-file grant can carry. Duplicated from `fs_proto::grant::MAX_NAME`
/// rather than imported, because `grant_plan` is the shell's parser and must not depend on the filesystem
/// contract to check a command line; the pair is pinned by a test in each crate so a change to one
/// without the other fails on the host in milliseconds.
pub const MAX_FILE_NAME: usize = 16;

/// Whether a designated name can travel as a grant at all. A name is a single component, the same
/// rule the FS server enforces (DECISIONS §27): a path is not something this shell can express,
/// because there is no namespace here to walk.
pub fn file_name_fits(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_FILE_NAME
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&b'\\')
}

// ---- small byte helpers (no_std, no alloc) ----

/// Trim ASCII whitespace from both ends.
pub fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && s[a].is_ascii_whitespace() {
        a += 1;
    }
    while b > a && s[b - 1].is_ascii_whitespace() {
        b -= 1;
    }
    &s[a..b]
}

/// Split off the first whitespace-delimited word; return `(word, rest)` where `rest` keeps its
/// internal spacing (so `echo` can print it verbatim). The word keeps its quotes if it had any;
/// [`parse`] takes them off, because whether the word is a builtin is a question about what it
/// says rather than about how it was spelled.
fn split_first_word(s: &[u8]) -> (&[u8], &[u8]) {
    let mut i = word::span(s, 0, &|b| b.is_ascii_whitespace());
    let word = &s[..i];
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    (word, &s[i..])
}

/// Parse a base-10 `u64`. `None` for empty or any non-digit byte, so `--mem twelve` is a missing
/// grant rather than a silent zero.
pub fn parse_u64(s: &[u8]) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(v)
}

// ---- the two-tier interrupt escalation policy (DECISIONS §24) ----

/// What the shell should do this step of watching a foreground job for `^C`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Nothing this step.
    None,
    /// Deliver the cooperative interrupt: ask the job to stop itself (the first tier).
    Cooperative,
    /// Tear the job down: it did not stop when asked (the forcible tier).
    Forcible,
}

/// Poll ticks the shell waits for a cooperative exit after the first `^C` before it escalates on a
/// timeout. This is the "shell-side timeout" DECISIONS §24 left to the shell: a job that ignores the
/// cooperative signal is still torn down without needing a second keystroke. In ticks (the shell's
/// own watch loop iterations), not wall time, so it is deterministic and testable without a clock.
pub const COOP_GRACE_TICKS: u32 = 200;

/// The two-tier escalation policy, as a small state machine the shell drives while a foreground job
/// runs. It holds where `^C` routing lives (DECISIONS §24: in the shell, userspace, because job
/// control is the shell's knowledge). Pure logic, host-tested, so the counts and the grace window
/// are pinned without an emulator.
///
/// The shell feeds it two events: [`on_interrupt`](Escalation::on_interrupt) when it observes a
/// fresh `^C`, and [`on_tick`](Escalation::on_tick) each watch-loop iteration. The first `^C` asks
/// the job to stop ([`Action::Cooperative`]); a second `^C`, or the grace window elapsing with no
/// clean exit, tears it down ([`Action::Forcible`]). After a forcible decision the machine is spent
/// and returns [`Action::None`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Escalation {
    interrupts: u32,
    grace_left: u32,
    coop_sent: bool,
    done: bool,
}

impl Escalation {
    pub fn new() -> Self {
        Escalation {
            interrupts: 0,
            grace_left: 0,
            coop_sent: false,
            done: false,
        }
    }

    /// A fresh `^C` was observed. The first asks the job to stop and arms the grace window; a second
    /// (while the same job is still foreground) escalates to a forcible teardown.
    pub fn on_interrupt(&mut self) -> Action {
        if self.done {
            return Action::None;
        }
        self.interrupts += 1;
        if self.interrupts == 1 {
            self.coop_sent = true;
            self.grace_left = COOP_GRACE_TICKS;
            Action::Cooperative
        } else {
            self.done = true;
            Action::Forcible
        }
    }

    /// A watch-loop tick with no new `^C`. Once the cooperative signal is out, the grace window
    /// counts down; when it reaches zero the job is torn down even without a second `^C`, so a job
    /// that ignores the cooperative signal does not hang the prompt forever.
    pub fn on_tick(&mut self) -> Action {
        if self.done || !self.coop_sent {
            return Action::None;
        }
        self.grace_left = self.grace_left.saturating_sub(1);
        if self.grace_left == 0 {
            self.done = true;
            Action::Forcible
        } else {
            Action::None
        }
    }

    /// Whether the policy has reached a forcible teardown (the shell stops watching after this).
    pub fn spent(&self) -> bool {
        self.done
    }
}

impl Default for Escalation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use expand::{Expander, MAX_NAMES};

    use super::*;

    /// Plan a line with **nothing expanded**, which is every line that has no pattern on it. The two
    /// shims shadow the real functions so the tests below read as they did before the globbing lane,
    /// and the tests that *do* expand pass an [`Expansion`] explicitly and are grouped at the end.
    fn plan(run: &RunSpec<'_>, holds: Holdings) -> Result<Endowment, Refusal> {
        super::plan(run, holds, Expansion::none())
    }

    fn plan_against(
        run: &RunSpec<'_>,
        prog: Prog,
        m: Manifest,
        holds: Holdings,
    ) -> Result<Endowment, Refusal> {
        super::plan_against(run, prog, m, holds, Expansion::none())
    }

    #[test]
    fn empty_and_blank_lines_are_empty() {
        assert_eq!(parse(b""), Command::Empty);
        assert_eq!(parse(b"   "), Command::Empty);
        assert_eq!(parse(b"\t \n"), Command::Empty);
    }

    /// The tokenizer's edges, exactly. Trailing whitespace must scan to `len` and stop (one byte
    /// past is a fault) and must yield no token, empty or otherwise; a line with more tokens than
    /// the buffer drops the rest rather than writing past it.
    #[test]
    fn tokenize_stops_at_its_buffer_and_at_trailing_whitespace() {
        let mut out = [&b""[..]; 8];
        assert_eq!(tokenize(b"  a  bb  ", &mut out), [&b"a"[..], &b"bb"[..]]);

        let mut two = [&b""[..]; 2];
        assert_eq!(tokenize(b"a b c", &mut two), [&b"a"[..], &b"b"[..]]);
    }

    /// **Whitespace inside quotes is not a separator** (milestone 67), which is what makes a name
    /// with a space in it a single token and therefore a single designation.
    #[test]
    fn a_quoted_span_is_one_token_however_many_spaces_are_in_it() {
        let mut out = [&b""[..]; 8];
        assert_eq!(
            tokenize(b"wc \"my notes.txt\"", &mut out),
            [&b"wc"[..], &b"\"my notes.txt\""[..]],
        );
        // The quotes stay on here: what a token *means* is `word::read`'s question, and keeping the
        // two apart is what lets one place refuse `a"b"` for every scanner.
        assert_eq!(
            tokenize(b"echo 'a  b' c", &mut out),
            [&b"echo"[..], &b"'a  b'"[..], &b"c"[..]],
        );
    }

    /// The parser takes the quotes off, and what is left is the designation.
    #[test]
    fn a_quoted_operand_designates_the_bytes_between_the_quotes() {
        let r = parse_run(b"wc \"my notes.txt\"");
        assert_eq!(r.prog, b"wc");
        assert_eq!(r.positionals(), [&b"my notes.txt"[..]]);
        assert!(r.quoted(0), "the flag is what stops it being expanded");
        assert_eq!(r.misquoted, None);
        // A bare operand is unchanged, so nothing about an ordinary line moved.
        assert!(!parse_run(b"wc notes.txt").quoted(0));
    }

    /// **A quoted token is never an option**, which is the sharpest edge quoting has here: `-r` is
    /// what widens `rm`'s grant from one name to a subtree walk, and a quoted `-r` is a file name.
    #[test]
    fn quoting_keeps_a_dash_from_widening_a_grant() {
        let r = parse_run(b"rm \"-r\"");
        assert_eq!(r.options(), b"");
        assert_eq!(r.positionals(), [&b"-r"[..]]);
        // And unquoted it is still the option it always was.
        assert_eq!(parse_run(b"rm -r x").options(), b"r");
        // Same rule for `--mem`, which is an option that moves memory.
        let r = parse_run(b"budgeter \"--mem\" 16");
        assert_eq!(r.mem, None);
        assert_eq!(r.positionals(), [&b"--mem"[..], &b"16"[..]]);
        // ... while a quoted *value* is still the number it says.
        assert_eq!(parse_run(b"budgeter --mem \"16\"").mem, Some(16));
    }

    /// A builtin's name is a word like any other, so quoting it still names it. Nothing about
    /// authority turns on this; what turns on it is that a reader can quote anything without
    /// wondering which words are exempt.
    #[test]
    fn a_quoted_builtin_name_still_names_the_builtin() {
        assert!(matches!(parse(b"\"help\""), Command::Help));
        assert!(matches!(parse(b"'echo' hi"), Command::Echo(b"hi")));
    }

    /// **A line whose quotes do not make sense is refused before a program is named**, because
    /// otherwise "no such program" would be a true sentence about a word nobody typed.
    #[test]
    fn a_misquoted_line_is_refused_ahead_of_everything_else() {
        let r = parse_run(b"wc 'my notes.txt");
        assert_eq!(r.misquoted, Some(Refusal::UnclosedQuote));
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::UnclosedQuote));
        // Even when the program does not exist, so the ordering is real rather than incidental.
        let r = parse_run(b"nosuchprog 'x");
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::UnclosedQuote));
        // And the joining case.
        assert_eq!(
            parse_run(b"wc a\"b\"").misquoted,
            Some(Refusal::PartlyQuoted)
        );
    }

    /// `--mem` at the very end of the line reads no value, and reads no memory past the token
    /// array either; the missing value stays `None` for the plan to refuse.
    #[test]
    fn a_trailing_mem_flag_is_a_missing_value_not_a_read_past_the_line() {
        let r = parse_run(b"budgeter --mem");
        assert_eq!(r.prog, b"budgeter");
        assert_eq!(r.mem, None);
    }

    /// `--mem N` is exactly two tokens: the value is consumed with the flag and must not reappear
    /// as an operand. The cursor step is the code under test, so the flag sits at index 1, where
    /// stepping by the wrong arithmetic visibly lands somewhere else.
    #[test]
    fn a_mem_flag_consumes_its_value_and_nothing_after_it() {
        let r = parse_run(b"budgeter --mem 4");
        assert_eq!(r.mem, Some(4));
        assert!(r.positionals().is_empty(), "the value became an operand");
        assert_eq!(r.unexpected, None);
    }

    /// One past either ceiling lands in `unexpected`; the arrays are exactly their declared sizes,
    /// so an off-by-one here is a write past the end, not a smaller refusal.
    #[test]
    fn the_flag_and_positional_ceilings_refuse_rather_than_overflow() {
        // Nine letters in one cluster, and MAX_FLAGS is eight.
        let r = parse_run(b"rm -abcdefghi x");
        assert_eq!(r.options(), b"");
        assert_eq!(r.unexpected, Some(&b"-abcdefghi"[..]));

        // Five operands, and MAX_POSITIONALS is four.
        let r = parse_run(b"prog a b c d e");
        assert_eq!(r.positionals().len(), MAX_POSITIONALS);
        assert_eq!(r.unexpected, Some(&b"e"[..]));
    }

    #[test]
    fn help_is_a_builtin_and_everything_else_is_a_program() {
        // The whole of milestone 47's `run` removal, in three lines: three reserved words, and any
        // other first token is a program name. Nobody has to know which class a command is in.
        assert_eq!(parse(b"help"), Command::Help);
        assert_eq!(parse(b"  help  "), Command::Help);
        let Command::Run(r) = parse(b"frobnicate") else {
            panic!("a bare word must be a program invocation")
        };
        assert_eq!(r.prog, b"frobnicate");

        // Milestone 47's navigation builtins. They parse to themselves, and, because builtins win
        // over programs, **the program namespace must never learn one of these names**: it would be
        // unreachable, and the reader would have no way to tell why.
        assert_eq!(parse(b"pwd"), Command::Pwd);
        assert_eq!(parse(b"cd deeper"), Command::Cd(b"deeper"));
        assert_eq!(parse(b"cd"), Command::Cd(b""), "bare `cd` is your root");
        assert_eq!(parse(b"ls"), Command::Ls(b""));
        assert_eq!(parse(b"ls  sub "), Command::Ls(b"sub"));
        assert_eq!(parse(b"mkdir logs"), Command::Mkdir(b"logs"));
        assert_eq!(
            parse(b"apropos capability"),
            Command::Apropos(b"capability")
        );
        assert_eq!(
            parse(b"apropos  capability "),
            Command::Apropos(b"capability")
        );
        // With no operand it is still the builtin, so the shell answers "that verb needs a word"
        // rather than looking for a program nobody has.
        assert_eq!(parse(b"apropos"), Command::Apropos(b""));
        // **And `rm` is a program**, which is the whole of milestone 47's rmdir lane at this level.
        // A builtin would have shadowed the name, so this line is also the check that it no longer
        // does; the manifest is what decides what the operand grants.
        let Command::Run(r) = parse(b"rm old.txt") else {
            panic!("`rm` must parse as a program invocation, not a builtin")
        };
        assert_eq!(r.prog, b"rm");
        assert_eq!(Prog::from_name(b"rm"), Some(Prog::Rm));
        for reserved in [
            &b"help"[..],
            b"echo",
            b"caps",
            b"time",
            b"cd",
            b"pwd",
            b"ls",
            b"mkdir",
        ] {
            assert!(
                Prog::from_name(reserved).is_none(),
                "`{}` is a builtin, so a program of that name could never be run",
                core::str::from_utf8(reserved).unwrap(),
            );
        }
    }

    #[test]
    fn echo_keeps_internal_spacing() {
        assert_eq!(parse(b"echo hello world"), Command::Echo(b"hello world"));
        assert_eq!(parse(b"echo   two  spaces"), Command::Echo(b"two  spaces"));
        assert_eq!(parse(b"echo"), Command::Echo(b""));
    }

    #[test]
    fn a_bare_program_name_spawns_it() {
        // What `run worker 9` used to say. The name is the command; the manifest places the 9.
        let Command::Run(r) = parse(b"worker 9") else {
            panic!("not an invocation")
        };
        assert_eq!(r.prog, b"worker");
        assert_eq!(r.positionals(), [&b"9"[..]]);
        assert_eq!(r.mem, None);
        let e = plan(&r, Holdings::default()).unwrap();
        assert_eq!(e.prog, Prog::Worker);
        assert_eq!(e.arg, 9);
        assert_eq!(e.mem_pages, 0);
        assert!(e.reports);
    }

    #[test]
    fn worker_needs_an_integer_argument() {
        let Command::Run(r) = parse(b"worker") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::ArgRequired));
        // And a token that cannot be an integer is a missing argument, not a file: worker's
        // manifest declares no file, so there is no other slot the word could have meant.
        let Command::Run(r) = parse(b"worker eight") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::ArgRequired));
    }

    #[test]
    fn worker_refuses_a_memory_grant_whichever_side_the_flag_is_typed() {
        // `--mem` survives the grammar change as an ordinary flag, and with the verb gone it reads
        // where a Unix user would type it: after the command name. Both spellings are the same
        // grant, so both must reach the same refusal.
        for line in [&b"worker 3 --mem 8"[..], b"--mem 8 worker 3"] {
            let Command::Run(r) = parse(line) else {
                panic!()
            };
            assert_eq!(r.prog, b"worker");
            assert_eq!(
                plan(&r, Holdings::default()),
                Err(Refusal::MemForbidden),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    #[test]
    fn budgeter_needs_a_memory_grant() {
        let Command::Run(r) = parse(b"budgeter") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::MemRequired));
    }

    #[test]
    fn budgeter_with_mem_plans_the_grant() {
        let Command::Run(r) = parse(b"budgeter --mem 16") else {
            panic!()
        };
        let e = plan(&r, Holdings::default()).unwrap();
        assert_eq!(e.prog, Prog::Budgeter);
        assert_eq!(e.mem_pages, 16);
        assert_eq!(e.arg, 0);
    }

    #[test]
    fn budgeter_mem_out_of_range() {
        let Command::Run(r) = parse(b"budgeter --mem 999") else {
            panic!()
        };
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::MemOutOfRange { min: 1, max: 64 })
        );
        let Command::Run(r0) = parse(b"budgeter --mem 0") else {
            panic!()
        };
        assert_eq!(
            plan(&r0, Holdings::default()),
            Err(Refusal::MemOutOfRange { min: 1, max: 64 })
        );
    }

    /// The declared range is inclusive at both ends: a grant of exactly `min` or exactly `max`
    /// pages plans, and only one past either edge refuses.
    #[test]
    fn a_memory_grant_at_either_end_of_the_declared_range_plans() {
        for (line, pages) in [(&b"budgeter --mem 1"[..], 1), (b"budgeter --mem 64", 64)] {
            let Command::Run(r) = parse(line) else {
                panic!()
            };
            assert_eq!(
                plan(&r, Holdings::default()).unwrap().mem_pages,
                pages,
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    #[test]
    fn budgeter_takes_no_argument() {
        let Command::Run(r) = parse(b"budgeter --mem 8 5") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::ArgForbidden));
    }

    #[test]
    fn unknown_program_is_refused_by_name() {
        let Command::Run(r) = parse(b"frobnicate 1") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::NoSuchProgram));
        // A mistyped builtin lands here too, now that the first word is either a builtin or a
        // program, so the line has to point at both halves of what the prompt understands.
        let Command::Run(r) = parse(b"hlep") else {
            panic!()
        };
        let msg = plan(&r, Holdings::default()).unwrap_err().message();
        assert!(msg.contains("no such program"), "{msg}");
        assert!(msg.contains("help"), "{msg}");
    }

    #[test]
    fn a_bare_unplaceable_token_is_still_refused() {
        // **The safety property the `file:` prefix was credited with, proven without it.** The old
        // note argued the prefix stopped a stray word from becoming a capability transfer. It did
        // not: the manifest did. `worker 5 extra` has no slot for `extra` because worker declares
        // `FileSpec::Forbidden`, so nothing is granted and the prompt says why.
        let Command::Run(r) = parse(b"worker 5 extra") else {
            panic!()
        };
        assert_eq!(r.positionals(), [&b"5"[..], &b"extra"[..]]);
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::FileForbidden));
        // In a shell that DOES hold a directory the answer is identical, which is the point: the
        // manifest decides, not the endowment.
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::FileForbidden));
    }

    #[test]
    fn the_manifest_refuses_a_file_to_a_program_that_declares_none() {
        // The same rule from the other side: a name at a program with no file slot, in a shell that
        // could back one. Refused rather than granted-and-ignored, because a name the program has
        // no use for is authority the user thought they were moving.
        let Command::Run(r) = parse(b"heeder report.txt") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Heeder, Prog::Heeder.manifest(), WITH_DIR),
            Err(Refusal::FileForbidden),
        );
        assert_eq!(
            Refusal::FileForbidden.message(),
            "takes no file; drop the name"
        );
    }

    #[test]
    fn an_unknown_flag_never_reaches_the_file_position() {
        // The one thing a designator prefix genuinely bought: a token that is *shaped* like a flag
        // must not fall through into a grant. A shell that read `--secret` as a file name would
        // turn a typo into a capability transfer, which is the failure the prefix was defending
        // against, so the defence stays where it actually applies.
        let Command::Run(r) = parse(b"wc --secret report.txt") else {
            panic!()
        };
        assert_eq!(r.unexpected, Some(&b"--secret"[..]));
        assert_eq!(
            plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR),
            Err(Refusal::Unexpected),
        );
    }

    /// A manifest no shipped program declares yet: one endowed a readable file. Checked through
    /// [`plan_against`] so the `FileSpec::Required` logic is live and tested rather than a branch
    /// nothing reaches, which is the same door milestone 23 will come through.
    const READS_A_FILE: Manifest = Manifest {
        arg: ArgSpec::Forbidden,
        mem: MemSpec::Forbidden,
        file: FileSpec::Required { writable: false },
        dir: DirSpec::Forbidden,
        flags: NO_FLAGS,
        output: OutputSpec::Words,
        input: InputSpec::Forbidden,
        reports: true,
        interruptible: false,
        clock: false,
        domain: false,
    };

    /// The writable twin: a program that is endowed a file it may write.
    const WRITES_A_FILE: Manifest = Manifest {
        file: FileSpec::Required { writable: true },
        ..READS_A_FILE
    };

    /// The shape milestone 47 says nothing ships yet and everything hinges on: a program that takes
    /// **both** an integer and a file. It is the reason the prefix could be removed now (positional
    /// resolution is unambiguous while at most one bare token is a file) and the reason `ArgSpec`
    /// will have to grow position and arity later.
    const STAMPS_A_FILE: Manifest = Manifest {
        arg: ArgSpec::Required,
        mem: MemSpec::Forbidden,
        file: FileSpec::Required { writable: true },
        dir: DirSpec::Forbidden,
        flags: NO_FLAGS,
        output: OutputSpec::Words,
        input: InputSpec::Forbidden,
        reports: true,
        interruptible: false,
        clock: false,
        domain: false,
    };

    /// A shell that WAS granted a directory to narrow, standing at its root.
    const WITH_DIR: Holdings = Holdings {
        dir: true,
        cwd: nav::Cwd::root(),
    };

    #[test]
    fn a_bare_name_is_the_grant_and_the_manifest_is_the_direction() {
        // The headline: naming a file IS the grant, and the direction comes from the manifest, not
        // from anything typed. `wc report.txt` reads; the identical line against a writing program
        // writes. That opposite-authority-same-syntax pair is exactly why `file:` came out: it
        // decorated the half that was already visible.
        let Command::Run(r) = parse(b"wc report.txt") else {
            panic!()
        };
        assert_eq!(r.positionals(), [&b"report.txt"[..]]);
        let e = plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR).unwrap();
        let g = e.file.expect("the name did not become a grant");
        assert_eq!(g.name.as_bytes(), b"report.txt");
        assert!(
            !g.writable,
            "the manifest declared a read, so the grant reads"
        );

        let e = plan_against(&r, Prog::Worker, WRITES_A_FILE, WITH_DIR).unwrap();
        assert!(
            e.file.unwrap().writable,
            "the same command line against a writing program grants a writable file",
        );
    }

    #[test]
    fn a_file_whose_name_is_all_digits_is_still_a_file() {
        // Why the parser refuses to classify: a shape-based rule ("a number is the argument") would
        // read `wc 2026` as a missing file and an unexpected integer. The manifest knows this
        // program has no argument slot and one file slot, so there is only one thing the token can
        // be.
        let Command::Run(r) = parse(b"wc 2026") else {
            panic!()
        };
        let e = plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR).unwrap();
        assert_eq!(e.file.unwrap().name.as_bytes(), b"2026");
    }

    #[test]
    fn positionals_fill_the_manifest_slots_in_the_order_typed() {
        // The integer first, then the name, for a program that declares both. This is the shape
        // milestone 47 deferred rather than designed, so pin the behaviour it does have: the order
        // is the order, and swapping the tokens is a refusal rather than a surprise grant.
        let Command::Run(r) = parse(b"stamp 7 report.txt") else {
            panic!()
        };
        let e = plan_against(&r, Prog::Worker, STAMPS_A_FILE, WITH_DIR).unwrap();
        assert_eq!(e.arg, 7);
        assert_eq!(e.file.unwrap().name.as_bytes(), b"report.txt");
        assert!(e.file.unwrap().writable);

        let Command::Run(swapped) = parse(b"stamp report.txt 7") else {
            panic!()
        };
        assert_eq!(
            plan_against(&swapped, Prog::Worker, STAMPS_A_FILE, WITH_DIR),
            Err(Refusal::ArgRequired),
        );
    }

    #[test]
    fn a_program_endowed_a_file_is_refused_when_the_command_names_none() {
        // The manifest's whole job: catch the mismatch at the prompt instead of letting the program
        // fault on an empty slot somewhere deep inside itself.
        let Command::Run(r) = parse(b"wc") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR),
            Err(Refusal::FileRequired)
        );
        assert_eq!(
            Refusal::FileRequired.message(),
            "is granted one file; name it on the line"
        );
    }

    #[test]
    fn a_name_that_cannot_travel_as_a_grant_is_refused_at_the_prompt() {
        // A grant's name rides in two argument words, so a component that does not fit them cannot
        // travel; `..` designates a directory rather than a file, so it is not a thing to grant.
        // Both are refused where they were typed rather than turning into an ENOENT from a server
        // that was asked something meaningless.
        for line in [&b"wc this-name-is-far-too-long.txt"[..], b"wc .."] {
            let Command::Run(r) = parse(line) else {
                panic!()
            };
            assert_eq!(
                plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR),
                Err(Refusal::FileNotNameable),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
        // **An absolute path is planned, and it is rooted in this shell's own namespace.** From
        // `/logs`, `/etc/passwd` designates `passwd` under `/etc` and not under `/logs/etc`, which
        // is the whole of what the leading slash means. It grants nothing extra: `/etc` is a
        // directory this shell could have reached by walking, and if it cannot walk there the
        // server says so at run time exactly as it would for `cd`.
        let mut deep = WITH_DIR;
        assert!(deep.cwd.apply(nav::path(b"logs").unwrap().steps()).is_ok());
        let Command::Run(r) = parse(b"wc /etc/passwd") else {
            panic!()
        };
        let g = plan_against(&r, Prog::Worker, READS_A_FILE, deep)
            .unwrap()
            .file
            .unwrap();
        assert_eq!(g.name.as_bytes(), b"passwd");
        let mut buf = [0u8; nav::RENDER_MAX];
        let n = g.dir.render(&mut buf);
        assert_eq!(&buf[..n], b"/etc");

        // And it stops at the root like everything else: `/..` names the level above the only root
        // there is, so there is nothing to express and nothing was denied.
        let Command::Run(r) = parse(b"wc /../passwd") else {
            panic!()
        };
        let refused = plan_against(&r, Prog::Worker, READS_A_FILE, deep);
        assert_eq!(refused, Err(Refusal::FileNotNameable));
        assert!(!refused.unwrap_err().message().contains("denied"));
    }

    /// **The cwd stops at the process boundary**, which is the rule most likely to be got wrong.
    /// `wc report.txt` resolves the name against the shell's position *at the moment the grant is
    /// made*, and the grant records where that was. A path operand is walked here, once, so the
    /// child never sees one.
    #[test]
    fn a_name_is_resolved_against_the_shells_position_at_grant_time() {
        let mut holds = WITH_DIR;
        assert!(holds.cwd.apply(nav::path(b"logs").unwrap().steps()).is_ok());

        let Command::Run(r) = parse(b"wc report.txt") else {
            panic!()
        };
        let here = plan_against(&r, Prog::Worker, READS_A_FILE, holds)
            .unwrap()
            .file
            .unwrap();
        assert_eq!(here.name.as_bytes(), b"report.txt");
        assert_eq!(here.dir.depth(), 1, "resolved where the shell was standing");

        // A path operand is resolved to a directory plus one component, so the FS contract still
        // only ever sees a single name relative to a capability presented to it (§27 intact).
        let Command::Run(r) = parse(b"wc 2026/report.txt") else {
            panic!()
        };
        let deeper = plan_against(&r, Prog::Worker, READS_A_FILE, holds)
            .unwrap()
            .file
            .unwrap();
        assert_eq!(deeper.name.as_bytes(), b"report.txt");
        assert_eq!(deeper.dir.depth(), 2);
        assert_eq!(deeper.dir.component(1), b"2026");

        // **And the grant does not follow the shell.** Moving afterwards cannot change what an
        // already-planned grant means, because the grant carries a value and not a pointer to
        // wherever the shell happens to be next. This is the whole of "the child cannot re-resolve".
        holds.cwd.ascend();
        assert_eq!(here.dir.depth(), 1, "the grant moved when the shell did");

        // `..` in a grant's path is resolved at the prompt too, and clamps at the root exactly as
        // `cd` does: there is nothing above the root to name, so the grant cannot be for a file up
        // there whatever the token says.
        let Command::Run(r) = parse(b"wc ../../secret") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR),
            Err(Refusal::FileNotNameable),
            "a grant cannot name its way out of the shell's root",
        );
    }

    #[test]
    fn the_no_such_capability_refusal_is_about_what_the_shell_holds() {
        // Phase 1 hardcoded this refusal, which was true then and would have quietly become a lie.
        // The same command line must read as "you hold nothing that could grant this" in a shell
        // that was granted no directory, and as a real grant in one that was.
        let Command::Run(r) = parse(b"wc report.txt") else {
            panic!()
        };
        assert_eq!(
            plan_against(&r, Prog::Worker, READS_A_FILE, WITH_DIR)
                .map(|e| e.file.map(|g| g.name.as_bytes().to_vec()))
                .unwrap(),
            Some(b"report.txt".to_vec()),
            "with a directory in hand, the same line is a grant",
        );
        let refused = plan_against(&r, Prog::Worker, READS_A_FILE, Holdings::default());
        assert_eq!(
            refused,
            Err(Refusal::NoSuchCapability(CapKind::File)),
            "with no directory in hand, it is the milestone's headline refusal",
        );
        assert!(
            refused
                .unwrap_err()
                .message()
                .contains("no such capability")
        );
        // And it is reached only through a manifest that declares a file. Nothing in the static
        // table declares one, so the same *line* is refused for its name before a file is ever
        // considered. (`wc` used to be the name this line typed, and milestone 50 shipped a `wc`,
        // which is why it is spelled with a name nothing will ever claim.)
        let Command::Run(named) = parse(b"nosuchprog report.txt") else {
            panic!()
        };
        assert_eq!(plan(&named, WITH_DIR), Err(Refusal::NoSuchProgram));
    }

    // ---- milestone 50: the operators, checked against the manifests that ship ----

    /// Plan one stage of a line with the operators' answer folded in. The shim the operator tests
    /// use, for the reason the other two shims exist: the lines below should read like command
    /// lines and not like plumbing.
    fn plan_streams(
        run: &RunSpec<'_>,
        holds: Holdings,
        streams: Streams,
    ) -> Result<Endowment, Refusal> {
        super::plan_stage(run, holds, Expansion::none(), streams)
    }

    /// Parse a whole line and plan every stage of it, which is what the shell does. Returns the
    /// endowments in order, or the first refusal with the stage it came from.
    ///
    /// It exists so the tests below can be written as the lines a person types. **The redirection
    /// targets are designated here too**, which is the part worth reading: a `>` name becomes a
    /// writable [`FileGrant`] and a `<` name a readable one, and both are resolved against the same
    /// `Holdings` an operand would be.
    /// What [`plan_line`] answers: the planned stages and how many there are, or the stage index
    /// that refused and why. Named because the tuple is long enough that clippy is right about it.
    type Planned = Result<([Option<Endowment>; line::MAX_STAGES], usize), (usize, Refusal)>;

    fn plan_line(text: &[u8], holds: Holdings) -> Planned {
        let l = line::split(text).map_err(|r| (0, r))?;
        let out = match l.output {
            Some(t) => Some(redirect_target(t, holds, true).map_err(|r| (l.stage_count() - 1, r))?),
            None => None,
        };
        let inp = match l.input {
            Some(t) => Some(redirect_target(t, holds, false).map_err(|r| (0, r))?),
            None => None,
        };
        // A `2>` target is designated exactly as a `>` target is, and it is one destination for the
        // whole line: the shell mints one diagnostic endpoint and every declaring stage writes to
        // it, so every stage sees the same answer here.
        let diag = match l.diagnostics {
            Some(t) => Some((
                redirect_target(t, holds, true).map_err(|r| (l.stage_count() - 1, r))?,
                l.diagnostics_mode(),
            )),
            None => None,
        };
        let mut plans: [Option<Endowment>; line::MAX_STAGES] = [None; line::MAX_STAGES];
        for (i, stage) in l.stages().iter().enumerate() {
            let Command::Run(run) = parse(stage) else {
                return Err((i, Refusal::NotAPipelineStage));
            };
            let streams = Streams {
                sink: l.sink_for(i, out),
                source: l.source_for(i, inp),
                diagnostics: diag,
            };
            plans[i] = Some(plan_streams(&run, holds, streams).map_err(|r| (i, r))?);
        }
        Ok((plans, l.stage_count()))
    }

    /// **The headline, at the level this crate can prove it: `>` and `|` are one field.**
    ///
    /// The same `date`, planned three ways, differs in exactly one thing. Nothing about the
    /// program's own endowment (its argument, its memory, its files, its options) moves, because
    /// redirection is not a grant to the program at all: it is a different capability in a slot
    /// that was always going to hold one.
    #[test]
    fn redirecting_and_piping_change_one_field_and_nothing_else() {
        let (plain, _) = plan_line(b"date", WITH_DIR).unwrap();
        let (piped, n) = plan_line(b"date | wc", WITH_DIR).unwrap();
        let (filed, _) = plan_line(b"date > report.txt", WITH_DIR).unwrap();
        assert_eq!(n, 2);
        let (a, b, c) = (plain[0].unwrap(), piped[0].unwrap(), filed[0].unwrap());
        assert_eq!(a.sink, line::Sink::Report);
        assert_eq!(b.sink, line::Sink::Pipe);
        assert!(matches!(c.sink, line::Sink::File(g, _) if g.name.as_bytes() == b"report.txt"));
        // And everything else is identical, which is the claim. Compared field by field rather than
        // by clearing `sink` and comparing wholes, so a field added later has to be thought about.
        for other in [b, c] {
            assert_eq!(a.prog, other.prog);
            assert_eq!(a.arg, other.arg);
            assert_eq!(a.mem_pages, other.mem_pages);
            assert_eq!(a.file, other.file);
            assert_eq!(a.dir, other.dir);
            assert_eq!(a.flags, other.flags);
            assert_eq!(a.source, other.source);
            assert_eq!(a.reports, other.reports);
            assert_eq!(a.interruptible, other.interruptible);
        }
    }

    /// **`>>` changes nothing about the endowment, which is what makes it a shell-side open mode.**
    ///
    /// The stronger version of the test above: `date > f` and `date >> f` plan to endowments that
    /// are equal in every field including the `FileGrant` the sink names, and differ only in a
    /// [`line::Mode`] the child has no way to observe. Append is not a second capability, and
    /// nothing about what `date` holds records which operator was typed.
    #[test]
    fn append_and_truncate_plan_the_same_endowment() {
        let (trunc, _) = plan_line(b"date > report.txt", WITH_DIR).unwrap();
        let (app, _) = plan_line(b"date >> report.txt", WITH_DIR).unwrap();
        let (t, a) = (trunc[0].unwrap(), app[0].unwrap());
        let (line::Sink::File(tg, tm), line::Sink::File(ag, am)) = (t.sink, a.sink) else {
            panic!("a redirection did not plan to a file sink");
        };
        assert_eq!(tg, ag, "the two spellings designate the same file");
        assert_eq!((tm, am), (line::Mode::Truncate, line::Mode::Append));
        // Everything else, compared through the whole value with the one differing field made
        // equal. Cheaper than the field-by-field comparison above and sound here because the only
        // thing that may differ is inside `sink`.
        let mut a_flat = a;
        a_flat.sink = t.sink;
        assert_eq!(t, a_flat, "`>>` changed something other than the open mode");
    }

    /// The joints of a pipeline are pipes, its head reads and its tail writes. Three stages, so the
    /// middle one is a real middle and not an end wearing a disguise.
    #[test]
    fn a_pipeline_wires_its_joints_and_only_its_ends_touch_a_file() {
        let (p, n) = plan_line(b"date | wc | wc > out.txt", WITH_DIR).unwrap();
        assert_eq!(n, 3);
        assert_eq!(p[0].unwrap().source, line::Source::None);
        assert_eq!(p[0].unwrap().sink, line::Sink::Pipe);
        assert_eq!(p[1].unwrap().source, line::Source::Pipe);
        assert_eq!(p[1].unwrap().sink, line::Sink::Pipe);
        assert_eq!(p[2].unwrap().source, line::Source::Pipe);
        assert!(
            matches!(p[2].unwrap().sink, line::Sink::File(g, _) if g.name.as_bytes() == b"out.txt")
        );
    }

    /// **The refusal Unix cannot produce.** `wc` declares that it reads a stream, so a `wc` with
    /// nothing feeding it is caught at the prompt rather than becoming a process blocked on a
    /// receive that will never complete. On Unix the same command is a shell that appears to hang.
    #[test]
    fn a_reader_with_nothing_to_read_is_refused_at_the_prompt() {
        assert_eq!(plan_line(b"wc", WITH_DIR), Err((0, Refusal::InputRequired)));
        // And the three ways to give it one all satisfy it.
        assert!(plan_line(b"wc report.txt", WITH_DIR).is_ok());
        assert!(plan_line(b"wc < report.txt", WITH_DIR).is_ok());
        assert!(plan_line(b"date | wc", WITH_DIR).is_ok());
        let msg = Refusal::InputRequired.message();
        assert!(msg.contains("name a file"), "{msg}");
    }

    /// **`wc report.txt` is `wc < report.txt` with the operator left out**, and typing the name is
    /// what moves the authority: the same command with no name is refused at the prompt, above.
    ///
    /// The manifest does all the work, exactly as it does for the `file:` prefix milestone 47
    /// deleted. `wc` declares that it reads a stream and declares no file and no directory, so a
    /// bare name after its declared grants can only be the thing feeding it.
    ///
    /// **What the child ends up holding is narrower than a file capability**, and the assertion is
    /// written to say so: the plan puts the name in the *source*, not in `Endowment::file`. The
    /// shell opens it and streams the bytes, so `wc` gets an endpoint it cannot seek, re-read, stat
    /// or point at a second name, and cannot tell from a pipe.
    #[test]
    fn naming_a_file_to_a_reader_is_the_operator_left_out() {
        let e = plan_line(b"wc report.txt", WITH_DIR).unwrap().0[0].unwrap();
        let line::Source::File(g) = e.source else {
            panic!("the name did not become the input: {:?}", e.source)
        };
        assert_eq!(g.name.as_bytes(), b"report.txt");
        assert!(!g.writable, "a reader reads");
        assert!(
            e.file.is_none(),
            "this is a stream, not the per-file capability FileSpec describes",
        );
        // The same line against a shell that holds no directory is the milestone's headline
        // refusal, and it is a statement about a cspace rather than about the calendar.
        assert_eq!(
            plan_line(b"wc report.txt", Holdings::default()),
            Err((0, Refusal::NoSuchCapability(CapKind::File))),
        );
    }

    /// **An explicit `<` wins, and a line that names its input twice is refused**, because the
    /// second name then has nowhere to go. Nothing is silently dropped: an operand the planner
    /// cannot place is the same refusal it always was.
    #[test]
    fn an_input_named_twice_is_refused_rather_than_one_of_them_ignored() {
        let e = plan_line(b"wc < a.txt", WITH_DIR).unwrap().0[0].unwrap();
        let line::Source::File(g) = e.source else {
            panic!()
        };
        assert_eq!(g.name.as_bytes(), b"a.txt");
        assert!(plan_line(b"wc b.txt < a.txt", WITH_DIR).is_err());
        // And a stage fed by a pipe has no room for an operand either.
        assert!(plan_line(b"date | wc b.txt", WITH_DIR).is_err());
    }

    /// The mirror: a program that reads nothing, handed an input. Authority that moved for no
    /// reason, which this shell refuses in every other position too.
    #[test]
    fn a_program_that_reads_nothing_is_refused_an_input() {
        assert_eq!(
            plan_line(b"date < report.txt", WITH_DIR),
            Err((0, Refusal::InputForbidden)),
        );
        assert_eq!(
            plan_line(b"date | date", WITH_DIR),
            Err((1, Refusal::InputForbidden)),
        );
    }

    /// **A program whose answer is a register cannot be redirected**, and the manifest is what says
    /// so. `worker 9 > out.txt` would otherwise write a `u64` into a byte sink, which is a file
    /// with nothing legible in it and no error anywhere.
    #[test]
    fn only_a_byte_stream_can_be_redirected() {
        assert_eq!(
            plan_line(b"worker 9 > out.txt", WITH_DIR),
            Err((0, Refusal::NotAByteStream)),
        );
        assert_eq!(
            plan_line(b"worker 9 | wc", WITH_DIR),
            Err((0, Refusal::NotAByteStream)),
        );
        // The interrupt demonstrators hold no output capability at all, which is the third case and
        // reaches the same refusal from the other side.
        assert_eq!(
            plan_line(b"heeder | wc", WITH_DIR),
            Err((0, Refusal::NotAByteStream)),
        );
        // A `Bytes` program in the same position is fine, so the refusal is about the declaration
        // and not about the operator.
        assert!(plan_line(b"date > out.txt", WITH_DIR).is_ok());
    }

    // ---- `2>`: the second stream is a declaration, not a number (DECISIONS §67) ----

    /// **The declaration is what `2>` binds to, and a program that declares none is refused.**
    ///
    /// This is the whole of §67 at the level this crate can prove it. `date` declares a second
    /// output, so the operator has something to name; `wc` writes one stream and its diagnostics
    /// ride it, so the same operator names nothing and the line does not run. Nothing here consults
    /// a number: the refusal comes from a manifest that is silent about a second stream.
    #[test]
    fn a_diagnostic_redirection_binds_to_a_declaration_and_refuses_without_one() {
        let e = plan_line(b"date 2> err.txt", WITH_DIR).unwrap().0[0].unwrap();
        let line::Diagnostics::File(g, mode) = e.diagnostics else {
            panic!("2> did not plan to a file: {:?}", e.diagnostics)
        };
        assert_eq!(g.name.as_bytes(), b"err.txt");
        assert_eq!(mode, line::Mode::Truncate);
        assert!(g.writable, "the shell writes that file");
        // The output is untouched. `2>` says where the *second* stream goes and nothing else.
        assert_eq!(e.sink, line::Sink::Report);

        // And the refusal, which is the truthful one rather than a permission.
        assert_eq!(
            plan_line(b"wc gate.txt 2> err.txt", WITH_DIR),
            Err((0, Refusal::NoDiagnosticStream)),
        );
        let msg = Refusal::NoDiagnosticStream.message();
        assert!(msg.contains("declares no second output"), "{msg}");
        // A program with no byte stream at all reaches it too, from the other side.
        assert_eq!(
            plan_line(b"worker 9 2> err.txt", WITH_DIR),
            Err((0, Refusal::NoDiagnosticStream)),
        );
    }

    /// **A declared second stream exists whether or not the line mentions it**, which is the half of
    /// §67 that fixes the motivating loss.
    ///
    /// `date > when.txt` with no `2>` on it still plans a second destination, and that destination
    /// is *not* the file: the shell prints those bytes. A `date` whose complaint went where its
    /// answer went is exactly what the decision was taken to end, so the assertion is that the two
    /// destinations differ on a line that names only one of them.
    #[test]
    fn a_declarer_gets_a_second_destination_with_no_operator_on_the_line() {
        let e = plan_line(b"date", WITH_DIR).unwrap().0[0].unwrap();
        assert_eq!(e.diagnostics, line::Diagnostics::Printed);

        let e = plan_line(b"date > when.txt", WITH_DIR).unwrap().0[0].unwrap();
        assert!(matches!(e.sink, line::Sink::File(g, _) if g.name.as_bytes() == b"when.txt"));
        assert_eq!(
            e.diagnostics,
            line::Diagnostics::Printed,
            "a redirected `date` must not write its complaint into the file",
        );

        // And a program that declares none has none to place, on any line.
        for text in [&b"wc gate.txt"[..], b"wc gate.txt > out.txt", b"worker 9"] {
            let e = plan_line(text, WITH_DIR).unwrap().0[0].unwrap();
            assert_eq!(
                e.diagnostics,
                line::Diagnostics::None,
                "{}",
                core::str::from_utf8(text).unwrap(),
            );
        }
    }

    /// **`2>` changes one field**, which is the same claim `>` and `>>` already make and is what
    /// says the second stream is a destination rather than a different program.
    #[test]
    fn a_diagnostic_redirection_changes_one_field_and_nothing_else() {
        let plain = plan_line(b"date > out.txt", WITH_DIR).unwrap().0[0].unwrap();
        let redirected = plan_line(b"date > out.txt 2>> err.txt", WITH_DIR)
            .unwrap()
            .0[0]
            .unwrap();
        assert_eq!(redirected.diagnostics, {
            let (g, _) = match redirected.diagnostics {
                line::Diagnostics::File(g, m) => (g, m),
                other => panic!("2>> did not plan to a file: {other:?}"),
            };
            line::Diagnostics::File(g, line::Mode::Append)
        });
        let mut flat = redirected;
        flat.diagnostics = plain.diagnostics;
        assert_eq!(
            plain, flat,
            "`2>` changed something other than the second stream"
        );
    }

    /// The slot the declaration names is high and out of the way, because how many low slots a child
    /// gets depends on the wiring. A declaration that landed among the ordinary grants would move
    /// under the program, and the program probes one number.
    #[test]
    fn the_declared_slot_is_out_of_reach_of_the_ordinary_grants() {
        let slot = Prog::Date
            .manifest()
            .output
            .diagnostics_slot()
            .expect("date declares a second stream");
        assert_eq!(slot, DIAGNOSTICS_SLOT);
        // Four is the most ordinary grants any manifest here asks for (output, clock, input,
        // budget), and fifteen is `abi::fault::FAULT_EP_SLOT`. This has to sit between them.
        assert!(slot > 4 && slot < 15, "slot {slot} is not out of the way");
        // And every other program declares none, so nothing else needs the slot at all.
        for p in [
            Prog::Worker,
            Prog::Budgeter,
            Prog::Heeder,
            Prog::Spinner,
            Prog::Rm,
            Prog::Wc,
        ] {
            assert_eq!(p.manifest().output.diagnostics_slot(), None, "{p:?}");
        }
    }

    /// A declared second stream does not change what `>` and `|` may substitute: the first stream is
    /// still the sink contract. If it did, declaring diagnostics would silently make a program
    /// unpipeable, which would be a cost §67 does not pay.
    #[test]
    fn declaring_a_second_stream_leaves_the_first_one_redirectable() {
        assert!(Prog::Date.manifest().output.is_byte_stream());
        assert!(plan_line(b"date | wc", WITH_DIR).is_ok());
        assert!(plan_line(b"date > out.txt", WITH_DIR).is_ok());
        // And `2>` rides alongside a pipe, at the tail where it is written.
        let (p, n) = plan_line(b"date | wc", WITH_DIR).unwrap();
        assert_eq!(n, 2);
        assert_eq!(p[0].unwrap().diagnostics, line::Diagnostics::Printed);
        assert_eq!(p[1].unwrap().diagnostics, line::Diagnostics::None);
    }

    /// The grant refusal wins over the stream refusal when a line is wrong about both, because what
    /// a program is endowed is a bigger fact about the line than where its bytes were going.
    #[test]
    fn a_missing_argument_is_reported_before_a_missing_byte_stream() {
        // `worker` needs an integer AND has no bytes to redirect. Both are true; one is printed.
        assert_eq!(
            plan_line(b"worker > out.txt", WITH_DIR),
            Err((0, Refusal::ArgRequired)),
        );
    }

    /// A redirection is a designation like any other, so a shell that holds no directory cannot
    /// back one. The same sentence an operand gets, for the same reason.
    #[test]
    fn a_redirection_needs_a_directory_to_resolve_against() {
        assert_eq!(
            plan_line(b"date > report.txt", Holdings::default()),
            Err((0, Refusal::NoSuchCapability(CapKind::File))),
        );
        // A pipeline with no file on it needs no directory at all, which is what makes `date | wc`
        // runnable in a shell that holds nothing but a spawn channel.
        assert!(plan_line(b"date | wc", Holdings::default()).is_ok());
    }

    /// A redirection resolves its leading path exactly as an operand does, against where the shell
    /// stands, and records the result as a value.
    #[test]
    fn a_redirection_resolves_its_leading_path() {
        let mut deeper = nav::Cwd::root();
        deeper.apply(nav::path(b"logs").unwrap().steps()).unwrap();
        let holds = Holdings {
            dir: true,
            cwd: deeper,
        };
        let (p, _) = plan_line(b"date > old/report.txt", holds).unwrap();
        let line::Sink::File(g, _) = p[0].unwrap().sink else {
            panic!("not a file sink")
        };
        assert_eq!(g.name.as_bytes(), b"report.txt");
        assert!(g.writable, "a `>` target is written");
        let mut buf = [0u8; nav::RENDER_MAX];
        let n = g.dir.render(&mut buf);
        assert_eq!(&buf[..n], b"/logs/old");
        // An absolute path resolves from this shell's root rather than from where it stands, so
        // the same line typed in two directories names one file.
        let line::Sink::File(g, _) = plan_line(b"date > /old/report.txt", holds).unwrap().0[0]
            .unwrap()
            .sink
        else {
            panic!("not a file sink")
        };
        let n = g.dir.render(&mut buf);
        assert_eq!(&buf[..n], b"/old");
    }

    /// A `<` target is readable and a `>` target is writable, and that is the one direction this
    /// shell reads off the line rather than out of a manifest. It is not an exception: the program
    /// is handed a sink or a source, neither of which is a file capability.
    #[test]
    fn the_operator_decides_the_direction() {
        let (p, _) = plan_line(b"wc < in.txt > out.txt", WITH_DIR).unwrap();
        let line::Source::File(i) = p[0].unwrap().source else {
            panic!()
        };
        let line::Sink::File(o, _) = p[0].unwrap().sink else {
            panic!()
        };
        assert!(!i.writable);
        assert!(o.writable);
    }

    // ---- milestone 50's draining lane: the input operand inside a pipeline, and the one wait
    // point ----

    /// **`wc report.txt | wc` names its input the way `wc report.txt` does**, and the planner is
    /// the only thing that knows it: an input operand is a trailing positional at a program that
    /// declares an input, not a token any grammar could classify.
    ///
    /// This is the fact the shell was dropping. It planned the stage correctly and then wired the
    /// pipeline off the **line**, which has no `<` on it, so the head was spawned with an empty
    /// input slot. A `recv` there answers `NoSuchSlot` rather than blocking and every reader reads
    /// that as end of document, so the stage reported an empty stream: a wrong answer, not a hang,
    /// which is why nothing caught it.
    #[test]
    fn an_input_operand_survives_a_pipeline() {
        let (p, n) = plan_line(b"wc report.txt | wc", WITH_DIR).unwrap();
        assert_eq!(n, 2);
        let line::Source::File(g) = p[0].unwrap().source else {
            panic!("the head stage's operand did not become a source")
        };
        assert_eq!(g.name.as_bytes(), b"report.txt");
        assert!(!g.writable, "a stream is read, whatever the operator");
        assert_eq!(p[0].unwrap().sink, line::Sink::Pipe);
        // And the same name written out loud plans to the same thing, which is what makes the
        // operand a spelling rather than a second mechanism.
        let (explicit, _) = plan_line(b"wc < report.txt | wc", WITH_DIR).unwrap();
        assert_eq!(p[0], explicit[0]);
    }

    /// A filter that answers while it reads: the manifest shape no shipped program declares yet.
    /// Planned through [`plan_against_with`] so [`check_chain`] is live, tested logic rather than a
    /// branch nothing reaches, exactly as [`READS_A_FILE`] keeps `FileSpec::Required` live.
    const RENDERS_AS_IT_READS: Manifest = Manifest {
        arg: ArgSpec::Forbidden,
        mem: MemSpec::Forbidden,
        file: FileSpec::Forbidden,
        dir: DirSpec::Forbidden,
        flags: NO_FLAGS,
        output: OutputSpec::Bytes,
        input: InputSpec::Required {
            writes_while_reading: true,
        },
        reports: true,
        interruptible: false,
        clock: false,
        domain: false,
    };

    /// Plan one stage against an explicit manifest, with the operators' answer folded in.
    fn plan_as(m: Manifest, text: &[u8], streams: Streams) -> Endowment {
        let Command::Run(r) = parse(text) else {
            panic!()
        };
        plan_against_with(&r, Prog::Wc, m, WITH_DIR, Expansion::none(), streams).unwrap()
    }

    /// **The declaration reaches the endowment**, which is what lets a whole line be checked rather
    /// than one stage. A program that answers while it reads says so; `wc` does not.
    #[test]
    fn writing_while_reading_is_a_declaration_the_plan_carries() {
        let streams = Streams {
            source: line::Source::File(FileGrant {
                dir: nav::Cwd::root(),
                name: Name::new(b"page.md").unwrap(),
                writable: false,
            }),
            ..Streams::default()
        };
        assert!(plan_as(RENDERS_AS_IT_READS, b"wc", streams).writes_while_reading);
        assert!(!plan_as(Prog::Wc.manifest(), b"wc", streams).writes_while_reading);
    }

    /// **One wait point, stated as a rule about a line.**
    ///
    /// The shell can be at one end of a chain or the other, never both, because a `SEND` blocks
    /// until somebody receives and there is no select. So a line whose bytes all come from this
    /// shell needs at least one stage that reads to the end before it writes; with one, the feed
    /// completes before anything travels back, and with none, both processes stop.
    #[test]
    fn a_chain_this_shell_feeds_needs_one_stage_that_reads_to_the_end() {
        let src = Streams {
            source: line::Source::File(FileGrant {
                dir: nav::Cwd::root(),
                name: Name::new(b"page.md").unwrap(),
                writable: false,
            }),
            ..Streams::default()
        };
        let piped = Streams {
            sink: line::Sink::Pipe,
            ..src
        };
        let renderer = plan_as(RENDERS_AS_IT_READS, b"wc", src);
        let counter = plan_as(Prog::Wc.manifest(), b"wc", src);
        let renderer_piped = plan_as(RENDERS_AS_IT_READS, b"wc", piped);

        // `doc page.md`: this shell writes the file in and reads the render back. Nothing else is
        // in the chain, so there is nobody either process could be waiting on but the other.
        assert_eq!(
            check_chain(true, &[Some(renderer)]),
            Err(Refusal::NoReaderButThisShell),
        );
        // `doc page.md | wc`: the `wc` absorbs the whole stream, so the feed finishes first.
        assert_eq!(
            check_chain(true, &[Some(renderer_piped), Some(counter)]),
            Ok(()),
        );
        // `wc page.md`: the barrier is the only stage, which is why this worked all along and why
        // nobody noticed the rule until a second reader existed.
        assert_eq!(check_chain(true, &[Some(counter)]), Ok(()));
        // `something | doc`: a chain this shell does not feed is somebody else's rendezvous.
        assert_eq!(check_chain(false, &[Some(renderer)]), Ok(()));
        // `ls > out.txt`: the shell is the producer and no stage was spawned at all, so there is no
        // second process for it to deadlock against.
        assert_eq!(check_chain(true, &[None, None]), Ok(()));
    }

    /// The refusal names what a person can do about it, and it is about the **line**: the program
    /// is fine, and so is the shell.
    #[test]
    fn the_both_ends_refusal_names_the_fix() {
        assert!(
            Refusal::NoReaderButThisShell
                .message()
                .contains("give it a reader that is not this shell"),
        );
    }

    /// A builtin is not a pipeline stage the planner spawns, because it spawns nothing. The shell
    /// handles the producing ones (`echo`, `ls`, `pwd`) itself; everything else is refused here
    /// rather than parsed into a program that does not exist.
    #[test]
    fn a_builtin_is_not_something_the_planner_spawns() {
        assert_eq!(
            plan_line(b"cd logs | wc", WITH_DIR),
            Err((0, Refusal::NotAPipelineStage)),
        );
    }

    #[test]
    fn a_grant_name_limit_matches_the_filesystem_contract() {
        // `grant_plan` deliberately does not depend on `fs_proto` (the shell's parser must not need the
        // filesystem contract to check a command line), so the constant is duplicated. That is only
        // safe if a change to one without the other fails here, on the host, in milliseconds.
        assert_eq!(MAX_FILE_NAME, fs_proto::grant::MAX_NAME);
        assert!(file_name_fits(b"sixteen-bytes!!!"));
        assert!(!file_name_fits(b"seventeen-bytes!!"));
        assert!(!file_name_fits(b""));
    }

    #[test]
    fn a_second_integer_is_unexpected_not_ignored() {
        let Command::Run(r) = parse(b"worker 3 5") else {
            panic!()
        };
        assert_eq!(r.positionals(), [&b"3"[..], &b"5"[..]]);
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::Unexpected));
    }

    #[test]
    fn mem_flag_must_have_a_numeric_value() {
        // `--mem twelve` is a missing grant, not a silent zero: parse_u64 rejects it, so budgeter
        // sees "no --mem given" and refuses.
        let Command::Run(r) = parse(b"budgeter --mem twelve") else {
            panic!()
        };
        assert_eq!(r.mem, None);
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::MemRequired));
    }

    #[test]
    fn caps_previews_the_command_you_would_have_typed() {
        // The new spelling, and the reason for it: the tail is the line itself, so what you inspect
        // and what you run cannot drift apart. With `run` gone there is no verb left to echo.
        assert_eq!(parse(b"caps"), Command::Caps(b""));
        assert_eq!(
            parse(b"caps budgeter --mem 16"),
            Command::Caps(b"budgeter --mem 16")
        );
        let Command::Caps(tail) = parse(b"caps budgeter --mem 16") else {
            panic!()
        };
        let (Command::Run(previewed), Command::Run(typed)) =
            (parse(tail), parse(b"budgeter --mem 16"))
        else {
            panic!()
        };
        assert_eq!(
            plan(&previewed, Holdings::default()),
            plan(&typed, Holdings::default()),
            "the preview must plan the identical endowment to the command itself",
        );
    }

    /// **`time` runs the command you typed, so its tail has to parse back to that command**
    /// (milestone 86).
    ///
    /// The same property `caps` has, and asserted the same way, because it is the same claim: the
    /// tail is the line with the prefix word taken off, so what is timed and what would have run
    /// plan the identical endowment. A prefix word that rewrote its tail (dropping an option,
    /// re-tokenizing, inserting a grant) would make "what you time is what you run" a slogan.
    #[test]
    fn time_runs_the_command_you_would_have_typed() {
        assert_eq!(parse(b"time"), Command::Time(b""));
        assert_eq!(
            parse(b"time budgeter --mem 16"),
            Command::Time(b"budgeter --mem 16")
        );
        let Command::Time(tail) = parse(b"time budgeter --mem 16") else {
            panic!()
        };
        let (Command::Run(timed), Command::Run(typed)) = (parse(tail), parse(b"budgeter --mem 16"))
        else {
            panic!()
        };
        assert_eq!(
            plan(&timed, Holdings::default()),
            plan(&typed, Holdings::default()),
            "timing a command must not change what it is granted",
        );

        // **The operators survive the prefix**, which is the reason the tail is a whole line rather
        // than a token list. `time date | wc` is a timed *pipeline*; a `time` that took the first
        // word would time `date` and silently drop the rest, which is the bug `caps` already has a
        // test for one layer up (`swish::route`).
        assert_eq!(parse(b"time date | wc"), Command::Time(b"date | wc"));
    }

    #[test]
    fn date_takes_nothing_from_the_command_line() {
        // `date` is the first program whose authority is entirely init's to give (a read-only
        // mapping of the clock page), so its grant expression is empty and both halves matter:
        // typing nothing works, and typing anything is refused rather than passed along.
        let Command::Run(r) = parse(b"date") else {
            panic!()
        };
        let e = plan(&r, Holdings::default()).unwrap();
        assert_eq!(e.prog, Prog::Date);
        assert_eq!((e.arg, e.mem_pages), (0, 0));
        assert!(e.file.is_none());
        assert!(e.reports, "it answers with a line of text");
        assert!(!e.interruptible, "it prints once and exits");

        let Command::Run(r) = parse(b"date 5") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::ArgForbidden));
        let Command::Run(r) = parse(b"date report.txt") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::FileForbidden));
        let Command::Run(r) = parse(b"date --mem 4") else {
            panic!()
        };
        assert_eq!(plan(&r, Holdings::default()), Err(Refusal::MemForbidden));
    }

    // ---- `rm` is a program, and its operand grants a directory (milestone 47's rmdir lane) ----

    /// **The headline at this level: naming something to remove grants the directory that holds
    /// it.** `rm old.txt` hands `rm` a capability to the directory `old.txt` is in, plus that one
    /// name, because taking a name away is an operation on a directory and no per-file capability
    /// can express it.
    ///
    /// The grant is therefore **wider than the name typed**, which is stated here rather than
    /// hidden: the same capability could remove anything else in that directory. Narrowing it to
    /// the names on the line is the nameset caretaker globbing needs, and it is the same shape.
    #[test]
    fn naming_something_to_remove_grants_the_directory_that_holds_it() {
        let Command::Run(r) = parse(b"rm old.txt") else {
            panic!()
        };
        let e = plan(&r, WITH_DIR).unwrap();
        assert_eq!(e.prog, Prog::Rm);
        let g = e.dir.expect("the operand did not become a directory grant");
        assert_eq!(g.names.only(), Some(&b"old.txt"[..]));
        assert_eq!(g.dir.depth(), 0, "resolved where the shell was standing");
        assert!(
            !g.subtree,
            "without -r the capability must not reach below the directory it was given",
        );
        assert!(e.file.is_none(), "rm takes no file capability, ever");
        assert_eq!((e.arg, e.mem_pages), (0, 0));

        // A path operand is walked at the prompt, so the grant is the directory it landed in and
        // the child still only ever sees one component (§27 intact).
        let Command::Run(r) = parse(b"rm logs/2026/old.txt") else {
            panic!()
        };
        let g = plan(&r, WITH_DIR).unwrap().dir.unwrap();
        assert_eq!(g.names.only(), Some(&b"old.txt"[..]));
        assert_eq!(g.dir.depth(), 2);
        assert_eq!(g.dir.component(0), b"logs");
        assert_eq!(g.dir.component(1), b"2026");
    }

    /// **Typing `-r` is what hands over more**, and that is the property worth having a test for:
    /// the recursion is not a branch in the program that a bug could take anyway. Without the
    /// option the capability cannot walk or list, so a `rm` that tried would be told there is no
    /// such name.
    #[test]
    fn the_recursion_option_is_what_widens_the_grant() {
        let narrow = {
            let Command::Run(r) = parse(b"rm logs") else {
                panic!()
            };
            plan(&r, WITH_DIR).unwrap()
        };
        let wide = {
            let Command::Run(r) = parse(b"rm -r logs") else {
                panic!()
            };
            plan(&r, WITH_DIR).unwrap()
        };
        assert_eq!(narrow.dir.unwrap().names, wide.dir.unwrap().names);
        assert!(!narrow.dir.unwrap().subtree);
        assert!(wide.dir.unwrap().subtree, "-r must widen the grant");
        assert_eq!(narrow.flags, 0);
        assert_eq!(wide.flags, rmopt::RECURSIVE);
    }

    /// A manifest whose recursion letter is not its first option, which no shipped program has:
    /// `rm` declares `r` first, so its bit is zero, where `1 << bit` and every wrong spelling of it
    /// agree. Through [`plan_against`], the door an external manifest (milestone 23) comes through.
    const SWEEPS_A_TREE: Manifest = Manifest {
        file: FileSpec::Forbidden,
        dir: DirSpec::Required {
            subtree_flag: Some(b'r'),
        },
        // `r` deliberately second, so the widening reads bit 1.
        flags: b"vr",
        ..READS_A_FILE
    };

    /// The subtree check reads the recursion letter's **own** bit out of the flags word, wherever
    /// the manifest declares it, and widening must follow that bit rather than position zero.
    #[test]
    fn the_recursion_letter_widens_by_its_own_bit_wherever_it_is_declared() {
        let Command::Run(r) = parse(b"sweep -r logs") else {
            panic!()
        };
        let e = plan_against(&r, Prog::Rm, SWEEPS_A_TREE, WITH_DIR).unwrap();
        assert_eq!(e.flags, 1 << 1);
        assert!(
            e.dir.unwrap().subtree,
            "-r declared at bit 1 must still widen the grant",
        );
    }

    /// The option letters and the bits `rmopt` names are one thing spelled twice (the manifest has
    /// the letters, the program has the bits), so a change to either without the other has to fail
    /// here in milliseconds. A cluster is one token, as every Unix `rm -rf` is typed.
    #[test]
    fn rm_options_are_numbered_by_their_position_in_its_manifest() {
        let Command::Run(r) = parse(b"rm -rfv doomed") else {
            panic!()
        };
        assert_eq!(r.options(), b"rfv");
        let e = plan(&r, WITH_DIR).unwrap();
        assert_eq!(e.flags, rmopt::RECURSIVE | rmopt::FORCE | rmopt::VERBOSE);
        // Separately typed, in any order, is the same grant: options are a set.
        let Command::Run(r) = parse(b"rm -v -f -r doomed") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR).unwrap().flags, e.flags);
        // And each letter alone lands on exactly its own bit.
        for (line, want) in [
            (&b"rm -r x"[..], rmopt::RECURSIVE),
            (b"rm -f x", rmopt::FORCE),
            (b"rm -v x", rmopt::VERBOSE),
        ] {
            let Command::Run(r) = parse(line) else {
                panic!()
            };
            assert_eq!(
                plan(&r, WITH_DIR).unwrap().flags,
                want,
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    /// An option nothing declares is refused **at the prompt**, not passed along unread. It matters
    /// more here than it looks: an option can widen a grant, so a shell that silently dropped one
    /// would be a shell whose printed preview and actual transfer could disagree.
    #[test]
    fn an_option_the_manifest_does_not_declare_is_refused() {
        let Command::Run(r) = parse(b"rm -q doomed") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::NoSuchOption));
        // A cluster is refused whole, on its first unknown letter: `-rq` must not half-apply.
        let Command::Run(r) = parse(b"rm -rq doomed") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::NoSuchOption));
        // And a program that declares no options at all refuses every one.
        let Command::Run(r) = parse(b"worker -r 9") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::NoSuchOption));
        assert!(
            !Refusal::NoSuchOption.message().contains("denied"),
            "a refusal is a fact about the command, never a policy",
        );
    }

    /// `rm` with nothing to remove, and `rm` in a shell that holds no directory. The second is the
    /// milestone's headline refusal reaching a **shipped** program for the first time: until now no
    /// program in the static table declared a grant the interactive shell cannot back.
    #[test]
    fn rm_needs_an_operand_and_something_to_narrow() {
        let Command::Run(r) = parse(b"rm") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::PathRequired));
        let Command::Run(r) = parse(b"rm -rf") else {
            panic!()
        };
        assert_eq!(
            plan(&r, WITH_DIR),
            Err(Refusal::PathRequired),
            "options are not operands: `rm -rf` names nothing",
        );

        let Command::Run(r) = parse(b"rm doomed") else {
            panic!()
        };
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::NoSuchCapability(CapKind::File)),
            "a shell granted no directory holds nothing that could back this",
        );
    }

    /// The same shape refusals a file grant gets, because they are the same resolver: a name that
    /// cannot travel in a grant is refused where it was typed, and `..` clamps at the shell's root
    /// so a grant cannot name its way out **whether or not it started at the root**.
    #[test]
    fn a_directory_grant_cannot_be_named_out_of_the_shells_root() {
        for (line, want) in [
            (&b"rm /.."[..], Refusal::FileNotNameable),
            (b"rm /../passwd", Refusal::FileNotNameable),
            (b"rm ..", Refusal::FileNotNameable),
            (b"rm ../motd", Refusal::FileNotNameable),
            (
                b"rm this-name-is-far-too-long.txt",
                Refusal::FileNotNameable,
            ),
        ] {
            let Command::Run(r) = parse(line) else {
                panic!()
            };
            assert_eq!(
                plan(&r, WITH_DIR),
                Err(want),
                "{}",
                core::str::from_utf8(line).unwrap(),
            );
        }
    }

    /// **One operand, and a second is refused rather than ignored.** `rm a b` is a real Unix line
    /// and this shell does not run it: a manifest grants one directory, and two names in two
    /// different directories are two grants. That is the same widening globbing needs (`rm *.txt`
    /// is the same question), so it waits for the same answer instead of getting a private one.
    #[test]
    fn rm_takes_one_operand_because_a_manifest_grants_one_directory() {
        let Command::Run(r) = parse(b"rm a b") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::Unexpected));
        assert!(
            !Refusal::Unexpected.message().contains("no such"),
            "the second name is unplaceable, not unknown",
        );
    }

    /// The directory grant is resolved **at grant time** and recorded as a value, exactly as a file
    /// grant is, so a `cd` afterwards cannot change what an already-planned `rm` would take away.
    #[test]
    fn a_directory_grant_is_fixed_where_it_was_planned() {
        let mut holds = WITH_DIR;
        assert!(holds.cwd.apply(nav::path(b"logs").unwrap().steps()).is_ok());
        let Command::Run(r) = parse(b"rm -r old") else {
            panic!()
        };
        let g = plan(&r, holds).unwrap().dir.unwrap();
        assert_eq!(g.dir.depth(), 1);
        assert_eq!(g.dir.component(0), b"logs");
        holds.cwd.ascend();
        assert_eq!(g.dir.depth(), 1, "the grant followed the shell");
    }

    // ---- globbing: the expansion is the grant (milestone 47's globbing lane) ----

    /// A directory as the shell would enumerate it: two names the pattern takes, one it does not,
    /// and a subdirectory it does not either. The last two are the controls, and they are what make
    /// every claim below a statement about *which* names rather than about how many.
    const LISTING: [(&[u8], bool); 4] = [
        (b"one.txt", false),
        (b"two.txt", false),
        (b"three.log", false),
        (b"logs", true),
    ];

    /// Expand a pattern over [`LISTING`], the way both `echo` and the shell's grant path do.
    fn expanded(pattern: &[u8]) -> Result<expand::NameSet, Refusal> {
        let mut e = Expander::new(pattern);
        for (name, is_dir) in LISTING {
            e.offer(name, is_dir);
        }
        e.finish()
    }

    /// **The headline, at the level a shell can check it: what `echo *.txt` shows is exactly what
    /// `rm *.txt` transfers.**
    ///
    /// The two halves are a separate claim each, and only together do they mean anything:
    ///
    /// 1. the planner grants **the set the expander produced**, unnarrowed, unreordered and with
    ///    nothing added, so nothing between the display and the transfer can edit the authority;
    /// 2. the set is **not** everything in the directory, which is what makes claim 1 more than
    ///    "the grant is the directory".
    ///
    /// The other half of the demonstration is in the guest, where the shell derives the two
    /// independently over a real `READDIR` and compares them (`kernel::user::glob_grant_tests`).
    /// This is the part that can be checked in milliseconds.
    #[test]
    fn what_echo_shows_is_what_rm_would_transfer() {
        let shown = expanded(b"*.txt").expect("the pattern matches two of these names");

        let Command::Run(r) = parse(b"rm *.txt") else {
            panic!("`rm *.txt` must parse as a program invocation")
        };
        let granted = super::plan(&r, WITH_DIR, Expansion::at(0, shown))
            .unwrap()
            .dir
            .expect("the pattern did not become a directory grant")
            .names;

        assert_eq!(granted, shown, "the grant is not what the expansion showed");
        assert_eq!(granted.len(), 2);
        assert!(granted.contains(b"one.txt") && granted.contains(b"two.txt"));
        assert!(
            !granted.contains(b"three.log") && !granted.contains(b"logs"),
            "the grant reached a name in the directory that the pattern did not match",
        );
        // And `caps rm *.txt` previews the identical endowment, because the preview re-parses the
        // command you would have typed and plans it against the same expansion.
        let Command::Caps(tail) = parse(b"caps rm *.txt") else {
            panic!()
        };
        let Command::Run(previewed) = parse(tail) else {
            panic!()
        };
        assert_eq!(
            super::plan(&previewed, WITH_DIR, Expansion::at(0, shown)),
            super::plan(&r, WITH_DIR, Expansion::at(0, shown)),
        );
    }

    /// **A literal operand is the set of one**, which is the whole of "this generalizes
    /// `fs_file_caretaker` rather than replacing it": the same grant shape carries both, and a name
    /// that was typed designates itself whatever anybody hands the planner alongside it.
    #[test]
    fn a_literal_operand_is_the_set_of_one_and_ignores_any_expansion() {
        let Command::Run(r) = parse(b"rm old.txt") else {
            panic!()
        };
        let names = plan(&r, WITH_DIR).unwrap().dir.unwrap().names;
        assert_eq!(names.len(), 1);
        assert_eq!(names.only(), Some(&b"old.txt"[..]));

        // A set offered for a token with no magic in it must not be able to replace what was typed.
        // Nothing should ever do this; the point is that it cannot change the grant if it does.
        let stray = expanded(b"*.txt").unwrap();
        assert_eq!(
            super::plan(&r, WITH_DIR, Expansion::at(0, stray))
                .unwrap()
                .dir
                .unwrap()
                .names,
            names,
            "a typed name must designate itself, whatever expansion is offered with it",
        );
    }

    /// **A pattern never becomes a name.** Without an expansion behind it the plan refuses, rather
    /// than granting a capability for a name with a `*` in it: that name is well-formed here
    /// (nothing refuses `*` in a component), so the grant would be silently useless today and live
    /// the moment anything created a file called `*.txt`.
    #[test]
    fn a_pattern_with_nothing_to_expand_it_against_is_refused_not_taken_literally() {
        let Command::Run(r) = parse(b"rm *.txt") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::Unexpanded));
        assert!(
            file_name_fits(b"*.txt"),
            "the refusal above is the ONLY thing stopping a pattern becoming a grantable name",
        );
        // In a shell holding no directory the bigger fact wins: there is nothing to narrow, and
        // therefore nothing to expand against either.
        assert_eq!(
            plan(&r, Holdings::default()),
            Err(Refusal::NoSuchCapability(CapKind::File)),
        );
    }

    /// The two costs the roadmap said to design rather than discover, at the prompt.
    #[test]
    fn an_empty_match_and_an_oversized_one_are_both_refused_before_anything_spawns() {
        let Command::Run(r) = parse(b"rm *.rs") else {
            panic!()
        };
        // Nothing matched. The expander refuses first, and the planner refuses an empty set handed
        // to it anyway, so there are two doors and neither leads to a spawn.
        assert_eq!(expanded(b"*.rs"), Err(Refusal::NoMatch));
        assert_eq!(
            super::plan(&r, WITH_DIR, Expansion::at(0, expand::NameSet::empty())),
            Err(Refusal::NoMatch),
        );
        // Too many matched: the grant is refused whole. Sixteen of seventeen names would have been
        // a preview and a transfer that disagreed.
        let mut over = Expander::new(b"*");
        let mut names = [[b'n'; 4]; MAX_NAMES + 1];
        for (i, n) in names.iter_mut().enumerate() {
            n[2] = b'0' + (i / 10) as u8;
            n[3] = b'0' + (i % 10) as u8;
        }
        for n in &names {
            over.offer(&n[..], false);
        }
        assert_eq!(over.finish(), Err(Refusal::TooManyNames));
        assert!(
            Refusal::TooManyNames.message().contains("8"),
            "a bound the prompt will not name is a bound nobody can work around",
        );
    }

    /// A pattern lives in the last component or nowhere, and a program granted **one file** needs a
    /// pattern that designates one name. Both are facts about what was designated; neither is a
    /// permission.
    #[test]
    fn a_pattern_is_refused_where_it_cannot_mean_one_thing() {
        let Command::Run(r) = parse(b"rm */old.txt") else {
            panic!()
        };
        assert_eq!(plan(&r, WITH_DIR), Err(Refusal::PatternInPath));

        // A file grant takes one name, so a pattern that matched two has designated something this
        // program cannot be handed.
        let Command::Run(r) = parse(b"wc *.txt") else {
            panic!()
        };
        assert_eq!(
            super::plan_against(
                &r,
                Prog::Worker,
                READS_A_FILE,
                WITH_DIR,
                Expansion::at(0, expanded(b"*.txt").unwrap()),
            ),
            Err(Refusal::AmbiguousFile),
        );
        // And one that matched exactly one name is a perfectly good way to name it.
        let Command::Run(r) = parse(b"wc *.log") else {
            panic!()
        };
        let g = super::plan_against(
            &r,
            Prog::Worker,
            READS_A_FILE,
            WITH_DIR,
            Expansion::at(0, expanded(b"*.log").unwrap()),
        )
        .unwrap()
        .file
        .unwrap();
        assert_eq!(g.name.as_bytes(), b"three.log");
    }

    /// A pattern operand is resolved against the shell's position at **plan time**, exactly as a
    /// typed name is, and the leading path is walked here rather than passed on.
    #[test]
    fn a_pattern_is_resolved_where_the_shell_was_standing() {
        let mut holds = WITH_DIR;
        assert!(holds.cwd.apply(nav::path(b"logs").unwrap().steps()).is_ok());
        let Command::Run(r) = parse(b"rm -r 2026/*.txt") else {
            panic!()
        };
        let g = super::plan(&r, holds, Expansion::at(0, expanded(b"*.txt").unwrap()))
            .unwrap()
            .dir
            .unwrap();
        assert_eq!(g.dir.depth(), 2);
        assert_eq!(g.dir.component(0), b"logs");
        assert_eq!(g.dir.component(1), b"2026");
        assert!(g.subtree, "-r still widens a set grant");
        holds.cwd.ascend();
        assert_eq!(g.dir.depth(), 2, "the grant followed the shell");
    }

    /// The set bound is one number spelled in two crates, for [`MAX_FILE_NAME`]'s reason: `grant_plan`
    /// must be able to check a command line without linking the filesystem contract, so the pair is
    /// pinned here rather than shared.
    #[test]
    fn the_set_bound_matches_the_contract_that_carries_the_set() {
        assert_eq!(MAX_NAMES, fs_proto::nameset::MAX_NAMES);
        assert_eq!(expand::MAX_NAME, fs_proto::grant::MAX_NAME);
        // And the widest set this crate can plan must fit the buffer that contract sizes for it.
        let widest: [(&[u8], bool); MAX_NAMES] = [(b"sixteen-bytes!!!", false); MAX_NAMES];
        let mut buf = [0u8; fs_proto::nameset::BYTES];
        assert!(fs_proto::nameset::encode(&widest, &mut buf).is_some());
    }

    /// **Every id init can index resolves, and round trips through both names.**
    ///
    /// The loop is over `0..PROG_COUNT` rather than over a written-out list of variants, and that is
    /// the fix for a hazard this test had twice. The list said "every variant" in its own comment and
    /// was short of `Wc` once; by milestone 126 it was also short of `Doc`, `Ps` and `Pgrep`, so the
    /// three newest programs were the three nothing here checked. A list somebody has to extend is
    /// the bottom rung of CLAUDE.md's ladder.
    ///
    /// **What this actually catches, corrected 2026-08-18 by measurement rather than by reading.**
    /// The sentence here used to claim that a variant added without an arm in [`Prog::from_id`]
    /// fails here, and one added without widening [`PROG_COUNT`] fails the `from_name` sweep below.
    /// The first half is true only on a condition it did not state, and the second half is false.
    /// Adding a variant with a new id and skipping [`Prog::from_id`], [`Prog::from_name`] **and**
    /// [`PROG_COUNT`] compiles and passes every test in this crate: the sweep counts up to the
    /// constant, so it never reaches the new id, and `from_id(PROG_COUNT)` answers `None` precisely
    /// *because* the arm is missing. Widening [`PROG_COUNT`] is what arms this test, and it then
    /// names both missing arms in turn.
    ///
    /// So [`PROG_COUNT`] is the keystone rather than one item of three, and nothing in this crate
    /// enforces it: Rust gives no way to count an enum's variants without a derive macro this tree
    /// has not taken. That is a real gap and it is recorded where a reader meets it, in
    /// notes/adding-a-program.md's `BUGS` and in its step 6, which says to edit the constant first
    /// and let this test find the rest.
    #[test]
    fn prog_id_round_trips() {
        for id in 0..PROG_COUNT as u64 {
            let p = Prog::from_id(id)
                .unwrap_or_else(|| panic!("init indexes slot {id} and no program claims it"));
            assert_eq!(
                p.id(),
                id,
                "{p:?} does not answer to the id it decoded from"
            );
            assert_eq!(Prog::from_name(p.name().as_bytes()), Some(p));
        }
        assert_eq!(Prog::from_id(PROG_COUNT as u64), None);
        assert_eq!(Prog::from_id(99), None);
        // And no program is reachable by a name init cannot load it by, which is the other direction:
        // a variant with an id and no `from_name` arm would be unspawnable and invisible above.
        assert_eq!(
            Prog::from_name(b"pgrep"),
            Some(Prog::Pgrep),
            "the newest program is not reachable from the prompt",
        );
    }

    #[test]
    fn parse_u64_rejects_junk() {
        assert_eq!(parse_u64(b"123"), Some(123));
        assert_eq!(parse_u64(b""), None);
        assert_eq!(parse_u64(b"1a"), None);
        assert_eq!(parse_u64(b"-1"), None);
    }

    #[test]
    fn worker_and_budgeter_are_not_interruptible() {
        // Fast jobs finish in one step; the shell just waits for them, no ^C tier.
        assert!(!Prog::Worker.manifest().interruptible);
        assert!(!Prog::Budgeter.manifest().interruptible);
        let Command::Run(r) = parse(b"worker 9") else {
            panic!()
        };
        assert!(!plan(&r, Holdings::default()).unwrap().interruptible);
    }

    #[test]
    fn first_interrupt_is_cooperative_second_is_forcible() {
        let mut e = Escalation::new();
        assert_eq!(e.on_interrupt(), Action::Cooperative);
        assert!(!e.spent());
        assert_eq!(e.on_interrupt(), Action::Forcible);
        assert!(e.spent());
        // Spent: further events do nothing.
        assert_eq!(e.on_interrupt(), Action::None);
        assert_eq!(e.on_tick(), Action::None);
    }

    #[test]
    fn a_cooperative_signal_times_out_into_a_forcible_teardown() {
        // The "shell-side timeout": the first ^C asks nicely; if the job never exits, the grace
        // window elapsing tears it down without a second keystroke.
        let mut e = Escalation::new();
        assert_eq!(e.on_interrupt(), Action::Cooperative);
        // Ticks short of the window do nothing.
        for _ in 0..COOP_GRACE_TICKS - 1 {
            assert_eq!(e.on_tick(), Action::None);
        }
        // The last tick of the window escalates.
        assert_eq!(e.on_tick(), Action::Forcible);
        assert!(e.spent());
    }

    #[test]
    fn ticks_before_any_interrupt_do_nothing() {
        // No ^C yet: the grace window is not armed, so watching a well-behaved job never escalates.
        let mut e = Escalation::new();
        for _ in 0..COOP_GRACE_TICKS * 2 {
            assert_eq!(e.on_tick(), Action::None);
        }
        assert!(!e.spent());
        // And a first ^C after a long quiet run is still cooperative.
        assert_eq!(e.on_interrupt(), Action::Cooperative);
    }
}
