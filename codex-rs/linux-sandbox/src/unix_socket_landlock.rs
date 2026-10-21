//! Enforces a [`UnixSocketPolicy`] with Landlock's
//! `LANDLOCK_ACCESS_FS_RESOLVE_UNIX` right (ABI 9, Linux 7.1).
//!
//! The helper restricts itself before it forks the command, so the command
//! inherits the domain. The kernel then checks every `connect()` and
//! addressed `sendmsg()` to a pathname socket against rules on the policy's
//! pinned entries, with the same semantics the supervisor implements: a rule
//! on a socket allows that inode, a rule on a directory allows the sockets
//! beneath it, and lookups follow symlinks. Sockets created inside the domain
//! stay reachable, and abstract sockets created outside it are refused.
//!
//! Unlike the supervisor, the kernel checks datagram sends too, so Unix
//! datagram sockets stay usable.
use std::io;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;

use crate::unix_socket_policy::UnixSocketPolicy;

const LANDLOCK_CREATE_RULESET_VERSION: libc::c_uint = 1 << 0;
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;
const LANDLOCK_ACCESS_FS_RESOLVE_UNIX: u64 = 1 << 16;
const LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;

/// The first ABI with `LANDLOCK_ACCESS_FS_RESOLVE_UNIX`.
const RESOLVE_UNIX_ABI: libc::c_long = 9;

/// `struct landlock_ruleset_attr`, as of ABI 6.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// `struct landlock_path_beneath_attr`, which the kernel declares packed.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// Whether this kernel's Landlock can enforce a Unix socket allowlist.
pub(crate) fn supported() -> bool {
    // SAFETY: the version query takes no attribute.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    abi >= RESOLVE_UNIX_ABI
}

/// Restricts the calling thread, and the command it later forks, to the
/// pathname sockets `policy` allows. Sets `no_new_privs`, which Landlock
/// requires.
pub(crate) fn restrict_current_thread(policy: &UnixSocketPolicy) -> io::Result<()> {
    let attr = RulesetAttr {
        handled_access_fs: LANDLOCK_ACCESS_FS_RESOLVE_UNIX,
        handled_access_net: 0,
        scoped: LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET,
    };
    // SAFETY: `attr` is a valid ruleset attribute of the given size.
    let ruleset = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr,
            size_of::<RulesetAttr>(),
            0,
        )
    };
    if ruleset < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor, owned here.
    let ruleset = unsafe { OwnedFd::from_raw_fd(ruleset as libc::c_int) };
    for entry in policy.entries() {
        let rule = PathBeneathAttr {
            allowed_access: LANDLOCK_ACCESS_FS_RESOLVE_UNIX,
            parent_fd: entry.as_raw_fd(),
        };
        // SAFETY: `rule` is a valid path-beneath rule; the entry's O_PATH
        // handle stays open for the call.
        let added = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                LANDLOCK_RULE_PATH_BENEATH,
                &rule,
                0,
            )
        };
        if added < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: prctl takes no pointers here.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: restrict_self takes the ruleset descriptor and flags.
    if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
#[path = "unix_socket_landlock_tests.rs"]
mod tests;
