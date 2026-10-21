#!/bin/sh
# Installer for the JSBtechnologies fork of Codex (Linux).
#
# This is NOT the upstream OpenAI installer (install.sh beside it). It
# downloads a fork release (tag fork-v<version>) from
# https://github.com/JSBtechnologies/codex/releases and installs it in the same
# layout upstream's standalone installer uses, so the daemon tooling recognises
# it as a complete package:
#
#   $CODEX_HOME/packages/standalone/releases/<version>/   the extracted package
#   $CODEX_HOME/packages/standalone/current               symlink to the release
#   $CODEX_INSTALL_DIR/codex                              symlink to current/bin/codex
#
# Usage:
#   curl -fsSL https://github.com/JSBtechnologies/codex/releases/latest/download/install-fork.sh | sh
#
# Environment:
#   CODEX_FORK_REPO    GitHub repo to install from     (default: JSBtechnologies/codex)
#   CODEX_RELEASE      fork version, e.g. 0.1.0, or latest (default: latest)
#   CODEX_FORK_BASE_URL  download assets from this URL instead of the GitHub
#                      release (a mirror, or a local http.server when testing);
#                      CODEX_RELEASE must then name a version
#   CODEX_INSTALL_DIR  where the codex symlink goes      (default: ~/.local/bin)
#   CODEX_HOME         Codex home                        (default: ~/.codex)
#
# Needs: curl, tar, sha256sum. Linux x86_64 only for now.
set -eu

REPO="${CODEX_FORK_REPO:-JSBtechnologies/codex}"
RELEASE="${CODEX_RELEASE:-latest}"
BIN_DIR="${CODEX_INSTALL_DIR:-$HOME/.local/bin}"
CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
STANDALONE_ROOT="$CODEX_HOME_DIR/packages/standalone"
RELEASES_DIR="$STANDALONE_ROOT/releases"
CURRENT_LINK="$STANDALONE_ROOT/current"

say()  { printf '%s\n' "$*"; }
fail() { printf 'install-fork: %s\n' "$*" >&2; exit 1; }

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || fail "missing required tool: $tool"
done

os="$(uname -s)"
arch="$(uname -m)"
[ "$os" = "Linux" ] || fail "this fork currently ships Linux packages only (got $os)"
case "$arch" in
  x86_64|amd64) target="x86_64-unknown-linux-gnu" ;;
  *) fail "no fork package for architecture $arch yet (x86_64 only)" ;;
esac

# Resolve the fork tag. "latest" asks GitHub for the repo's latest release,
# which the fork-release workflow marks; anything else is a fork version.
if [ "$RELEASE" = "latest" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$tag" ] || fail "could not resolve the latest release of $REPO"
else
  case "$RELEASE" in
    fork-v*) tag="$RELEASE" ;;
    v*)      tag="fork-$RELEASE" ;;
    *)       tag="fork-v$RELEASE" ;;
  esac
fi
case "$tag" in
  fork-v*) ;;
  *) fail "release tag '$tag' is not a fork release (expected fork-v<version>)" ;;
esac
version="${tag#fork-v}"
base_url="${CODEX_FORK_BASE_URL:-https://github.com/$REPO/releases/download/$tag}"
asset="codex-package-$target.tar.gz"

say "Installing Codex fork $version ($REPO, $target)"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading $asset"
curl -fsSL --progress-bar -o "$tmp/$asset" "$base_url/$asset"
curl -fsSL -o "$tmp/SHA256SUMS" "$base_url/codex-package_SHA256SUMS"
expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
[ -n "$expected" ] || fail "no checksum for $asset in the release"
actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
[ "$actual" = "$expected" ] || fail "checksum mismatch for $asset: expected $expected, got $actual"
say "Checksum OK"

# Stage, then move into place so a failed install never leaves a half release.
release_dir="$RELEASES_DIR/$version"
stage="$RELEASES_DIR/.staging.$version.$$"
mkdir -p "$RELEASES_DIR"
rm -rf "$stage"
mkdir -p "$stage"
tar -xzf "$tmp/$asset" -C "$stage"
[ -f "$stage/codex-package.json" ] || fail "archive has no codex-package.json; not a Codex package"
chmod 0755 "$stage/bin/codex" "$stage/bin/codex-code-mode-host" "$stage/codex-path/rg"
[ -f "$stage/codex-resources/bwrap" ] && chmod 0755 "$stage/codex-resources/bwrap"
ln -sf "bin/codex" "$stage/codex"
rm -rf "$release_dir"
mv "$stage" "$release_dir"

# Point `current` at the new release atomically.
tmp_link="$STANDALONE_ROOT/.current.$$"
ln -s "$release_dir" "$tmp_link"
mv -T "$tmp_link" "$CURRENT_LINK" 2>/dev/null || { rm -f "$CURRENT_LINK"; mv "$tmp_link" "$CURRENT_LINK"; }

# Visible command. Only the entrypoint is linked; codex finds
# codex-code-mode-host, bwrap and rg beside the real binary.
mkdir -p "$BIN_DIR"
if [ -e "$BIN_DIR/codex" ] && [ ! -L "$BIN_DIR/codex" ]; then
  say "Note: $BIN_DIR/codex is a regular file (another install?); replacing it with a symlink"
fi
tmp_bin="$BIN_DIR/.codex.$$"
ln -s "$CURRENT_LINK/bin/codex" "$tmp_bin"
mv -T "$tmp_bin" "$BIN_DIR/codex" 2>/dev/null || { rm -f "$BIN_DIR/codex"; mv "$tmp_bin" "$BIN_DIR/codex"; }

reported="$("$BIN_DIR/codex" --version 2>/dev/null || true)"
[ -n "$reported" ] || fail "installed codex does not run; check glibc (needs 2.35+): $(ldd --version 2>&1 | head -n 1)"
say "Installed: $reported -> $release_dir"

if command -v codex >/dev/null 2>&1 && [ "$(command -v codex)" != "$BIN_DIR/codex" ]; then
  say "Warning: another codex is earlier on PATH: $(command -v codex)"
fi
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "Add to PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac
say "Run: codex"
