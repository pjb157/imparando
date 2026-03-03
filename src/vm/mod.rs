pub mod firecracker;
pub mod network;
pub mod overlay;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use uuid::Uuid;

use crate::config::Config;
use firecracker::FirecrackerClient;
use network::NetworkManager;
use overlay::OverlayManager;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Creating,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    pub status: SessionStatus,
    pub repos: Vec<String>,
    pub vcpus: u8,
    pub memory_mb: u32,
    pub private_repos: bool,
    pub created_at: DateTime<Utc>,
    pub tap_name: Option<String>,
    pub vm_ip: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub name: String,
    pub repos: Vec<String>,
    pub vcpus: u8,
    pub memory_mb: u32,
    pub private_repos: bool,
}

// ── Internal VM state ─────────────────────────────────────────────────────────

struct VmChannels {
    /// Broadcast sender — subscribers receive VM stdout bytes.
    output_tx: broadcast::Sender<bytes::Bytes>,
    /// Unbounded sender — callers push bytes into VM stdin.
    input_tx: mpsc::UnboundedSender<bytes::Bytes>,
}

// ── SessionManager ────────────────────────────────────────────────────────────

pub type SharedSessionManager = Arc<SessionManager>;

pub struct SessionManager {
    config: Config,
    sessions: RwLock<HashMap<Uuid, Session>>,
    channels: RwLock<HashMap<Uuid, VmChannels>>,
}

impl SessionManager {
    pub fn new(config: Config) -> Arc<Self> {
        Arc::new(Self {
            config,
            sessions: RwLock::new(HashMap::new()),
            channels: RwLock::new(HashMap::new()),
        })
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub async fn list_sessions(&self) -> Vec<Session> {
        let mut sessions: Vec<Session> = self.sessions.read().await.values().cloned().collect();
        sessions.sort_by_key(|s| s.created_at);
        sessions
    }

    pub async fn get_session(&self, id: Uuid) -> Option<Session> {
        self.sessions.read().await.get(&id).cloned()
    }

    /// Returns a broadcast receiver for VM stdout and a clone of the stdin sender.
    pub async fn terminal_channels(
        &self,
        id: Uuid,
    ) -> Option<(broadcast::Receiver<bytes::Bytes>, mpsc::UnboundedSender<bytes::Bytes>)> {
        let channels = self.channels.read().await;
        channels.get(&id).map(|c| (c.output_tx.subscribe(), c.input_tx.clone()))
    }

    // ── Session lifecycle ─────────────────────────────────────────────────────

    pub async fn create_session(
        self: Arc<Self>,
        req: CreateSessionRequest,
    ) -> Result<Session> {
        // Hold the write lock for the entire check+insert to prevent TOCTOU races.
        let session = {
            let mut sessions = self.sessions.write().await;
            if sessions.len() >= self.config.max_sessions {
                bail!("Maximum session limit ({}) reached", self.config.max_sessions);
            }
            if sessions.values().any(|s| s.name == req.name) {
                bail!("A session named '{}' already exists", req.name);
            }
            let session = Session {
                id: Uuid::new_v4(),
                name: req.name,
                status: SessionStatus::Creating,
                repos: req.repos,
                vcpus: req.vcpus.clamp(1, 4),
                memory_mb: req.memory_mb.clamp(512, 4096),
                private_repos: req.private_repos,
                created_at: Utc::now(),
                tap_name: None,
                vm_ip: None,
                error: None,
            };
            sessions.insert(session.id, session.clone());
            session
        };

        let manager = Arc::clone(&self);
        let id = session.id;
        tokio::spawn(async move {
            if let Err(e) = manager.boot_vm(id).await {
                tracing::error!(session_id = %id, error = %e, "VM boot failed");
                manager.set_status(id, SessionStatus::Failed, Some(e.to_string())).await;
            }
        });

        Ok(session)
    }

    pub async fn stop_session(self: Arc<Self>, id: Uuid) -> Result<()> {
        let session = self.get_session(id).await;
        match session {
            None => bail!("Session {id} not found"),
            Some(s) if s.status == SessionStatus::Stopped => bail!("Session already stopped"),
            Some(s) if s.status == SessionStatus::Stopping => bail!("Session is already stopping"),
            _ => {}
        }

        self.set_status(id, SessionStatus::Stopping, None).await;

        // Drop channels — this disconnects all WebSocket clients.
        self.channels.write().await.remove(&id);

        let manager = Arc::clone(&self);
        tokio::spawn(async move {
            if let Err(e) = manager.teardown_vm(id).await {
                tracing::error!(session_id = %id, error = %e, "VM teardown error");
            }
            manager.set_status(id, SessionStatus::Stopped, None).await;
        });

        Ok(())
    }

    pub async fn delete_session(&self, id: Uuid) -> Result<()> {
        let session = self
            .get_session(id).await
            .ok_or_else(|| anyhow::anyhow!("Session {id} not found"))?;
        match session.status {
            SessionStatus::Running
            | SessionStatus::Starting
            | SessionStatus::Creating
            | SessionStatus::Stopping => {
                bail!("Stop the session before deleting it");
            }
            _ => {}
        }
        self.sessions.write().await.remove(&id);
        self.channels.write().await.remove(&id);
        Ok(())
    }

    /// Gracefully stop all running sessions. Called on process shutdown.
    pub async fn shutdown(self: Arc<Self>) {
        let ids: Vec<Uuid> = {
            self.sessions
                .read()
                .await
                .values()
                .filter(|s| {
                    matches!(
                        s.status,
                        SessionStatus::Running
                            | SessionStatus::Starting
                            | SessionStatus::Creating
                    )
                })
                .map(|s| s.id)
                .collect()
        };

        if ids.is_empty() {
            return;
        }

        tracing::info!(count = ids.len(), "Shutting down sessions");

        // Remove all channels first to disconnect WebSocket clients.
        {
            let mut channels = self.channels.write().await;
            for id in &ids {
                channels.remove(id);
            }
        }

        // Teardown all VMs concurrently.
        let handles: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let this = Arc::clone(&self);
                tokio::spawn(async move {
                    if let Err(e) = this.teardown_vm(id).await {
                        tracing::warn!(session_id = %id, error = %e, "Shutdown teardown error");
                    }
                })
            })
            .collect();

        for handle in handles {
            let _ = handle.await;
        }
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    async fn set_status(&self, id: Uuid, status: SessionStatus, error: Option<String>) {
        if let Some(s) = self.sessions.write().await.get_mut(&id) {
            s.status = status;
            s.error = error;
        }
    }

    async fn set_network_info(&self, id: Uuid, tap_name: String, vm_ip: String) {
        if let Some(s) = self.sessions.write().await.get_mut(&id) {
            s.tap_name = Some(tap_name);
            s.vm_ip = Some(vm_ip);
        }
    }

    /// Full VM boot sequence. Runs in a background task.
    async fn boot_vm(&self, id: Uuid) -> Result<()> {
        // Use `?` rather than `unwrap()` — the session could theoretically be
        // deleted between create_session inserting it and this task starting.
        let session = self
            .get_session(id)
            .await
            .ok_or_else(|| anyhow::anyhow!("Session {id} disappeared before boot"))?;

        let vm_index = self.assign_vm_index().await?;
        let tap_name = format!("tap{vm_index}");
        let vm_ip = format!("172.16.{vm_index}.2");
        let gw_ip = format!("172.16.{vm_index}.1");
        let tap_ip = format!("{gw_ip}/24");
        let mac = format!("AA:FC:00:00:00:{vm_index:02X}");

        let run_dir = self.config.run_dir.join(id.to_string());
        tokio::fs::create_dir_all(&run_dir).await?;

        // 1. Create overlay rootfs
        let overlay_path = self.config.data_dir.join("overlays").join(format!("{id}.ext4"));
        tokio::fs::create_dir_all(overlay_path.parent().unwrap()).await?;

        let ssh_key = if session.private_repos {
            let key_path = &self.config.ssh_key_path;
            if key_path.exists() {
                Some(tokio::fs::read_to_string(key_path).await?)
            } else {
                tracing::warn!("Private repos requested but SSH key not found at {key_path:?}");
                None
            }
        } else {
            None
        };

        OverlayManager::create_overlay(
            &self.config.base_rootfs_path,
            &overlay_path,
            &session.repos,
            ssh_key.as_deref(),
            &vm_ip,
            &gw_ip,
            self.config.anthropic_api_key.as_deref(),
            self.config.claude_oauth_token.as_deref(),
        )
        .await?;

        // 2. Set up TAP networking
        NetworkManager::setup_tap(&tap_name, &tap_ip).await?;
        self.set_network_info(id, tap_name.clone(), vm_ip).await;

        // 3. Spawn Firecracker
        let api_sock = run_dir.join("api.sock");
        let stderr_log = run_dir.join("firecracker.log");
        let kernel_path = self.config.kernel_path.to_str().unwrap().to_string();
        let overlay_path_str = overlay_path.to_str().unwrap().to_string();

        tracing::info!(session_id = %id, bin = ?self.config.firecracker_bin, "Spawning Firecracker");
        let stderr_file = std::fs::File::create(&stderr_log)?;
        let mut child = tokio::process::Command::new(&self.config.firecracker_bin)
            .arg("--api-sock")
            .arg(&api_sock)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr_file)
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn Firecracker ({:?}): {e}", self.config.firecracker_bin))?;

        let child_stdin = child.stdin.take().unwrap();
        let child_stdout = child.stdout.take().unwrap();

        // 4. Wait for API socket
        tracing::info!(session_id = %id, socket = ?api_sock, "Waiting for Firecracker API socket");
        if let Err(e) = FirecrackerClient::wait_for_socket(&api_sock, std::time::Duration::from_secs(5)).await {
            let stderr = tokio::fs::read_to_string(&stderr_log).await.unwrap_or_default();
            if !stderr.is_empty() {
                tracing::error!(session_id = %id, "Firecracker stderr:\n{stderr}");
            }
            return Err(e);
        }

        // 5. Configure VM via Firecracker API
        tracing::info!(session_id = %id, "Configuring VM (machine, kernel, rootfs, network)");
        let fc = FirecrackerClient::new(api_sock.to_str().unwrap());

        let config_result = async {
            fc.configure_machine(session.vcpus, session.memory_mb).await?;
            fc.configure_boot_source(&kernel_path).await?;
            fc.configure_rootfs(&overlay_path_str).await?;
            fc.configure_network(&tap_name, &mac).await?;
            fc.start().await
        }.await;

        if let Err(ref e) = config_result {
            let stderr = tokio::fs::read_to_string(&stderr_log).await.unwrap_or_default();
            if !stderr.is_empty() {
                tracing::error!(session_id = %id, "Firecracker stderr:\n{stderr}");
            }
            tracing::error!(session_id = %id, "Firecracker API configuration failed: {e}");
            return Err(config_result.unwrap_err());
        }
        tracing::info!(session_id = %id, "VM started");

        self.set_status(id, SessionStatus::Starting, None).await;

        // 6. Wire up terminal I/O channels
        let (output_tx, _) = broadcast::channel::<bytes::Bytes>(256);
        let (input_tx, input_rx) = mpsc::unbounded_channel::<bytes::Bytes>();

        self.channels
            .write()
            .await
            .insert(id, VmChannels { output_tx: output_tx.clone(), input_tx });

        // Task: VM stdout → broadcast channel
        let output_tx_clone = output_tx.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut stdout = child_stdout;
            let mut buf = vec![0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let _ = output_tx_clone.send(bytes::Bytes::copy_from_slice(&buf[..n]));
                    }
                }
            }
            tracing::debug!(session_id = %id, "VM stdout closed");
        });

        // Task: input channel → VM stdin
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child_stdin;
            let mut rx = input_rx;
            while let Some(data) = rx.recv().await {
                if stdin.write_all(&data).await.is_err() {
                    break;
                }
            }
            tracing::debug!(session_id = %id, "VM stdin closed");
        });

        // Task: wait for Firecracker to exit, then clean up.
        // This is the sole owner of cleanup for a fully-booted VM.
        {
            let run_dir_owned = run_dir.clone();
            let tap_name_owned = tap_name.clone();
            let overlay_owned = overlay_path.clone();
            tokio::spawn(async move {
                let _ = child.wait().await;
                tracing::info!(session_id = %id, "Firecracker process exited");
                let _ = NetworkManager::teardown_tap(&tap_name_owned).await;
                let _ = tokio::fs::remove_file(&overlay_owned).await;
                let _ = tokio::fs::remove_dir_all(&run_dir_owned).await;
            });
        }

        self.set_status(id, SessionStatus::Running, None).await;
        Ok(())
    }

    /// Signal the VM to stop. Cleanup is handled by the process-exit watcher
    /// spawned in boot_vm. For VMs that never fully booted, cleans up directly.
    async fn teardown_vm(&self, id: Uuid) -> Result<()> {
        let api_sock = self.config.run_dir.join(id.to_string()).join("api.sock");
        let vm_started = api_sock.exists();

        if vm_started {
            // VM is running — send graceful shutdown signal. The process-exit
            // watcher task (spawned in boot_vm) will handle TAP/overlay cleanup
            // once the process exits.
            let fc = FirecrackerClient::new(api_sock.to_str().unwrap());
            let _ = fc.send_ctrl_alt_del().await;
            // Give Firecracker a moment to begin shutdown.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        } else {
            // VM never fully started — no watcher task was spawned, so we clean
            // up directly here.
            let session = self.get_session(id).await;
            if let Some(s) = session {
                if let Some(ref tap) = s.tap_name {
                    let _ = NetworkManager::teardown_tap(tap).await;
                }
            }
            let overlay = self.config.data_dir.join("overlays").join(format!("{id}.ext4"));
            let _ = tokio::fs::remove_file(&overlay).await;
            let _ = tokio::fs::remove_dir_all(self.config.run_dir.join(id.to_string())).await;
        }

        Ok(())
    }

    /// Pick the lowest unused TAP index (0–9). Returns an error if all slots are full.
    async fn assign_vm_index(&self) -> Result<u8> {
        let sessions = self.sessions.read().await;
        let used: std::collections::HashSet<u8> = sessions
            .values()
            .filter_map(|s| {
                s.tap_name
                    .as_ref()
                    .and_then(|t| t.strip_prefix("tap"))
                    .and_then(|n| n.parse().ok())
            })
            .collect();
        (0u8..10)
            .find(|i| !used.contains(i))
            .ok_or_else(|| anyhow::anyhow!("All VM slots are in use (max 10 concurrent sessions)"))
    }
}
