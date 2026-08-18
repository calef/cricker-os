//! **Navigation in a system with no global namespace** (milestone 47's commands).
//!
//! `cd`, `pwd` and `ls` are *builtins*, in the same category as `caps`: they spawn nothing, need no
//! grant, and confer no new authority, because the shell is reading and rebinding a capability it
//! already holds. That is not a technicality, it retires a real worry: a listing **program** would
//! have to be handed the power to read everything it lists. This module is the pure half of those
//! builtins, host-tested in milliseconds; the requests they make live in `user/src/swish.rs`.
//!
//! # A working directory was never the problem
//!
//! In capability terms a working directory is *a directory capability the shell holds, used as the
//! default base for resolving names*. Held by the shell that is entirely legitimate, the same as its
//! untyped budget. What is bad on Unix is three specific things, and this module answers each:
//!
//! 1. **Children inherit it silently.** Here they do not: a name on the command line is resolved
//!    against the shell's position **at the moment the grant is made** ([`crate::FileGrant`]), and
//!    the child receives a capability to that one file. The child has no cwd and cannot re-resolve
//!    anything, which is why the convenience is the shell's and the authority is explicit.
//! 2. **Relative paths resolve implicitly**, so a program's reach depends on invisible state. Here
//!    the resolution happens once, at the prompt, in a value you can print.
//! 3. **`..` walks out**, so the cwd bounds nothing. Here it cannot: see below.
//!
//! # The three earned divergences
//!
//! Divergence from Unix is a tax on every user forever, so it has to be forced by the model rather
//! than chosen. Three are:
//!
//! - **`/` is the root of *your* namespace** ([`Path::from_root`]), which is Plan 9's answer and
//!   not DOS's. There is no global namespace to root a path in, so an absolute path names the one
//!   root you have: the directory capability you were granted. Two shells both type `/report.txt`
//!   and open different files, or one of them opens nothing, because the syntax is rooted in what
//!   each holds. It confers nothing: `/a/b` reaches exactly what `cd a; cd b` reaches, so this is
//!   syntax for a walk you could already take. Until 2026-08-18 the syntax was refused outright
//!   (`Refused::Absolute`), which was the honest statement of a namespace that did not exist yet
//!   rather than a position; `pwd` has printed `/a/b` since the day it was written, and a shell
//!   that prints a path you cannot type back is the tell that the refusal had outlived itself.
//! - **`..` stops at your root** ([`Cwd::ascend`] returns `false`, and nothing is sent). You descend
//!   from what you hold and never ascend past it. Chroot's shape, reached from the other direction,
//!   and here it is not a check that could be wrong: the shell holds a *stack* of directory
//!   capabilities it descended through, `..` pops one, and at the root there is nothing to pop. The
//!   FS server would refuse the name anyway (`..` is not a component it accepts), so the two
//!   mechanisms agree without either relying on the other.
//! - **`pwd` is relative to your root** ([`Cwd::render`]), because naming anything above it implies
//!   a namespace that does not exist.
//!
//! What that buys, and Unix cannot: **every shell has its own root**. Two shells hold different
//! subtrees and neither can name the other's files, not by policy but because no capability reaching
//! them exists.

/// The deepest a shell tracks below its own root.
///
/// A bound rather than a policy: the shell keeps one directory capability per level so `..` can pop
/// one, and it has no allocator, so the stack is an array. Eight is far past any tree a demonstrator
/// walks by hand, and going deeper is [`Refused::TooDeep`] rather than a silently truncated path.
pub const MAX_DEPTH: usize = 8;

/// The longest single component a name may have, which is [`crate::MAX_FILE_NAME`]: a shell that
/// could `cd` into a name it cannot grant would be able to reach a place it cannot talk about.
pub const MAX_NAME: usize = crate::MAX_FILE_NAME;

/// The bytes [`Cwd::render`] can need: a slash per level, plus each name, plus the leading slash a
/// root renders as on its own.
pub const RENDER_MAX: usize = 1 + MAX_DEPTH * (1 + MAX_NAME);

/// Why a token cannot be navigated. Each is a fact about the *name*, never about a permission,
/// which is the distinction the whole model rests on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// A component that is not a name: empty, longer than [`MAX_NAME`], or carrying a byte a name
    /// cannot carry.
    NotAName,
    /// More components than [`MAX_DEPTH`], or a descent that would take the shell past it.
    TooDeep,
    /// `..` at the root. The clamp, reported rather than silently ignored: at your root there is
    /// nothing above to name, and a shell that said nothing would leave a user believing they had
    /// moved.
    ///
    /// **An absolute path meets the same wall**, and that is the point of rooting one in your own
    /// namespace rather than in a global one: `/..` is refused here exactly as `..` at the root is,
    /// because both ask for the level above the only root there is.
    AtYourRoot,
}

impl Refused {
    /// The line the shell prints. In the capability model's voice, like [`crate::Refusal::message`]:
    /// each says what *is*, never what was denied.
    pub fn message(self) -> &'static str {
        match self {
            Refused::NotAName => "that is not a name: one component, at most 16 bytes",
            Refused::TooDeep => "too deep: this shell tracks at most 8 levels below its root",
            Refused::AtYourRoot => "you are at your root; there is nothing above it to name",
        }
    }
}

/// One step of a parsed path. `.` produces no step at all, and `..` produces [`Step::Up`], which the
/// shell executes by popping a capability rather than by sending a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step<'a> {
    /// Descend into this component.
    Down(&'a [u8]),
    /// Go back to the parent, if there is one to go back to.
    Up,
}

/// A parsed path: a bounded sequence of [`Step`]s and where they start from, validated before
/// anything is sent.
///
/// Validation is up front and total, so `cd a/b/c` fails at the prompt when `c` is unnameable rather
/// than half way through, having already moved. The shell then walks the steps one at a time,
/// because **the FS contract takes a single component per request** and this is where the roadmap's
/// "the resolver lives in the client's runtime" is actually true: the server never sees a path, and
/// §27's rule that open-by-path exists only inside a server, relative to one bound directory, holds
/// exactly as it did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Path<'a> {
    steps: [Step<'a>; MAX_DEPTH],
    n: usize,
    from_root: bool,
}

impl<'a> Path<'a> {
    /// The steps, in order.
    pub fn steps(&self) -> &[Step<'a>] {
        &self.steps[..self.n]
    }

    /// **Whether the token began with `/`**, meaning it is resolved from the holder's own root
    /// rather than from where the holder is standing.
    ///
    /// This is the whole of what an absolute path is here, and it is a fact about the *token*
    /// rather than about any capability: the caller supplies the root, so a holder can only ever
    /// root a path in the one it has. There is no second namespace for this bit to select.
    pub fn from_root(&self) -> bool {
        self.from_root
    }

    /// The last component, and the steps that lead to the directory holding it.
    ///
    /// This is **resolve-at-grant-time** in one function: `wc logs/report.txt` is a grant of one
    /// file, and the shell has to know which directory to narrow before it can build the grant. A
    /// path whose last step is `..` designates a directory, not a file, so it has no answer here.
    pub fn split_last_component(&self) -> Option<(&[Step<'a>], &'a [u8])> {
        match self.steps().split_last() {
            Some((Step::Down(name), lead)) => Some((lead, name)),
            _ => None,
        }
    }
}

/// Parse a token into a [`Path`], with no IO and no capability consulted.
///
/// Empty components are skipped, so `a//b` and `deeper/` mean what they do everywhere else; that is
/// Unix's behaviour and there is no divergence to earn. A leading `/` sets [`Path::from_root`],
/// which is the biggest fact about the token and the only one the steps themselves cannot carry:
/// `/a` and `a` parse to the same single step and mean different places.
pub fn path(token: &[u8]) -> Result<Path<'_>, Refused> {
    let from_root = token.first() == Some(&b'/');
    if token.is_empty() {
        return Err(Refused::NotAName);
    }
    let mut steps = [Step::Up; MAX_DEPTH];
    let mut n = 0;
    for part in token.split(|&b| b == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if n == MAX_DEPTH {
            return Err(Refused::TooDeep);
        }
        steps[n] = if part == b".." {
            Step::Up
        } else {
            if !component_fits(part) {
                return Err(Refused::NotAName);
            }
            Step::Down(part)
        };
        n += 1;
    }
    if n == 0 && !from_root {
        // `.` on its own: it names the place you already are, which no verb here needs and no verb
        // here can act on. Refusing it beats acting on a name the user did not type.
        //
        // **`/` on its own is not that case**, which is why the two are told apart here. It names
        // your root, which is a place you can go and a place you can list, and it is the one thing
        // `pwd` prints that a user would otherwise be unable to type back.
        return Err(Refused::NotAName);
    }
    Ok(Path {
        steps,
        n,
        from_root,
    })
}

/// Whether one component can be a name at all. The same rule the FS server enforces at its own
/// boundary (`check_component`), stated here so the shell refuses at the prompt rather than sending
/// a request it knows will be `EINVAL`.
pub fn component_fits(part: &[u8]) -> bool {
    !part.is_empty()
        && part.len() <= MAX_NAME
        && part != b"."
        && part != b".."
        && !part.contains(&b'/')
        && !part.contains(&b'\\')
        && !part.contains(&b':')
        && !part.contains(&0)
}

/// **Where a shell is, relative to its own root**: the components it descended through, and nothing
/// above them.
///
/// It is a value, not a handle, which is what makes [`crate::FileGrant`] able to carry one: a grant
/// planned at the prompt records the directory it was resolved against, and a later `cd` cannot
/// change what that grant means.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cwd {
    names: [[u8; MAX_NAME]; MAX_DEPTH],
    lens: [u8; MAX_DEPTH],
    depth: usize,
}

/// Written as the path it means. The derived form is 137 bytes of mostly zeroes, which turns any
/// assertion failure that carries one (and [`crate::Endowment`] carries one) into something nobody
/// will read.
impl core::fmt::Debug for Cwd {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.depth == 0 {
            return f.write_str("/");
        }
        for level in 0..self.depth {
            f.write_str("/")?;
            match core::str::from_utf8(self.component(level)) {
                Ok(s) => f.write_str(s)?,
                Err(_) => write!(f, "{:?}", self.component(level))?,
            }
        }
        Ok(())
    }
}

impl Default for Cwd {
    fn default() -> Self {
        Cwd::root()
    }
}

impl Cwd {
    /// At the root of your namespace, which is where every shell starts and the only place a shell
    /// with no directory capability can be.
    pub const fn root() -> Self {
        Cwd {
            names: [[0; MAX_NAME]; MAX_DEPTH],
            lens: [0; MAX_DEPTH],
            depth: 0,
        }
    }

    /// How many levels below the root, 0 at the root.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Whether this is the root of its namespace.
    pub fn is_root(&self) -> bool {
        self.depth == 0
    }

    /// Record a descent. `false` if the name does not fit or the shell is already as deep as it
    /// tracks; the caller must not have sent the request in that case.
    pub fn descend(&mut self, name: &[u8]) -> bool {
        if self.depth == MAX_DEPTH || !component_fits(name) {
            return false;
        }
        self.names[self.depth][..name.len()].copy_from_slice(name);
        self.lens[self.depth] = name.len() as u8;
        self.depth += 1;
        true
    }

    /// Pop one level. **`false` at the root, which is the clamp**: there is nothing above it, so
    /// there is nothing to pop and no request to send.
    pub fn ascend(&mut self) -> bool {
        if self.depth == 0 {
            return false;
        }
        self.depth -= 1;
        true
    }

    /// The component at `level` (0 is the first below the root).
    pub fn component(&self, level: usize) -> &[u8] {
        &self.names[level][..self.lens[level] as usize]
    }

    /// The last component, or `None` at the root. What `ls` prints as a heading and what a shell
    /// needs to name the directory it is standing in.
    pub fn last(&self) -> Option<&[u8]> {
        self.depth.checked_sub(1).map(|l| self.component(l))
    }

    /// Render as a path, **relative to this shell's root**: `/` at the root, `/a/b` below it. The
    /// leading slash is honest rather than borrowed: `/` is the root of *your* namespace, which is
    /// Plan 9's answer and the only one that does not imply a namespace above you.
    ///
    /// Returns the number of bytes written. `out` should be [`RENDER_MAX`] bytes; a shorter buffer
    /// truncates rather than panicking, because a shell rendering a prompt must not fault.
    pub fn render(&self, out: &mut [u8]) -> usize {
        let mut n = 0;
        let mut put = |b: u8, n: &mut usize| {
            if *n < out.len() {
                out[*n] = b;
                *n += 1;
            }
        };
        if self.depth == 0 {
            put(b'/', &mut n);
            return n;
        }
        for level in 0..self.depth {
            put(b'/', &mut n);
            for &b in self.component(level) {
                put(b, &mut n);
            }
        }
        n
    }

    /// Apply a sequence of steps, as [`path`] parsed them, **without sending anything**: this is the
    /// planning half, used to resolve a name at grant time. The shell's `cd` walks the same steps
    /// against real capabilities and keeps this in step with them.
    ///
    /// **All of it or none of it**: a step that cannot be taken leaves the position exactly where it
    /// was. `cd a/b` failing at `b` and leaving you in `a` is the shape of bug that makes the next
    /// command act somewhere the user did not think they were, and Unix's `chdir` does not do it
    /// either. The shell's `cd` unwinds the capabilities it opened for the same reason.
    pub fn apply(&mut self, steps: &[Step<'_>]) -> Result<(), Refused> {
        let mut next = *self;
        for step in steps {
            match step {
                Step::Up => {
                    if !next.ascend() {
                        return Err(Refused::AtYourRoot);
                    }
                }
                Step::Down(name) => {
                    if !component_fits(name) {
                        return Err(Refused::NotAName);
                    }
                    if !next.descend(name) {
                        return Err(Refused::TooDeep);
                    }
                }
            }
        }
        *self = next;
        Ok(())
    }

    /// **Where a token leaves you**, which is [`apply`](Cwd::apply) with the one thing a token
    /// carries that a step sequence does not: whether it started at your root.
    ///
    /// Every caller that resolves a *token* should use this rather than `apply`, because a path
    /// that began with `/` means the same thing from anywhere and `apply` alone would silently
    /// resolve it from wherever the holder happened to be standing. That failure would not be a
    /// refusal, it would be an answer, and this file's own history says the dangerous refusal is
    /// the one that answers.
    ///
    /// It returns a new position rather than mutating, because most callers want to know where a
    /// name resolves *without moving*: a grant is planned against the position a token names, and
    /// the shell stays where it is.
    pub fn resolve(&self, p: &Path<'_>) -> Result<Cwd, Refused> {
        let mut next = if p.from_root() { Cwd::root() } else { *self };
        next.apply(p.steps())?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert what `pwd` would print. A helper rather than a returned `String` because this crate is
    /// `no_std` and has no allocator, in tests or out.
    fn assert_pwd(cwd: &Cwd, want: &[u8]) {
        let mut buf = [0u8; RENDER_MAX];
        let n = cwd.render(&mut buf);
        assert_eq!(
            core::str::from_utf8(&buf[..n]),
            core::str::from_utf8(want),
            "pwd",
        );
    }

    /// Walk a token from a starting position, the way `cd` does.
    fn cd(cwd: &mut Cwd, token: &str) -> Result<(), Refused> {
        let p = path(token.as_bytes())?;
        *cwd = cwd.resolve(&p)?;
        Ok(())
    }

    /// Capture `{:?}` without an allocator, for the same reason [`assert_pwd`] takes a buffer:
    /// this crate has no `String`, in tests or out.
    fn assert_debug(v: &dyn core::fmt::Debug, want: &str) {
        use core::fmt::Write;
        struct Buf {
            bytes: [u8; 64],
            n: usize,
        }
        impl Write for Buf {
            fn write_str(&mut self, s: &str) -> core::fmt::Result {
                let b = s.as_bytes();
                self.bytes[self.n..self.n + b.len()].copy_from_slice(b);
                self.n += b.len();
                Ok(())
            }
        }
        let mut buf = Buf {
            bytes: [0; 64],
            n: 0,
        };
        write!(buf, "{v:?}").unwrap();
        assert_eq!(core::str::from_utf8(&buf.bytes[..buf.n]), Ok(want));
    }

    /// The wording, exactly. Each message is the fixed half of a line the prompt prints, so a
    /// drift here is user-visible; a containment check would also pass an empty string.
    #[test]
    fn each_refusal_is_its_own_exact_sentence() {
        assert_eq!(
            Refused::NotAName.message(),
            "that is not a name: one component, at most 16 bytes",
        );
        assert_eq!(
            Refused::TooDeep.message(),
            "too deep: this shell tracks at most 8 levels below its root",
        );
        assert_eq!(
            Refused::AtYourRoot.message(),
            "you are at your root; there is nothing above it to name",
        );
    }

    /// `{:?}` on a position prints the path it means, which is the reason the impl is written by
    /// hand; pinned exactly, like a message, because every failing assertion that carries an
    /// endowment renders one. `last` and `is_root` ride along: they are the same two facts (the
    /// root, and the deepest component) read without a formatter.
    #[test]
    fn a_position_debugs_as_the_path_it_means() {
        let mut cwd = Cwd::root();
        assert_debug(&cwd, "/");
        assert_eq!(cwd.last(), None, "the root has no last component");
        cd(&mut cwd, "a/b").unwrap();
        assert_debug(&cwd, "/a/b");
        assert!(!cwd.is_root());
        assert_eq!(cwd.last(), Some(&b"b"[..]));
    }

    /// `RENDER_MAX` is the worst case exactly: the deepest position with the widest names fills
    /// all but one of its bytes, and the spare byte is the lone slash a root renders as, which
    /// never coexists with a level. Slack would hide a truncation bug behind a buffer that always
    /// happened to fit; a byte short would truncate a real prompt.
    #[test]
    fn render_max_is_the_worst_case_plus_the_roots_lone_slash() {
        let mut cwd = Cwd::root();
        for _ in 0..MAX_DEPTH {
            assert!(cwd.descend(b"sixteen-bytes!!!"));
        }
        let mut buf = [0u8; RENDER_MAX];
        let n = cwd.render(&mut buf);
        assert_eq!(n, RENDER_MAX - 1);
        // And nothing was truncated to make that true: a wider buffer renders the same count.
        let mut wide = [0u8; 2 * RENDER_MAX];
        assert_eq!(cwd.render(&mut wide), n);
    }

    /// **`pwd` is relative to your root**, and the root renders as `/` because it is the root of the
    /// only namespace you have.
    #[test]
    fn pwd_is_relative_to_your_own_root() {
        let mut cwd = Cwd::root();
        assert_pwd(&cwd, b"/");
        assert!(cwd.is_root());

        cd(&mut cwd, "logs").unwrap();
        assert_pwd(&cwd, b"/logs");
        cd(&mut cwd, "2026/july").unwrap();
        assert_pwd(&cwd, b"/logs/2026/july");
        assert_eq!(cwd.depth(), 3);
    }

    /// **`..` stops at your root**, which is the divergence and the safety property in one line. It
    /// is not a check on a path: at the root there is no capability to pop, so there is nothing to
    /// get wrong, and no request is sent for the FS server to have to refuse.
    #[test]
    fn dot_dot_clamps_at_your_root_and_never_climbs_out() {
        let mut cwd = Cwd::root();
        assert_eq!(cd(&mut cwd, ".."), Err(Refused::AtYourRoot));
        assert_pwd(&cwd, b"/"); // "a refused `..` must not have moved anything"

        cd(&mut cwd, "a/b").unwrap();
        cd(&mut cwd, "..").unwrap();
        assert_pwd(&cwd, b"/a");
        cd(&mut cwd, "..").unwrap();
        assert_pwd(&cwd, b"/");

        // And no amount of `..` in one token gets above it either, which is the case a per-step
        // check would pass and a "count the ups" one would not. The whole token is refused, so the
        // shell is still where it was rather than part of the way out.
        let mut cwd = Cwd::root();
        cd(&mut cwd, "a").unwrap();
        assert_eq!(cd(&mut cwd, "../../.."), Err(Refused::AtYourRoot));
        assert_pwd(&cwd, b"/a"); // "a refused path must move nothing at all"
    }

    /// `..` inside a path composes the way it does everywhere, as long as it stays inside.
    #[test]
    fn dot_dot_composes_inside_a_path() {
        let mut cwd = Cwd::root();
        cd(&mut cwd, "a/b/../c").unwrap();
        assert_pwd(&cwd, b"/a/c");
        cd(&mut cwd, "./d/.").unwrap();
        assert_pwd(&cwd, b"/a/c/d");
    }

    /// **`/` is the root of your own namespace**, which is Plan 9's answer: the syntax survives,
    /// and what it roots in is the one directory capability the holder was granted.
    #[test]
    fn an_absolute_path_is_rooted_in_your_own_namespace() {
        let p = path(b"/etc/passwd").unwrap();
        assert!(p.from_root());
        assert_eq!(p.steps(), &[Step::Down(b"etc"), Step::Down(b"passwd")]);

        // `/` alone is your root: a place, not a name, so it has no final component to act on.
        let root = path(b"/").unwrap();
        assert!(root.from_root());
        assert!(root.steps().is_empty());
        assert_eq!(root.split_last_component(), None);

        // A relative token that merely contains a slash is unchanged, and is *not* from the root.
        assert!(!path(b"a/b").unwrap().from_root());
    }

    /// **An absolute path means the same place wherever you stand**, which is the whole of what
    /// the leading `/` buys and the one thing a step sequence cannot say on its own.
    #[test]
    fn an_absolute_path_does_not_depend_on_where_you_are() {
        let mut deep = Cwd::root();
        cd(&mut deep, "a/b/c").unwrap();
        let mut shallow = Cwd::root();

        cd(&mut deep, "/logs/2026").unwrap();
        cd(&mut shallow, "/logs/2026").unwrap();
        assert_pwd(&deep, b"/logs/2026");
        assert_pwd(&shallow, b"/logs/2026");

        // And `pwd`'s output is now a token you can type back, which it was not before: this is
        // the round trip, and its absence was the tell that the refusal had outlived its reason.
        let mut buf = [0u8; RENDER_MAX];
        let n = deep.render(&mut buf);
        let mut elsewhere = Cwd::root();
        cd(&mut elsewhere, "somewhere/else").unwrap();
        let p = path(&buf[..n]).unwrap();
        assert_eq!(elsewhere.resolve(&p).unwrap(), deep);
    }

    /// **An absolute path stops at your root exactly as `..` does**, which is why rooting the
    /// syntax in your own namespace grants nothing: there is no level above the root to name, and
    /// `/..` asks for the same nonexistent place that `..` at the root asks for.
    #[test]
    fn an_absolute_path_cannot_climb_above_your_root() {
        let mut cwd = Cwd::root();
        cd(&mut cwd, "a/b").unwrap();
        assert_eq!(cd(&mut cwd, "/.."), Err(Refused::AtYourRoot));
        assert_eq!(cd(&mut cwd, "/../../elsewhere"), Err(Refused::AtYourRoot));
        assert_eq!(cd(&mut cwd, "/a/../../b"), Err(Refused::AtYourRoot));
        // A refused absolute path moves nothing, the same all-or-nothing rule a relative one has.
        assert_pwd(&cwd, b"/a/b");
    }

    /// **What an absolute path reaches, a walk could already have reached**: `/a/b` from anywhere
    /// is `cd` to the root and then two descents, so the syntax adds no reach. That is the
    /// property that keeps a namespace from becoming ambient authority, and it is stated here as
    /// an equality between two ways of arriving rather than as a claim in prose.
    #[test]
    fn an_absolute_path_reaches_exactly_what_a_walk_from_the_root_reaches() {
        for token in ["/a/b", "/logs/2026/july", "/", "/a/b/../c"] {
            let mut absolute = Cwd::root();
            cd(&mut absolute, "somewhere/deep").unwrap();
            cd(&mut absolute, token).unwrap();

            let mut walked = Cwd::root();
            for step in path(token.as_bytes()).unwrap().steps() {
                match step {
                    Step::Down(name) => assert!(walked.descend(name)),
                    Step::Up => assert!(walked.ascend()),
                }
            }
            assert_eq!(absolute, walked, "{token} did not land where a walk lands");
        }
    }

    /// The component rules, which are the FS server's own, checked at the prompt so a request that
    /// cannot succeed is never sent.
    #[test]
    fn a_component_that_cannot_be_a_name_is_refused_before_anything_is_sent() {
        assert_eq!(path(b""), Err(Refused::NotAName));
        assert_eq!(path(b"."), Err(Refused::NotAName));
        assert_eq!(path(b"/seventeen-bytes!!"), Err(Refused::NotAName));
        assert_eq!(path(b"a/seventeen-bytes!!"), Err(Refused::NotAName));
        assert_eq!(path(b"a/b\\c"), Err(Refused::NotAName));
        assert_eq!(path(b"a:b"), Err(Refused::NotAName));
        // Nine components, one more than the shell tracks. Refused rather than truncated: a
        // truncated path names a different directory.
        assert_eq!(path(b"a/a/a/a/a/a/a/a/a"), Err(Refused::TooDeep));
    }

    /// A descent past the depth the shell tracks is refused, and refused **before** it moves.
    #[test]
    fn a_path_deeper_than_the_shell_tracks_is_refused_rather_than_truncated() {
        let mut cwd = Cwd::root();
        for _ in 0..MAX_DEPTH {
            cd(&mut cwd, "d").unwrap();
        }
        assert_eq!(cd(&mut cwd, "d"), Err(Refused::TooDeep));
        assert_eq!(cwd.depth(), MAX_DEPTH);
    }

    /// The grant-time split: the directory to narrow, and the one name in it.
    #[test]
    fn a_token_splits_into_the_directory_to_resolve_and_the_final_name() {
        let p = path(b"logs/2026/report.txt").unwrap();
        let (lead, name) = p.split_last_component().unwrap();
        assert_eq!(name, b"report.txt");
        assert_eq!(lead, &[Step::Down(b"logs"), Step::Down(b"2026")]);

        // A bare name is the common case: no lead, and the name is resolved where you stand.
        let p = path(b"report.txt").unwrap();
        let (lead, name) = p.split_last_component().unwrap();
        assert!(lead.is_empty());
        assert_eq!(name, b"report.txt");

        // A path ending in `..` designates a directory, so it cannot be the file half of a grant.
        assert!(path(b"a/..").unwrap().split_last_component().is_none());
    }

    /// A render into a short buffer truncates instead of faulting: a shell drawing a prompt must
    /// never be the thing that takes the session down.
    #[test]
    fn rendering_into_a_short_buffer_truncates_rather_than_faulting() {
        let mut cwd = Cwd::root();
        cd(&mut cwd, "abc/def").unwrap();
        let mut small = [0u8; 5];
        let n = cwd.render(&mut small);
        assert_eq!(&small[..n], b"/abc/");
        let mut none = [0u8; 0];
        assert_eq!(cwd.render(&mut none), 0);
    }
}
