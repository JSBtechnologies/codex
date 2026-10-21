# CI in the fork (JSBtechnologies/codex)

Upstream's CI assumes OpenAI infrastructure: self-hosted runner groups
(`<repo>-runners`, `<repo>-windows-x64`, `<repo>-linux-x64-xl`), paid
`macos-15-xlarge` runners, a BuildBuddy remote cache, code-signing and
publishing secrets, and an OpenAI API key for the issue bots. None of that
exists in the fork, so those jobs either fail in seconds or queue forever, and
every PR shows red checks that mean nothing. This document records exactly what
the fork turns off, so a red check here is a real finding.

## What runs on a PR

`blocking-ci.yml` is the PR gate. In the fork it calls:

| reusable workflow | what it checks |
|---|---|
| `blob-size-policy.yml` | no oversized blobs committed |
| `cargo-deny.yml` | licenses and advisories |
| `codespell.yml` | spelling |
| `repo-checks.yml` | Cargo manifests inherit workspace settings, TUI/core boundary, Bazel clippy lints |
| `rust-ci.yml` | format, clippy, tests for changed crates, `cargo shear`, the argument-comment lint for packages |

Removed from the gate in the fork, in the workflow file with a comment: the
`bazel` and `sdk` calls. Inside `rust-ci.yml`, the "Argument comment lint -
Linux/macOS/Windows" job is guarded with `if: github.repository ==
'openai/codex'` and the `results` job accepts it being skipped outside upstream.

## What is disabled as a repository setting

`scripts/fork-ci-disable.sh` disables, via `gh workflow disable`, the workflows
that only make sense upstream: post-merge full CI on self-hosted runners, the
V8 canary, all upstream release and publishing workflows, the issue bots, the
stale-PR sweeper and the CLA assistant. Run it again after merging upstream if
new infra-bound workflows appear. `gh workflow enable <file>` reverts any of
them.

## What the fork adds

`fork-release.yml` builds and publishes fork releases on `fork-v*` tags using
GitHub-hosted runners only. See the header of that file.

## Merging upstream

The fork's edits to upstream workflow files are deliberately small and marked
with `Fork:` comments: `blocking-ci.yml` (two calls and two `needs` entries
removed) and `rust-ci.yml` (one `if:` and one branch in `results`). When an
upstream merge conflicts there, keep the fork's version of those lines.
