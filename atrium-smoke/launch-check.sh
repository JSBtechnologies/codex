#!/usr/bin/env bash
# Does a bare `codex` from a cargo-install build start the TUI, or die with
# "this CLI has no complete local package"? Runs each variant in a pty for a few
# seconds and reports. Also forces the feature on to show the error still exists
# when opted in, proving the default is what changed.
set -u
cd "$(dirname "$0")/.."
log_dir=${XDG_CACHE_HOME:-$HOME/.cache}
mkdir -p "$log_dir"

run_variant() { # $1 = label, rest = command
  local label=$1; shift
  local f="$log_dir/codex-launch-$label.log"
  rm -f "$f"
  timeout 10 script -qefc "$*" "$f" >/dev/null 2>&1
  local bytes; bytes=$(wc -c < "$f")
  printf "%-22s bytes=%-7s " "$label" "$bytes"
  if grep -a -q "no complete local package" "$f"; then echo "PACKAGE ERROR"
  elif grep -a -q -i -E "sign in|log in|welcome|trust|codex" "$f"; then echo "TUI started"
  else echo "unclear"; fi
}

echo "codex: $(command -v codex) ($(codex --version))"
run_variant default        codex
run_variant no-daemon-flag codex --no-daemon
run_variant forced-on      codex -c features.daemon_auto_start=true
