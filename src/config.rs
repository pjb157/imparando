use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use std::path::PathBuf;

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "imparando", about = "Firecracker microVM manager for Claude Code agents")]
pub struct Cli {
    /// Path to a TOML config file
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Dashboard username
    #[arg(long)]
    pub user: Option<String>,

    /// Dashboard password
    #[arg(long)]
    pub pass: Option<String>,

    /// Port to listen on [default: 8080]
    #[arg(long)]
    pub port: Option<u16>,

    /// Directory for base image and overlays [default: /var/lib/imparando]
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// Directory for Firecracker sockets and per-session state [default: /run/imparando]
    #[arg(long)]
    pub run_dir: Option<PathBuf>,

    /// Path to SSH key injected into VMs for private repos [default: ~/.ssh/id_rsa]
    #[arg(long)]
    pub ssh_key: Option<PathBuf>,

    /// Maximum concurrent VM sessions [default: 10]
    #[arg(long)]
    pub max_sessions: Option<usize>,

    /// Anthropic API key injected into every VM
    #[arg(long)]
    pub anthropic_api_key: Option<String>,

    /// Claude Code OAuth token injected into every VM
    #[arg(long)]
    pub claude_oauth_token: Option<String>,

    /// Path to the Firecracker binary [default: /usr/local/bin/firecracker]
    #[arg(long)]
    pub firecracker_bin: Option<PathBuf>,
}

// ── Config file (TOML) ────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct FileConfig {
    user: Option<String>,
    pass: Option<String>,
    port: Option<u16>,
    data_dir: Option<PathBuf>,
    run_dir: Option<PathBuf>,
    ssh_key: Option<PathBuf>,
    max_sessions: Option<usize>,
    anthropic_api_key: Option<String>,
    claude_oauth_token: Option<String>,
    firecracker_bin: Option<PathBuf>,
}

// ── Resolved config ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Config {
    pub user: String,
    pub pass: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
    pub ssh_key_path: PathBuf,
    pub max_sessions: usize,
    pub kernel_path: PathBuf,
    pub base_rootfs_path: PathBuf,
    pub firecracker_bin: PathBuf,
    pub anthropic_api_key: Option<String>,
    pub claude_oauth_token: Option<String>,
}

impl Config {
    /// Load config with priority: CLI args > config file > env vars > defaults.
    pub fn load(cli: &Cli) -> Result<Self> {
        // Load file config if --config was given
        let file: FileConfig = match &cli.config {
            Some(path) => {
                let raw = std::fs::read_to_string(path)
                    .with_context(|| format!("reading config file {path:?}"))?;
                toml::from_str(&raw)
                    .with_context(|| format!("parsing config file {path:?}"))?
            }
            None => FileConfig::default(),
        };

        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());

        let data_dir = cli
            .data_dir
            .clone()
            .or(file.data_dir)
            .or_else(|| std::env::var("IMPARANDO_DATA_DIR").ok().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/var/lib/imparando"));

        Ok(Config {
            user: resolve_required("user", cli.user.clone(), file.user, "IMPARANDO_USER")?,
            pass: resolve_required("pass", cli.pass.clone(), file.pass, "IMPARANDO_PASS")?,
            port: cli
                .port
                .or(file.port)
                .or_else(|| std::env::var("IMPARANDO_PORT").ok()?.parse().ok())
                .unwrap_or(8080),
            run_dir: cli
                .run_dir
                .clone()
                .or(file.run_dir)
                .or_else(|| std::env::var("IMPARANDO_RUN_DIR").ok().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/run/imparando")),
            ssh_key_path: cli
                .ssh_key
                .clone()
                .or(file.ssh_key)
                .or_else(|| std::env::var("IMPARANDO_SSH_KEY").ok().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from(format!("{home}/.ssh/id_rsa"))),
            max_sessions: cli
                .max_sessions
                .or(file.max_sessions)
                .or_else(|| std::env::var("IMPARANDO_MAX_SESSIONS").ok()?.parse().ok())
                .unwrap_or(10),
            firecracker_bin: cli
                .firecracker_bin
                .clone()
                .or(file.firecracker_bin)
                .or_else(|| std::env::var("FIRECRACKER_BIN").ok().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/usr/local/bin/firecracker")),
            anthropic_api_key: cli
                .anthropic_api_key
                .clone()
                .or(file.anthropic_api_key)
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok()),
            claude_oauth_token: cli
                .claude_oauth_token
                .clone()
                .or(file.claude_oauth_token)
                .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok()),
            kernel_path: data_dir.join("vmlinux"),
            base_rootfs_path: data_dir.join("base.ext4"),
            data_dir,
        })
    }
}

fn resolve_required(
    name: &str,
    cli: Option<String>,
    file: Option<String>,
    env_key: &str,
) -> Result<String> {
    cli.or(file)
        .or_else(|| std::env::var(env_key).ok())
        .with_context(|| {
            format!(
                "'{name}' is required — set via --{name}, config file, or {env_key} env var"
            )
        })
}
