#!/usr/bin/env bash
# Build a fork release on this machine instead of on GitHub Actions, and
# optionally publish it.
#
# Produces the same assets as .github/workflows/fork-release.yml:
#   codex-package-x86_64-unknown-linux-gnu.tar.gz (+ .tar.zst when zstd exists)
#   codex-package_SHA256SUMS, install-fork.sh, migrate-codex-home.sh (if present)
#
# Usage:
#   scripts/fork-release-local.sh 0.2.0             # build + smoke test only
#   scripts/fork-release-local.sh 0.2.0 --publish   # also tag fork-v0.2.0 and create the release
#   scripts/fork-release-local.sh 0.2.0 --ref main  # build another ref (default: HEAD)
#
# The build runs in a throwaway git worktree, so the version stamp never
# touches your checkout; it reuses codex-rs/target as the cargo target dir.
#
# glibc: the package needs this machine's glibc or newer. Built on Ubuntu
# 24.04 that is 2.39, so it will not run on Ubuntu 22.04 / Debian 12 (the
# GitHub workflow builds on 22.04 for a 2.35 floor). The floor is printed.
#
# Needs: cargo, python3 3.11+, build-essential pkg-config libssl-dev libcap-dev,
# and for --publish the GitHub CLI (gh, or gh.exe from WSL).
set -euo pipefail

fail() { printf 'fork-release-local: %s\n' "$*" >&2; exit 1; }

VERSION=""; PUBLISH=0; REF="HEAD"
while [ $# -gt 0 ]; do
  case "$1" in
    --publish) PUBLISH=1; shift ;;
    --ref) REF="$2"; shift 2 ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) fail "unknown option $1" ;;
    *) VERSION="$1"; shift ;;
  esac
done
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]] || fail "usage: $0 <semver> [--publish] [--ref REF]"

REPO_ROOT="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
FORK_REPO="${CODEX_FORK_REPO:-JSBtechnologies/codex}"
TARGET="x86_64-unknown-linux-gnu"
TAG="fork-v${VERSION}"
FORK_VERSION="${VERSION}+fork"
OUT="${CODEX_FORK_DIST:-$HOME/.cache/codex-fork-release}/${VERSION}"
WT="$OUT/worktree"
SHA="$(git -C "$REPO_ROOT" rev-parse "$REF")"

GH=""
if [ "$PUBLISH" = 1 ]; then
  GH="$(command -v gh || command -v gh.exe || true)"
  [ -n "$GH" ] || fail "--publish needs the GitHub CLI (gh or gh.exe)"
  [ -z "$(git -C "$REPO_ROOT" status --porcelain)" ] || fail "working tree is not clean"
fi

echo "== fork release $FORK_VERSION from $(git -C "$REPO_ROOT" log -1 --format='%h %s' "$SHA")"
rm -rf "$OUT"; mkdir -p "$OUT/dist"
git -C "$REPO_ROOT" worktree prune
git -C "$REPO_ROOT" worktree add -q --detach "$WT" "$SHA"
cleanup() { git -C "$REPO_ROOT" worktree remove --force "$WT" 2>/dev/null || true; }
trap cleanup EXIT

cd "$WT"
sed -i "0,/^version = \"0.0.0\"/s//version = \"${FORK_VERSION}\"/" codex-rs/Cargo.toml
grep -m1 '^version' codex-rs/Cargo.toml
(cd codex-rs && cargo update --workspace --offline 2>/dev/null || cargo update --workspace)

export CODEX_REPO_ROOT="$WT"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/codex-rs/target}"
export STABLE_GIT_COMMIT="$SHA"
rustup target add "$TARGET" >/dev/null 2>&1 || true

archives=(--archive-output "$OUT/dist/codex-package-${TARGET}.tar.gz")
command -v zstd >/dev/null && archives+=(--archive-output "$OUT/dist/codex-package-${TARGET}.tar.zst")
PKG="$OUT/package"
python3 scripts/build_codex_package.py \
  --target "$TARGET" --variant codex --cargo-profile release --strip all \
  --package-version "$FORK_VERSION" --package-dir "$PKG" "${archives[@]}" --force

echo "== smoke test"
"$PKG/bin/codex" --version | grep -F "$FORK_VERSION"
"$PKG/bin/codex" --help | grep -q -- "--trust-folder"
for f in bin/codex-code-mode-host codex-resources/bwrap codex-path/rg; do test -x "$PKG/$f"; done
[ -e "$PKG/bin/codex-exec-server" ] && echo "includes codex-exec-server"
echo "glibc floor: $(objdump -T "$PKG/bin/codex" | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -n 1)"

cp scripts/install/install-fork.sh "$OUT/dist/"
[ -f scripts/install/migrate-codex-home.sh ] && cp scripts/install/migrate-codex-home.sh "$OUT/dist/"
(cd "$OUT/dist" && sha256sum codex-package-*.tar.* > codex-package_SHA256SUMS)
ls -l "$OUT/dist"

if [ "$PUBLISH" != 1 ]; then
  echo "== built, not published. Assets: $OUT/dist"
  echo "   publish with: $0 $VERSION --publish"
  exit 0
fi

echo "== publishing $TAG"
cd "$REPO_ROOT"
git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || git tag -a "$TAG" -m "Codex fork v${VERSION} (JSBtechnologies)" "$SHA"
git push origin "$TAG"
cat > "$OUT/notes.md" <<EOF
**This is a release of the JSBtechnologies fork of Codex, not an OpenAI release.**

Fork version: \`${FORK_VERSION}\` (tag \`${TAG}\`, commit \`${SHA}\`), built locally with \`scripts/fork-release-local.sh\`.

## Install (Linux x86_64)

\`\`\`sh
curl -fsSL https://github.com/${FORK_REPO}/releases/download/${TAG}/install-fork.sh | sh
\`\`\`

Needs glibc $(objdump -T "$PKG/bin/codex" | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -n 1 | sed 's/GLIBC_//') or newer.
EOF
# gh.exe needs Windows paths; run from the dist dir with relative names.
cd "$OUT/dist"
cp ../notes.md notes.md
"$GH" release create "$TAG" --repo "$FORK_REPO" --verify-tag --latest \
  --title "Codex fork v${VERSION} (JSBtechnologies)" --notes-file notes.md \
  codex-package-*.tar.* codex-package_SHA256SUMS ./*.sh
rm -f notes.md
echo "== published https://github.com/${FORK_REPO}/releases/tag/${TAG}"
