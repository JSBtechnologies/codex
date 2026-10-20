//! Enforces a [`UnixSocketPolicy`] by brokering `connect(2)` through seccomp
//! user notification (`seccomp_unotify(2)`).
//!
//! The sandboxed command runs under a second seccomp filter that stops every
//! `connect()` and hands it to the sandbox helper, the command's parent. The
//! helper copies the address out of the caller's memory, takes a duplicate of
//! the caller's socket with `pidfd_getfd(2)` and makes the call itself:
//!
//! - an `AF_UNIX` socket may reach a pathname socket the policy allows, or one
//!   bound inside the sandbox; abstract addresses are refused;
//! - any other socket is connected as asked, from the sandbox's own network
//!   namespace, so what it can reach is unchanged.
//!
//! The kernel never re-runs a call the helper has checked, so the caller can't
//! change the address or the descriptor afterwards, and the helper connects
//! through an `O_PATH` handle to the socket it checked, so a rename can't swap
//! the target either.
//!
//! A stream or seqpacket socket reaches a path only through `connect()`. A
//! datagram socket names one on every send, so seccomp refuses Unix datagram
//! sockets while this supervisor enforces the policy.
use std::ffi::CString;
use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::io;
use std::io::Read;
use std::io::Write;
use std::mem::offset_of;
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use crate::unix_socket_policy::FileId;
use crate::unix_socket_policy::UnixSocketPolicy;
use crate::unix_socket_policy::fstat;
use crate::unix_socket_policy::open_path;

/// An errno returned to the sandboxed caller.
type Errno = libc::c_int;

/// The audit architecture of this build; the network seccomp filter supports
/// the same two.
const AUDIT_ARCH: Option<u32> = if cfg!(target_arch = "x86_64") {
    Some(0xc000_003e)
} else if cfg!(target_arch = "aarch64") {
    Some(0xc000_00b7)
} else {
    None
};

/// Marks x32 system calls, which share x86_64's audit architecture but not its
/// numbers, so neither seccomp filter would recognise them.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;

/// At most this many calls are brokered at once; more fail with `EAGAIN`.
const MAX_IN_FLIGHT: usize = 256;

/// Once the helper has picked a call up, only a fatal signal can interrupt the
/// caller. Otherwise a signal would restart a `connect()` the helper may
/// already have made.
const FILTER_FLAGS: libc::c_ulong =
    libc::SECCOMP_FILTER_FLAG_NEW_LISTENER | libc::SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV;

const HANDOFF_OK: u8 = 1;

/// The seccomp listener's ioctls (`_IOWR`/`_IOW` with magic `'!'`), which
/// this libc doesn't define yet.
const fn seccomp_ioctl(direction: u64, nr: u64, size: usize) -> libc::Ioctl {
    ((direction << 30) | ((size as u64) << 16) | ((b'!' as u64) << 8) | nr) as libc::Ioctl
}
const IOC_WRITE: u64 = 1;
const IOC_READ_WRITE: u64 = 3;
const SECCOMP_IOCTL_NOTIF_RECV: libc::Ioctl =
    seccomp_ioctl(IOC_READ_WRITE, 0, size_of::<libc::seccomp_notif>());
const SECCOMP_IOCTL_NOTIF_SEND: libc::Ioctl =
    seccomp_ioctl(IOC_READ_WRITE, 1, size_of::<libc::seccomp_notif_resp>());
const SECCOMP_IOCTL_NOTIF_ID_VALID: libc::Ioctl = seccomp_ioctl(IOC_WRITE, 2, size_of::<u64>());

const MAX_ADDRESS_LEN: usize = size_of::<libc::sockaddr_storage>();

/// Brokers one sandboxed command's `connect()` calls.
pub(crate) struct UnixSocketSupervisor {
    policy: UnixSocketPolicy,
    bound_sockets: Option<BoundSockets>,
    filter: Vec<libc::sock_filter>,
    handoff: Handoff,
}

impl UnixSocketSupervisor {
    /// Checks that this kernel and sandbox can run the supervisor, and
    /// prepares it for one command.
    ///
    /// Runs before the network seccomp filter, which refuses the netlink
    /// socket the supervisor keeps, and while the helper is single-threaded.
    pub(crate) fn prepare(policy: UnixSocketPolicy) -> io::Result<Self> {
        let arch = AUDIT_ARCH.ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "unsupported architecture")
        })?;
        // Callers are found under /proc by the thread id seccomp reports in
        // this PID namespace, which is only right when /proc belongs to it.
        // The helper is the namespace's init, so a /proc from an outer
        // namespace never names it by the same number.
        if fs::read_link("/proc/self")?.as_os_str() != std::process::id().to_string().as_str() {
            return Err(io::Error::other(
                "/proc does not belong to the sandbox's PID namespace",
            ));
        }
        let filter = connect_filter(arch);
        probe(&filter)?;
        Ok(Self {
            policy,
            // Without socket diagnostics only allowed sockets connect.
            bound_sockets: BoundSockets::open().ok(),
            filter,
            handoff: Handoff::new()?,
        })
    }

    /// In the forked command, before exec: install the filter and hand its
    /// listener to the helper. Exits on failure, so the command never runs
    /// unsupervised.
    pub(crate) fn enter(&self) {
        if let Err(message) = install_and_hand_off(&self.filter, &self.handoff) {
            exit_in_child(message);
        }
    }

    /// In the helper after fork: take the command's listener and broker its
    /// calls on a background thread for the rest of the helper's life.
    pub(crate) fn serve(self, command_pid: libc::pid_t) -> io::Result<()> {
        // The command runs as the same user, so while the helper is dumpable
        // the command could take the listener or write the helper's memory.
        // SAFETY: prctl takes no pointers here.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let Self {
            policy,
            bound_sockets,
            mut handoff,
            ..
        } = self;
        let listener = handoff.receive_listener(command_pid)?;
        let broker = Arc::new(Broker {
            listener,
            policy,
            bound_sockets,
            in_flight: AtomicUsize::new(0),
        });
        std::thread::Builder::new()
            .name("unix-socket-supervisor".to_string())
            .spawn(move || broker.run())?;
        handoff.release_child()
    }
}

/// The filter the command runs under: every `connect()` goes to the helper.
fn connect_filter(arch: u32) -> Vec<libc::sock_filter> {
    fn statement(code: u32, k: u32) -> libc::sock_filter {
        libc::sock_filter {
            code: code as u16,
            jt: 0,
            jf: 0,
            k,
        }
    }
    fn jump(test: u32, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
        libc::sock_filter {
            code: (libc::BPF_JMP | test | libc::BPF_K) as u16,
            jt,
            jf,
            k,
        }
    }
    let jump_if_equal = |k: u32, jt: u8, jf: u8| jump(libc::BPF_JEQ, k, jt, jf);
    let load = |offset: usize| statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, offset as u32);
    let ret = |action: u32| statement(libc::BPF_RET | libc::BPF_K, action);
    let refuse = || ret(libc::SECCOMP_RET_ERRNO | libc::EPERM as u32);

    let mut program = vec![
        load(offset_of!(libc::seccomp_data, arch)),
        jump_if_equal(arch, 1, 0),
        // The network seccomp filter kills other architectures too.
        ret(libc::SECCOMP_RET_KILL_PROCESS),
        load(offset_of!(libc::seccomp_data, nr)),
    ];
    #[cfg(target_arch = "x86_64")]
    program.extend([jump(libc::BPF_JSET, X32_SYSCALL_BIT, 0, 1), refuse()]);
    program.extend([
        // The helper holds the listener; the command must not take it.
        jump_if_equal(libc::SYS_pidfd_getfd as u32, 0, 1),
        refuse(),
        jump_if_equal(libc::SYS_connect as u32, 0, 1),
        ret(libc::SECCOMP_RET_USER_NOTIF),
        ret(libc::SECCOMP_RET_ALLOW),
    ]);
    program
}

/// Runs the handoff once with a throwaway child, so a kernel or sandbox that
/// can't support the supervisor is found before seccomp admits Unix sockets.
fn probe(filter: &[libc::sock_filter]) -> io::Result<()> {
    let mut handoff = Handoff::new()?;
    // SAFETY: the helper is single-threaded here, and the child only makes
    // allocation-free calls before exiting.
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(io::Error::last_os_error());
    }
    if child == 0 {
        // SAFETY: no_new_privs lets this unprivileged child install a filter.
        let ok = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0
            && install_and_hand_off(filter, &handoff).is_ok();
        // SAFETY: leave without running the parent's exit handlers.
        unsafe { libc::_exit(if ok { 0 } else { 1 }) };
    }
    let result = handoff.receive_listener(child).and_then(|_listener| {
        // The broker also reads its callers' memory.
        File::open(format!("/proc/{child}/mem"))?;
        handoff.release_child()
    });
    drop(handoff);
    let mut status = 0;
    // SAFETY: reaps the child forked above.
    unsafe { libc::waitpid(child, &mut status, 0) };
    result
}

/// Two pipes over which the command hands its filter's listener to the
/// helper: it sends the descriptor's number, and waits until the helper has
/// taken a copy.
struct Handoff {
    number_read: File,
    number_write: Option<OwnedFd>,
    release_read: Option<OwnedFd>,
    release_write: File,
}

impl Handoff {
    fn new() -> io::Result<Self> {
        let (number_read, number_write) = pipe()?;
        let (release_read, release_write) = pipe()?;
        Ok(Self {
            number_read: File::from(number_read),
            number_write: Some(number_write),
            release_read: Some(release_read),
            release_write: File::from(release_write),
        })
    }

    /// In the helper: take a copy of the listener of child `pid`.
    fn receive_listener(&mut self, pid: libc::pid_t) -> io::Result<OwnedFd> {
        // Drop the child's ends, so a read sees the child exit.
        self.number_write = None;
        self.release_read = None;
        let mut number = [0; size_of::<libc::c_int>()];
        self.number_read.read_exact(&mut number).map_err(|_| {
            io::Error::other("the sandboxed command could not install its seccomp filter")
        })?;
        pidfd_getfd(&pidfd_open(pid, 0)?, libc::c_int::from_ne_bytes(number))
    }

    /// In the helper: let the child go on.
    fn release_child(&mut self) -> io::Result<()> {
        self.release_write.write_all(&[HANDOFF_OK])
    }
}

/// In a forked child: install the filter, hand its listener to the helper and
/// wait until the helper holds a copy. Allocation-free, as it runs between
/// fork and exec.
fn install_and_hand_off(
    filter: &[libc::sock_filter],
    handoff: &Handoff,
) -> Result<(), &'static str> {
    let (Some(number_write), Some(release_read)) = (&handoff.number_write, &handoff.release_read)
    else {
        return Err("the seccomp handoff was already used");
    };
    let program = libc::sock_fprog {
        len: filter.len() as libc::c_ushort,
        filter: filter.as_ptr().cast_mut(),
    };
    // SAFETY: `program` points at a valid BPF program for the whole call.
    let listener = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            FILTER_FLAGS,
            &program,
        )
    };
    if listener < 0 {
        return Err("cannot install the Unix socket supervisor's seccomp filter");
    }
    let listener = listener as libc::c_int;
    let number = listener.to_ne_bytes();
    // SAFETY: writes from a live buffer to a pipe this process owns.
    let written = unsafe {
        libc::write(
            number_write.as_raw_fd(),
            number.as_ptr().cast(),
            number.len(),
        )
    };
    let mut release = 0_u8;
    // SAFETY: reads into a live byte from a pipe this process owns.
    let released = written == number.len() as isize
        && unsafe { libc::read(release_read.as_raw_fd(), (&raw mut release).cast(), 1) } == 1
        && release == HANDOFF_OK;
    // SAFETY: the helper holds its own copy; the command must not keep one.
    unsafe { libc::close(listener) };
    if released {
        Ok(())
    } else {
        Err("the sandbox helper did not take the seccomp listener")
    }
}

fn exit_in_child(message: &'static str) -> ! {
    for part in ["codex-linux-sandbox: ", message, "\n"] {
        // SAFETY: writes a static string to stderr.
        unsafe { libc::write(libc::STDERR_FILENO, part.as_ptr().cast(), part.len()) };
    }
    // SAFETY: leave without running the parent's exit handlers.
    unsafe { libc::_exit(1) }
}

/// Answers the command's `connect()` calls.
struct Broker {
    listener: OwnedFd,
    policy: UnixSocketPolicy,
    bound_sockets: Option<BoundSockets>,
    in_flight: AtomicUsize,
}

impl Broker {
    fn run(self: Arc<Self>) {
        loop {
            // SAFETY: seccomp_notif is plain data, and RECV wants it zeroed.
            let mut request: libc::seccomp_notif = unsafe { std::mem::zeroed() };
            // SAFETY: RECV writes into `request`, which outlives the call.
            let received = unsafe {
                libc::ioctl(
                    self.listener.as_raw_fd(),
                    SECCOMP_IOCTL_NOTIF_RECV,
                    &mut request,
                )
            };
            if received < 0 {
                match io::Error::last_os_error().raw_os_error() {
                    // Interrupted, or the caller died before the call was
                    // picked up.
                    Some(libc::EINTR | libc::ENOENT) => continue,
                    // With no one listening, the kernel fails later calls.
                    _ => return,
                }
            }
            // A connect() can block on a full listen queue or a slow
            // handshake, so each call gets its own thread, up to a limit.
            if self.in_flight.fetch_add(1, Ordering::SeqCst) >= MAX_IN_FLIGHT {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                self.respond(request.id, Err(libc::EAGAIN));
                continue;
            }
            let broker = Arc::clone(&self);
            let spawned = std::thread::Builder::new().spawn(move || {
                broker.answer(&request);
                broker.in_flight.fetch_sub(1, Ordering::SeqCst);
            });
            if spawned.is_err() {
                self.in_flight.fetch_sub(1, Ordering::SeqCst);
                self.respond(request.id, Err(libc::EAGAIN));
            }
        }
    }

    fn answer(&self, request: &libc::seccomp_notif) {
        self.respond(request.id, self.connect(request));
    }

    fn respond(&self, id: u64, result: Result<(), Errno>) {
        let mut response = libc::seccomp_notif_resp {
            id,
            val: 0,
            error: result.err().map_or(0, |errno| -errno),
            flags: 0,
        };
        // SAFETY: SEND reads `response`, which outlives the call. It fails
        // only when the caller is gone, and then there is no one to answer.
        unsafe {
            libc::ioctl(
                self.listener.as_raw_fd(),
                SECCOMP_IOCTL_NOTIF_SEND,
                &mut response,
            )
        };
    }

    /// Makes the caller's `connect()` on its behalf, or refuses it.
    fn connect(&self, request: &libc::seccomp_notif) -> Result<(), Errno> {
        let caller = Caller {
            listener: &self.listener,
            id: request.id,
            tid: request.pid,
        };
        let [fd, address_ptr, address_len, ..] = request.data.args;
        // connect(2) takes an int descriptor and length; the kernel ignores
        // the upper bits.
        let (fd, address_len) = (fd as u32 as libc::c_int, address_len as u32 as libc::c_int);
        // Checked in the order connect(2) checks them.
        let socket = caller.take_fd(fd)?;
        let mut storage = [0_u8; MAX_ADDRESS_LEN];
        let address = usize::try_from(address_len)
            .ok()
            .and_then(|len| storage.get_mut(..len))
            .ok_or(libc::EINVAL)?;
        caller.read_memory(address_ptr, address)?;
        if socket_domain(&socket)? == libc::AF_UNIX
            && let Some(path) = unix_socket_path(address)?
        {
            let target = self.resolve(&caller, path)?;
            return connect_through(&socket, &target);
        }
        connect_raw(&socket, address)
    }

    /// Opens the socket `path` names for the caller, as `connect(2)` would
    /// resolve it, if the caller may reach it.
    fn resolve(&self, caller: &Caller<'_>, path: &[u8]) -> Result<OwnedFd, Errno> {
        let path = Path::new(OsStr::from_bytes(path));
        let target = if path.is_absolute() {
            open_path(None, path)
        } else {
            let cwd = caller.open_proc("cwd", libc::O_PATH | libc::O_DIRECTORY)?;
            open_path(Some(&cwd), path)
        }
        .map_err(|err| errno(&err))?;
        let stat = fstat(&target).map_err(|err| errno(&err))?;
        if !stat.is_socket {
            return Err(libc::ECONNREFUSED);
        }
        if self.policy.allows(&target) || self.bound_in_sandbox(stat.id) {
            Ok(target)
        } else {
            Err(libc::EPERM)
        }
    }

    /// Whether the socket file `target` belongs to a socket bound inside the
    /// sandbox. Socket diagnostics list only the sockets of the sandbox's own
    /// network namespace, which sockets bound outside the sandbox are not in.
    fn bound_in_sandbox(&self, target: FileId) -> bool {
        let Some(bound_sockets) = &self.bound_sockets else {
            return false;
        };
        // The kernel's internal device encoding, which diagnostics use.
        let dev = (libc::major(target.dev) << 20) | libc::minor(target.dev);
        bound_sockets.listening().is_ok_and(|sockets| {
            sockets.iter().any(|socket| {
                // Diagnostics truncate inode numbers to 32 bits, so the
                // socket's bound name must also lead to exactly this file.
                socket.dev == dev
                    && socket.ino == target.ino as u32
                    && socket.name.as_deref().is_some_and(|name| {
                        let name = Path::new(OsStr::from_bytes(name));
                        name.is_absolute()
                            && open_path(None, name)
                                .and_then(|fd| fstat(&fd))
                                .is_ok_and(|stat| stat.id == target)
                    })
            })
        })
    }
}

/// The thread whose `connect()` is being answered.
struct Caller<'a> {
    listener: &'a OwnedFd,
    id: u64,
    tid: u32,
}

impl Caller<'_> {
    /// Fails once the call is no longer pending: the caller died, and its
    /// thread id may already name another thread.
    fn check_pending(&self) -> Result<(), Errno> {
        // SAFETY: ID_VALID reads the id, which outlives the call.
        let pending = unsafe {
            libc::ioctl(
                self.listener.as_raw_fd(),
                SECCOMP_IOCTL_NOTIF_ID_VALID,
                &self.id,
            )
        };
        if pending == 0 {
            Ok(())
        } else {
            Err(libc::ENOENT)
        }
    }

    /// Opens `/proc/<tid>/<name>`, checking afterwards that it is the
    /// caller's.
    fn open_proc(&self, name: &str, flags: libc::c_int) -> Result<OwnedFd, Errno> {
        let path = CString::new(format!("/proc/{}/{name}", self.tid)).map_err(|_| libc::EINVAL)?;
        // SAFETY: a valid NUL-terminated path.
        let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(last_errno());
        }
        // SAFETY: `fd` was just returned by open and is owned here.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        self.check_pending()?;
        Ok(fd)
    }

    fn read_memory(&self, address: u64, buffer: &mut [u8]) -> Result<(), Errno> {
        if buffer.is_empty() {
            return Ok(());
        }
        let memory = File::from(self.open_proc("mem", libc::O_RDONLY)?);
        memory
            .read_exact_at(buffer, address)
            .map_err(|_| libc::EFAULT)
    }

    /// A duplicate of the caller's descriptor `fd`.
    fn take_fd(&self, fd: libc::c_int) -> Result<OwnedFd, Errno> {
        let tid = self.tid as libc::pid_t;
        let pidfd = match pidfd_open(tid, libc::PIDFD_THREAD) {
            Ok(pidfd) => pidfd,
            // Before Linux 6.9 only a thread-group leader has a pidfd.
            Err(err) if err.raw_os_error() == Some(libc::EINVAL) => {
                pidfd_open(self.thread_group()?, 0).map_err(|err| errno(&err))?
            }
            Err(err) => return Err(errno(&err)),
        };
        self.check_pending()?;
        pidfd_getfd(&pidfd, fd).map_err(|err| errno(&err))
    }

    fn thread_group(&self) -> Result<libc::pid_t, Errno> {
        let status =
            fs::read_to_string(format!("/proc/{}/status", self.tid)).map_err(|err| errno(&err))?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("Tgid:"))
            .and_then(|tgid| tgid.trim().parse().ok())
            .ok_or(libc::ESRCH)
    }
}

/// The pathname in a Unix socket address, or `None` for an address the
/// kernel rejects by itself. Abstract addresses are refused.
fn unix_socket_path(address: &[u8]) -> Result<Option<&[u8]>, Errno> {
    let Some((family, path)) = address.split_first_chunk::<2>() else {
        return Ok(None);
    };
    if libc::sa_family_t::from_ne_bytes(*family) != libc::AF_UNIX as libc::sa_family_t {
        return Ok(None);
    }
    if address.len() > size_of::<libc::sockaddr_un>() {
        return Err(libc::EINVAL);
    }
    match path.first() {
        None => Ok(None),
        Some(0) => Err(libc::EPERM),
        Some(_) => Ok(Some(
            path.iter()
                .position(|byte| *byte == 0)
                .map_or(path, |end| &path[..end]),
        )),
    }
}

fn socket_domain(socket: &OwnedFd) -> Result<libc::c_int, Errno> {
    let mut domain: libc::c_int = 0;
    let mut len = size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `len` bytes into `domain`.
    let result = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_DOMAIN,
            (&raw mut domain).cast(),
            &mut len,
        )
    };
    if result < 0 {
        return Err(last_errno());
    }
    Ok(domain)
}

/// Connects through the `O_PATH` handle to the checked socket, so the
/// connection reaches that socket even if its path changes meanwhile.
fn connect_through(socket: &OwnedFd, target: &OwnedFd) -> Result<(), Errno> {
    let mut address = (libc::AF_UNIX as libc::sa_family_t).to_ne_bytes().to_vec();
    address.extend_from_slice(format!("/proc/self/fd/{}", target.as_raw_fd()).as_bytes());
    address.push(0);
    connect_raw(socket, &address)
}

fn connect_raw(socket: &OwnedFd, address: &[u8]) -> Result<(), Errno> {
    // SAFETY: `address` is valid for its length.
    let result = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            address.as_ptr().cast(),
            address.len() as libc::socklen_t,
        )
    };
    if result < 0 {
        return Err(last_errno());
    }
    Ok(())
}

fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 writes two descriptors into `fds`.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both descriptors were just created and are owned here.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn pidfd_open(pid: libc::pid_t, flags: libc::c_uint) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open takes no pointers.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor, owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

fn pidfd_getfd(pidfd: &OwnedFd, target: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_getfd takes no pointers.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), target, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a new descriptor, owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) })
}

fn errno(err: &io::Error) -> Errno {
    err.raw_os_error().unwrap_or(libc::EIO)
}

fn last_errno() -> Errno {
    errno(&io::Error::last_os_error())
}

const SOCK_DIAG_BY_FAMILY: u16 = 20;
const UDIAG_SHOW_NAME: u32 = 0x1;
const UDIAG_SHOW_VFS: u32 = 0x2;
const UNIX_DIAG_NAME: u16 = 0;
const UNIX_DIAG_VFS: u16 = 1;
const TCP_LISTEN: u32 = 10;
const NLMSG_HEADER_LEN: usize = 16;
const UNIX_DIAG_MSG_LEN: usize = 16;

/// A bound socket's file, as socket diagnostics describe it: its device and
/// truncated inode number, and the pathname it was bound to.
#[derive(Debug, PartialEq, Eq)]
struct SocketFile {
    dev: u32,
    ino: u32,
    name: Option<Vec<u8>>,
}

/// Socket diagnostics (`sock_diag(7)`) for the sandbox's network namespace.
struct BoundSockets(Mutex<DiagSocket>);

struct DiagSocket {
    fd: OwnedFd,
    seq: u32,
}

impl BoundSockets {
    fn open() -> io::Result<Self> {
        // SAFETY: socket takes no pointers.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                libc::NETLINK_SOCK_DIAG,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(Mutex::new(DiagSocket {
            // SAFETY: a new descriptor, owned here.
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            seq: 0,
        })))
    }

    /// The files of the listening Unix sockets in this network namespace.
    fn listening(&self) -> io::Result<Vec<SocketFile>> {
        let mut socket = self
            .0
            .lock()
            .map_err(|_| io::Error::other("socket diagnostics lock poisoned"))?;
        socket.seq = socket.seq.wrapping_add(1);
        let seq = socket.seq;
        let request = dump_request(seq);
        // SAFETY: sends from a live buffer.
        if unsafe {
            libc::send(
                socket.fd.as_raw_fd(),
                request.as_ptr().cast(),
                request.len(),
                0,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // The kernel fills dump messages of at most 32 KiB.
        let mut buffer = vec![0_u8; 32 * 1024];
        let mut files = Vec::new();
        loop {
            // SAFETY: receives into a live buffer of the given length.
            let len = unsafe {
                libc::recv(
                    socket.fd.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };
            let len = usize::try_from(len).map_err(|_| io::Error::last_os_error())?;
            if parse_dump(&buffer[..len], seq, &mut files)? {
                return Ok(files);
            }
        }
    }
}

/// A `SOCK_DIAG_BY_FAMILY` dump request for listening Unix sockets and their
/// files.
fn dump_request(seq: u32) -> Vec<u8> {
    let mut request = Vec::with_capacity(40);
    // struct nlmsghdr
    request.extend(40_u32.to_ne_bytes());
    request.extend(SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    request.extend(((libc::NLM_F_REQUEST | libc::NLM_F_DUMP) as u16).to_ne_bytes());
    request.extend(seq.to_ne_bytes());
    request.extend(0_u32.to_ne_bytes());
    // struct unix_diag_req
    request.push(libc::AF_UNIX as u8);
    request.push(0);
    request.extend(0_u16.to_ne_bytes());
    request.extend((1_u32 << TCP_LISTEN).to_ne_bytes());
    request.extend(0_u32.to_ne_bytes());
    request.extend((UDIAG_SHOW_NAME | UDIAG_SHOW_VFS).to_ne_bytes());
    request.extend([0; 8]);
    request
}

/// Collects the socket files in one batch of dump replies to `seq`, and says
/// whether the dump is done.
fn parse_dump(mut batch: &[u8], seq: u32, files: &mut Vec<SocketFile>) -> io::Result<bool> {
    let malformed = || io::Error::other("malformed socket diagnostics reply");
    while !batch.is_empty() {
        let len = read_u32(batch, 0).ok_or_else(malformed)? as usize;
        let kind = read_u16(batch, 4).ok_or_else(malformed)?;
        let reply_seq = read_u32(batch, 8).ok_or_else(malformed)?;
        let payload = batch.get(NLMSG_HEADER_LEN..len).ok_or_else(malformed)?;
        if reply_seq == seq {
            match kind {
                kind if kind == libc::NLMSG_DONE as u16 => return Ok(true),
                kind if kind == libc::NLMSG_ERROR as u16 => {
                    let code = read_u32(payload, 0).ok_or_else(malformed)? as i32;
                    return Err(io::Error::from_raw_os_error(-code));
                }
                SOCK_DIAG_BY_FAMILY => files.extend(socket_file(payload)),
                _ => {}
            }
        }
        batch = batch.get(align4(len)..).unwrap_or_default();
    }
    Ok(false)
}

/// The file in one `struct unix_diag_msg` and its attributes.
fn socket_file(message: &[u8]) -> Option<SocketFile> {
    let mut attributes = message.get(UNIX_DIAG_MSG_LEN..)?;
    let mut file = None;
    let mut name = None;
    while !attributes.is_empty() {
        let len = usize::from(read_u16(attributes, 0)?);
        let kind = read_u16(attributes, 2)?;
        let payload = attributes.get(4..len)?;
        match kind {
            UNIX_DIAG_VFS => file = Some((read_u32(payload, 4)?, read_u32(payload, 0)?)),
            // A pathname, up to its terminator; abstract names start with NUL.
            UNIX_DIAG_NAME => {
                name = payload
                    .split(|byte| *byte == 0)
                    .next()
                    .filter(|path| !path.is_empty())
                    .map(<[u8]>::to_vec);
            }
            _ => {}
        }
        attributes = attributes.get(align4(len)..).unwrap_or_default();
    }
    let (dev, ino) = file?;
    Some(SocketFile { dev, ino, name })
}

fn align4(len: usize) -> usize {
    len.next_multiple_of(4)
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(*bytes.get(at..)?.first_chunk()?))
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(*bytes.get(at..)?.first_chunk()?))
}

#[cfg(test)]
#[path = "unix_socket_supervisor_tests.rs"]
mod tests;
