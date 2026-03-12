use anyhow::{anyhow, Result};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct FirecrackerClient {
    socket_path: String,
}

impl FirecrackerClient {
    pub fn new(socket_path: impl Into<String>) -> Self {
        Self { socket_path: socket_path.into() }
    }

    /// Poll until the Firecracker API socket appears or timeout expires.
    pub async fn wait_for_socket(path: &Path, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if path.exists() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!("Timeout waiting for Firecracker socket at {path:?}"));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String)> {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.do_request(method, path, body),
        )
        .await
        .map_err(|_| anyhow!("Firecracker API request timed out: {method} {path}"))
        .and_then(|r| r)
    }

    async fn do_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String)> {
        let mut stream = UnixStream::connect(&self.socket_path).await?;

        // Write the full request before doing any reading.
        // Firecracker uses HTTP/1.1 keep-alive, so we must parse by Content-Length
        // rather than reading until EOF (which would block forever).
        let request = match body {
            Some(b) => format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccept: */*\r\n\r\n{b}",
                b.len()
            ),
            None => format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: */*\r\n\r\n"
            ),
        };
        stream.write_all(request.as_bytes()).await?;

        // Wrap in BufReader only after writing — this avoids splitting the stream,
        // which could cause the write to buffer and never be flushed before we read.
        let mut reader = BufReader::new(stream);

        // Parse status line
        let mut status_line = String::new();
        reader.read_line(&mut status_line).await?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);

        // Read headers, extract Content-Length.
        // 204/304/1xx have no body and often no Content-Length — default to 0.
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(val) = trimmed.to_lowercase().strip_prefix("content-length:") {
                content_length = val.trim().parse().unwrap_or(0);
            }
        }

        // Read exactly Content-Length bytes (0 for 204 No Content, etc.)
        let mut body_bytes = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body_bytes).await?;
        }

        Ok((status, String::from_utf8_lossy(&body_bytes).into_owned()))
    }

    async fn put(&self, path: &str, body: &str) -> Result<()> {
        let (status, resp_body) = self.request("PUT", path, Some(body)).await?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(anyhow!("Firecracker PUT {path} → {status}: {resp_body}"))
        }
    }

    pub async fn configure_machine(&self, vcpus: u8, memory_mb: u32) -> Result<()> {
        self.put(
            "/machine-config",
            &serde_json::json!({
                "vcpu_count": vcpus,
                "mem_size_mib": memory_mb,
                "smt": false
            })
            .to_string(),
        )
        .await
    }

    pub async fn configure_boot_source(&self, kernel_path: &str) -> Result<()> {
        self.put(
            "/boot-source",
            &serde_json::json!({
                "kernel_image_path": kernel_path,
                "boot_args": "root=/dev/vda rw console=ttyS0 reboot=k panic=1 pci=off init=/startup.sh net.ifnames=0 biosdevname=0"
            })
            .to_string(),
        )
        .await
    }

    pub async fn configure_rootfs(&self, rootfs_path: &str) -> Result<()> {
        self.put(
            "/drives/rootfs",
            &serde_json::json!({
                "drive_id": "rootfs",
                "path_on_host": rootfs_path,
                "is_root_device": true,
                "is_read_only": false
            })
            .to_string(),
        )
        .await
    }

    pub async fn configure_network(&self, tap_name: &str, mac: &str) -> Result<()> {
        self.put(
            "/network-interfaces/eth0",
            &serde_json::json!({
                "iface_id": "eth0",
                "host_dev_name": tap_name,
                "guest_mac": mac
            })
            .to_string(),
        )
        .await
    }

    pub async fn configure_entropy(&self) -> Result<()> {
        self.put("/entropy", "{}").await
    }

    pub async fn start(&self) -> Result<()> {
        self.put(
            "/actions",
            &serde_json::json!({ "action_type": "InstanceStart" }).to_string(),
        )
        .await
    }

    /// Send Ctrl+Alt+Del to trigger a graceful shutdown inside the guest.
    pub async fn send_ctrl_alt_del(&self) -> Result<()> {
        self.put(
            "/actions",
            &serde_json::json!({ "action_type": "SendCtrlAltDel" }).to_string(),
        )
        .await
    }
}
