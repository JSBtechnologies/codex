# atrium smoke tests for this codex fork

## Installing from source

`cargo install --path cli` gives you `codex`, but codex runs shell commands
through a sibling helper that the release package ships next to it in `bin/`.
A local build needs that helper installed into the same directory too, or every
command fails before it runs with "codex-code-mode-host is missing":

```sh
cd codex-rs
cargo install --path cli --locked
cargo install --path code-mode-host --locked
```

(`rg` and `bwrap` are the only other runtime pieces, and system ones on PATH work.)

The host embeds V8 and downloads a prebuilt at build time. The sandbox-enabled
build the workspace needs is published under OpenAI's `rusty-v8-v<version>`
release tag, not on denoland/rusty_v8, so the repo's `.cargo/config.toml` points
`RUSTY_V8_MIRROR` / `RUSTY_V8_MIRROR_TAG` there by default. Build from inside
the repo so that config applies.

This fork adds a Unix-socket allowlist to the Linux sandbox
(`[permissions.<profile>.network.unix_sockets]`, see `codex-rs/linux-sandbox/README.md`).
atrium uses exactly that to let a sandboxed codex pane reach its control socket
(`ATRIUM_CTL`) and nothing else. These two tests check it end to end on a WSL2 /
Linux box with the `cargo install --path cli --locked` build on PATH.

## 1. Sandbox-level test (no atrium needed)

```sh
./atrium-smoke/unix-socket-smoke.sh
```

Starts two `socat` listeners, then runs `codex sandbox` three times under a
throwaway `CODEX_HOME` whose `config.toml` has the managed network proxy on
(`features.network_proxy`, `network.enabled`, `mode = "limited"`) and:

| run | allowlist | expected |
|-----|-----------|----------|
| socket entry | `"…/allowed/ctl.sock" = "allow"` | that socket connects, the other is refused |
| directory entry | `"…/allowed" = "allow"` | socket under the dir connects, the other is refused |
| none | (empty table) | both refused, and `socket(AF_UNIX)` itself is refused |

Needs `bwrap`, `socat`, `python3`. Exit code 0 means every check passed.

Observed on WSL2 kernel 6.6 (seccomp broker path, denials are `EPERM`):

```
passed=6 failed=0
```

On Linux 7.1+ the Landlock path is used instead and denials come back `EACCES`;
the script accepts either.

## 2. atrium-level test (one codex pane)

`atrium.fleet.json` defines fleet `codex-ctl-smoke`: a single codex agent at
`trust: accept` (workspace sandbox) with `allow_ctl: true`. Its kickoff has it run
`atrium ctl whoami`, set and read a board key, publish on the `smoke` topic, and
connect to a socket that is *not* allowlisted (which must fail). It writes the
outcome to `atrium-smoke/RESULT.md` with a final `VERDICT:` line.

One-time setup:

```sh
codex login                    # the pane needs a signed-in codex
```

Folder trust is a choice in this fork. Either answer "Trust this folder?" once
by running `codex` in the repo, or pass `--trust-folder`, which treats the
working directory as trusted for that run only and writes nothing to
`config.toml`. The fleet file passes it in the agent's `cmd`, so a hands-off
launch never stalls on the prompt. `launch-check.sh` verifies both behaviors.

A plain `codex` from a `cargo install` build fails with "this CLI has no complete
local package": upstream now launches the TUI through a shared app-server daemon
that it installs from a packaged release, which a local build does not carry.
`--no-daemon` skips that, and atrium already passes it to every codex pane. To make
it the default for your own launches, add to `~/.codex/config.toml`:

```toml
[features]
daemon_auto_start = false
```

Then:

```sh
./atrium-smoke/run-fleet.sh    # serves the denied socket, runs `atrium fleet up codex-ctl-smoke`
```

Watch the pane. When it is done, close the fleet; the script prints `RESULT.md`.
A `VERDICT: PASS` means a codex pane under the workspace sandbox reached atrium's
control socket through the allowlist while an arbitrary socket stayed blocked.

What atrium checks before launching: it probes `codex` for the `network_proxy`
feature and writes `permissions.<profile>.network.{enabled,mode,unix_sockets}`
for the pane. If the status bar says "codex has no network_proxy feature", the
`codex` on PATH is not this build.
