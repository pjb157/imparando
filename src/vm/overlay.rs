use anyhow::Result;
use std::io::Read;
use std::path::Path;
use tokio::process::Command;

use crate::prompts::built_in_prompts;
use crate::vm::AgentKind;

pub struct OverlayManager;

const VM_MTU: &str = "576";
const VM_ADVMSS: &str = "536";

fn host_codex_auth_exists(host_home: &Path) -> bool {
    host_home.join(".codex/auth.json").exists()
}

fn validate_repo_url(url: &str) -> Result<()> {
    let forbidden = [';', '&', '|', '$', '`', '(', ')', '<', '>', '\n', '\r', '\\'];
    if url.chars().any(|c| forbidden.contains(&c)) {
        return Err(anyhow::anyhow!("Repository URL contains forbidden characters: {url}"));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") && !url.starts_with("git@") {
        return Err(anyhow::anyhow!("Repository URL must start with https://, http://, or git@: {url}"));
    }
    Ok(())
}

impl OverlayManager {
    /// Copy the base rootfs to overlay_path and write a startup script into it.
    pub async fn create_overlay(
        base_path: &Path,
        overlay_path: &Path,
        ttyd_bin: &Path,
        auth_home: &Path,
        app_port: u16,
        session_id: &str,
        repos: &[String],
        agent: AgentKind,
        ssh_key: Option<&str>,
        github_token: Option<&str>,
        vm_ip: &str,
        gw_ip: &str,
        anthropic_api_key: Option<&str>,
        claude_oauth_token: Option<&str>,
        openai_api_key: Option<&str>,
    ) -> Result<()> {
        for repo in repos {
            validate_repo_url(repo)?;
        }

        let effective_openai_api_key = openai_api_key
            .map(str::to_owned)
            .or_else(|| read_codex_api_key_from_host(auth_home));
        let codex_auth_available = effective_openai_api_key.is_some() || host_codex_auth_exists(auth_home);

        run(
            "cp",
            &[
                "--sparse=always",
                "--reflink=auto",
                base_path.to_str().unwrap(),
                overlay_path.to_str().unwrap(),
            ],
        )
        .await?;

        let script = build_startup_script(
            app_port,
            session_id,
            repos,
            agent,
            ssh_key,
            github_token,
            vm_ip,
            gw_ip,
            anthropic_api_key,
            claude_oauth_token,
            effective_openai_api_key.as_deref(),
            codex_auth_available,
        );

        let mount_dir = overlay_path.with_extension("mnt");
        tokio::fs::create_dir_all(&mount_dir).await?;

        let mount_dir_str = mount_dir.to_str().unwrap();
        let overlay_str = overlay_path.to_str().unwrap();

        run("mount", &["-o", "loop", overlay_str, mount_dir_str]).await?;

        let write_result = async {
            // Write startup script
            let script_path = mount_dir.join("startup.sh");
            tokio::fs::write(&script_path, &script).await?;

            // Override the PostgreSQL helper in each overlay so guest boot
            // does not depend on setuid `su`, which is unreliable here.
            let postgres_helper_path = mount_dir.join("usr/local/bin/start-postgres.sh");
            if let Some(parent) = postgres_helper_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&postgres_helper_path, postgres_start_script()).await?;
            run("chmod", &["+x", postgres_helper_path.to_str().unwrap()]).await?;

            // Seed the guest CRNG from host entropy so TLS clients don't block
            // waiting for early-boot randomness inside minimal microVMs.
            let mut seed = [0u8; 512];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
            tokio::fs::write(mount_dir.join("etc/imparando-random-seed"), seed).await?;

            // Avoid "(none)" hostname resolution warnings from sudo.
            tokio::fs::write(
                mount_dir.join("etc/hostname"),
                "imparando\n",
            )
            .await?;
            tokio::fs::write(
                mount_dir.join("etc/hosts"),
                "127.0.0.1 localhost imparando\n::1 localhost ip6-localhost ip6-loopback\n",
            )
            .await?;

            // Copy ttyd binary
            let ttyd_dest = mount_dir.join("usr/local/bin/ttyd");
            run("mkdir", &["-p", mount_dir.join("usr/local/bin").to_str().unwrap()]).await?;
            tokio::fs::copy(ttyd_bin, &ttyd_dest).await?;
            run("chmod", &["+x", ttyd_dest.to_str().unwrap()]).await?;

            let git_credential_helper = mount_dir.join("usr/local/bin/git-credential-imparando");
            tokio::fs::write(&git_credential_helper, git_credential_helper_script()).await?;
            run("chmod", &["+x", git_credential_helper.to_str().unwrap()]).await?;

            let prompt_dir = mount_dir.join("root/.imparando/prompts");
            tokio::fs::create_dir_all(&prompt_dir).await?;
            for prompt in built_in_prompts() {
                let filename = prompt
                    .vm_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(prompt.id);
                tokio::fs::write(prompt_dir.join(filename), prompt.body).await?;
            }

            let workspace_dir = mount_dir.join("root/workspace");
            tokio::fs::create_dir_all(&workspace_dir).await?;
            tokio::fs::write(workspace_dir.join("AGENTS.md"), shared_agents_md()).await?;
            tokio::fs::write(workspace_dir.join("CLAUDE.md"), shared_claude_md()).await?;

            // Write SSH key if provided
            if let Some(key) = ssh_key {
                let ssh_dir = mount_dir.join("root/.ssh");
                tokio::fs::create_dir_all(&ssh_dir).await?;
                tokio::fs::write(ssh_dir.join("id_rsa"), key).await?;
                run("chmod", &["700", ssh_dir.to_str().unwrap()]).await?;
                run("chmod", &["600", ssh_dir.join("id_rsa").to_str().unwrap()]).await?;
                tokio::fs::write(
                    ssh_dir.join("known_hosts"),
                    "github.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFZM1E9o2Iod1UrQ=\n\
gitlab.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBFSMqzJeV9rUzU4kWitGgoYIoqG5oqZyYVOiGseX7xyFI9OIVUQ9k6b1FTAQ5RCFF7a7gJBnwlh8RRa4Og/vLu0=\n",
                )
                .await?;
            }

            if agent == AgentKind::Claude
                && anthropic_api_key.is_none()
                && claude_oauth_token.is_none()
            {
                copy_claude_auth(auth_home, &mount_dir).await?;
            }
            if agent == AgentKind::Codex && effective_openai_api_key.is_none() {
                copy_codex_auth(auth_home, &mount_dir).await?;
            }

            run("chmod", &["+x", script_path.to_str().unwrap()]).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        let umount_result = run("umount", &[mount_dir_str]).await;
        let _ = tokio::fs::remove_dir(&mount_dir).await;

        write_result?;
        umount_result?;

        tracing::info!(overlay = overlay_str, "Overlay rootfs prepared");
        Ok(())
    }
}

fn sh_single_quote(s: &str) -> String {
    s.replace('\'', "'\"'\"'")
}

fn read_codex_api_key_from_host(host_home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(host_home.join(".codex/auth.json")).ok()?;
    let data: serde_json::Value = serde_json::from_str(&raw).ok()?;
    data.get("OPENAI_API_KEY")?.as_str().map(str::to_owned)
}

fn github_repo_path(url: &str) -> Option<&str> {
    url.strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
}

fn postgres_start_script() -> &'static str {
    r#"#!/bin/bash
# Start PostgreSQL in the microVM (no systemd).
set -euo pipefail
if [ -d /usr/lib/postgresql/17 ]; then
  PG_VERSION=17
else
  PG_VERSION=$(ls /usr/lib/postgresql | sort -V | tail -1)
fi
PG_DATA="/var/lib/postgresql/$PG_VERSION/main"
PG_BIN="/usr/lib/postgresql/$PG_VERSION/bin"
RUN_AS_POSTGRES=(chroot --userspec=postgres:postgres /)

mkdir -p "$PG_DATA" /var/run/postgresql /var/log/postgresql
chown -R postgres:postgres /var/lib/postgresql /var/run/postgresql /var/log/postgresql

if [ ! -s "$PG_DATA/PG_VERSION" ]; then
  rm -rf "$PG_DATA"
  mkdir -p "$PG_DATA"
  chown -R postgres:postgres /var/lib/postgresql /var/run/postgresql /var/log/postgresql
  "${RUN_AS_POSTGRES[@]}" "$PG_BIN/initdb" -D "$PG_DATA"
fi

chmod 700 "$PG_DATA"
sed -i "s/^#listen_addresses = .*/listen_addresses = '127.0.0.1'/" "$PG_DATA/postgresql.conf"

if ! grep -q "^unix_socket_directories = '/var/run/postgresql'" "$PG_DATA/postgresql.conf"; then
  printf "\nunix_socket_directories = '/var/run/postgresql'\n" >> "$PG_DATA/postgresql.conf"
fi

"${RUN_AS_POSTGRES[@]}" "$PG_BIN/pg_ctl" -D "$PG_DATA" -l /var/log/postgresql/postgresql.log start -w
echo "PostgreSQL started on port 5432"
"#
}

fn repo_dir_name(url: &str) -> String {
    let path = github_repo_path(url).unwrap_or(url);
    path.rsplit('/')
        .next()
        .unwrap_or("repo")
        .trim_end_matches(".git")
        .to_string()
}

fn clone_url_for_repo(url: &str, github_token: Option<&str>) -> String {
    match (github_repo_path(url), github_token) {
        (Some(path), Some(_)) => format!("https://github.com/{path}"),
        _ => url.to_string(),
    }
}

fn git_credential_helper_script() -> &'static str {
    r#"#!/bin/sh
if [ "$1" != "get" ]; then
  exit 0
fi

protocol=""
host=""
path=""

while IFS= read -r line && [ -n "$line" ]; do
  case "$line" in
    protocol=*) protocol=${line#protocol=} ;;
    host=*) host=${line#host=} ;;
    path=*) path=${line#path=} ;;
  esac
done

if [ "$protocol" = "https" ] && [ "$host" = "github.com" ] && [ -n "${IMPARANDO_HOST_API:-}" ] && [ -n "${IMPARANDO_SESSION_ID:-}" ] && [ -n "$path" ]; then
  repo_path=${path#/}
  repo_url="https://github.com/$repo_path"
  encoded_repo=$(printf '%s' "$repo_url" | sed 's/%/%25/g; s/:/%3A/g; s,/,%2F,g')
  token=$(curl -fsS "${IMPARANDO_HOST_API}/api/sessions/${IMPARANDO_SESSION_ID}/github-token?repo=${encoded_repo}") || exit 1
  printf 'username=%s\n' 'x-access-token'
  printf 'password=%s\n' "$token"
fi
"#
}

fn shared_agents_md() -> &'static str {
    r#"# Workspace Guidance

This VM is managed by Imparando. Read this before making environment changes.

## Core Rules

- Prefer using the existing environment instead of reinstalling tooling.
- Do not run browser or device login flows inside the VM for GitHub, Claude, or Codex unless explicitly instructed.
- Keep git remotes clean HTTPS URLs without embedded credentials.
- When creating git commits, always use Conventional Commit style messages.
- Prefer reporting concrete diagnostics over inventing a new environment setup.

## Prompt References

Useful guidance files live in `/root/.imparando/prompts`:

- `/root/.imparando/prompts/github-auth.md`
- `/root/.imparando/prompts/postgres-start.md`

If you need git push help, read the GitHub auth prompt first.
If PostgreSQL is not reachable on `127.0.0.1:5432`, read the Postgres recovery prompt first.

## GitHub

- Expected git credential helper: `/usr/local/bin/git-credential-imparando`
- Do not run `gh auth login`
- Do not embed tokens into `origin`

## PostgreSQL

- Expected local address: `127.0.0.1:5432`
- Do not use `systemctl`
- First recovery command: `/usr/local/bin/start-postgres.sh`
"#
}

fn shared_claude_md() -> &'static str {
    r#"# Claude Workspace Guidance

This VM is managed by Imparando. Prefer the existing environment and read local guidance files before changing auth or database setup.

## Start Here

- Shared workspace guidance: `/root/workspace/AGENTS.md`
- GitHub auth guide: `/root/.imparando/prompts/github-auth.md`
- Postgres recovery guide: `/root/.imparando/prompts/postgres-start.md`

## Important Constraints

- Do not run browser/device auth flows inside the VM unless explicitly required.
- Do not rewrite git remotes to include tokens.
- When creating git commits, always use Conventional Commit style messages.
- If PostgreSQL is down, use `/usr/local/bin/start-postgres.sh` before attempting manual cluster setup.
"#
}

async fn copy_if_exists(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Ok(());
    }

    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::copy(src, dst).await?;
    run("chmod", &["600", dst.to_str().unwrap()]).await?;
    Ok(())
}

async fn copy_claude_auth(host_home: &Path, mount_dir: &Path) -> Result<()> {
    let vm_claude_dir = mount_dir.join("root/.claude");
    tokio::fs::create_dir_all(&vm_claude_dir).await?;

    copy_if_exists(
        &host_home.join(".claude/.credentials.json"),
        &vm_claude_dir.join(".credentials.json"),
    )
    .await?;
    copy_if_exists(
        &host_home.join(".claude.json"),
        &mount_dir.join("root/.claude.json"),
    )
    .await?;
    Ok(())
}

async fn copy_codex_auth(host_home: &Path, mount_dir: &Path) -> Result<()> {
    let vm_codex_dir = mount_dir.join("root/.codex");
    tokio::fs::create_dir_all(&vm_codex_dir).await?;

    copy_if_exists(
        &host_home.join(".codex/auth.json"),
        &vm_codex_dir.join("auth.json"),
    )
    .await?;
    copy_if_exists(
        &host_home.join(".codex/config.toml"),
        &vm_codex_dir.join("config.toml"),
    )
    .await?;

    let config_path = vm_codex_dir.join("config.toml");
    let mut config = if config_path.exists() {
        tokio::fs::read_to_string(&config_path).await.unwrap_or_default()
    } else {
        String::new()
    };
    if !config.ends_with('\n') && !config.is_empty() {
        config.push('\n');
    }
    if !config.contains("\napproval_policy = ") && !config.starts_with("approval_policy = ") {
        config.push_str("approval_policy = \"never\"\n");
    }
    if !config.contains("\nsandbox_mode = ") && !config.starts_with("sandbox_mode = ") {
        config.push_str("sandbox_mode = \"danger-full-access\"\n");
    }
    tokio::fs::write(&config_path, config).await?;
    run("chmod", &["600", config_path.to_str().unwrap()]).await?;
    Ok(())
}

fn build_startup_script(
    app_port: u16,
    session_id: &str,
    repos: &[String],
    agent: AgentKind,
    ssh_key: Option<&str>,
    github_token: Option<&str>,
    vm_ip: &str,
    gw_ip: &str,
    anthropic_api_key: Option<&str>,
    claude_oauth_token: Option<&str>,
    openai_api_key: Option<&str>,
    codex_auth_available: bool,
) -> String {
    let mut lines = vec![
        "#!/bin/bash".to_string(),
        "set -e".to_string(),
        String::new(),
        "# Mount essential filesystems".to_string(),
        "mount -t proc proc /proc 2>/dev/null || true".to_string(),
        "mount -t sysfs sysfs /sys 2>/dev/null || true".to_string(),
        "mount -t devtmpfs devtmpfs /dev 2>/dev/null || true".to_string(),
        "mkdir -p /dev/pts".to_string(),
        "mount -t devpts devpts /dev/pts 2>/dev/null || true".to_string(),
        "hostname imparando 2>/dev/null || true".to_string(),
        String::new(),
        "# Seed the kernel RNG early to prevent TLS tools from blocking waiting".to_string(),
        "# for entropy in tiny Firecracker guests.".to_string(),
        "if [ -f /etc/imparando-random-seed ]; then".to_string(),
        "  cat /etc/imparando-random-seed > /dev/urandom 2>/dev/null || true".to_string(),
        "fi".to_string(),
        "if command -v haveged >/dev/null 2>&1; then".to_string(),
        "  haveged -w 1024 >/var/log/haveged.log 2>&1 &".to_string(),
        "fi".to_string(),
        String::new(),
        "# Network setup — use reduced MTU to avoid packet drops".to_string(),
        "# through iptables FORWARD chains on k8s hosts.".to_string(),
        format!("ip addr add {vm_ip}/24 dev eth0"),
        "ip link set lo up".to_string(),
        "ip link set eth0 up".to_string(),
        format!("ip link set eth0 mtu {VM_MTU}"),
        "if command -v ethtool >/dev/null 2>&1; then ethtool -K eth0 tx off sg off tso off gso off gro off rx off || true; fi".to_string(),
        format!("ip route add default via {gw_ip} advmss {VM_ADVMSS}"),
        "echo 'nameserver 8.8.8.8' > /etc/resolv.conf".to_string(),
        String::new(),
        "# Route outbound HTTP(S) through the host-side CONNECT proxy.".to_string(),
        format!("export HTTP_PROXY='http://{gw_ip}:3128'"),
        format!("export HTTPS_PROXY='http://{gw_ip}:3128'"),
        format!("export ALL_PROXY='http://{gw_ip}:3128'"),
        format!("export http_proxy='http://{gw_ip}:3128'"),
        format!("export https_proxy='http://{gw_ip}:3128'"),
        format!("export all_proxy='http://{gw_ip}:3128'"),
        "export NO_PROXY='localhost,127.0.0.1,::1'".to_string(),
        "export no_proxy='localhost,127.0.0.1,::1'".to_string(),
        "export HOME='/root'".to_string(),
        "export CODEX_HOME='/root/.codex'".to_string(),
        "export PATH='/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin'".to_string(),
        "export CARGO_BUILD_JOBS='2'".to_string(),
        "export npm_config_jobs='2'".to_string(),
        "export CMAKE_BUILD_PARALLEL_LEVEL='2'".to_string(),
        "export MAKEFLAGS='-j2'".to_string(),
        "export PYTHONUNBUFFERED='1'".to_string(),
        "mkdir -p /root/tmp".to_string(),
        "export TMPDIR='/root/tmp'".to_string(),
        String::new(),
        "# Add guest swap so short-lived compile or agent spikes do not kill the VM.".to_string(),
        "if [ ! -f /swapfile ]; then".to_string(),
        "  if command -v fallocate >/dev/null 2>&1; then".to_string(),
        "    fallocate -l 4G /swapfile 2>/dev/null || dd if=/dev/zero of=/swapfile bs=1M count=4096 status=none".to_string(),
        "  else".to_string(),
        "    dd if=/dev/zero of=/swapfile bs=1M count=4096 status=none".to_string(),
        "  fi".to_string(),
        "  chmod 600 /swapfile".to_string(),
        "  mkswap /swapfile >/dev/null 2>&1 || true".to_string(),
        "fi".to_string(),
        "swapon /swapfile >/dev/null 2>&1 || true".to_string(),
        String::new(),
    ];

    if ssh_key.is_some() {
        lines.push("# SSH setup".to_string());
        lines.push("chmod 700 /root/.ssh".to_string());
        lines.push("chmod 600 /root/.ssh/id_rsa".to_string());
        lines.push("eval $(ssh-agent -s)".to_string());
        lines.push("ssh-add /root/.ssh/id_rsa".to_string());
        lines.push(String::new());
    }

    if anthropic_api_key.is_some() || claude_oauth_token.is_some() {
        lines.push("# Claude Code credentials".to_string());
        if let Some(key) = anthropic_api_key {
            let escaped = key.replace('\'', "'\"'\"'");
            lines.push(format!("export ANTHROPIC_API_KEY='{escaped}'"));
        }
        if let Some(token) = claude_oauth_token {
            let escaped = token.replace('\'', "'\"'\"'");
            lines.push(format!("export CLAUDE_CODE_OAUTH_TOKEN='{escaped}'"));
        }
        lines.push(String::new());
    }

    if let Some(key) = openai_api_key {
        lines.push("# Codex credentials".to_string());
        let escaped = key.replace('\'', "'\"'\"'");
        lines.push(format!("export OPENAI_API_KEY='{escaped}'"));
        lines.push(String::new());
    }

    lines.push("export TERM=xterm-256color".to_string());
    lines.push(format!("export IMPARANDO_HOST_API='http://{gw_ip}:{app_port}'"));
    lines.push(format!("export IMPARANDO_SESSION_ID='{session_id}'"));
    lines.push(format!("export IMPARANDO_SESSION_BRANCH='imparando/{session_id}'"));
    if github_token.is_some() {
        lines.push("git config --global credential.helper /usr/local/bin/git-credential-imparando".to_string());
        lines.push("git config --global credential.useHttpPath true".to_string());
    }
    lines.push(String::new());

    lines.push("# Start PostgreSQL".to_string());
    lines.push("if [ -x /usr/local/bin/start-postgres.sh ]; then".to_string());
    lines.push("  /usr/local/bin/start-postgres.sh || echo 'WARNING: PostgreSQL failed to start'".to_string());
    lines.push("fi".to_string());
    lines.push(String::new());

    lines.push("mkdir -p /root/workspace".to_string());
    lines.push("cd /root/workspace".to_string());
    lines.push(String::new());

    lines.push("# Start ttyd with tmux immediately, clone repos in the background.".to_string());
    lines.push("if command -v tmux >/dev/null 2>&1; then".to_string());
    lines.push("  tmux new-session -d -s main -c /root/workspace".to_string());
    lines.push("  tmux set-option -t main history-limit 50000".to_string());
    lines.push("  tmux set-option -t main mouse off".to_string());
    lines.push("  tmux set-option -t main terminal-overrides 'xterm*:smcup@:rmcup@'".to_string());
    lines.push("  tmux set-window-option -t main alternate-screen off".to_string());

    if !repos.is_empty() {
        // Clone repos inside a background script that runs in tmux
        // so the user can see progress in the terminal.
        let clone_cmds: Vec<String> = repos.iter().map(|repo| {
            let clone_url = clone_url_for_repo(repo, github_token);
            let repo_dir = repo_dir_name(repo);
            let branch = format!("imparando/{session_id}");
            format!(
                "git clone '{}' '{}' || echo 'WARNING: failed to clone {}'; \
                if [ -d '{}' ]; then \
                  cd '{}' && \
                  git fetch origin main && \
                  git checkout -B '{}' origin/main && \
                  git config push.default current && \
                  git config remote.origin.push 'HEAD:refs/heads/{}' && \
                  cd /root/workspace; \
                fi",
                sh_single_quote(&clone_url),
                sh_single_quote(&repo_dir),
                sh_single_quote(repo),
                sh_single_quote(&repo_dir),
                sh_single_quote(&repo_dir),
                sh_single_quote(&branch),
                sh_single_quote(&branch),
            )
        }).collect();
        let mut all_clones = clone_cmds.join(" && ");
        all_clones.push_str("; echo \"--- repos ready ---\"");
        let agent_cmd = match agent {
            AgentKind::Claude if anthropic_api_key.is_some() || claude_oauth_token.is_some() => {
                Some("claude --dangerously-skip-permissions")
            }
            AgentKind::Claude => Some("echo 'Claude selected, but no ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN was injected.'"),
            AgentKind::Codex if codex_auth_available => Some("codex --dangerously-bypass-approvals-and-sandbox"),
            AgentKind::Codex => Some("echo 'Codex selected, but no host Codex auth or OPENAI_API_KEY was available.'"),
        };
        if let Some(agent_cmd) = agent_cmd {
            all_clones.push_str("; ");
            all_clones.push_str(agent_cmd);
        }
        lines.push(format!("  tmux send-keys -t main '{}' Enter", all_clones));
    } else {
        let agent_cmd = match agent {
            AgentKind::Claude if anthropic_api_key.is_some() || claude_oauth_token.is_some() => {
                Some("claude --dangerously-skip-permissions")
            }
            AgentKind::Claude => Some("echo 'Claude selected, but no ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN was injected.'"),
            AgentKind::Codex if codex_auth_available => Some("codex --dangerously-bypass-approvals-and-sandbox"),
            AgentKind::Codex => Some("echo 'Codex selected, but no host Codex auth or OPENAI_API_KEY was available.'"),
        };
        if let Some(agent_cmd) = agent_cmd {
            lines.push(format!("  tmux send-keys -t main '{}' Enter", agent_cmd));
        }
    }

    lines.push("  while true; do".to_string());
    lines.push("    if ttyd -W tmux attach -t main; then".to_string());
    lines.push("      echo 'ttyd exited cleanly, restarting in 1s' > /dev/ttyS0".to_string());
    lines.push("    else".to_string());
    lines.push("      rc=$?".to_string());
    lines.push("      echo \"WARNING: ttyd exited with status ${rc}, restarting in 1s\" > /dev/ttyS0".to_string());
    lines.push("    fi".to_string());
    lines.push("    sleep 1".to_string());
    lines.push("  done".to_string());
    lines.push("else".to_string());
    lines.push("  while true; do".to_string());
    lines.push("    if ttyd -W bash; then".to_string());
    lines.push("      echo 'ttyd exited cleanly, restarting in 1s' > /dev/ttyS0".to_string());
    lines.push("    else".to_string());
    lines.push("      rc=$?".to_string());
    lines.push("      echo \"WARNING: ttyd exited with status ${rc}, restarting in 1s\" > /dev/ttyS0".to_string());
    lines.push("    fi".to_string());
    lines.push("    sleep 1".to_string());
    lines.push("  done".to_string());
    lines.push("fi".to_string());

    lines.join("\n")
}

async fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd).args(args).status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Command '{cmd} {}' failed with exit code {:?}",
            args.join(" "),
            status.code()
        ))
    }
}
