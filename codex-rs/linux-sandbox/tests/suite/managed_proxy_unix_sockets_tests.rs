//! Unix-socket grants under the managed proxy: explicit grants reach only the
//! granted sockets, and never widen network isolation.

use super::*;
use codex_network_proxy::ConfigReloader;
use codex_network_proxy::ConfigReloaderFuture;
use codex_network_proxy::ConfigState;
use codex_network_proxy::NetworkProxy;
use codex_network_proxy::NetworkProxyConfig;
use codex_network_proxy::NetworkProxyConstraints;
use codex_network_proxy::NetworkProxyState;
use codex_network_proxy::build_config_state;
use pretty_assertions::assert_eq;
use std::ffi::OsStr;
use std::os::unix::net::UnixDatagram;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

/// Printed by the helper when the kernel or sandbox can't enforce an allowlist.
const ALLOWLIST_NOT_ENFORCED: &str = "Unix socket allowlist not enforced";

struct TestConfigReloader(ConfigState);

impl ConfigReloader for TestConfigReloader {
    fn source_label(&self) -> String {
        "managed proxy Unix socket test".to_string()
    }

    fn maybe_reload(&self) -> ConfigReloaderFuture<'_, Option<ConfigState>> {
        Box::pin(async { Ok(None) })
    }

    fn reload_now(&self) -> ConfigReloaderFuture<'_, ConfigState> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

fn managed_network_config(
    dangerously_allow_all_unix_sockets: bool,
    allow_unix_sockets: &[&Path],
) -> NetworkProxyConfig {
    let mut config = NetworkProxyConfig {
        enabled: true,
        proxy_url: "http://127.0.0.1:9".to_string(),
        dangerously_allow_all_unix_sockets: Some(dangerously_allow_all_unix_sockets),
        ..Default::default()
    };
    if !allow_unix_sockets.is_empty() {
        config.set_allow_unix_sockets(
            allow_unix_sockets
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
        );
    }
    config
}

/// Runs a python3 `probe` under the managed proxy with `config`.
async fn run_managed_python(
    config: NetworkProxyConfig,
    probe: &str,
    args: &[&OsStr],
) -> std::process::Output {
    let mut env = create_env_from_core_vars();
    strip_proxy_env(&mut env);
    let state = build_config_state(
        config,
        NetworkProxyConstraints::default(),
        codex_network_proxy::Platform::native(),
    )
    .expect("valid managed network configuration");
    let network = NetworkProxy::builder()
        .state(Arc::new(NetworkProxyState::with_reloader(
            state.clone(),
            Arc::new(TestConfigReloader(state)),
        )))
        .managed_by_codex(/*managed_by_codex*/ false)
        .build()
        .await
        .expect("build managed network proxy");
    let prepared = network
        .prepare_for_optional_environment(env, /*environment_id*/ None)
        .expect("prepare managed network policy");
    let cwd = std::env::current_dir().expect("current directory");
    let mut command = Command::new(env!("CARGO_BIN_EXE_codex-linux-sandbox"));
    command
        .arg("--sandbox-policy-cwd")
        .arg(&cwd)
        .arg("--permission-profile")
        .arg(serde_json::to_string(&PermissionProfile::Disabled).unwrap())
        .arg("--managed-network")
        .arg(serde_json::to_string(&prepared.sandbox_context).unwrap())
        .args(["--", "python3", "-c", probe])
        .args(args)
        .env_clear()
        .envs(prepared.env)
        .kill_on_drop(true);
    tokio::time::timeout(OPERATION_TIMEOUT, command.output())
        .await
        .expect("Unix socket probe timed out")
        .expect("Unix socket probe should execute")
}

async fn unix_socket_skip_reason() -> Option<String> {
    if let Some(reason) = managed_proxy_skip_reason().await {
        return Some(reason);
    }
    if !Command::new("python3")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|output| output.status.success())
    {
        return Some("python3 is unavailable".to_string());
    }
    None
}

/// Why this kernel or sandbox can't enforce an allowlist, if it can't.
async fn allowlist_unsupported_reason() -> Option<String> {
    let temp = tempfile::tempdir().expect("socket directory");
    let socket_path = temp.path().join("probe.sock");
    let _listener = UnixListener::bind(&socket_path).expect("host Unix listener");
    let output = run_managed_python(
        managed_network_config(
            /*dangerously_allow_all_unix_sockets*/ false,
            &[&socket_path],
        ),
        "import socket; socket.socket(socket.AF_UNIX).close()",
        &[],
    )
    .await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    (!output.status.success() && stderr.contains(ALLOWLIST_NOT_ENFORCED))
        .then(|| stderr.trim().to_string())
}

#[tokio::test]
async fn managed_proxy_allow_all_unix_sockets_preserves_network_isolation() {
    if let Some(reason) = unix_socket_skip_reason().await {
        eprintln!("skipping managed proxy Unix socket test: {reason}");
        return;
    }
    let allowlist_enforced = match allowlist_unsupported_reason().await {
        None => true,
        Some(reason) => {
            eprintln!("Unix socket allowlist unsupported, expecting denial: {reason}");
            false
        }
    };

    let temp = tempfile::tempdir().expect("socket directory");
    let socket_path = temp.path().join("daemon.sock");
    let listener = UnixListener::bind(&socket_path).expect("host Unix listener");
    let mut control = UnixStream::connect(&socket_path).expect("host Unix control connection");
    let (mut control_peer, _) = listener.accept().expect("accept host Unix control");
    control.write_all(b"ok").expect("write host control");
    let mut control_payload = [0; 2];
    control_peer
        .read_exact(&mut control_payload)
        .expect("read host control");
    assert_eq!(control_payload, *b"ok");

    // Use another loopback address so the namespace's proxy listener cannot
    // collide with this host-only endpoint even if it selects the same port.
    let tcp_listener =
        TcpListener::bind((Ipv4Addr::new(127, 0, 0, 2), 0)).expect("host TCP listener");
    let tcp_addr = tcp_listener.local_addr().expect("host TCP address");
    let _tcp_control = TcpStream::connect(tcp_addr).expect("host TCP control connection");
    let _tcp_peer = tcp_listener.accept().expect("accept host TCP control");

    let cases = [
        ("default denial", false, false, false),
        ("path grant", false, true, allowlist_enforced),
        ("explicit allow-all", true, false, true),
    ];
    let expected_connections = cases.iter().filter(|(.., allowed)| *allowed).count();
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + OPERATION_TIMEOUT * 4;
        let mut served = 0;
        while served < expected_connections {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(OPERATION_TIMEOUT))
                        .expect("set Unix read timeout");
                    let mut request = [0; 4];
                    stream.read_exact(&mut request).expect("read Unix request");
                    assert_eq!(request, *b"ping");
                    stream.write_all(b"pong").expect("write Unix response");
                    served += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "Unix request timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept Unix request: {error}"),
            }
        }
    });

    let probe = r#"
import errno, socket, sys
path, port, mode = sys.argv[1:]
try:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
except OSError as error:
    assert mode == 'deny' and error.errno == errno.EPERM, error
else:
    assert mode == 'allow', 'Unix socket unexpectedly allowed'
    with client:
        client.settimeout(2)
        client.connect(path)
        client.sendall(b'ping')
        assert client.recv(4, socket.MSG_WAITALL) == b'pong'
for address in [('127.0.0.2', int(port)), ('192.0.2.1', 80)]:
    try:
        connection = socket.create_connection(address, timeout=0.5)
    except OSError:
        pass
    else:
        connection.close()
        raise AssertionError(('direct network unexpectedly allowed', address))
for family in (socket.AF_NETLINK, getattr(socket, 'AF_VSOCK', 40)):
    try:
        unexpected = socket.socket(family, socket.SOCK_STREAM)
    except OSError as error:
        assert error.errno == errno.EPERM, (family, error)
    else:
        unexpected.close()
        raise AssertionError(('socket family unexpectedly allowed', family))
"#;
    let port = tcp_addr.port().to_string();
    for (label, dangerously_allow_all_unix_sockets, grant_path, allowed) in cases {
        let granted: &[&Path] = if grant_path { &[&socket_path] } else { &[] };
        let output = run_managed_python(
            managed_network_config(dangerously_allow_all_unix_sockets, granted),
            probe,
            &[
                socket_path.as_os_str(),
                OsStr::new(&port),
                OsStr::new(if allowed { "allow" } else { "deny" }),
            ],
        )
        .await;
        assert!(
            output.status.success(),
            "{label}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    server.join().expect("Unix server should finish");
}

#[tokio::test]
async fn managed_proxy_unix_socket_allowlist_reaches_only_granted_sockets() {
    if let Some(reason) = unix_socket_skip_reason().await {
        eprintln!("skipping Unix socket allowlist test: {reason}");
        return;
    }
    if let Some(reason) = allowlist_unsupported_reason().await {
        eprintln!("skipping Unix socket allowlist test: {reason}");
        return;
    }

    let temp = tempfile::tempdir().expect("socket directory");
    let host = temp.path().join("host");
    let granted_dir = temp.path().join("granted-dir");
    let other_dir = temp.path().join("other-dir");
    let links = temp.path().join("links");
    let own_dir = temp.path().join("own");
    for dir in [&host, &granted_dir, &other_dir, &links, &own_dir] {
        std::fs::create_dir(dir).expect("create socket directory");
    }
    let allowed = host.join("allowed.sock");
    let unlisted = host.join("unlisted.sock");
    let beneath = granted_dir.join("beneath.sock");
    let other = other_dir.join("other.sock");
    let service = host.join("service.sock");
    let _listeners = [&allowed, &unlisted, &beneath, &other]
        .map(|path| UnixListener::bind(path).expect("host Unix listener"));
    let allowed_datagram = host.join("allowed.dgram");
    let unlisted_datagram = host.join("unlisted.dgram");
    let _datagrams = [&allowed_datagram, &unlisted_datagram]
        .map(|path| UnixDatagram::bind(path).expect("host Unix datagram socket"));
    let to_allowed = links.join("to-allowed");
    let to_unlisted = links.join("to-unlisted");
    std::os::unix::fs::symlink(&allowed, &to_allowed).expect("symlink to allowed");
    std::os::unix::fs::symlink(&unlisted, &to_unlisted).expect("symlink to unlisted");

    // The service is replaced while the command runs: the replacement is a
    // different socket from the one granted when the sandbox started.
    let service_listener = UnixListener::bind(&service).expect("service listener");
    let service_path = service.clone();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = service_listener.accept().expect("accept service request");
        stream
            .set_read_timeout(Some(OPERATION_TIMEOUT))
            .expect("set service read timeout");
        let mut request = [0; 4];
        stream
            .read_exact(&mut request)
            .expect("read service request");
        assert_eq!(request, *b"swap");
        drop(service_listener);
        std::fs::remove_file(&service_path).expect("remove original service");
        let replacement = UnixListener::bind(&service_path).expect("replacement listener");
        stream.write_all(b"done").expect("write service response");
        let mut closed = [0; 1];
        let _ = stream.read(&mut closed);
        drop(replacement);
    });

    let probe = r#"
import ctypes, errno, os, socket, sys, urllib.parse
(allowed, unlisted, beneath, other, service, to_allowed, to_unlisted, own_dir,
 allowed_datagram, unlisted_datagram) = sys.argv[1:]
libc = ctypes.CDLL(None, use_errno=True)

# Landlock enforces the allowlist from ABI 9 (Linux 7.1); older kernels use
# the helper's connect() supervisor, which refuses datagram sockets.
SYS_landlock_create_ruleset = 444
landlock = libc.syscall(SYS_landlock_create_ruleset, None, 0, 1) >= 9
denied = errno.EACCES if landlock else errno.EPERM

def connect(path, kind=socket.SOCK_STREAM):
    client = socket.socket(socket.AF_UNIX, kind)
    client.settimeout(2)
    try:
        client.connect(path)
    except OSError as error:
        client.close()
        return error.errno
    return client

def reaches(path, kind=socket.SOCK_STREAM):
    client = connect(path, kind)
    assert isinstance(client, socket.socket), ('expected to connect', path, client)
    client.close()

def refused(path, kind=socket.SOCK_STREAM):
    result = connect(path, kind)
    if isinstance(result, socket.socket):
        result.close()
    assert result == denied, ('expected', denied, path, result)

reaches(allowed)
reaches(beneath)
reaches(to_allowed)
refused(unlisted)
refused(other)
refused(to_unlisted)
if landlock:
    # Abstract sockets created inside the sandbox stay reachable.
    own_abstract = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    own_abstract.bind('\0codex-own-abstract')
    own_abstract.listen()
    reaches('\0codex-own-abstract')
    own_abstract.close()
else:
    assert connect('\0codex-abstract') == errno.EPERM

os.chdir(os.path.dirname(allowed))
reaches(os.path.basename(allowed))
refused(os.path.basename(unlisted))

for kind in (socket.SOCK_STREAM, socket.SOCK_SEQPACKET):
    own = os.path.join(own_dir, f'own-{kind}.sock')
    listener = socket.socket(socket.AF_UNIX, kind)
    listener.bind(own)
    listener.listen()
    reaches(own, kind)
    listener.close()

if landlock:
    # Landlock checks addressed sends, so datagram sockets stay usable.
    sender = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    sender.sendto(b'x', allowed_datagram)
    try:
        sender.sendto(b'x', unlisted_datagram)
    except OSError as error:
        assert error.errno == errno.EACCES, error
    else:
        raise AssertionError('datagram to an unlisted socket unexpectedly sent')
    sender.close()
    left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
    left.send(b'x')
    assert right.recv(1) == b'x'
else:
    # Landlock's hook runs after the socket type check, so only the
    # supervisor refuses a seqpacket connect to a stream socket by policy.
    refused(unlisted, socket.SOCK_SEQPACKET)
    for make in (lambda: socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM),
                 lambda: socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)):
        try:
            make()
        except OSError as error:
            assert error.errno == errno.EPERM, error
        else:
            raise AssertionError('Unix datagram socket unexpectedly allowed')
left, right = socket.socketpair()
left.sendall(b'x')
assert right.recv(1) == b'x'

client = connect(service)
assert isinstance(client, socket.socket), client
client.sendall(b'swap')
assert client.recv(4, socket.MSG_WAITALL) == b'done'
refused(service)
client.close()

if not landlock:
    # The helper brokering connect() is this command's parent; it must not
    # give up its seccomp listener or its memory.
    SYS_pidfd_getfd = 438
    helper = os.getppid()
    helper_pidfd = os.pidfd_open(helper)
    for fd in range(64):
        if libc.syscall(SYS_pidfd_getfd, helper_pidfd, fd, 0) >= 0:
            raise AssertionError(('took a descriptor from the sandbox helper', fd))
    try:
        open(f'/proc/{helper}/mem', 'r+b').close()
    except OSError:
        pass
    else:
        raise AssertionError('opened the sandbox helper memory')

proxy_port = urllib.parse.urlsplit(os.environ['HTTP_PROXY']).port
socket.create_connection(('127.0.0.1', proxy_port), timeout=2).close()
try:
    socket.create_connection(('192.0.2.1', 80), timeout=0.5).close()
except OSError:
    pass
else:
    raise AssertionError('direct network unexpectedly allowed')
for family in (socket.AF_NETLINK, getattr(socket, 'AF_VSOCK', 40)):
    try:
        socket.socket(family, socket.SOCK_STREAM).close()
    except OSError as error:
        assert error.errno == errno.EPERM, (family, error)
    else:
        raise AssertionError(('socket family unexpectedly allowed', family))
"#;
    let output = run_managed_python(
        managed_network_config(
            /*dangerously_allow_all_unix_sockets*/ false,
            &[&allowed, &granted_dir, &service, &allowed_datagram],
        ),
        probe,
        &[
            allowed.as_os_str(),
            unlisted.as_os_str(),
            beneath.as_os_str(),
            other.as_os_str(),
            service.as_os_str(),
            to_allowed.as_os_str(),
            to_unlisted.as_os_str(),
            own_dir.as_os_str(),
            allowed_datagram.as_os_str(),
            unlisted_datagram.as_os_str(),
        ],
    )
    .await;
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.join().expect("service should finish");
}
