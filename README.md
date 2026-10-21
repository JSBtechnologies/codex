<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>
<strong>This fork has an implementation of a broker design. The helper intercepts connect() with seccomp user notification, copies the address, takes the socket with pidfd_getfd, and connects itself through an O_PATH handle to the checked socket. It never continues the call in the kernel. Entries are pinned by inode at startup and judged after symlinks; abstract addresses are refused; sockets bound inside the sandbox stay reachable. Unix datagram sockets are refused because they name a destination on every send; seqpacket ignores the sendto address, so it stays allowed. If the kernel can't support it, it fails closed and keeps today's EPERM. <br>
On Linux 7.1+ the fork uses Landlock (<code>LANDLOCK_ACCESS_FS_RESOLVE_UNIX</code>) instead: the kernel checks connect() and addressed sends itself, so Unix datagram sockets work there, and the broker covers older kernels.</strong> 
<br>To use it, list the allowed sockets under <code>[permissions.&lt;profile&gt;.network.unix_sockets]</code> with the managed network proxy enabled. See <a href="codex-rs/linux-sandbox/README.md">codex-rs/linux-sandbox/README.md</a> for the matching rules and per-kernel behavior.
<br><br>
<strong>Installing the fork (Linux x86_64, prebuilt):</strong>
<pre>curl -fsSL https://github.com/JSBtechnologies/codex/releases/latest/download/install-fork.sh | sh</pre>
Fork releases are tagged <code>fork-v&lt;version&gt;</code>, the binary reports <code>&lt;version&gt;+fork</code>, and they are built by <a href=".github/workflows/fork-release.yml">.github/workflows/fork-release.yml</a> from the same package layout upstream ships (<code>codex-package-&lt;target&gt;.tar.gz</code>). The installer puts the package under <code>~/.codex/packages/standalone</code> and links <code>~/.local/bin/codex</code>. Needs glibc 2.35+ (Ubuntu 22.04+, Debian 12+, WSL2). Windows packages are the next step. To cut a release: <code>git tag fork-v0.1.0 &amp;&amp; git push origin fork-v0.1.0</code>. To build from source instead, see <a href="atrium-smoke/README.md">atrium-smoke/README.md</a>.

## Quickstart

### Installing and running Codex CLI

Run the following on Mac or Linux to install Codex CLI:

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | sh
```

Run the following on Windows to install Codex CLI:

```shell
powershell -ExecutionPolicy ByPass -c "irm https://chatgpt.com/codex/install.ps1 | iex"
```

The standalone installers download from `https://releases.openai.com/codex` by default and fall back to GitHub Releases if a metadata or asset download is unavailable. To force GitHub Releases, set `CODEX_INSTALLER_USE_RELEASES_OPENAI_COM` to `false` (`0` and `no` are also accepted):

```shell
curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_INSTALLER_USE_RELEASES_OPENAI_COM=false sh
```

```powershell
$env:CODEX_INSTALLER_USE_RELEASES_OPENAI_COM='false'; irm https://chatgpt.com/codex/install.ps1 | iex
```

Codex CLI can also be installed via the following package managers:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
