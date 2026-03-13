//! A minimal HTTP CONNECT proxy that runs on the VM gateway IP.
//!
//! Firecracker VMs on hosts with complex iptables (k8s/Calico) often have
//! broken TLS because MSS clamping doesn't work.  This proxy sidesteps the
//! problem entirely: the VM connects to the proxy over plain TCP on the
//! local 172.16.x.1 gateway, and the proxy opens the real TCP connection
//! from the host's network stack (which works fine).

use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Start the CONNECT proxy on the given address (e.g. 0.0.0.0:3128).
pub async fn run_connect_proxy(bind: SocketAddr) -> anyhow::Result<()> {
    let listener = TcpListener::bind(bind).await?;
    tracing::info!(%bind, "HTTP CONNECT proxy listening");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "proxy accept error");
                continue;
            }
        };
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer).await {
                tracing::debug!(error = %e, %peer, "proxy connection error");
            }
        });
    }
}

/// Read bytes until we find \r\n\r\n (end of HTTP headers).
/// Returns the position of the first byte after the blank line.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
}

async fn handle_connection(mut client: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
    // Read the HTTP request headers (up to 8KB).
    let mut buf = vec![0u8; 8192];
    let mut total = 0;

    let header_end = loop {
        if total >= buf.len() {
            anyhow::bail!("request headers too large");
        }
        let n = client.read(&mut buf[total..]).await?;
        if n == 0 {
            anyhow::bail!("client disconnected before sending complete headers");
        }
        total += n;
        if let Some(end) = find_header_end(&buf[..total]) {
            break end;
        }
    };

    // Parse the request line.
    let header_str = String::from_utf8_lossy(&buf[..header_end]);
    let first_line = header_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        client.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await?;
        return Ok(());
    }

    let method = parts[0];
    let target_or_url = parts[1];

    if method.eq_ignore_ascii_case("CONNECT") {
        // ── CONNECT tunnel ──
        let target = target_or_url; // "host:port"
        let mut upstream = match TcpStream::connect(target).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(%peer, %target, error = %e, "CONNECT upstream failed");
                client
                    .write_all(format!("HTTP/1.1 502 Bad Gateway\r\n\r\n{e}").as_bytes())
                    .await?;
                return Ok(());
            }
        };

        tracing::debug!(%peer, %target, "CONNECT tunnel established");
        // Disable Nagle on both sides to ensure data flows immediately.
        client.set_nodelay(true)?;
        upstream.set_nodelay(true)?;
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;

        // Forward any data that arrived after the headers (unlikely for CONNECT).
        let extra = &buf[header_end..total];
        if !extra.is_empty() {
            upstream.write_all(extra).await?;
        }

        // Read the first chunk from the VM and log it for debugging.
        let mut first_buf = vec![0u8; 4096];
        let first_n = client.read(&mut first_buf).await?;
        if first_n == 0 {
            tracing::warn!(%peer, %target, "VM sent no data after CONNECT 200");
            return Ok(());
        }
        let hex: String = first_buf[..first_n.min(64)]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(%peer, %target, bytes = first_n, hex, "first chunk from VM");
        upstream.write_all(&first_buf[..first_n]).await?;

        // Continue relaying bidirectionally immediately. Some TLS handshakes
        // span multiple client packets, so waiting for an upstream response
        // before forwarding more client bytes can deadlock.
        match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
            Ok((c2u, u2c)) => {
                tracing::info!(%peer, %target, c2u, u2c, "relay finished");
            }
            Err(e) => {
                tracing::debug!(%peer, %target, error = %e, "relay error");
            }
        }
    } else {
        // ── Plain HTTP proxy (absolute URL) ──
        let url = target_or_url.to_string();
        let stripped = url.strip_prefix("http://").unwrap_or(&url);
        let (host_port, path) = match stripped.find('/') {
            Some(i) => (&stripped[..i], &stripped[i..]),
            None => (stripped, "/"),
        };
        let addr = if host_port.contains(':') {
            host_port.to_string()
        } else {
            format!("{host_port}:80")
        };

        let mut upstream = TcpStream::connect(&addr).await?;
        // Re-emit request with relative path and forward original headers.
        let rewritten = header_str.replacen(target_or_url, path, 1);
        upstream.write_all(rewritten.as_bytes()).await?;

        // Forward any body data that arrived with the headers.
        let extra = &buf[header_end..total];
        if !extra.is_empty() {
            upstream.write_all(extra).await?;
        }

        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    }

    Ok(())
}
