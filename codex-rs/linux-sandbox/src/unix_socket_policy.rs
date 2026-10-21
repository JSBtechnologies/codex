//! The per-path Unix socket allowlist of a managed network policy, resolved
//! for one sandboxed command.
//!
//! `permissions.<name>.network.unix_sockets` names the Unix-domain sockets a
//! sandboxed command may connect to. macOS enforces it in Seatbelt; on Linux
//! the supervisor in [`crate::unix_socket_supervisor`] enforces it with this
//! policy.
//!
//! Semantics, matching Seatbelt's `subpath` rules and Landlock's inode rules:
//!
//! - Each entry is resolved once, when the sandbox starts. Every command runs
//!   in a fresh sandbox, so a service restarted between commands is picked up
//!   by the next one; a socket replaced while a command runs is a different
//!   inode and is not allowed.
//! - An entry naming a socket allows that socket. An entry naming a directory
//!   allows the sockets beneath it.
//! - A connection is judged by the socket it reaches, after symlinks: a link to
//!   an allowed socket is allowed, a link to anything else is not.
//! - An entry that does not exist, or is neither a socket nor a directory, when
//!   the sandbox starts allows nothing.
use std::ffi::CString;
use std::io;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;

/// The identity of a file: its device and inode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileId {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

/// What `fstat` says about an open file.
pub(crate) struct Stat {
    pub(crate) id: FileId,
    pub(crate) is_socket: bool,
    pub(crate) is_dir: bool,
}

pub(crate) fn fstat(fd: &OwnedFd) -> io::Result<Stat> {
    // SAFETY: `st` is fully written by a successful fstat.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let kind = st.st_mode & libc::S_IFMT;
    Ok(Stat {
        id: FileId {
            dev: st.st_dev,
            ino: st.st_ino,
        },
        is_socket: kind == libc::S_IFSOCK,
        is_dir: kind == libc::S_IFDIR,
    })
}

/// Open `path` relative to `dir` (or absolutely) as an `O_PATH` handle,
/// following symlinks as `connect(2)` does.
pub(crate) fn open_path(dir: Option<&OwnedFd>, path: &Path) -> io::Result<OwnedFd> {
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let dirfd = dir.map_or(libc::AT_FDCWD, AsRawFd::as_raw_fd);
    // SAFETY: a valid NUL-terminated path and directory descriptor.
    let fd = unsafe { libc::openat(dirfd, c_path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by openat and is owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The path an open handle currently refers to, as the kernel reports it.
fn current_path(fd: &OwnedFd) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// One resolved allowlist entry, kept open for the life of the sandbox.
struct Pinned {
    /// Where the entry resolved to when the sandbox started.
    path: PathBuf,
    id: FileId,
    /// Held so the inode stays allocated: its number can't be reused by
    /// another file while the sandbox compares against it.
    _fd: OwnedFd,
}

/// The resolved allowlist.
#[derive(Default)]
pub(crate) struct UnixSocketPolicy {
    sockets: Vec<Pinned>,
    dirs: Vec<Pinned>,
}

impl UnixSocketPolicy {
    /// Resolve `entries` (absolute paths from the managed network policy).
    /// Relative, missing and unusable entries allow nothing.
    pub(crate) fn resolve(entries: &[String]) -> Self {
        let mut policy = Self::default();
        for entry in entries {
            let path = Path::new(entry);
            if !path.is_absolute() {
                continue;
            }
            let Ok(fd) = open_path(None, path) else {
                continue;
            };
            let (Ok(stat), Ok(resolved)) = (fstat(&fd), current_path(&fd)) else {
                continue;
            };
            let pinned = Pinned {
                path: resolved,
                id: stat.id,
                _fd: fd,
            };
            if stat.is_socket {
                policy.sockets.push(pinned);
            } else if stat.is_dir {
                policy.dirs.push(pinned);
            }
        }
        policy
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sockets.is_empty() && self.dirs.is_empty()
    }

    /// Is the socket behind `target` (an `O_PATH` handle to the file a
    /// connection would reach) allowed?
    pub(crate) fn allows(&self, target: &OwnedFd) -> bool {
        let Ok(stat) = fstat(target) else {
            return false;
        };
        if !stat.is_socket {
            return false;
        }
        if self.sockets.iter().any(|pinned| pinned.id == stat.id) {
            return true;
        }
        if self.dirs.is_empty() {
            return false;
        }
        let Ok(path) = current_path(target) else {
            return false;
        };
        self.dirs.iter().any(|dir| {
            path != dir.path
                && path.starts_with(&dir.path)
                // The directory at that path must still be the one resolved
                // at start, not another one moved into its place.
                && open_path(None, &dir.path)
                    .and_then(|fd| fstat(&fd))
                    .is_ok_and(|now| now.id == dir.id)
        })
    }
}

#[cfg(test)]
#[path = "unix_socket_policy_tests.rs"]
mod tests;
