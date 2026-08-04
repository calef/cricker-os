//! **Expansion: what a pattern on the command line designates** (milestone 47's globbing lane).
//!
//! `crates/glob` answers "does this pattern match this name", and nothing else: it is a total
//! function on two byte strings with no filesystem in it. This module is the layer that turns that
//! into an **authority**, because on this system a glob is not a convenience for typing fewer
//! characters.
//!
//! # The property the whole lane exists for
//!
//! **The expansion you see is the grant.** `echo *.txt` prints literally the authority that `rm
//! *.txt` would transfer, because the matched set *is* the namespace the caretaker will serve. Unix
//! cannot make that claim: `rm`'s authority never came from the command line at all, and the glob
//! only told it which of its existing powers to use.
//!
//! That claim is only worth anything if the two go through **one** expander, which is what
//! [`Expander`] is for: `echo` drives it and so does the grant planner ([`crate::plan`]), over the
//! same directory listing, and neither can be talked into a different answer. The shell's guest test
//! runs both and compares.
//!
//! # Why the names are owned
//!
//! A name a pattern produced is **not a slice of the command line**. It comes out of a directory
//! listing the shell reads in rounds into one reused buffer, so a borrow would dangle at the second
//! round. [`Name`] and [`NameSet`] therefore own their bytes, and the consequence is worth naming
//! because it is the same rule the rest of a grant already follows: an [`crate::Endowment`] carries
//! values, not pointers back at the shell, so nothing that happens after a grant is planned can
//! change what it means.
//!
//! # What is not here
//!
//! No `**`, no qualifiers, no expansion of anything but the **last component** of a token. All
//! three are notes/glob.md's boundaries and none of them is a scheduling excuse: `**` is a
//! *traversal* feature (descending means holding a capability for the directory below), and a
//! qualifier like `*(.)` needs type, mtime and size per candidate, which turns one enumeration into
//! N `FSTAT`s and needs a read right beyond enumerate. Both are authority questions wearing matching
//! costumes, so they do not get answered inside a matcher.

use crate::Refusal;
use crate::nav::{self, Step};

/// The longest name a grant can carry, which is [`crate::MAX_FILE_NAME`].
pub const MAX_NAME: usize = crate::MAX_FILE_NAME;

/// **The most names one expansion may grant**, and the point where Unix's `ARG_MAX` reappears as a
/// *capability* limit rather than a buffer limit.
///
/// Unix refuses "argument list too long" because a fixed-size buffer ran out, which is why `xargs`
/// exists. Here the ceiling is that **you cannot hand a child a capability it can neither hold nor
/// show**: the set travels in a frame the caretaker keeps whole in a local (it has no allocator),
/// and the shell carries it through planning on the stack it has.
///
/// **Eight, measured rather than chosen.** Sixteen was the first answer and the machine refused it:
/// a set travels by value through the expander, the [`Expansion`], `designate`'s return and the
/// [`crate::Endowment`], four frames a debug build does not collapse, and the shell ran off the
/// bottom of its stack planning one grant. Eight names of sixteen bytes is 152 bytes a copy. It
/// matches `fs_proto::nameset::MAX_NAMES`, pinned by a host test.
///
/// The number matters less than the failure's shape: exceeding it is [`Refusal::TooManyNames`], a
/// refusal **at the prompt with nothing spawned**, never a truncation. A glob that quietly granted a
/// prefix of what it matched would be the worst thing this lane could produce, because the printed
/// preview and the actual transfer would disagree and only the printed one is checkable.
pub const MAX_NAMES: usize = 8;

/// One name, owned. See the module note: a name a pattern produced is not a slice of the line.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; MAX_NAME],
    len: u8,
}

/// Written as the name it holds, because a `Debug` full of zero padding is one nobody reads, and
/// [`crate::Endowment`] carries these into every failing assertion.
impl core::fmt::Debug for Name {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match core::str::from_utf8(self.as_bytes()) {
            Ok(s) => write!(f, "{s:?}"),
            Err(_) => write!(f, "{:?}", self.as_bytes()),
        }
    }
}

impl Name {
    /// A name, or `None` if it cannot travel in a grant at all. **`None` rather than a truncation**,
    /// for [`crate::file_name_fits`]'s reason: a truncated filename is a different file, and this is
    /// the path that decides what gets granted.
    pub fn new(name: &[u8]) -> Option<Name> {
        if !crate::file_name_fits(name) {
            return None;
        }
        let mut bytes = [0u8; MAX_NAME];
        bytes[..name.len()].copy_from_slice(name);
        Some(Name {
            bytes,
            len: name.len() as u8,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

/// **The namespace a grant designates**: one name for a plain token, the matched names for a
/// pattern.
///
/// This is the type that makes the milestone's finding true in the code rather than only in the
/// prose. `fs_file_caretaker` serves "a namespace of exactly one name"; globbing generalizes it to
/// a set, and a literal operand is simply the set of one. Same caretaker shape, same `fs_proto`
/// protocol above and below, wider namespace, nothing new in the kernel.
///
/// Each name carries what the directory said it was, because the caretaker answers `READDIR` from
/// the set itself (`fs_proto::nameset`), and because a listing is a rendering of authority.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NameSet {
    names: [Name; MAX_NAMES],
    dirs: [bool; MAX_NAMES],
    n: usize,
}

impl core::fmt::Debug for NameSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_list()
            .entries(self.iter().map(|(name, _)| name))
            .finish()
    }
}

impl Default for NameSet {
    fn default() -> Self {
        NameSet::empty()
    }
}

impl NameSet {
    pub const fn empty() -> Self {
        NameSet {
            names: [Name {
                bytes: [0; MAX_NAME],
                len: 0,
            }; MAX_NAMES],
            dirs: [false; MAX_NAMES],
            n: 0,
        }
    }

    /// The set of exactly one name: what a token with no pattern in it designates.
    pub fn one(name: &[u8], is_dir: bool) -> Option<NameSet> {
        let mut set = NameSet::empty();
        set.push(name, is_dir).then_some(set)
    }

    /// Add a name. `false` if the set is full or the name cannot travel in a grant; the caller must
    /// turn that into a refusal rather than carrying on with a smaller set.
    pub fn push(&mut self, name: &[u8], is_dir: bool) -> bool {
        if self.n == MAX_NAMES || self.contains(name) {
            return false;
        }
        match Name::new(name) {
            Some(n) => {
                self.names[self.n] = n;
                self.dirs[self.n] = is_dir;
                self.n += 1;
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    pub fn contains(&self, name: &[u8]) -> bool {
        self.iter().any(|(n, _)| n == name)
    }

    /// `(name, is_dir)` per name, in the order the directory yielded them.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], bool)> {
        (0..self.n).map(|i| (self.names[i].as_bytes(), self.dirs[i]))
    }

    /// The one name, when the set holds exactly one. A grant that must designate a single thing (a
    /// per-file grant) asks this and refuses when the answer is `None`.
    pub fn only(&self) -> Option<&[u8]> {
        (self.n == 1).then(|| self.names[0].as_bytes())
    }
}

/// **The one expander both `echo` and the grant planner drive.**
///
/// The caller enumerates a directory (which needs `ENUMERATE` on it, and nothing more) and offers
/// every entry; this decides membership and refuses at the two edges. Keeping it a fed collector
/// rather than a function over a slice is what lets the shell read a directory in rounds out of one
/// reused page without ever holding the whole listing.
pub struct Expander<'a> {
    pattern: &'a [u8],
    set: NameSet,
    /// A matched name the set could not take. Kept as a refusal rather than dropped, because the
    /// two ways it happens are the two ways a grant could silently become a prefix of what the
    /// command matched.
    refused: Option<Refusal>,
}

impl<'a> Expander<'a> {
    /// Expand `pattern`, which is one component: it is matched against a name, never against a path.
    pub fn new(pattern: &'a [u8]) -> Self {
        Expander {
            pattern,
            set: NameSet::empty(),
            refused: None,
        }
    }

    /// Offer one directory entry.
    ///
    /// The dot rule is `glob`'s default (`Dot::Special`), so `*` does not match `.config`, and that
    /// default is not a compatibility habit here: `rm *` hands a child authority over everything it
    /// matched, so the default reading is the one that **grants less**. A user who wants the
    /// dotfiles asks for them and takes the larger grant deliberately.
    pub fn offer(&mut self, name: &[u8], is_dir: bool) {
        if !glob::matches(self.pattern, name) || self.set.contains(name) {
            return;
        }
        if !crate::file_name_fits(name) {
            // It matched and it cannot travel in a grant. Refusing is the only honest answer: a
            // grant missing a name the command matched is a grant that does not mean what the line
            // said.
            self.refused.get_or_insert(Refusal::MatchNotNameable);
            return;
        }
        if !self.set.push(name, is_dir) {
            self.refused.get_or_insert(Refusal::TooManyNames);
        }
    }

    /// The set, or the refusal to print.
    ///
    /// **A pattern that matched nothing is a refusal**, which is zsh's default rather than bash's
    /// (bash passes the pattern through as a literal word).
    ///
    /// **The obvious argument for it is wrong, and it is worth recording which one.** The plausible
    /// version is that passing the pattern through is harmless because a name containing `*` would
    /// be refused further along. It would not: neither [`crate::file_name_fits`] nor the FS server's
    /// own `check_component` rejects `*` (they refuse the empty name, `.`, `..`, `/`, `\`, `:` and
    /// NUL, and nothing else), so `*.rs` is a perfectly well-formed component. Checked, not
    /// remembered.
    ///
    /// What that leaves is the real argument, and it is worse than the one it replaces. Passing the
    /// pattern through would build a **grant whose namespace is a name nobody has**, which is
    /// useless today and live tomorrow: the moment anything creates a file called `*.rs`, a command
    /// somebody typed months ago starts designating it. A grant should not be able to acquire a
    /// referent after it is written.
    ///
    /// And the model forces the same answer from the other side. **The expansion is the grant**, so
    /// an empty expansion is an empty grant, and running the command would be running it with an
    /// authority nobody named. Saying so at the prompt is the same rule that makes an unplaceable
    /// token a refusal instead of a shrug.
    pub fn finish(self) -> Result<NameSet, Refusal> {
        if let Some(r) = self.refused {
            return Err(r);
        }
        if self.set.is_empty() {
            return Err(Refusal::NoMatch);
        }
        Ok(self.set)
    }
}

/// **Whether a token's last component is a pattern**, refusing one anywhere earlier.
///
/// `logs/*.txt` is a pattern over the names in `logs`; `*/report.txt` is not something this system
/// can answer, and the reason is notes/glob.md's rather than a limitation of the matcher:
/// descending into a directory means **holding a capability for it**, so a pattern that selects
/// directories to walk is asking an authority question, and hiding one inside a string matcher is
/// the exact mistake this OS exists to not make.
///
/// A token ending in `..` designates a directory rather than a name, so there is no pattern in it.
pub fn magic_component(token: &[u8]) -> Result<bool, Refusal> {
    let parsed = nav::path(token).map_err(crate::nav_refusal)?;
    let steps = parsed.steps();
    let Some((last, lead)) = steps.split_last() else {
        return Ok(false);
    };
    for step in lead {
        if let Step::Down(name) = step
            && glob::has_magic(name)
        {
            return Err(Refusal::PatternInPath);
        }
    }
    Ok(match last {
        Step::Down(name) => glob::has_magic(name),
        Step::Up => false,
    })
}

/// **What the shell expanded, handed to the planner.**
///
/// The shell expands *before* it plans, which is also what Unix does, so there is no divergence to
/// earn here. The structural consequence is that [`crate::plan`] must see the **set** rather than
/// the pattern, since the endowment is the set; this is that argument, and the positional index is
/// carried so the placement stays explicit rather than being inferred from which slot happens to
/// want a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Expansion {
    at: Option<usize>,
    names: NameSet,
}

impl Expansion {
    /// Nothing on the line was a pattern, so every token stands for itself.
    pub const fn none() -> Self {
        Expansion {
            at: None,
            names: NameSet::empty(),
        }
    }

    /// The positional at `index` was a pattern, and these are the names it matched.
    pub fn at(index: usize, names: NameSet) -> Self {
        Expansion {
            at: Some(index),
            names,
        }
    }

    /// The names for the positional at `index`, if that is the one the shell expanded.
    pub fn for_positional(&self, index: usize) -> Option<NameSet> {
        (self.at == Some(index)).then_some(self.names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the expander over a listing, the way both `echo` and the planner do.
    fn expand(pattern: &[u8], listing: &[(&[u8], bool)]) -> Result<NameSet, Refusal> {
        let mut e = Expander::new(pattern);
        for &(name, is_dir) in listing {
            e.offer(name, is_dir);
        }
        e.finish()
    }

    const DIR: [(&[u8], bool); 4] = [
        (b"one.txt", false),
        (b"two.txt", false),
        (b"three.log", false),
        (b"logs", true),
    ];

    /// The base case, and the one every claim below is a variation on: the set is what matched, in
    /// listing order, and the entry type rides along because the caretaker serves the listing.
    #[test]
    fn the_set_is_what_matched_and_nothing_else() {
        let set = expand(b"*.txt", &DIR).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(b"one.txt") && set.contains(b"two.txt"));
        assert!(
            !set.contains(b"three.log"),
            "a name in the same directory that did not match is not in the grant",
        );
        assert_eq!(set.only(), None, "two names is not one name");

        let dirs = expand(b"log*", &DIR).unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs.only(), Some(&b"logs"[..]));
        assert_eq!(dirs.iter().next(), Some((&b"logs"[..], true)));
    }

    /// **A pattern that matched nothing is a refusal.** zsh's default, not bash's, and the reason is
    /// in [`Expander::finish`]: the expansion is the grant, so an empty expansion is an empty grant.
    #[test]
    fn a_pattern_that_matched_nothing_refuses_rather_than_passing_itself_through() {
        assert_eq!(expand(b"*.rs", &DIR), Err(Refusal::NoMatch));
        assert!(!Refusal::NoMatch.message().contains("denied"));
        // **And this is why bash's answer is not merely untidy here.** A pattern is a perfectly
        // well-formed component: nothing in this system refuses `*` in a name. So passing it
        // through would build a grant whose namespace is a name nobody has, and which starts
        // designating a real file the moment anything creates one called `*.rs`.
        assert!(
            crate::file_name_fits(b"*.rs"),
            "if this ever becomes false, the refusal above is still right for the other reason",
        );
    }

    /// **The bound is loud.** Seventeen matches is a refusal with nothing granted, never sixteen
    /// names quietly handed over.
    #[test]
    fn more_matches_than_the_bound_is_a_refusal_and_not_a_truncation() {
        let mut names = [[b'a'; 3]; MAX_NAMES + 1];
        for (i, n) in names.iter_mut().enumerate() {
            n[1] = b'a' + (i / 10) as u8;
            n[2] = b'0' + (i % 10) as u8;
        }
        let listing: [(&[u8], bool); MAX_NAMES + 1] =
            core::array::from_fn(|i| (&names[i][..], false));
        assert_eq!(expand(b"a*", &listing), Err(Refusal::TooManyNames));
        // Exactly the bound is fine: this is a ceiling that is met, not one with slack in it.
        assert_eq!(
            expand(b"a*", &listing[..MAX_NAMES]).unwrap().len(),
            MAX_NAMES
        );
    }

    /// A name that matched and cannot travel in a grant is a refusal too, for the same reason: the
    /// grant would otherwise be a prefix of what the command matched.
    #[test]
    fn a_matched_name_too_long_to_grant_refuses_the_whole_expansion() {
        let listing: [(&[u8], bool); 2] = [(b"one.txt", false), (b"seventeen-bytes!!", false)];
        assert_eq!(expand(b"*", &listing), Err(Refusal::MatchNotNameable));
    }

    /// The dot rule, which is a policy about **how much a default grants** rather than a matching
    /// fact: `rm *` must not reach a dotfile nobody typed.
    #[test]
    fn a_bare_star_does_not_reach_a_dotfile() {
        let listing: [(&[u8], bool); 2] = [(b".config", false), (b"visible", false)];
        let set = expand(b"*", &listing).unwrap();
        assert_eq!(set.only(), Some(&b"visible"[..]));
        assert_eq!(
            expand(b".*", &listing).unwrap().only(),
            Some(&b".config"[..]),
            "asking for them by name is how you take the larger grant",
        );
    }

    /// A pattern lives in the **last** component or nowhere, because selecting directories to walk
    /// is an authority question and not a matching one.
    #[test]
    fn a_pattern_may_only_be_the_last_component() {
        assert_eq!(magic_component(b"*.txt"), Ok(true));
        assert_eq!(magic_component(b"logs/*.txt"), Ok(true));
        assert_eq!(magic_component(b"report.txt"), Ok(false));
        assert_eq!(magic_component(b"logs/report.txt"), Ok(false));
        assert_eq!(
            magic_component(b"*/report.txt"),
            Err(Refusal::PatternInPath)
        );
        assert_eq!(magic_component(b"a/*/b"), Err(Refusal::PatternInPath));
        // The refusals a path already has still come first: the token is parsed before anything
        // asks whether it has magic in it.
        assert_eq!(magic_component(b"/etc/*"), Err(Refusal::NoAbsolutePath));
    }

    /// An expansion is placed at the positional it came from, and nowhere else.
    #[test]
    fn an_expansion_belongs_to_one_positional() {
        let set = NameSet::one(b"x", false).unwrap();
        let e = Expansion::at(1, set);
        assert_eq!(e.for_positional(0), None);
        assert_eq!(e.for_positional(1).unwrap().only(), Some(&b"x"[..]));
        assert_eq!(Expansion::none().for_positional(0), None);
    }

    /// The hand-written `Debug` impls exist so a failing assertion prints names instead of zero
    /// padding, so they are pinned exactly, like a message: a `Name` prints as the name, a
    /// `NameSet` as the list. Captured into a buffer because this crate has no allocator, in
    /// tests or out.
    #[test]
    fn names_and_sets_debug_as_what_they_hold() {
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
        let assert_debug = |v: &dyn core::fmt::Debug, want: &str| {
            let mut buf = Buf {
                bytes: [0; 64],
                n: 0,
            };
            write!(buf, "{v:?}").unwrap();
            assert_eq!(core::str::from_utf8(&buf.bytes[..buf.n]), Ok(want));
        };
        assert_debug(&Name::new(b"a.txt").unwrap(), "\"a.txt\"");
        // The set renders its names as byte lists, not through `Name`'s impl: `iter` yields
        // `&[u8]`, and a slice debugs as numbers. Pinned as it is; if the set is ever taught to
        // print `["a.txt", "logs"]`, this is the test that says so on purpose.
        let set = expand(b"*", &[(b"a.txt", false), (b"logs", true)]).unwrap();
        assert_debug(&set, "[[97, 46, 116, 120, 116], [108, 111, 103, 115]]");
    }

    /// A listing that offers the same name twice (`READDIR` re-reads the directory per round, so a
    /// name added or removed mid-listing can be seen twice) must not put it in the grant twice.
    #[test]
    fn a_name_offered_twice_is_in_the_set_once() {
        let listing: [(&[u8], bool); 3] = [(b"a.txt", false), (b"a.txt", false), (b"b.txt", false)];
        assert_eq!(expand(b"*.txt", &listing).unwrap().len(), 2);
    }
}
