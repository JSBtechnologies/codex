use super::PathBeneathAttr;
use super::RulesetAttr;
use super::restrict_current_thread;
use super::supported;
use crate::unix_socket_policy::UnixSocketPolicy;
use pretty_assertions::assert_eq;
use std::mem::size_of;
use std::os::unix::net::UnixDatagram;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;

#[test]
fn attributes_match_the_kernel_layout() {
    assert_eq!(size_of::<RulesetAttr>(), 24);
    assert_eq!(size_of::<PathBeneathAttr>(), 12);
}

fn connect_errno(path: &Path) -> Option<i32> {
    UnixStream::connect(path)
        .err()
        .and_then(|err| err.raw_os_error())
}

#[test]
fn a_restricted_thread_reaches_only_allowed_sockets() {
    if !supported() {
        eprintln!("skipping: Landlock RESOLVE_UNIX needs Linux 7.1");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let allowed = dir.path().join("allowed.sock");
    let unlisted = dir.path().join("unlisted.sock");
    let granted = dir.path().join("granted");
    std::fs::create_dir(&granted).expect("mkdir granted");
    let beneath = granted.join("beneath.sock");
    let unlisted_datagram = dir.path().join("unlisted.dgram");
    let _listeners =
        [&allowed, &unlisted, &beneath].map(|path| UnixListener::bind(path).expect("bind"));
    let _datagram = UnixDatagram::bind(&unlisted_datagram).expect("bind datagram");
    let policy = UnixSocketPolicy::resolve(&[
        allowed.to_string_lossy().into_owned(),
        granted.to_string_lossy().into_owned(),
    ]);
    let own = dir.path().join("own.sock");

    // Landlock restricts only the calling thread, so the test harness is
    // left alone.
    let results = std::thread::spawn(move || {
        restrict_current_thread(&policy).expect("restrict thread");
        let _own = UnixListener::bind(&own).expect("bind own");
        let sender = UnixDatagram::unbound().expect("datagram socket");
        [
            connect_errno(&allowed),
            connect_errno(&beneath),
            connect_errno(&own),
            connect_errno(&unlisted),
            sender
                .send_to(b"x", &unlisted_datagram)
                .err()
                .and_then(|err| err.raw_os_error()),
        ]
    })
    .join()
    .expect("restricted thread");

    assert_eq!(
        results,
        [None, None, None, Some(libc::EACCES), Some(libc::EACCES)]
    );
}
