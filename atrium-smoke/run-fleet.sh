#!/usr/bin/env bash
# Bring up the one-agent atrium fleet in ./atrium.fleet.json so a sandboxed codex
# pane can prove it reaches atrium's control socket (and nothing else).
#
# Before the first run:
#   codex login                       # the pane needs a signed-in codex
#   cd .. && codex                    # answer codex's "Trust this folder?" once, then quit
#   (or add to ~/.codex/config.toml:  [projects."<repo path>"]  trust_level = "trusted")
#
# The kickoff makes the agent connect to a socket that is NOT allowlisted and
# expects that to fail; this script serves it so the failure is a real denial,
# not "no such file".
set -u
cd "$(dirname "$0")"
DENIED=/tmp/codex-atrium-smoke-denied.sock
rm -f "$DENIED" RESULT.md
socat "UNIX-LISTEN:$DENIED,fork" SYSTEM:"echo you-should-not-see-this" &
SOCAT=$!
trap 'kill $SOCAT 2>/dev/null; rm -f "$DENIED"' EXIT

atrium fleet up codex-ctl-smoke "$@"

echo
if [ -f RESULT.md ]; then
  echo "== atrium-smoke/RESULT.md =="; cat RESULT.md
else
  echo "no RESULT.md written; read the pane's transcript"
fi
