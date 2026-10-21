#!/usr/bin/env bash
# Smoke test for this fork's Unix-socket allowlist: the feature atrium relies on to
# let a sandboxed codex agent reach its control socket (ATRIUM_CTL) and nothing else.
#
# It runs `codex sandbox` under a throwaway CODEX_HOME whose config.toml carries the
# same shape atrium writes for a codex pane:
#
#   [features] network_proxy = true
#   [permissions.<profile>]          extends = ":workspace"
#   [permissions.<profile>.network]  enabled = true, mode = "limited"
#   [permissions.<profile>.network.unix_sockets]  "<socket path>" = "allow"
#
# and checks, from inside the sandbox, that:
#   1. an allowlisted socket connects,
#   2. a socket that is not allowlisted is refused,
#   3. with no allowlist at all, both are refused (fail closed),
#   4. a socket in an allowlisted *directory* connects (directory entries),
#   5. (only inside an atrium pane) `atrium ctl whoami` reaches ATRIUM_CTL.
#
# Needs: codex (this fork), bwrap, socat, python3. No atrium session required.
set -u

CODEX=${CODEX:-codex}
PROFILE=atrium_smoke
# Not under /tmp: codex refuses to place its helper binaries under a temp dir
# and warns on every run if CODEX_HOME lives there.
mkdir -p "${XDG_CACHE_HOME:-$HOME/.cache}"
WORK=$(mktemp -d "${XDG_CACHE_HOME:-$HOME/.cache}/codex-atrium-smoke.XXXXXX")
ALLOWED_DIR=$WORK/allowed
DENIED_DIR=$WORK/denied
mkdir -p "$ALLOWED_DIR" "$DENIED_DIR" "$WORK/home-allow" "$WORK/home-none" "$WORK/home-dir"
ALLOWED=$ALLOWED_DIR/ctl.sock
DENIED=$DENIED_DIR/other.sock
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; rm -rf "$WORK"; }
trap cleanup EXIT

for tool in "$CODEX" bwrap socat python3; do
  command -v "$tool" >/dev/null || { echo "missing: $tool"; exit 2; }
done

echo "codex:  $(command -v "$CODEX") ($("$CODEX" --version 2>/dev/null))"
echo "kernel: $(uname -r)"

socat "UNIX-LISTEN:$ALLOWED,fork" SYSTEM:"echo hello-from-allowed" & PIDS+=($!)
socat "UNIX-LISTEN:$DENIED,fork" SYSTEM:"echo hello-from-denied" & PIDS+=($!)
sleep 0.3

# Three CODEX_HOMEs: one allowlisting the socket, one allowlisting its directory,
# one with the managed network on but no allowlist.
write_config() { # $1 = dir, $2 = unix_sockets table body (may be empty)
  cat > "$1/config.toml" <<EOF
default_permissions = "$PROFILE"

[features]
network_proxy = true

[permissions.$PROFILE]
extends = ":workspace"

[permissions.$PROFILE.network]
enabled = true
mode = "limited"

[permissions.$PROFILE.network.unix_sockets]
$2
EOF
}
write_config "$WORK/home-allow" "\"$ALLOWED\" = \"allow\""
write_config "$WORK/home-dir"   "\"$ALLOWED_DIR\" = \"allow\""
write_config "$WORK/home-none"  ""

# Probe: connect to each socket path given as an argument, one result line per path.
PROBE=$(cat <<'PY'
import socket, sys
for path in sys.argv[1:]:
    s = None
    try:
        # With no allowlist the sandbox refuses socket(AF_UNIX) itself, so the
        # constructor sits inside the try as well.
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(3)
        s.connect(path)
        print(f"OK {path} {s.recv(64).decode().strip()}")
    except OSError as e:
        print(f"FAIL {path} errno={e.errno} {e.strerror}")
    finally:
        if s is not None:
            s.close()
PY
)

run_sandbox() { # $1 = CODEX_HOME dir, rest = socket paths to probe
  local home=$1; shift
  CODEX_HOME=$home "$CODEX" sandbox --permission-profile "$PROFILE" \
    -- python3 -c "$PROBE" "$@" 2>&1
}

pass=0; fail=0
check() { # $1 = description, $2 = expected prefix, $3 = actual line
  if [[ "$3" == "$2"* ]]; then echo "PASS  $1"; pass=$((pass+1))
  else echo "FAIL  $1"; echo "      got: ${3:-<no output>}"; fail=$((fail+1)); fi
}
line_for() { echo "$1" | grep -F -- "$2" | head -n1; }

echo
echo "== allowlist: socket $ALLOWED =="
out=$(run_sandbox "$WORK/home-allow" "$ALLOWED" "$DENIED"); echo "$out" | sed 's/^/   /'
check "allowlisted socket connects"    "OK $ALLOWED hello-from-allowed" "$(line_for "$out" "$ALLOWED")"
check "non-allowlisted socket refused" "FAIL $DENIED"                   "$(line_for "$out" "$DENIED")"

echo
echo "== allowlist: directory $ALLOWED_DIR =="
out=$(run_sandbox "$WORK/home-dir" "$ALLOWED" "$DENIED"); echo "$out" | sed 's/^/   /'
check "socket under allowlisted directory connects" "OK $ALLOWED hello-from-allowed" "$(line_for "$out" "$ALLOWED")"
check "socket outside allowlisted directory refused" "FAIL $DENIED"                  "$(line_for "$out" "$DENIED")"

echo
echo "== no allowlist (fail closed) =="
out=$(run_sandbox "$WORK/home-none" "$ALLOWED" "$DENIED"); echo "$out" | sed 's/^/   /'
check "socket refused when nothing is listed"       "FAIL $ALLOWED" "$(line_for "$out" "$ALLOWED")"
check "other socket refused when nothing is listed" "FAIL $DENIED"  "$(line_for "$out" "$DENIED")"

if [ -n "${ATRIUM_CTL:-}" ]; then
  echo
  echo "== inside atrium: ATRIUM_CTL=$ATRIUM_CTL =="
  if out=$(atrium ctl whoami 2>&1) && [ -n "$out" ]; then
    echo "$out" | sed 's/^/   /'; echo "PASS  atrium ctl whoami from this pane"; pass=$((pass+1))
  else
    echo "$out" | sed 's/^/   /'; echo "FAIL  atrium ctl whoami from this pane"; fail=$((fail+1))
  fi
fi

echo
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
