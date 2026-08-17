//! **Share-relative paths, parsed once at the wire's edge** (milestone 54's subdirectory half).
//!
//! The share was flat until this module existed: [`crate::server`] rejected any name containing a
//! backslash with `STATUS_OBJECT_PATH_NOT_FOUND` and the [`crate::share::Share`] trait took a bare
//! name. A Time Machine sparsebundle is a **directory of band files**, so a flat share cannot hold
//! one, which makes this the largest single piece between milestone 54 and milestone 55.
//!
//! # Why the parsing is here and not in the backing
//!
//! Everything in this module is a decision about **what a client is allowed to say**, and that is
//! wire format: a path traversal is a wire attack, and `..` must die at the edge rather than
//! wherever a backing happens to look at it. Keeping it in this crate is rule 7 (AGENTS.md), and it
//! is what makes the refusals below host-testable and Kani-reachable rather than buried in a
//! `no_std` binary.
//!
//! So a backing never sees a raw client string. It receives a [`Path`], which is a type that cannot
//! be constructed without going through [`Path::parse`], and therefore cannot hold `..`, an empty
//! component, or a separator it would have to split on again.
//!
//! # The refusals, and what each one is for
//!
//! - **`..` is [`PathError::Traversal`]**, refused rather than resolved. Resolving it would mean
//!   this crate deciding what is above the share root, and the whole capability argument is that
//!   there is no above: `fs_proto`'s `OPENDIR` answers `EINVAL` for a name that is not a single
//!   component for the same reason. Refusing is also the only answer that stays correct if a
//!   component is a symlink one day.
//! - **`.` is [`PathError::Traversal`] too.** It is harmless to resolve and it is not harmless to
//!   *accept*: a client that sends `a\.\b` and one that sends `a\b` would then name the same file by
//!   two paths, and this share's handles carry their path as their identity (rename and
//!   delete-on-close both read it back). One path per name is cheaper to hold than a canonicaliser.
//! - **An empty component is [`PathError::Empty`]**, which is what `a\\b` and a leading `\\` are.
//!   The one exception is a **trailing** separator, which is stripped: clients do send `dir\`, and
//!   refusing it would be pedantry with a real cost.
//! - **A forward slash is [`PathError::Separator`].** SMB's separator is `\`; `/` is a character
//!   Windows forbids in a name and Unix uses as a separator, so accepting it would let one client
//!   address a file that another client, and the filesystem underneath, spell differently.
//! - **Over-long is [`PathError::TooLong`]**, per component ([`MAX_COMPONENT`]) and for the whole
//!   path ([`MAX_PATH`]). Both are this share's bounds rather than the filesystem's: a handle keeps
//!   its own copy of its path, so the bound is what a connection's handle table costs.
//!
//! # BUGS
//!
//! - **The reserved characters SMB forbids in a name are not checked** (`:` `*` `?` `"` `<` `>`
//!   `|`). A client that creates `a?b` gets a file called `a?b` on the filesystem, which no Windows
//!   client will be able to open afterwards. The filesystem takes the name, so this is a
//!   compatibility gap rather than a safety one, and it is recorded rather than fixed because
//!   nothing macOS sends contains them.
//! - **Case is folded before this module runs**, by [`crate::utf16le_to_ascii_lower`], so
//!   `Bands\1A` and `bands\1a` are the same path here and the second is what reaches the
//!   filesystem. The crate BUGS already record that an upper-case name on disk is unreachable; a
//!   path makes that true per component.
//! - **Depth is bounded only by [`MAX_PATH`].** A path of 64 one-byte components is legal, and the
//!   fs-backed share pays one `OPENDIR` per component to walk it, so a deep path is a slow one.
//!   Nothing refuses it, because there is no depth at which the answer becomes wrong.

/// **The longest share-relative path**, in bytes, separators included. A handle stores its whole
/// path (rename, delete-on-close and `QUERY_INFO`'s name class all read it back and none of them
/// can re-derive it from a listing that moves), so this is what a connection's handle table costs:
/// [`crate::server::MAX_HANDLES`] times this.
///
/// 128 is chosen against the workload rather than picked round: a sparsebundle's deepest path is
/// `<name>.sparsebundle\bands\<hex>`, which is comfortably inside it with room for a folder or two
/// above.
pub const MAX_PATH: usize = 128;

/// The longest single component, matching [`crate::server::MAX_NAME`]: a path is made of names, and
/// a component that would not be a legal name is not made legal by having a directory in front of
/// it.
pub const MAX_COMPONENT: usize = 64;

/// The separator on the wire. SMB is Windows-descended and Windows-descended is backslash.
pub const SEP: u8 = b'\\';

/// Why a path was refused. Each one becomes a different NT status (see
/// [`crate::server::status_for_path`]), because "you named something above the share" and "that name
/// is too long" are different things for a client to do about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathError {
    /// A `.` or `..` component. See the module header on why neither is resolved.
    Traversal,
    /// An empty component: `a\\b`, or a leading separator after the first is stripped.
    Empty,
    /// A forward slash, which is not this protocol's separator and not a legal name character.
    Separator,
    /// Longer than [`MAX_PATH`], or a component longer than [`MAX_COMPONENT`].
    TooLong,
}

/// **A share-relative path that has already been checked.** Components separated by one [`SEP`],
/// no leading or trailing separator, and no component that is empty, `.`, or `..`. The empty path
/// is the share root, which is the one path with no components.
///
/// Constructible only by [`Path::parse`] and [`Path::ROOT`], which is the point: a function taking a
/// `Path` cannot be handed a raw client string by mistake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Path<'a>(&'a [u8]);

impl<'a> Path<'a> {
    /// The share's root directory: the path with no components.
    pub const ROOT: Path<'static> = Path(b"");

    /// **Parse one client-supplied path.** `raw` is the lower-cased ASCII the wire decoded
    /// ([`crate::utf16le_to_ascii_lower`]); a leading separator and a single trailing one are
    /// stripped, and everything else must already be well-formed.
    ///
    /// An empty result is [`Path::ROOT`], which is what a CREATE of the share itself sends.
    pub fn parse(raw: &'a [u8]) -> Result<Path<'a>, PathError> {
        let mut s = raw;
        // A leading separator is the client saying "from the share root", which is where every path
        // here starts anyway. Only one: `\\a` has an empty first component and is a malformed path,
        // not a UNC one, because the share is already chosen by TREE_CONNECT.
        if s.first() == Some(&SEP) {
            s = &s[1..];
        }
        // A single trailing separator is `dir\`, which clients do send for a directory.
        if s.last() == Some(&SEP) {
            s = &s[..s.len() - 1];
        }
        if s.is_empty() {
            return Ok(Path::ROOT);
        }
        if s.len() > MAX_PATH {
            return Err(PathError::TooLong);
        }
        for comp in s.split(|&b| b == SEP) {
            if comp.is_empty() {
                return Err(PathError::Empty);
            }
            if comp == b"." || comp == b".." {
                return Err(PathError::Traversal);
            }
            if comp.contains(&b'/') {
                return Err(PathError::Separator);
            }
            if comp.len() > MAX_COMPONENT {
                return Err(PathError::TooLong);
            }
        }
        Ok(Path(s))
    }

    /// The path as it is stored and compared: components joined by [`SEP`], no leading or trailing
    /// one. Empty for the root.
    pub const fn as_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Is this the share's own root directory?
    pub const fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// The components, in order. Empty for the root.
    ///
    /// Not `slice::split`: that yields one empty item for an empty slice, which would make the root
    /// look like a path with one nameless component. The root has none, and a walk over it must do
    /// nothing rather than try to descend into `""`.
    pub fn components(&self) -> Components<'a> {
        Components {
            rest: if self.0.is_empty() {
                None
            } else {
                Some(self.0)
            },
        }
    }

    /// How many components the path has. 0 for the root.
    pub fn depth(&self) -> usize {
        if self.is_root() {
            0
        } else {
            self.0.iter().filter(|&&b| b == SEP).count() + 1
        }
    }

    /// The last component: the name being opened, created or removed. Empty for the root, which
    /// names nothing.
    pub fn name(&self) -> &'a [u8] {
        match self.0.iter().rposition(|&b| b == SEP) {
            Some(i) => &self.0[i + 1..],
            None => self.0,
        }
    }

    /// The directory the last component lives in. [`Path::ROOT`] for a one-component path, and
    /// the root's parent is itself: a share has nothing above it, which is the whole confinement
    /// claim rather than a convenience.
    pub fn parent(&self) -> Path<'a> {
        match self.0.iter().rposition(|&b| b == SEP) {
            Some(i) => Path(&self.0[..i]),
            None => Path::ROOT,
        }
    }

    /// Is `self` `other` or below it? What a caller asks before moving a directory into its own
    /// subtree, which is the one rename a filesystem cannot do.
    pub fn starts_with(&self, other: Path<'_>) -> bool {
        if other.is_root() {
            return true;
        }
        if !self.0.starts_with(other.as_bytes()) {
            return false;
        }
        // A prefix must end on a component boundary: `bandsx` does not start with `bands`.
        matches!(self.0.get(other.as_bytes().len()), None | Some(&SEP))
    }
}

/// What [`Path::components`] returns. A named type rather than an `impl Iterator` because a
/// backing walking a path stores one in a local and `impl Trait` in return position cannot be
/// named at the call site in a `no_std` binary without extra ceremony.
pub struct Components<'a> {
    rest: Option<&'a [u8]>,
}

impl<'a> Iterator for Components<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let rest = self.rest?;
        match rest.iter().position(|&b| b == SEP) {
            Some(i) => {
                self.rest = Some(&rest[i + 1..]);
                Some(&rest[..i])
            }
            None => {
                self.rest = None;
                Some(rest)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::vec::Vec;

    fn comps(p: Path<'_>) -> Vec<&[u8]> {
        p.components().collect()
    }

    #[test]
    fn a_flat_name_and_a_nested_one_both_parse() {
        let p = Path::parse(b"hello.txt").unwrap();
        assert_eq!(comps(p), [b"hello.txt".as_slice()]);
        assert_eq!(p.depth(), 1);
        assert_eq!(p.name(), b"hello.txt");
        assert!(p.parent().is_root());

        let p = Path::parse(b"backup.sparsebundle\\bands\\0a").unwrap();
        assert_eq!(
            comps(p),
            [
                b"backup.sparsebundle".as_slice(),
                b"bands".as_slice(),
                b"0a".as_slice()
            ]
        );
        assert_eq!(p.depth(), 3);
        assert_eq!(p.name(), b"0a");
        assert_eq!(p.parent().as_bytes(), b"backup.sparsebundle\\bands");
        assert_eq!(p.parent().parent().as_bytes(), b"backup.sparsebundle");
        assert!(p.parent().parent().parent().is_root());
    }

    /// The three shapes a client writes the root as, all one path. A CREATE of the share itself
    /// sends the empty name; a browse sends `\`.
    #[test]
    fn the_root_is_the_path_with_no_components() {
        for raw in [b"".as_slice(), b"\\".as_slice()] {
            let p = Path::parse(raw).unwrap();
            assert!(p.is_root(), "{raw:?}");
            assert_eq!(p.depth(), 0);
            assert_eq!(comps(p).len(), 0);
            assert_eq!(p.name(), b"");
        }
        assert!(Path::ROOT.is_root());
        // The root's parent is itself: there is nothing above a share.
        assert!(Path::ROOT.parent().is_root());
    }

    /// A leading separator is stripped and a trailing one is stripped, and neither changes what the
    /// path names. The doubling is not the same thing and is refused.
    #[test]
    fn one_leading_and_one_trailing_separator_are_stripped() {
        assert_eq!(Path::parse(b"\\a\\b").unwrap().as_bytes(), b"a\\b");
        assert_eq!(Path::parse(b"a\\b\\").unwrap().as_bytes(), b"a\\b");
        assert_eq!(Path::parse(b"\\a\\b\\").unwrap().as_bytes(), b"a\\b");
        assert_eq!(Path::parse(b"\\\\a").unwrap_err(), PathError::Empty);
        assert_eq!(Path::parse(b"a\\\\b").unwrap_err(), PathError::Empty);
        assert_eq!(Path::parse(b"a\\\\").unwrap_err(), PathError::Empty);
    }

    /// **The refusal that matters.** A traversal is refused rather than resolved, at every position
    /// a client can put one, so no backing ever has to be trusted to notice.
    #[test]
    fn no_path_can_climb_out_of_the_share() {
        for raw in [
            b"..".as_slice(),
            b"\\..".as_slice(),
            b"..\\..\\etc".as_slice(),
            b"a\\..\\b".as_slice(),
            b"a\\..".as_slice(),
            b"bands\\..\\..\\secrets".as_slice(),
        ] {
            assert_eq!(
                Path::parse(raw).unwrap_err(),
                PathError::Traversal,
                "{}",
                std::string::String::from_utf8_lossy(raw)
            );
        }
        // `.` too, and for a different reason: it is a second spelling of a path, and a handle's
        // path is its identity here.
        assert_eq!(Path::parse(b".").unwrap_err(), PathError::Traversal);
        assert_eq!(Path::parse(b"a\\.\\b").unwrap_err(), PathError::Traversal);

        // A name that merely begins with dots is a name, not a traversal. `..a` is a legal file.
        assert_eq!(Path::parse(b"..a").unwrap().name(), b"..a");
        assert_eq!(Path::parse(b"a\\...").unwrap().name(), b"...");
    }

    #[test]
    fn a_forward_slash_is_not_this_protocols_separator() {
        assert_eq!(Path::parse(b"a/b").unwrap_err(), PathError::Separator);
        assert_eq!(Path::parse(b"dir\\a/b").unwrap_err(), PathError::Separator);
        // And it is not a traversal escape either: the check is on the character, not on what
        // follows it.
        assert_eq!(Path::parse(b"a/../b").unwrap_err(), PathError::Separator);
    }

    #[test]
    fn the_two_length_bounds_are_separate_and_both_refuse() {
        let long_comp = [b'x'; MAX_COMPONENT + 1];
        assert_eq!(Path::parse(&long_comp).unwrap_err(), PathError::TooLong);
        let ok_comp = [b'x'; MAX_COMPONENT];
        assert_eq!(Path::parse(&ok_comp).unwrap().depth(), 1);

        // Legal components, illegal total. Built to exactly MAX_PATH (which passes) and then one
        // byte over (which does not), so the boundary itself is pinned rather than approached.
        let mut deep = Vec::new();
        while deep.len() < MAX_PATH {
            deep.push(if deep.len() % 3 == 2 { SEP } else { b'x' });
        }
        assert_eq!(deep.len(), MAX_PATH);
        assert!(Path::parse(&deep).is_ok(), "MAX_PATH itself must fit");
        deep.push(b'x');
        assert_eq!(Path::parse(&deep).unwrap_err(), PathError::TooLong);
    }

    /// `starts_with` must compare components, not bytes: the caller using it is refusing to move a
    /// directory into its own subtree, and a byte-prefix answer would refuse `bandsx` as well.
    #[test]
    fn containment_is_by_component_and_not_by_byte_prefix() {
        let bands = Path::parse(b"a\\bands").unwrap();
        assert!(Path::parse(b"a\\bands").unwrap().starts_with(bands));
        assert!(Path::parse(b"a\\bands\\0f").unwrap().starts_with(bands));
        assert!(!Path::parse(b"a\\bandsx").unwrap().starts_with(bands));
        assert!(!Path::parse(b"a").unwrap().starts_with(bands));
        // Everything is under the root, including the root.
        assert!(Path::parse(b"a\\b").unwrap().starts_with(Path::ROOT));
        assert!(Path::ROOT.starts_with(Path::ROOT));
        assert!(!Path::ROOT.starts_with(bands));
    }
}
