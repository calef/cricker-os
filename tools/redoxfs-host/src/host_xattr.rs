//! Putting an extended attribute back onto a **host** file, which is the last step of a recovery
//! (milestone 57).
//!
//! `std` wraps no part of this. The call is `setxattr` on macOS and `lsetxattr` on Linux, the
//! argument lists differ, and every other Unix spells it differently again (FreeBSD has
//! `extattr_set_link`), so this module is three implementations behind one signature and a fourth
//! arm that refuses honestly on a platform nobody has taught it.
//!
//! **Three properties the callers depend on, and each is a decision.**
//!
//! *Links are not followed.* macOS passes `XATTR_NOFOLLOW`; Linux uses the `l`-prefixed entry point.
//! An extract recreates a symlink as a symlink, so following it would hang the attributes of the
//! link onto whatever the *host* happens to have at the target path today, which is somebody else's
//! file. Linux then refuses `user.*` attributes on a symlink outright, and that refusal is reported
//! rather than hidden (see [`set`]).
//!
//! *A failure is an [`std::io::Error`] carrying the platform's errno*, because which errno it is
//! decides what a recovering human should do. `ENOTSUP` means the destination filesystem does not
//! hold attributes and the answer is to extract somewhere else; `EPERM` on Linux means the name has
//! no `user.` prefix; `E2BIG` means the value is over that filesystem's ceiling. A recovery tool
//! that printed "failed" for all three would be telling somebody at 2am to guess.
//!
//! *Names are bytes.* The store holds bytes ([`fs_proto::xattr::valid_name`] refuses only NUL and
//! over-length), and both platform calls take a NUL-terminated C string, so the only conversion
//! needed is the terminator. Nothing here invents a namespace prefix: a tool that silently rewrote
//! `foo` to `user.foo` to satisfy Linux would hand back a file whose metadata does not say what the
//! backup said.

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// macOS's "operate on the link itself" flag (`sys/xattr.h`). Linux spells the same intent as a
/// separate function, which is why this constant is macOS-only.
#[cfg(target_os = "macos")]
const XATTR_NOFOLLOW: libc::c_int = 0x0001;

/// A path and an attribute name as C strings, or an error naming which one could not be one.
///
/// A NUL inside either is the only way this fails. It cannot come from a store the FS server wrote
/// (`valid_name` refuses it on the way in), so reaching this arm means the blob is damaged or was
/// written by something else, and a recovery tool must say that rather than truncate the name at the
/// NUL and set a *different* attribute.
fn c_args(path: &Path, name: &[u8]) -> io::Result<(CString, CString)> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the path contains a NUL"))?;
    let c_name = CString::new(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the attribute name contains a NUL, so this blob was not written by the FS server",
        )
    })?;
    Ok((c_path, c_name))
}

/// Set one extended attribute on `path` itself, not following a symlink.
///
/// **The type code cannot come along.** No host filesystem has a per-attribute type word, so
/// [`fs_proto::xattr`]'s `kind` is dropped here and the caller reports it (notes/host-recovery.md).
/// The bytes are what a reader needs; the codes remain readable in the extracted `.cricker-attrs`
/// blobs, which is why those are still copied out.
#[cfg(target_os = "macos")]
pub fn set(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
    let (c_path, c_name) = c_args(path, name)?;
    // SAFETY: both pointers are NUL-terminated for the length libc will read, and `value` is a live
    // slice whose length is passed alongside it. The call reads and does not retain either.
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            XATTR_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Set one extended attribute on `path` itself, not following a symlink.
///
/// See the macOS arm for what "not following" buys and for why the type code is dropped. Linux adds
/// one refusal of its own that a recovering human meets immediately: attributes in the `user.`
/// namespace are not permitted on symlinks or on any non-regular file, so those come back `EPERM`
/// and are counted as skipped rather than swallowed.
#[cfg(target_os = "linux")]
pub fn set(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
    let (c_path, c_name) = c_args(path, name)?;
    // SAFETY: as the macOS arm. `flags` is 0, which is set-or-replace, matching the store's own
    // semantics (there is no XATTR_CREATE/XATTR_REPLACE in this contract).
    let rc = unsafe {
        libc::lsetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The platforms this tool has not been taught. It refuses rather than compiling to a silent
/// success, and the message says what the reader still has: the raw store, extracted beside the
/// files.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn set(_path: &Path, _name: &[u8], _value: &[u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this build of redoxfs-host has no extended-attribute call for this operating system; \
         the attributes are still in the extracted .cricker-attrs directory",
    ))
}

/// Read one extended attribute back off a host file, for the tests and for nothing else.
///
/// It exists so the round trip can be closed *here*, in the same run that wrote it, instead of
/// asking a reader to trust that `setxattr` returned zero. The recovery paths never call it.
#[cfg(target_os = "macos")]
pub fn get(path: &Path, name: &[u8]) -> io::Result<Vec<u8>> {
    let (c_path, c_name) = c_args(path, name)?;
    // SAFETY: a null buffer with a zero length is how both platforms are asked for the size.
    let len = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            XATTR_NOFOLLOW,
        )
    };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: `buf` is exactly the length just measured. A racing writer could have grown the value
    // in between, which the second call reports as ERANGE rather than overrunning.
    let got = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
            0,
            XATTR_NOFOLLOW,
        )
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// Read one extended attribute back off a host file, for the tests and for nothing else. See the
/// macOS arm.
#[cfg(target_os = "linux")]
pub fn get(path: &Path, name: &[u8]) -> io::Result<Vec<u8>> {
    let (c_path, c_name) = c_args(path, name)?;
    // SAFETY: a null buffer with a zero length is how both platforms are asked for the size.
    let len = unsafe { libc::lgetxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: `buf` is exactly the length just measured.
    let got = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr().cast(),
            buf.len(),
        )
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// See [`set`]'s fallback arm.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn get(_path: &Path, _name: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this build of redoxfs-host has no extended-attribute call for this operating system",
    ))
}
