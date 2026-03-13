# imparando

Self-hosted tool for spawning and managing [Firecracker](https://firecracker-microvm.github.io/) microVMs on Linux, each running a [Claude Code](https://docs.anthropic.com/en/docs/claude-code) agent. Access and prompt your agents from any device via a web terminal UI over Tailscale.

## How it works

Each session boots an isolated Firecracker microVM (~125ms), clones your repos, and launches Claude Code on the serial console. A single dashboard lets you create sessions, watch output live in a full terminal (xterm.js), and send prompts — from your laptop or phone.

```
Browser (xterm.js) ──WebSocket──▶ imparando ──serial console──▶ Firecracker VM
                                                                  └─ Claude Code
```

## Requirements

- Ubuntu Linux host with KVM (`ls /dev/kvm` should exist)
- Root or `CAP_NET_ADMIN` (needed for TAP devices and `mount`)
- Tailscale already configured on the host
- Rust toolchain (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)

## One-time setup

**1. Install Firecracker**

```bash
curl -LO https://github.com/firecracker-microvm/firecracker/releases/download/v1.7.0/firecracker-v1.7.0-x86_64.tgz
tar xf firecracker-v1.7.0-x86_64.tgz
sudo mv release-v1.7.0-x86_64/firecracker-v1.7.0-x86_64 /usr/local/bin/firecracker
```

**2. Download the kernel**

```bash
sudo ./scripts/download-kernel.sh
```

**3. Build the base rootfs** (Ubuntu 22.04 + Node.js + Claude Code — takes ~10 min)

```bash
sudo ./scripts/build-rootfs.sh
```

**4. Build imparando**

```bash
cargo build --release
```

## Running

**With a config file (recommended):**

```bash
sudo ./target/release/imparando --config /etc/imparando.toml
```

**With CLI flags:**

```bash
sudo ./target/release/imparando --user yourname --pass yourpassword --anthropic-api-key sk-ant-...
```

**With environment variables (backwards compatible):**

```bash
IMPARANDO_USER=yourname \
IMPARANDO_PASS=yourpassword \
ANTHROPIC_API_KEY=sk-ant-... \
OPENAI_API_KEY=sk-proj-... \
sudo ./target/release/imparando
```

Then open `http://<tailscale-ip>:8080` from any device on your Tailscale network.

## Configuration

Config is resolved in order of priority: **CLI flags > config file > environment variables > defaults**.

**Example `/etc/imparando.toml`:**

```toml
user = "yourname"
pass = "yourpassword"
anthropic_api_key = "sk-ant-..."
# claude_oauth_token = "..."   # alternative to anthropic_api_key
# openai_api_key = "sk-proj-..." # for Codex sessions
# github_app_id = 123456
# github_installation_id = 78901234
# github_app_private_key = "/etc/imparando/github-app.pem"
port = 8080
# data_dir = "/var/lib/imparando"
# run_dir  = "/run/imparando"
# ssh_key  = "/root/.ssh/id_rsa"
# max_sessions = 10
# max_total_vcpus = 20
# max_total_memory_mb = 51200
# firecracker_bin = "/usr/local/bin/firecracker"
```

| Key / Flag | Env var | Default | Description |
|---|---|---|---|
| `user` / `--user` | `IMPARANDO_USER` | — | Dashboard username (required) |
| `pass` / `--pass` | `IMPARANDO_PASS` | — | Dashboard password (required) |
| `anthropic_api_key` / `--anthropic-api-key` | `ANTHROPIC_API_KEY` | — | Injected into every VM at boot |
| `claude_oauth_token` / `--claude-oauth-token` | `CLAUDE_CODE_OAUTH_TOKEN` | — | OAuth token alternative to API key |
| `openai_api_key` / `--openai-api-key` | `OPENAI_API_KEY` | — | Injected into every VM at boot for Codex |
| `github_app_id` / `--github-app-id` | `GITHUB_APP_ID` | — | GitHub App ID used to mint installation tokens |
| `github_installation_id` / `--github-installation-id` | `GITHUB_INSTALLATION_ID` | — | GitHub App installation ID |
| `github_app_private_key` / `--github-app-private-key` | `GITHUB_APP_PRIVATE_KEY` | — | Path to GitHub App private key PEM |
| `port` / `--port` | `IMPARANDO_PORT` | `8080` | Port to listen on |
| `data_dir` / `--data-dir` | `IMPARANDO_DATA_DIR` | `/var/lib/imparando` | Base image and overlays |
| `run_dir` / `--run-dir` | `IMPARANDO_RUN_DIR` | `/run/imparando` | Firecracker sockets, per-session state |
| `auth_home` / `--auth-home` | `IMPARANDO_AUTH_HOME` | invoking user's home | Host home dir used to source Claude/Codex subscription auth files |
| `ssh_key` / `--ssh-key` | `IMPARANDO_SSH_KEY` | `~/.ssh/id_rsa` | Host SSH key injected for private repos |
| `max_sessions` / `--max-sessions` | `IMPARANDO_MAX_SESSIONS` | `10` | Max concurrent VMs |
| `max_total_vcpus` / `--max-total-vcpus` | `IMPARANDO_MAX_TOTAL_VCPUS` | `20` | Max total vCPUs across active VMs |
| `max_total_memory_mb` / `--max-total-memory-mb` | `IMPARANDO_MAX_TOTAL_MEMORY_MB` | `51200` | Max total RAM across active VMs in MiB |
| `firecracker_bin` / `--firecracker-bin` | `FIRECRACKER_BIN` | `/usr/local/bin/firecracker` | Path to firecracker binary |

## Creating a session

1. Click **+ New Session** in the dashboard
2. Give it a name
3. Choose an image profile
4. Add repo URLs (one per line — `https://` or `git@` format)
5. Choose an agent (`Claude` or `Codex`)
6. Choose vCPUs (1–4) and memory (512MB–8GB)
7. Check **Private repos** if any repos need SSH access
8. Click **Create** — the VM boots, clones repos, and starts the selected agent if credentials were injected

Built-in profile:

- `ts-rust-postgres`: TypeScript, Rust, PostgreSQL, Node.js, pnpm, just, and common development tooling with a `50 GiB` sparse rootfs image for build-heavy application work.

## Accessing the terminal

Click **Open** on any running session. You get a full terminal connected to the VM inside the browser. Works on mobile too — tap the terminal to bring up the keyboard.

Browser-based login flows are a poor fit for remote web terminals because OAuth callbacks target the browser's local machine, not the microVM. Prefer injecting credentials at boot:

- Claude: `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`
- Codex: `OPENAI_API_KEY`

Subscription-backed auth is also supported through host-side login reuse:

- Claude subscription: run `claude setup-token` or `claude auth login` once on the host. Imparando will copy `~/.claude/.credentials.json` and `~/.claude.json` into Claude VMs when no explicit Claude API key/token is configured.
- Codex / ChatGPT subscription: run `codex login` once on the host. Imparando will copy `~/.codex/auth.json` and `~/.codex/config.toml` into Codex VMs when no `OPENAI_API_KEY` is configured.

This keeps billing on your subscription-backed local login rather than requiring API keys for every session.

## Dangerous Agent Mode With Limited Git Writes

Auto-started Claude and Codex sessions now launch in dangerous/no-approval mode inside the VM.

To avoid giving those agents your personal Git identity, prefer GitHub App credentials over SSH keys:

- configure `github_app_id`, `github_installation_id`, and `github_app_private_key`
- protect `main` in GitHub so direct pushes are blocked and PRs are required

When a GitHub App is configured, GitHub repos are cloned over HTTPS with a short-lived installation token and each repo is moved onto a per-session branch:

```text
imparando/<session-id>
```

Imparando also sets the default push refspec so plain `git push` targets that branch instead of `main`.

This does not stop the agent from creating local commits. The hard enforcement is on the GitHub side via branch protection.

## CI bundles

GitHub Actions can build a distributable bundle and publish it to GHCR:

- Workflow: [.github/workflows/bundle.yml](/home/peter/imparando/.github/workflows/bundle.yml)
- Release workflow: [.github/workflows/release.yml](/home/peter/imparando/.github/workflows/release.yml)
- Bundle image Dockerfile: [Dockerfile.bundle](/home/peter/imparando/Dockerfile.bundle)
- Installer: [scripts/install-bundle.sh](/home/peter/imparando/scripts/install-bundle.sh)

The workflow builds:

- `target/release/imparando`
- `/var/lib/imparando/vmlinux`
- `/var/lib/imparando/ttyd`
- `/var/lib/imparando/base.ext4`

It uploads a tarball artifact and pushes `ghcr.io/<owner>/<repo>/bundle:latest`.

## Releases

Release versioning is driven by the Cargo package version in [Cargo.toml](/home/peter/imparando/Cargo.toml).

To publish a release:

1. Bump `version` in [Cargo.toml](/home/peter/imparando/Cargo.toml)
2. Commit the change
3. Create and push a matching git tag, for example:

```bash
git tag v0.1.0
git push origin main --tags
```

The release workflow will:

- verify `Cargo.toml` matches the git tag
- build the release binary and VM assets
- publish `imparando-bundle-vX.Y.Z-linux-amd64.tar.gz`
- publish a `.sha256` checksum file
- create a GitHub Release
- push GHCR tags like `ghcr.io/<owner>/<repo>/bundle:vX.Y.Z`, `ghcr.io/<owner>/<repo>/bundle:X.Y`, and `latest`

## Installing a bundle

After downloading a release tarball, install it with:

```bash
sudo ./scripts/install-bundle.sh ./imparando-bundle-v0.1.0-linux-amd64.tar.gz
```

Or directly from a GitHub release URL:

```bash
sudo ./scripts/install-bundle.sh \
  https://github.com/OWNER/REPO/releases/download/v0.1.0/imparando-bundle-v0.1.0-linux-amd64.tar.gz
```

By default this installs under `/opt/imparando`, updates `/opt/imparando/current`, and symlinks `/usr/local/bin/imparando`.

## Running as a service

For a resilient host setup, run imparando under `systemd` rather than from an SSH shell.

1. Install the bundle:

```bash
sudo ./scripts/install-bundle.sh ./imparando-bundle-v0.1.0-linux-amd64.tar.gz
```

2. Create config and environment directories:

```bash
sudo mkdir -p /etc/imparando
```

3. Write `/etc/imparando/config.toml`:

```toml
user = "yourname"
pass = "yourpassword"
port = 8080
data_dir = "/opt/imparando/current/data"
run_dir = "/run/imparando"
auth_home = "/home/peter"
```

4. Write `/etc/imparando/imparando.env` for any secrets you want injected:

```bash
ANTHROPIC_API_KEY=...
OPENAI_API_KEY=...
CLAUDE_CODE_OAUTH_TOKEN=...
GITHUB_APP_ID=123456
GITHUB_INSTALLATION_ID=78901234
GITHUB_APP_PRIVATE_KEY=/etc/imparando/github-app.pem
RUST_LOG=imparando=info,tower_http=info
```

If you rely on host subscription-backed login reuse instead of API keys, `auth_home` must point at the user home that contains `~/.claude` and `~/.codex`.

5. Install the unit:

```bash
sudo cp systemd/imparando.service /etc/systemd/system/imparando.service
sudo systemctl daemon-reload
sudo systemctl enable --now imparando
```

6. Check status and logs:

```bash
sudo systemctl status imparando
sudo journalctl -u imparando -f
```

This gives you:

- startup on boot
- restart after crashes
- independence from your SSH session

Because active sessions live under `/run/imparando`, a full host reboot will still terminate currently running microVMs. The service itself will come back automatically, but in-flight VM sessions are not yet persisted across host reboots.

## Private repos

If **Private repos** is checked, imparando copies your host's SSH key (`IMPARANDO_SSH_KEY`) into the VM at boot. The key is deleted when the session stops.

## Logs

Firecracker stderr for each VM is written to `/run/imparando/<session-id>/firecracker.log` while the session is running.

## Architecture

```
/var/lib/imparando/
  vmlinux           # Firecracker kernel (from download-kernel.sh)
  base.ext4         # Base rootfs image (from build-rootfs.sh)
  overlays/
    <uuid>.ext4     # Per-session copy, deleted on stop

/run/imparando/
  <uuid>/
    api.sock        # Firecracker API socket
    firecracker.log # Firecracker stderr
```

Each VM gets its own TAP device (`tap0`–`tap9`) and a private subnet (`172.16.{n}.0/24`) with outbound internet via iptables NAT. VMs are fully isolated at the hypervisor level.

Admission control is enforced on session creation using both:

- `max_sessions`
- total active `vCPU` budget
- total active memory budget

By default, Imparando conservatively allows up to `20` total vCPUs and `51200 MiB` (`50 GiB`) of active guest RAM on the host.

Profile selection is exposed through the API and UI. The current implementation ships one built-in profile, `ts-rust-postgres`, backed by the standard `base.ext4`. Additional profiles can be added later by defining new profile metadata and rootfs paths.
