//! **Navigation in a system with no global namespace** (milestone 47's commands).
//!
//! `cd`, `pwd` and `ls` are *builtins*, in the same category as `caps`: they spawn nothing, need no
//! grant, and confer no new authority, because the shell is reading and rebinding a capability it
//! already holds. That is not a technicality, it retires a real worry: a listing **program** would
//! have to be handed the power to read everything it lists. This module is the pure half of those
//! builtins, host-tested in milliseconds; the requests they make live in `user/src/shell.rs`.
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
//! - **No absolute paths** ([`Refused::Absolute`]). There is no namespace to root one in. The
//!   refusal is that the name **cannot be expressed**, which is why the `std` PAL answers
//!   `InvalidFilename` and not `PermissionDenied`: nothing checked a permission.
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
    /// It began with `/`. **There is no namespace to root an absolute path in**, so this is not a
    /// refusal of access, it is that the name has no meaning here. Plan 9 kept the syntax by making
    /// `/` the root of *your* namespace; that is the roadmap's recommendation and it is not built,
    /// so the syntax is refused rather than quietly reinterpreted as relative.
    Absolute,
    /// A component that is not a name: empty, longer than [`MAX_NAME`], or carrying a byte a name
    /// cannot carry.
    NotAName,
    /// More components than [`MAX_DEPTH`], or a descent that would take the shell past it.
    TooDeep,
    /// `..` at the root. The clamp, reported rather than silently ignored: at your root there is
    /// nothing above to name, and a shell that said nothing would leave a user believing they had
    /// moved.
    AtYourRoot,
}

impl Refused {
    /// The line the shell prints. In the capability model's voice, like [`crate::Refusal::message`]:
    /// each says what *is*, never what was denied.
    pub fn message(self) -> &'static str {
        match self {
            Refused::Absolute => {
                "no absolute path: there is no namespace here to root one in, so that name cannot \
                 be expressed"
            }
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

/// A parsed relative path: a bounded sequence of [`Step`]s, validated before anything is sent.
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
}

impl<'a> Path<'a> {
    /// The steps, in order.
    pub fn steps(&self) -> &[Step<'a>] {
        &self.steps[..self.n]
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
/// Unix's behaviour and there is no divergence to earn. A leading `/` is [`Refused::Absolute`],
/// which is checked first because it is the biggest fact about the token.
pub fn path(token: &[u8]) -> Result<Path<'_>, Refused> {
    if token.first() == Some(&b'/') {
        return Err(Refused::Absolute);
    }
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
    if n == 0 {
        // `.` or `/`-only: it names the place you already are, which no verb here needs and no
        // verb here can act on. Refusing it beats acting on a name the user did not type.
        return Err(Refused::NotAName);
    }
    Ok(Path { steps, n })
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
        cwd.apply(p.steps())
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

    /// **An absolute path is refused as unnameable, not as forbidden.** The distinction is the whole
    /// point: nothing checked a permission, the name has no referent here.
    #[test]
    fn an_absolute_path_cannot_be_expressed() {
        assert_eq!(path(b"/etc/passwd"), Err(Refused::Absolute));
        assert_eq!(path(b"/"), Err(Refused::Absolute));
        assert!(
            !Refused::Absolute.message().contains("denied"),
            "an unnameable name must not be reported as a permission",
        );
        // A relative token that merely contains a slash is fine: it is a path, and the shell walks
        // it one component at a time.
        assert!(path(b"a/b").is_ok());
    }

    /// The component rules, which are the FS server's own, checked at the prompt so a request that
    /// cannot succeed is never sent.
    #[test]
    fn a_component_that_cannot_be_a_name_is_refused_before_anything_is_sent() {
        assert_eq!(path(b""), Err(Refused::NotAName));
        assert_eq!(path(b"."), Err(Refused::NotAName));
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
