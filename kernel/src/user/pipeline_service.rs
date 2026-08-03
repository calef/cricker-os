use core::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::cap::{Rights, endpoint_cap, untyped_cap};
use crate::sched::EpId;

/// The `swish` binary's pipeline role (`user/src/swish.rs`).
const ROLE_PIPELINE: u64 = 3;

/// The VAs the shell hardcodes for its terminal pages. Must match user/src/swish.rs.
const OUT_VA: u64 = 0x0000_0000_00c0_0000;
const LINE_VA: u64 = 0x0000_0000_00b0_0000;

/// The budget the shell mints its pipes out of. Each pipeline splits a region off this and
/// gives it back, so one number covers a script of several lines; it matches `system_initializer`'s grant.
const SH_BUDGET_PAGES: u64 = 128;

/// Pages of stack **below** the one `run` maps. A shell needs more than a hand-sized program
/// for `spawn_fs_client`'s measured reason, and milestone 50 added an array of planned
/// endowments to what it carries by value.
const SHELL_EXTRA_STACK: usize = 6;

/// What the kernel holds of a scripted shell.
pub struct Wiring {
    /// The terminal endpoint the shell `CALL`s. The test serves it.
    pub term: EpId,
    /// Where the shell's printed bytes land, so the test can read them out.
    pub out_phys: u64,
}

/// The collected transcript. A static because the terminal service and the assertions are the
/// same thread and the buffer outlives one call; sized for the whole script with room to spare,
/// so a transcript that ran long is truncated rather than overrunning.
static TRANSCRIPT: spin::Mutex<[u8; 4096]> = spin::Mutex::new([0; 4096]);
static WRITTEN: AtomicUsize = AtomicUsize::new(0);

/// **Wire a scripted shell and the init service behind it.**
///
/// The endowment is deliberately the interactive one, slot for slot, because a witness with a
/// wider endowment would be proving something about a shell nobody runs.
pub fn start() -> Option<Wiring> {
    start_with(ROLE_PIPELINE, 0, None)
}

/// **The same shell, one capability wider**: a directory at slot 4 and the page it shares with
/// the FS server (milestone 50's `>` and `<`).
///
/// This is what makes the pair of witnesses worth having. [`start`] and this call spawn the same
/// ELF from the same archive with the same four slots, and the only difference between a
/// `date > report.txt` that is refused and one that writes a file is whether slot 4 is
/// occupied. Neither behaviour is a branch in the program; both are facts about a cspace.
///
/// `dir` is `(the narrowed directory endpoint, the physical frame it shares with the FS server)`,
/// which is what `fs_service::narrow_dir` hands back.
pub fn start_redirecting(dir: (EpId, u64), rights: u64) -> Option<Wiring> {
    start_with(ROLE_REDIRECT, rights, Some(dir))
}

/// The shell's redirection role (`user/src/swish.rs`).
const ROLE_REDIRECT: u64 = 4;

/// Where an FS client maps the page it shares with the FS server (`fs_service`'s
/// `FILE_VA_CLIENT`, and `user/src/swish.rs`'s `FS_VA`).
const FS_VA: u64 = 0x0000_0000_0060_0000;

fn start_with(role: u64, arg: u64, dir: Option<(EpId, u64)>) -> Option<Wiring> {
    let image = program("swish")?;
    let term = crate::sched::create_endpoint();
    let spawn_ep = crate::sched::create_endpoint();
    let result = crate::sched::create_endpoint();
    let budget = crate::untyped::create(SH_BUDGET_PAGES)?;
    let out_phys = crate::memory::alloc()?.addr();
    let line_phys = crate::memory::alloc()?.addr();
    // SAFETY: two freshly allocated frames, named through the direct map, owned by nobody yet.
    unsafe {
        core::ptr::write_bytes(
            mmu::phys_to_virt(out_phys) as *mut u8,
            0,
            FRAME_SIZE as usize,
        );
        core::ptr::write_bytes(
            mmu::phys_to_virt(line_phys) as *mut u8,
            0,
            FRAME_SIZE as usize,
        );
    }
    WRITTEN.store(0, Ordering::SeqCst);

    // init first, so a shell that spawns before the service is listening merely blocks in a
    // rendezvous rather than failing.
    crate::sched::spawn(move || init_service(spawn_ep, result))?;

    crate::sched::spawn(move || {
        // The extra stack goes in as ordinary mappings below the one `run` maps, which is the
        // shape `spawn_fs_client` uses and for the same measured reason: a shell carries a path
        // stack, a parsed line and now an array of planned endowments, all by value.
        let mut maps = [Mapping {
            va: OUT_VA,
            phys: out_phys,
            flags: Flags::user_data(),
        }; 3 + SHELL_EXTRA_STACK];
        maps[1] = Mapping {
            va: LINE_VA,
            phys: line_phys,
            flags: Flags::user_rodata(),
        };
        for (k, m) in maps[2..2 + SHELL_EXTRA_STACK].iter_mut().enumerate() {
            m.va = USER_STACK_VA - (k as u64 + 1) * FRAME_SIZE;
            m.phys = crate::memory::alloc()
                .expect("no frame for the shell's stack")
                .addr();
            m.flags = Flags::user_data();
        }
        // The FS page last, so a shell wired without a filesystem maps exactly what it did
        // before and the count is the only difference between the two wirings.
        let n = match dir {
            Some((_, file_shared)) => {
                maps[2 + SHELL_EXTRA_STACK] = Mapping {
                    va: FS_VA,
                    phys: file_shared,
                    flags: Flags::user_data(),
                };
                maps.len()
            }
            None => maps.len() - 1,
        };
        let grants: &[crate::cap::Cap] = &[
            endpoint_cap(term, Rights::WRITE),     // slot 0: the terminal
            endpoint_cap(spawn_ep, Rights::WRITE), // slot 1: direct init
            endpoint_cap(result, Rights::READ),    // slot 2: a child's answer
            untyped_cap(budget),                   // slot 3: what it mints pipes from
        ];
        match dir {
            Some((dir_ep, _)) => run(
                image,
                Spawn {
                    arg0: role,
                    arg1: arg,
                    arg2: 0,
                    grants: &[
                        grants[0],
                        grants[1],
                        grants[2],
                        grants[3],
                        // slot 4: the directory it resolves `>` and `<` against
                        endpoint_cap(dir_ep, Rights::WRITE),
                    ],
                    maps: &maps[..n],
                },
            ),
            None => run(
                image,
                Spawn {
                    arg0: role,
                    arg1: arg,
                    arg2: 0,
                    grants,
                    maps: &maps[..n],
                },
            ),
        }
    })?;

    Some(Wiring { term, out_phys })
}

/// **Serve the terminal until the shell says it is finished**, collecting everything it printed.
///
/// One `OP_WRITE` at a time, replied the way the real line discipline replies (the byte count),
/// because the shell blocks on that reply and a test that answered differently would be testing
/// a terminal nobody has.
pub fn transcript(w: &Wiring, sentinel: &[u8], out: &mut [u8]) -> usize {
    loop {
        let m = crate::sched::ipc_recv_cap(w.term);
        let (w0, slot) = (m[0], m[1]);
        let crate::cap::Object::Reply(caller) = crate::sched::current_cap(slot)
            .expect("the shell's terminal write carried no reply capability")
            .object
        else {
            panic!("the shell sent the terminal something that was not a CALL");
        };
        let n = line_editor::proto::len(w0);
        if line_editor::proto::op(w0) == line_editor::proto::OP_WRITE {
            let mut buf = TRANSCRIPT.lock();
            let at = WRITTEN.load(Ordering::SeqCst);
            let n = n.min(buf.len().saturating_sub(at));
            for i in 0..n {
                // SAFETY: the shell's output page, mapped read/write into it and named here
                // through the direct map; `n` is bounded by the page and by the buffer.
                buf[at + i] = unsafe {
                    core::ptr::read_volatile(
                        (mmu::phys_to_virt(w.out_phys) + i as u64) as *const u8,
                    )
                };
            }
            WRITTEN.store(at + n, Ordering::SeqCst);
        }
        crate::sched::ipc_reply(caller, [n as u64, 0]);
        crate::sched::delete_current_cap(slot).expect("consume the one-shot reply");

        let done = {
            let buf = TRANSCRIPT.lock();
            let len = WRITTEN.load(Ordering::SeqCst);
            len >= sentinel.len() && buf[len - sentinel.len()..len] == *sentinel
        };
        if done {
            let buf = TRANSCRIPT.lock();
            let len = WRITTEN.load(Ordering::SeqCst).min(out.len());
            out[..len].copy_from_slice(&buf[..len]);
            return len;
        }
    }
}

/// **The text the shell printed in response to `line`**, up to but not including the next
/// prompt.
///
/// The transcript is echoed prompts and answers, so slicing it this way is what lets an
/// assertion name the command it is about instead of counting lines. It lives here rather than
/// in one test module because both witnesses (the pipeline script and the redirection script)
/// read their transcripts the same way, and a second copy would be a second thing to keep true.
pub fn answer<'a>(t: &'a [u8], line: &[u8]) -> &'a [u8] {
    let mut needle = [0u8; 64];
    needle[0] = b'$';
    needle[1] = b' ';
    needle[2..2 + line.len()].copy_from_slice(line);
    needle[2 + line.len()] = b'\n';
    let needle = &needle[..3 + line.len()];
    let start = t
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| {
            panic!(
                "the shell never ran {:?}. transcript:\n{}",
                core::str::from_utf8(line).unwrap(),
                core::str::from_utf8(t).unwrap_or("<not utf-8>"),
            )
        })
        + needle.len();
    let rest = &t[start..];
    let end = rest
        .windows(2)
        .position(|w| w == b"$ ")
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Parse `wc`'s three numbers out of what the shell printed for it. `wc` prints
/// `lines words bytes` and a newline; the caller strips the shell's two-space prefix.
pub fn counts(said: &[u8]) -> (u64, u64, u64) {
    let text = core::str::from_utf8(said).expect("wc printed non-UTF-8");
    let mut it = text.split_ascii_whitespace().map(|w| {
        w.parse::<u64>()
            .unwrap_or_else(|_| panic!("wc printed {text:?}, which is not three numbers"))
    });
    let (l, w, b) = (
        it.next().expect("no line count"),
        it.next().expect("no word count"),
        it.next().expect("no byte count"),
    );
    assert!(
        it.next().is_none(),
        "wc printed more than three numbers: {text:?}"
    );
    (l, w, b)
}

/// **init, as the shell sees it**: the spawn protocol, including milestone 50's two delegated
/// capabilities.
///
/// The whole of what the operators added is here, and it is small: receive an endpoint, and put
/// it where the result endpoint would have gone. Nothing decides what is behind it, because
/// nothing here can find out.
fn init_service(spawn_ep: EpId, result: EpId) -> ! {
    loop {
        let m = crate::sched::ipc_recv(spawn_ep);
        let (w0, w1, w2) = (m[0], m[1], m[2]);
        let prog = grant_plan::Prog::from_id(grant_plan::spawnproto::prog_id(w0));
        let arg = grant_plan::spawnproto::arg(w1);
        let wiring = grant_plan::spawnproto::wiring(w2);

        // In the protocol's order. A capability received but never expected, or expected and
        // never sent, deadlocks both sides, which is why the order is the contract.
        let sink = if wiring.sink {
            take_endpoint(spawn_ep)
        } else {
            None
        };
        let source = if wiring.source {
            take_endpoint(spawn_ep)
        } else {
            None
        };
        // A `--mem` grant is received and dropped: no stage of the pipeline script asks for
        // one, and receiving it anyway is what keeps the two sides in lockstep if one ever
        // does. `user/src/system_initializer.rs` is where a budget actually reaches a child.
        if grant_plan::spawnproto::mem_pages(w2) > 0 {
            let m = crate::sched::ipc_recv_cap(spawn_ep);
            let _ = crate::sched::delete_current_cap(m[1]);
        }

        let image = prog.and_then(|p| program(p.name()));
        let started = match image {
            Some(image) => {
                // Slot 0 is the output: the result endpoint, or the sink the shell delegated.
                // Slot 1 is the input source when there is one. That is the whole difference.
                let out = sink.unwrap_or(result);
                crate::sched::spawn(move || match source {
                    Some(src) => run(
                        image,
                        Spawn {
                            arg0: arg,
                            arg1: 0,
                            arg2: 0,
                            grants: &[
                                endpoint_cap(out, Rights::WRITE),
                                endpoint_cap(src, Rights::READ),
                            ],
                            maps: &[],
                        },
                    ),
                    None => run(
                        image,
                        Spawn {
                            arg0: arg,
                            arg1: 0,
                            arg2: 0,
                            grants: &[endpoint_cap(out, Rights::WRITE)],
                            maps: &[],
                        },
                    ),
                })
                .is_some()
            }
            None => false,
        };

        // A redirected child's answer goes somewhere else, so the shell has nothing to read and
        // init owes it an ack. Unredirected, the child's own message is the shell's single read.
        if wiring.sink {
            crate::sched::ipc_send(
                result,
                [
                    if started {
                        grant_plan::spawnproto::SPAWN_OK
                    } else {
                        grant_plan::spawnproto::SPAWN_FAILED
                    },
                    0,
                    0,
                ],
            );
        } else if !started {
            crate::sched::ipc_send(result, [grant_plan::spawnproto::SPAWN_FAILED, 0, 0]);
        }
    }
}

/// Take one delegated capability and read the endpoint out of it. The slot is dropped straight
/// away: what init needs is the *name* of the endpoint, and holding the capability afterwards
/// would fill a cspace over a long session for nothing.
fn take_endpoint(ep: EpId) -> Option<EpId> {
    let m = crate::sched::ipc_recv_cap(ep);
    let slot = m[1];
    let cap = crate::sched::current_cap(slot).ok()?;
    let _ = crate::sched::delete_current_cap(slot);
    match cap.object {
        crate::cap::Object::Endpoint(id) => Some(id),
        _ => None,
    }
}
