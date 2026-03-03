# imparando — VM Manager & Claude Code Session UI

**Date:** 2026-03-03
**Status:** Approved

## Overview

`imparando` is a self-hosted tool for spawning and managing Firecracker microVMs on a Linux host. Each VM runs a Claude Code agent accessible via a web-based terminal UI. A single dashboard allows creating, monitoring, and interacting with up to 10 concurrent sessions from any device (phone, laptop) connected via Tailscale.

---

## Goals

- Spin up isolated Firecracker microVMs on demand, each running Claude Code
- Interact with each session via a full PTY terminal in the browser (xterm.js)
- Single dashboard UI with basic auth, served on one port, accessible via Tailscale
- Ephemeral VMs — filesystem wiped on stop
- Each session can clone specified git repos at boot (with optional SSH key injection for private repos)
- Per-session resource allocation: vCPUs and memory configurable at creation time
- Max 10 concurrent sessions

## Non-Goals

- Multi-user support / user accounts
- Persistent VM filesystems (snapshots, resume)
- Container-based sessions (Docker) — Firecracker only
- Kubernetes or distributed scheduling

---

## Architecture

### Single Binary

`imparando` is a single Rust binary with three logical subsystems:

```
┌─────────────────────────────────────────────────────────┐
│                  Ubuntu Host (your node)                 │
│                                                         │
│  ┌─────────────────────────────────────────────────┐   │
│  │          imparando (Rust binary)                 │   │
│  │                                                  │   │
│  │  ┌──────────┐  ┌──────────────┐  ┌───────────┐  │   │
│  │  │ HTTP API │  │ VM Manager   │  │ PTY Proxy │  │   │
│  │  │ (axum)   │  │ (Firecracker)│  │ (ws/pty)  │  │   │
│  │  └──────────┘  └──────────────┘  └───────────┘  │   │
│  └─────────────────────────────────────────────────┘   │
│       │                  │                              │
│       │          ┌───────┴──────┐                       │
│       │          │  MicroVMs    │                       │
│       │   ┌──────┴──┐  ┌───────┴─┐                     │
│       │   │  VM 1   │  │  VM 2   │  ...                 │
│       │   │ claude  │  │ claude  │                      │
│       │   │ code    │  │ claude  │                      │
│       │   └─────────┘  └─────────┘                     │
│                                                         │
│  Tailscale → :8080 (HTTP basic auth)                    │
└─────────────────────────────────────────────────────────┘
```

**Subsystem 1 — HTTP API + UI Server (axum)**

- Serves the static single-page dashboard (HTML/JS embedded in the binary)
- REST endpoints for session CRUD
- WebSocket endpoint per session for terminal I/O
- HTTP Basic Auth middleware on all routes (credentials from env vars)

**Subsystem 2 — VM Manager**

- Owns Firecracker process lifecycle (spawn, monitor, kill)
- Talks to Firecracker via its Unix socket API (`/run/imparando/vm-{id}/firecracker.sock`)
- Manages TAP devices and iptables rules for networking
- Prepares overlay images before boot
- Enforces max session limit (10)

**Subsystem 3 — PTY Proxy**

- Bridges WebSocket connections to Firecracker serial console Unix sockets
- Bidirectional byte streaming — no protocol translation needed
- Handles terminal resize events (TIOCSWINSZ forwarded to the serial console)
- Multiple browser tabs can connect to the same session simultaneously

---

## VM Lifecycle

### Creating a Session

1. User submits "New Session" form: name, repo URLs, vCPUs (1–4), memory (512MB–4GB), private repo toggle
2. Backend validates: session limit not exceeded, name unique, resources within bounds
3. Backend creates overlay image: `cp base.ext4 /var/lib/imparando/overlays/{id}.ext4`
4. Backend writes startup script into the overlay:
   - Git config + SSH key (if private repos)
   - `git clone` commands for each repo
   - Exec into Claude Code on the serial console
5. Backend creates TAP device (`tap{n}`) and configures iptables NAT
6. Backend calls Firecracker API to configure VM (kernel, rootfs, network, resources)
7. Backend starts the Firecracker process — VM boots in ~125ms
8. Session status transitions: `creating → starting → running`

### Stopping a Session

1. User clicks Stop (or session is idle-killed in a future version)
2. Backend sends SIGTERM to Firecracker process
3. Backend deletes overlay image
4. Backend removes TAP device and iptables rules
5. Session status: `stopping → stopped`

### Session States

```
created → starting → running → stopping → stopped
                ↑                              |
                └──── (restart, future) ───────┘
```

---

## Networking

Each VM gets:
- A TAP device on the host (`tap0`–`tap9`)
- A static private IP inside the VM: `192.168.100.{n+2}/24`
- Host-side TAP IP: `192.168.100.1` (default gateway inside VM)
- Outbound internet via iptables MASQUERADE on the host's default interface
- No inbound access to the VM from outside the host

The serial console (not SSH) is used for PTY access, so no per-VM port mapping is needed. The Rust backend proxies terminal I/O through the Firecracker serial console socket.

---

## PTY Proxy & Terminal Connection

```
Browser (xterm.js)
       │
       │  WebSocket  ws://.../api/sessions/{id}/terminal
       │
Rust PTY Proxy  (tokio async read/write)
       │
       │  Unix socket  /run/imparando/vm-{id}/console.sock
       │
Firecracker serial console
       │
       │  /dev/ttyS0 (inside VM)
       │
Claude Code (running as init process)
```

- xterm.js handles ANSI rendering, scrollback, copy/paste, mobile keyboard
- Terminal resize: browser sends `{"type":"resize","cols":N,"rows":N}` JSON frame; backend forwards as control bytes on the serial console
- Multiple browser clients can connect to the same session (same bytes broadcast to all)

---

## Frontend UI

Single HTML file with embedded JavaScript, served from the Rust binary via `include_str!`. No build step, no npm, no bundler.

**Dashboard (/):**
- Table of sessions: name, status badge, repos, vCPUs, memory, uptime, actions (Open / Stop / Delete)
- "New Session" button → modal form
- Auto-refreshes session list every 5 seconds via polling

**Session view (/session/{id}):**
- Full-viewport xterm.js terminal
- Thin top bar: session name, status, "Stop" button, back link
- Terminal connects immediately on page load via WebSocket
- Works on mobile — soft keyboard appears on tap

**New Session modal:**
- Name (text)
- Repos (textarea, one URL per line)
- vCPUs (select: 1 / 2 / 4)
- Memory (select: 512MB / 1GB / 2GB / 4GB)
- Private repos (checkbox — injects host SSH key)

**Auth:** HTTP Basic Auth enforced by axum middleware. Username and password set via `IMPARANDO_USER` and `IMPARANDO_PASS` environment variables.

---

## rootfs Image

### Base Image (`/var/lib/imparando/base.ext4`)

Minimal Ubuntu 22.04 rootfs (no systemd, no desktop) containing:
- Node.js LTS
- `@anthropic-ai/claude-code` (npm global install)
- `git`, `openssh-client`, `curl`, `ca-certificates`
- `tini` as PID 1 (reaps zombies, forwards signals)
- A small `/entrypoint.sh` that reads startup config and execs Claude Code

Built by `scripts/build-rootfs.sh` using `debootstrap` or a pre-built Ubuntu cloud image stripped down. This is a one-time setup step on the host.

### Kernel (`/var/lib/imparando/vmlinux`)

Downloaded from Firecracker's published kernel releases. No custom kernel compilation required.

`scripts/download-kernel.sh` fetches the appropriate version.

### Per-Session Overlay

A copy of `base.ext4` created per session. Before boot, the backend mounts it loopback and writes:
- `/startup.sh` — clones repos, sets git config
- `/root/.ssh/id_rsa` + `/root/.ssh/known_hosts` — if private repos requested (copied from host)
- `/root/.ssh/` permissions set to 700

The VM's `entrypoint.sh` runs `/startup.sh` then `exec claude` on `/dev/ttyS0`.

---

## Project Structure

```
imparando/
├── src/
│   ├── main.rs              # Binary entry point, config, startup
│   ├── api/
│   │   ├── mod.rs           # axum router, auth middleware
│   │   ├── sessions.rs      # CRUD endpoints
│   │   └── terminal.rs      # WebSocket upgrade + PTY proxy
│   ├── vm/
│   │   ├── mod.rs           # SessionManager, session state
│   │   ├── firecracker.rs   # Firecracker API client (HTTP over Unix socket)
│   │   ├── network.rs       # TAP device + iptables management
│   │   └── overlay.rs       # rootfs overlay creation + startup script injection
│   └── ui/
│       └── index.html       # Single-page UI (embedded via include_str!)
├── scripts/
│   ├── build-rootfs.sh      # One-time base image build
│   └── download-kernel.sh   # Fetch Firecracker kernel
├── docs/
│   └── plans/
│       └── 2026-03-03-vm-manager-design.md
└── Cargo.toml
```

---

## Key Dependencies

| Crate | Purpose |
|---|---|
| `axum` | HTTP server, WebSocket, routing |
| `tokio` | Async runtime |
| `reqwest` (Unix socket) | Firecracker API calls over Unix socket |
| `hyper` + `hyperlocal` | HTTP over Unix domain socket for Firecracker |
| `serde` / `serde_json` | Config and API serialization |
| `uuid` | Session IDs |
| `tokio-tungstenite` | WebSocket handling |
| `tracing` | Structured logging |

Frontend: `xterm.js` and `xterm-addon-fit` loaded from a CDN link in the HTML (no build step).

---

## Configuration

All configuration via environment variables:

| Variable | Default | Description |
|---|---|---|
| `IMPARANDO_USER` | — | Basic auth username (required) |
| `IMPARANDO_PASS` | — | Basic auth password (required) |
| `IMPARANDO_PORT` | `8080` | Listen port |
| `IMPARANDO_DATA_DIR` | `/var/lib/imparando` | Base images, overlays |
| `IMPARANDO_RUN_DIR` | `/run/imparando` | Firecracker sockets, per-session state |
| `IMPARANDO_SSH_KEY` | `~/.ssh/id_rsa` | Host SSH key path for private repo injection |
| `IMPARANDO_MAX_SESSIONS` | `10` | Maximum concurrent sessions |

---

## Setup on the Host (one-time)

```bash
# 1. Ensure KVM is available
ls /dev/kvm

# 2. Install Firecracker binary to PATH
# https://github.com/firecracker-microvm/firecracker/releases

# 3. Build the base rootfs image
sudo ./scripts/build-rootfs.sh

# 4. Download the kernel
./scripts/download-kernel.sh

# 5. Run imparando
IMPARANDO_USER=peter IMPARANDO_PASS=yourpass ./imparando
```

Tailscale exposes `:8080` to your phone and laptop. No additional port forwarding needed.

---

## Security Considerations

- Basic auth over Tailscale (encrypted tunnel) — acceptable for personal use
- SSH keys are written into VM overlays at boot and destroyed with the overlay on stop
- VMs have outbound internet but no inbound access from outside the host
- Firecracker provides strong VM-level isolation (separate kernel per VM)
- The binary must run as root (or with `CAP_NET_ADMIN`) for TAP device management — standard for Firecracker deployments

---

## Out of Scope (Future)

- Session snapshots / resume
- Idle timeout / auto-stop
- Resource usage metrics in the UI
- Multiple SSH keys or per-repo key selection
- HTTPS (Tailscale handles encryption)
- Log persistence across session restarts
