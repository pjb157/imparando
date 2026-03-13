use anyhow::Result;
use std::io::Read;
use std::path::Path;
use tokio::process::Command;

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

        tokio::fs::copy(base_path, overlay_path).await?;

        let script = build_startup_script(
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
        (Some(path), Some(token)) => format!("https://x-access-token:{token}@github.com/{path}"),
        _ => url.to_string(),
    }
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
    Ok(())
}

fn build_startup_script(
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
    lines.push(format!("export IMPARANDO_SESSION_BRANCH='imparando/{session_id}'"));
    if let Some(token) = github_token {
        let escaped = token.replace('\'', "'\"'\"'");
        lines.push(format!("export GITHUB_TOKEN='{escaped}'"));
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

    lines.push("  exec ttyd -W tmux attach -t main".to_string());
    lines.push("else".to_string());
    lines.push("  exec ttyd -W bash".to_string());
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
