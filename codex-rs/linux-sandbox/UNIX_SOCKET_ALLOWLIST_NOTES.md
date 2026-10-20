# Linux unix-socket allowlist: notes for the remaining work

Branch `linux-unix-socket-allowlist` on JSBtechnologies/codex. Upstream issue:
openai/codex#39140 ("Linux sandbox blocks AF_UNIX before configured
unix_sockets allowlist"). openai/codex does not accept external PRs
(`docs/contributing.md`); upstream the root cause, design and this branch on
that issue instead.

## The problem

`permissions.<name>.network.unix_sockets = { "<path>" = "allow" }` works on
macOS (Seatbelt) but does nothing on Linux:

- The list reaches the Linux helper as
  `ManagedNetworkSandboxContext.allow_unix_sockets`
  (`network-proxy/src/proxy.rs`) and is never read.
- `linux-sandbox/src/seccomp.rs`, `ProxyRouted` mode: `socket(AF_UNIX)` is
  refused with `EPERM` unless `dangerously_allow_all_unix_sockets` is set.
  `Restricted` mode refuses `connect()` outright. (A profile only gets a managed
  network context, and so an allowlist, in `ProxyRouted`.)
- Upstream's `linux-sandbox/tests/suite/managed_proxy_unix_sockets_tests.rs`
  asserts the current behaviour in its "path grant stays restricted" case.
- A read-only bind mount does not stop `connect()` to a socket, and the
  default profile mounts the host root read-only, so mount isolation alone
  can't confine which sockets are reachable. Enforcement needs a path check at
  connection time.

## Agreed semantics

Implemented in `unix_socket_policy.rs` (done, with unit tests):

- Entries are resolved once at sandbox start and pinned by inode. Each command
  gets a fresh sandbox, so a restarted service is picked up by the next
  command; a socket replaced mid-command is refused.
- A socket entry allows that socket; a directory entry allows sockets beneath
  it (Seatbelt `subpath` semantics), including ones created later.
- A connection is judged by the socket it reaches after symlinks.
- Missing, relative or non-socket/non-directory entries allow nothing.

Enforcement must also:

- refuse abstract sockets (the sandbox's network namespace already isolates
  them; Landlock adds `LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET`, ABI 6, where
  available);
- allow connections to sockets bound inside the sandbox itself (Landlock's
  same-domain rule);
- fail closed (today's `EPERM`) whenever the allowlist can't be enforced.

## Remaining work

1. **Landlock enforcement (Linux 7.1+, preferred).** `LANDLOCK_ACCESS_FS_RESOLVE_UNIX`
   (Landlock ABI 9) restricts `connect()` and addressed `sendmsg()` to pathname
   sockets by path hierarchy. Handle the right and grant it only on the pinned
   entries. The workspace pins `landlock` 0.4.4 (ABI 5), so this needs a crate
   bump or the raw `landlock_create_ruleset`/`landlock_add_rule`/
   `landlock_restrict_self` syscalls. Probe the ABI with
   `LANDLOCK_CREATE_RULESET_VERSION`.
2. **Supervisor enforcement (pre-7.1 kernels; owner: Jeffrie).** Per the design
   agreed in session: a seccomp user-notification supervisor
   (`seccomp_unotify(2)`) started before the command's filter. It decides each
   connection with `UnixSocketPolicy::allows` plus the sandbox-bound-socket
   rule above, and never lets the kernel re-run a call after checking it. When
   the supervisor is the enforcer, unix sockets are stream-only (datagram and
   seqpacket refused at `socket()`/`socketpair()`), because a datagram or
   seqpacket socket can address any socket path when sending. Supervising
   addressed sends instead is a possible follow-up, on by default with a switch
   to turn it off.
3. **seccomp changes (`seccomp.rs`).** In `ProxyRouted`, admit `AF_UNIX` only when
   one of the mechanisms above enforces a non-empty allowlist (stream-only
   under the supervisor). Without an allowlist, behaviour stays as upstream.
4. **Wiring (`linux_run_main.rs`).** In the inner stage, resolve
   `UnixSocketPolicy` from `managed_network.allow_unix_sockets` before
   `apply_permission_profile_to_current_thread`. Choose Landlock if ABI ≥ 9,
   else the supervisor if available, else neither (fail closed). For
   restricted-read profiles (`--tmpfs /`), the outer stage must also bind each
   allowed path into the sandbox.
5. **Tests (regular functional tests).** These are Linux integration tests in
   `linux-sandbox/tests/suite/`, modelled on the existing managed-proxy unix
   socket test (python3 probe, `PermissionProfile::Disabled`, managed network):
   - the allowed socket connects;
   - an unlisted socket in the same directory, and one in another directory, are refused;
   - a symlink to an allowed socket connects, and one to an unlisted socket is refused;
   - a socket replaced mid-command is refused;
   - abstract sockets are refused;
   - a socket bound inside the sandbox connects;
   - `socketpair` still works;
   - TCP, `AF_NETLINK` and `AF_VSOCK` stay blocked.

   Flip upstream's "path grant stays restricted" case. Skip a mechanism's tests
   when the kernel lacks it (Landlock needs 7.1+).
6. **Docs.** Update `linux-sandbox/README.md`, which says standalone `AF_UNIX`
   sockets are blocked unless allow-all is granted.

## Known differences from macOS

- Under the supervisor, a server sees the connecting user and group but the
  sandbox helper's pid (`SO_PEERCRED`).
- Under the supervisor, unix datagram and seqpacket sockets are unavailable in a
  sandbox with an allowlist.

## Consumer

atrium (github.com/nativelite/atrium) launches sandboxed codex panes with a
permission profile allowing only its control socket (`ATRIUM_CTL`). It works on
macOS today; on Linux it needs this branch. The user's Linux machine is WSL2
kernel 6.18 (no Landlock ABI 9), so the supervisor is what fixes it there.
