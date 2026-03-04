use anyhow::Result;
use std::path::Path;
use tokio::process::Command;

pub struct OverlayManager;

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
        repos: &[String],
        ssh_key: Option<&str>,
        vm_ip: &str,
        gw_ip: &str,
        anthropic_api_key: Option<&str>,
        claude_oauth_token: Option<&str>,
    ) -> Result<()> {
        for repo in repos {
            validate_repo_url(repo)?;
        }

        tokio::fs::copy(base_path, overlay_path).await?;

        let script = build_startup_script(repos, ssh_key, vm_ip, gw_ip, anthropic_api_key, claude_oauth_token);

        let mount_dir = overlay_path.with_extension("mnt");
        tokio::fs::create_dir_all(&mount_dir).await?;

        let mount_dir_str = mount_dir.to_str().unwrap();
        let overlay_str = overlay_path.to_str().unwrap();

        run("mount", &["-o", "loop", overlay_str, mount_dir_str]).await?;

        let write_result = async {
            // Write startup script
            let script_path = mount_dir.join("startup.sh");
            tokio::fs::write(&script_path, &script).await?;

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

fn build_startup_script(
    repos: &[String],
    ssh_key: Option<&str>,
    vm_ip: &str,
    gw_ip: &str,
    anthropic_api_key: Option<&str>,
    claude_oauth_token: Option<&str>,
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
        String::new(),
        "# Network setup".to_string(),
        format!("ip addr add {vm_ip}/24 dev eth0"),
        "ip link set eth0 up".to_string(),
        format!("ip route add default via {gw_ip}"),
        "echo 'nameserver 8.8.8.8' > /etc/resolv.conf".to_string(),
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

    lines.push("export TERM=xterm-256color".to_string());
    lines.push("mkdir -p /root/workspace".to_string());
    lines.push("cd /root/workspace".to_string());
    lines.push(String::new());

    if !repos.is_empty() {
        lines.push("# Clone repositories".to_string());
        for repo in repos {
            lines.push(format!("git clone '{repo}'"));
        }
        lines.push(String::new());
    }

    lines.push("# Start ttyd — provides a proper PTY for Claude Code".to_string());
    lines.push("exec ttyd -W claude".to_string());

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
