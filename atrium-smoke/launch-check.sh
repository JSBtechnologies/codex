#!/usr/bin/env bash
# Launch checks for a cargo-install build of this fork, run in real tmux panes
# so the TUI gets a proper terminal and actually renders:
#
#   1. a bare `codex` starts the TUI instead of dying with
#      "this CLI has no complete local package" (daemon_auto_start defaults off);
#      forcing the feature on must still show the error, proving the default moved;
#   2. in a directory codex has never seen, the default launch shows the
#      "Trust this folder?" prompt and `--trust-folder` does not.
#
# Needs tmux and a signed-in codex (otherwise the login screen shows first and
# the trust rows report that instead).
set -u
cd "$(dirname "$0")/.."
WAIT=${WAIT:-8}
log_dir=${XDG_CACHE_HOME:-$HOME/.cache}
mkdir -p "$log_dir"
command -v tmux >/dev/null || { echo "missing: tmux"; exit 2; }

pass=0; fail=0
verdict() { # $1 = label, $2 = outcome, $3 = expected outcome
  if [ "$2" = "$3" ]; then printf "PASS  %-20s %s\n" "$1" "$2"; pass=$((pass+1))
  else printf "FAIL  %-20s %s (expected: %s)\n" "$1" "$2" "$3"; fail=$((fail+1)); fi
}

# Run a command in a detached tmux pane, wait, capture what it drew, kill it.
capture() { # $1 = label, $2 = dir, rest = command
  local label=$1 dir=$2; shift 2
  local session="codex-check-$$-$label"
  local f="$log_dir/codex-launch-$label.txt"
  tmux kill-session -t "$session" 2>/dev/null
  tmux new-session -d -s "$session" -x 120 -y 40 -c "$dir" "$*; sleep 30"
  sleep "$WAIT"
  tmux capture-pane -pt "$session" > "$f" 2>/dev/null
  tmux kill-session -t "$session" 2>/dev/null
  printf "%s" "$f"
}

classify() { # $1 = capture file
  if grep -q "no complete local package" "$1"; then echo "package-error"
  elif grep -q "Trust and continue" "$1"; then echo "trust-prompt"
  elif grep -q -i -E "sign in|log in" "$1"; then echo "login-screen"
  elif grep -q -i -E "codex|openai|model|context" "$1"; then echo "tui"
  elif grep -q -i "unexpected argument" "$1"; then echo "unknown-flag"
  else echo "blank"; fi
}

echo "codex: $(command -v codex) ($(codex --version))"
echo "login: $(codex login status 2>&1 | head -n 1)"
echo

repo=$PWD
verdict default        "$(classify "$(capture default    "$repo" codex)")"                                    tui
verdict no-daemon-flag "$(classify "$(capture no-daemon  "$repo" codex --no-daemon)")"                        tui
verdict forced-on      "$(classify "$(capture forced-on  "$repo" codex -c features.daemon_auto_start=true)")" package-error

# codex only asks about trust inside a git repository; a plain directory is
# opened without a prompt. So the scratch dir is a fresh repo.
fresh=$(mktemp -d "$log_dir/codex-untrusted.XXXXXX")
git -C "$fresh" init -q
verdict untrusted-dir     "$(classify "$(capture untrusted "$fresh" codex)")"                trust-prompt
verdict trust-folder-flag "$(classify "$(capture trusted   "$fresh" codex --trust-folder)")" tui
rm -rf "$fresh"

echo
echo "captures in $log_dir/codex-launch-*.txt"
echo "passed=$pass failed=$fail"
[ "$fail" -eq 0 ]
