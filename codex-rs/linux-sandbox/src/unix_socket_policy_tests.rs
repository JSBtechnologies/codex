use super::UnixSocketPolicy;
use super::open_path;
use pretty_assertions::assert_eq;
use std::os::unix::net::UnixListener;
use std::path::Path;

fn entry(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn allows(policy: &UnixSocketPolicy, path: &Path) -> bool {
    open_path(None, path).is_ok_and(|fd| policy.allows(&fd))
}

#[test]
fn a_socket_entry_allows_that_socket_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let allowed = dir.path().join("allowed.sock");
    let other = dir.path().join("other.sock");
    let _a = UnixListener::bind(&allowed).expect("bind allowed");
    let _b = UnixListener::bind(&other).expect("bind other");

    let policy = UnixSocketPolicy::resolve(&[entry(&allowed)]);

    assert_eq!(allows(&policy, &allowed), true);
    assert_eq!(allows(&policy, &other), false);
}

#[test]
fn a_directory_entry_allows_the_sockets_beneath_it() {
    let root = tempfile::tempdir().expect("tempdir");
    let granted = root.path().join("granted");
    let nested = granted.join("nested");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&nested).expect("mkdir nested");
    std::fs::create_dir_all(&outside).expect("mkdir outside");
    let _a = UnixListener::bind(granted.join("a.sock")).expect("bind a");
    let _b = UnixListener::bind(nested.join("b.sock")).expect("bind b");
    let _c = UnixListener::bind(outside.join("c.sock")).expect("bind c");

    let policy = UnixSocketPolicy::resolve(&[entry(&granted)]);

    assert_eq!(allows(&policy, &granted.join("a.sock")), true);
    assert_eq!(allows(&policy, &nested.join("b.sock")), true);
    assert_eq!(allows(&policy, &outside.join("c.sock")), false);
    // A socket created in the directory after the sandbox started is beneath
    // it too.
    let _d = UnixListener::bind(granted.join("d.sock")).expect("bind d");
    assert_eq!(allows(&policy, &granted.join("d.sock")), true);
}

#[test]
fn a_connection_is_judged_by_the_socket_a_symlink_reaches() {
    let dir = tempfile::tempdir().expect("tempdir");
    let allowed = dir.path().join("allowed.sock");
    let other = dir.path().join("other.sock");
    let _a = UnixListener::bind(&allowed).expect("bind allowed");
    let _b = UnixListener::bind(&other).expect("bind other");
    let to_allowed = dir.path().join("to-allowed");
    let to_other = dir.path().join("to-other");
    std::os::unix::fs::symlink(&allowed, &to_allowed).expect("symlink to allowed");
    std::os::unix::fs::symlink(&other, &to_other).expect("symlink to other");

    let policy = UnixSocketPolicy::resolve(&[entry(&allowed)]);

    assert_eq!(allows(&policy, &to_allowed), true);
    assert_eq!(allows(&policy, &to_other), false);
}

#[test]
fn a_symlinked_entry_allows_the_socket_it_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let real = dir.path().join("real.sock");
    let _a = UnixListener::bind(&real).expect("bind real");
    let link = dir.path().join("link.sock");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let policy = UnixSocketPolicy::resolve(&[entry(&link)]);

    assert_eq!(allows(&policy, &real), true);
}

#[test]
fn a_socket_replaced_after_the_sandbox_started_is_not_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("service.sock");
    let original = UnixListener::bind(&path).expect("bind original");

    let policy = UnixSocketPolicy::resolve(&[entry(&path)]);
    assert_eq!(allows(&policy, &path), true);

    drop(original);
    std::fs::remove_file(&path).expect("remove original");
    let _replacement = UnixListener::bind(&path).expect("bind replacement");

    assert_eq!(allows(&policy, &path), false);
}

#[test]
fn unusable_entries_allow_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("plain-file");
    std::fs::write(&file, b"not a socket").expect("write file");
    let socket = dir.path().join("present.sock");
    let _a = UnixListener::bind(&socket).expect("bind present");

    let policy = UnixSocketPolicy::resolve(&[
        entry(&dir.path().join("missing.sock")),
        "relative.sock".to_string(),
        entry(&file),
    ]);

    assert_eq!(policy.is_empty(), true);
    assert_eq!(allows(&policy, &socket), false);
    // A granted path that is not a socket is never a connection target.
    let dir_policy = UnixSocketPolicy::resolve(&[entry(dir.path())]);
    assert_eq!(allows(&dir_policy, &file), false);
}
