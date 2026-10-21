#!/usr/bin/env bash
# Fork (JSBtechnologies/codex): disable the upstream workflows that cannot run
# here, so they stop producing false-positive failures in the Actions tab.
#
# Everything listed needs OpenAI-only infrastructure: self-hosted runner groups
# (<repo>-runners, <repo>-windows-x64, <repo>-linux-x64-xl), paid macOS runners,
# a BuildBuddy remote cache, code-signing / notarization secrets, an OpenAI API
# key for the issue bots, or publishing credentials (R2, PyPI, winget).
#
# This is a repository setting, not a code change, so it survives upstream
# merges but must be re-run if upstream adds new infra-bound workflows. It is
# reversible with `gh workflow enable <file>`. The PR gate itself
# (blocking-ci.yml) is trimmed in the workflow file instead; see docs/fork-ci.md.
#
# Usage: scripts/fork-ci-disable.sh [owner/repo]   (needs a logged-in `gh`)
set -eu
repo="${1:-JSBtechnologies/codex}"

disable=(
  postmerge-ci.yml                       # push to main -> rust-ci-full on self-hosted runners + v8-canary
  rust-ci-full.yml                       # also has its own push trigger; 32 self-hosted runner references
  v8-canary.yml                          # pull_request -> Bazel via BuildBuddy
  rust-release.yml                       # upstream release: self-hosted runners, signing, R2, winget
  rust-release-windows.yml               # upstream Windows release: self-hosted + Azure signing
  rust-release-provisioned-macos.yml     # upstream macOS release: notarization secrets
  rusty-v8-release.yml                   # upstream V8 prebuilt publishing (BuildBuddy)
  python-sdk-release.yml                 # PyPI publishing
  python-sdk-cli-release.yml             # PyPI publishing
  python-runtime-release.yml             # upstream runtime publishing
  issue-deduplicator.yml                 # needs CODEX_OPENAI_API_KEY
  issue-labeler.yml                      # needs CODEX_OPENAI_API_KEY
  issue-translator.yml                   # needs CODEX_OPENAI_API_KEY
  close-stale-contributor-prs.yml        # upstream contributor-PR policy
  cla.yml                                # upstream CLA assistant
)

for wf in "${disable[@]}"; do
  if gh workflow disable "$wf" --repo "$repo" 2>/dev/null; then
    echo "disabled  $wf"
  else
    echo "skipped   $wf (not found or already disabled)"
  fi
done

echo
echo "Still enabled (expected: blocking-ci and the reusable workflows it calls, plus fork-release):"
gh workflow list --repo "$repo" --json name,path,state --jq '.[] | select(.state == "active") | "  " + .path'
