use super::BoundSockets;
use super::SECCOMP_IOCTL_NOTIF_ID_VALID;
use super::SECCOMP_IOCTL_NOTIF_RECV;
use super::SECCOMP_IOCTL_NOTIF_SEND;
use super::SOCK_DIAG_BY_FAMILY;
use super::SocketFile;
use super::UNIX_DIAG_NAME;
use super::UNIX_DIAG_VFS;
use super::parse_dump;
use super::unix_socket_path;
use pretty_assertions::assert_eq;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;

#[test]
fn seccomp_ioctls_match_the_kernel_headers() {
    assert_eq!(
        [
            SECCOMP_IOCTL_NOTIF_RECV,
            SECCOMP_IOCTL_NOTIF_SEND,
            SECCOMP_IOCTL_NOTIF_ID_VALID,
        ],
        [0xc050_2100, 0xc018_2101, 0x4008_2102]
    );
}

fn address(path: &[u8]) -> Vec<u8> {
    let mut address = (libc::AF_UNIX as libc::sa_family_t).to_ne_bytes().to_vec();
    address.extend_from_slice(path);
    address
}

#[test]
fn a_pathname_address_names_its_path_up_to_the_terminator() {
    assert_eq!(
        unix_socket_path(&address(b"/run/app.sock\0junk")),
        Ok(Some(&b"/run/app.sock"[..]))
    );
    assert_eq!(
        unix_socket_path(&address(b"relative.sock")),
        Ok(Some(&b"relative.sock"[..]))
    );
}

#[test]
fn an_abstract_address_is_refused() {
    assert_eq!(unix_socket_path(&address(b"\0abstract")), Err(libc::EPERM));
}

#[test]
fn addresses_the_kernel_rejects_are_left_to_it() {
    assert_eq!(unix_socket_path(&address(b"")), Ok(None));
    assert_eq!(unix_socket_path(&[0]), Ok(None));
    let mut inet = (libc::AF_INET as libc::sa_family_t).to_ne_bytes().to_vec();
    inet.extend_from_slice(b"/not/a/path");
    assert_eq!(unix_socket_path(&inet), Ok(None));
    let too_long = address(&[b'a'; 109]);
    assert_eq!(unix_socket_path(&too_long), Err(libc::EINVAL));
}

fn reply(kind: u16, seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut message = ((16 + payload.len()) as u32).to_ne_bytes().to_vec();
    message.extend(kind.to_ne_bytes());
    message.extend(0_u16.to_ne_bytes());
    message.extend(seq.to_ne_bytes());
    message.extend(0_u32.to_ne_bytes());
    message.extend_from_slice(payload);
    while !message.len().is_multiple_of(4) {
        message.push(0);
    }
    message
}

fn socket_reply(seq: u32, file: Option<(u32, u32)>, name: &[u8]) -> Vec<u8> {
    // struct unix_diag_msg
    let mut payload = vec![libc::AF_UNIX as u8, libc::SOCK_STREAM as u8, 10, 0];
    payload.extend([0; 12]);
    // An attribute the dump doesn't use, to be skipped.
    payload.extend(7_u16.to_ne_bytes());
    payload.extend(0_u16.to_ne_bytes());
    payload.extend([b'x', b'y', b'z', 0]);
    payload.extend(((4 + name.len()) as u16).to_ne_bytes());
    payload.extend(UNIX_DIAG_NAME.to_ne_bytes());
    payload.extend_from_slice(name);
    while !payload.len().is_multiple_of(4) {
        payload.push(0);
    }
    if let Some((ino, dev)) = file {
        payload.extend(12_u16.to_ne_bytes());
        payload.extend(UNIX_DIAG_VFS.to_ne_bytes());
        payload.extend(ino.to_ne_bytes());
        payload.extend(dev.to_ne_bytes());
    }
    reply(SOCK_DIAG_BY_FAMILY, seq, &payload)
}

#[test]
fn a_dump_collects_the_files_of_its_own_replies() {
    let mut batch = socket_reply(7, Some((42, 0x0010_0003)), b"/run/a.sock\0");
    batch.extend(socket_reply(7, Some((43, 1)), b"\0abstract"));
    batch.extend(socket_reply(7, None, b"/run/no-file.sock\0"));
    // A late reply to an earlier dump is ignored.
    batch.extend(socket_reply(6, Some((99, 1)), b"/run/late.sock\0"));
    let mut files = Vec::new();

    assert_eq!(parse_dump(&batch, 7, &mut files).ok(), Some(false));
    assert_eq!(
        parse_dump(&reply(libc::NLMSG_DONE as u16, 7, &[0; 4]), 7, &mut files).ok(),
        Some(true)
    );
    assert_eq!(
        files,
        vec![
            SocketFile {
                dev: 0x0010_0003,
                ino: 42,
                name: Some(b"/run/a.sock".to_vec()),
            },
            SocketFile {
                dev: 1,
                ino: 43,
                name: None,
            },
        ]
    );
}

#[test]
fn a_dump_error_or_malformed_reply_fails() {
    let error = reply(libc::NLMSG_ERROR as u16, 1, &(-libc::ENOENT).to_ne_bytes());
    assert_eq!(
        parse_dump(&error, 1, &mut Vec::new()).map_err(|err| err.raw_os_error()),
        Err(Some(libc::ENOENT))
    );
    let mut truncated = socket_reply(1, Some((1, 1)), b"/a\0");
    truncated.truncate(20);
    assert!(parse_dump(&truncated, 1, &mut Vec::new()).is_err());
}

#[test]
fn diagnostics_list_a_listening_socket_bound_in_this_network_namespace() {
    let Ok(bound_sockets) = BoundSockets::open() else {
        eprintln!("skipping: socket diagnostics are unavailable");
        return;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("listening.sock");
    let _listener = UnixListener::bind(&path).expect("bind");
    let metadata = std::fs::metadata(&path).expect("stat socket");
    let file = SocketFile {
        dev: (libc::major(metadata.dev()) << 20) | libc::minor(metadata.dev()),
        ino: metadata.ino() as u32,
        name: Some(path.as_os_str().as_bytes().to_vec()),
    };

    let files = bound_sockets.listening().expect("dump listening sockets");

    assert_eq!(files.contains(&file), true);
}
