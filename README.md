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
port = 8080
# data_dir = "/var/lib/imparando"
# run_dir  = "/run/imparando"
# ssh_key  = "/root/.ssh/id_rsa"
# max_sessions = 10
# firecracker_bin = "/usr/local/bin/firecracker"
```

| Key / Flag | Env var | Default | Description |
|---|---|---|---|
| `user` / `--user` | `IMPARANDO_USER` | — | Dashboard username (required) |
| `pass` / `--pass` | `IMPARANDO_PASS` | — | Dashboard password (required) |
| `anthropic_api_key` / `--anthropic-api-key` | `ANTHROPIC_API_KEY` | — | Injected into every VM at boot |
| `claude_oauth_token` / `--claude-oauth-token` | `CLAUDE_CODE_OAUTH_TOKEN` | — | OAuth token alternative to API key |
| `port` / `--port` | `IMPARANDO_PORT` | `8080` | Port to listen on |
| `data_dir` / `--data-dir` | `IMPARANDO_DATA_DIR` | `/var/lib/imparando` | Base image and overlays |
| `run_dir` / `--run-dir` | `IMPARANDO_RUN_DIR` | `/run/imparando` | Firecracker sockets, per-session state |
| `ssh_key` / `--ssh-key` | `IMPARANDO_SSH_KEY` | `~/.ssh/id_rsa` | Host SSH key injected for private repos |
| `max_sessions` / `--max-sessions` | `IMPARANDO_MAX_SESSIONS` | `10` | Max concurrent VMs |
| `firecracker_bin` / `--firecracker-bin` | `FIRECRACKER_BIN` | `/usr/local/bin/firecracker` | Path to firecracker binary |

## Creating a session

1. Click **+ New Session** in the dashboard
2. Give it a name
3. Add repo URLs (one per line — `https://` or `git@` format)
4. Choose vCPUs (1–4) and memory (512MB–4GB)
5. Check **Private repos** if any repos need SSH access
6. Click **Create** — the VM boots and Claude Code starts automatically

## Accessing the terminal

Click **Open** on any running session. You get a full terminal connected to Claude Code inside the VM. Type prompts directly, see output in real time. Works on mobile too — tap the terminal to bring up the keyboard.

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
