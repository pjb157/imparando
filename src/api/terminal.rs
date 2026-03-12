use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use uuid::Uuid;

use crate::vm::SharedSessionManager;

type TtydWs = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

pub async fn terminal_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<Uuid>,
    State(manager): State<SharedSessionManager>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, id, manager))
}

async fn handle_socket(socket: WebSocket, id: Uuid, manager: SharedSessionManager) {
    let vm_ip = match manager.get_session(id).await.and_then(|s| s.vm_ip) {
        Some(ip) => ip,
        None => {
            tracing::warn!(session_id = %id, "Terminal WS: session not found or no IP");
            return;
        }
    };

    let ttyd_url = format!("ws://{}:7681/ws", vm_ip);
    tracing::info!(session_id = %id, url = %ttyd_url, "Connecting to ttyd");

    let mut ttyd_ws = match connect_ttyd(&ttyd_url, 60).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::error!(session_id = %id, error = %e, "Failed to connect to ttyd");
            return;
        }
    };

    // ttyd requires an AuthToken message before it starts PTY output.
    // Even with no auth configured, the client must send {"AuthToken":""}.
    if let Err(e) = ttyd_ws
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"AuthToken":""}"#.into(),
        ))
        .await
    {
        tracing::error!(session_id = %id, error = %e, "Failed to send ttyd AuthToken");
        return;
    }

    tracing::info!(session_id = %id, "Proxying terminal to ttyd");

    let (mut browser_tx, mut browser_rx) = socket.split();
    let (mut ttyd_tx, mut ttyd_rx) = ttyd_ws.split();

    let sid = id;
    // Browser → ttyd
    let b2t = tokio::spawn(async move {
        use tokio_tungstenite::tungstenite::Message as TMsg;
        while let Some(result) = browser_rx.next().await {
            match result {
                Err(e) => {
                    tracing::warn!(session_id = %sid, error = %e, "browser_rx error");
                    break;
                }
                Ok(msg) => {
                    let out = match &msg {
                        Message::Binary(data) => {
                            if !data.is_empty() {
                                tracing::debug!(session_id = %sid, type_byte = data[0], len = data.len(), "browser→ttyd binary");
                            }
                            TMsg::Binary(data.to_vec().into())
                        }
                        Message::Text(text) => {
                            tracing::debug!(session_id = %sid, len = text.len(), "browser→ttyd text");
                            TMsg::Text(text.clone().into())
                        }
                        Message::Close(_) => {
                            tracing::info!(session_id = %sid, "browser sent close");
                            break;
                        }
                        _ => continue,
                    };
                    if ttyd_tx.send(out).await.is_err() {
                        tracing::warn!(session_id = %sid, "ttyd_tx send failed");
                        break;
                    }
                }
            }
        }
        tracing::info!(session_id = %sid, "browser→ttyd loop ended");
    });

    let sid = id;
    // ttyd → browser
    let t2b = tokio::spawn(async move {
        use tokio_tungstenite::tungstenite::Message as TMsg;
        let mut msg_count: u64 = 0;
        while let Some(result) = ttyd_rx.next().await {
            match result {
                Err(e) => {
                    tracing::warn!(session_id = %sid, error = %e, "ttyd_rx error");
                    break;
                }
                Ok(msg) => {
                    msg_count += 1;
                    let out = match &msg {
                        TMsg::Binary(data) => {
                            if msg_count <= 10 || msg_count % 100 == 0 {
                                let type_byte = data.first().copied().unwrap_or(0);
                                tracing::info!(session_id = %sid, type_byte, len = data.len(), msg_count, "ttyd→browser binary");
                            }
                            Message::Binary(data.to_vec())
                        }
                        TMsg::Text(text) => {
                            tracing::info!(session_id = %sid, text = %text, "ttyd→browser text");
                            Message::Text(text.to_string())
                        }
                        TMsg::Close(_) => {
                            tracing::info!(session_id = %sid, "ttyd sent close");
                            break;
                        }
                        _ => continue,
                    };
                    if browser_tx.send(out).await.is_err() {
                        tracing::warn!(session_id = %sid, "browser_tx send failed");
                        break;
                    }
                }
            }
        }
        tracing::info!(session_id = %sid, msg_count, "ttyd→browser loop ended");
    });

    tokio::select! {
        _ = b2t => {}
        _ = t2b => {}
    }

    tracing::info!(session_id = %id, "Terminal WS disconnected");
}

async fn connect_ttyd(url: &str, timeout_secs: u64) -> anyhow::Result<TtydWs> {
    use tokio_tungstenite::tungstenite::http;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        // ttyd requires the "tty" WebSocket subprotocol — without it, the
        // connection is accepted but ttyd never routes messages to the PTY.
        let request = http::Request::builder()
            .uri(url)
            .header("Sec-WebSocket-Protocol", "tty")
            .header("Host", http::Uri::try_from(url).ok()
                .and_then(|u| u.authority().map(|a| a.to_string()))
                .unwrap_or_default())
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            .body(())
            .expect("valid request");

        match tokio_tungstenite::connect_async(request).await {
            Ok((ws, _)) => return Ok(ws),
            Err(e) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(anyhow::anyhow!("Timed out connecting to ttyd: {e}"));
                }
                tracing::debug!("ttyd not ready yet, retrying: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}
