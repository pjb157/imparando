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

    let ttyd_ws = match connect_ttyd(&ttyd_url, 60).await {
        Ok(ws) => ws,
        Err(e) => {
            tracing::error!(session_id = %id, error = %e, "Failed to connect to ttyd");
            return;
        }
    };

    tracing::info!(session_id = %id, "Proxying terminal to ttyd");

    let (mut browser_tx, mut browser_rx) = socket.split();
    let (mut ttyd_tx, mut ttyd_rx) = ttyd_ws.split();

    // Browser → ttyd
    let b2t = tokio::spawn(async move {
        use tokio_tungstenite::tungstenite::Message as TMsg;
        while let Some(Ok(msg)) = browser_rx.next().await {
            let out = match msg {
                Message::Binary(data) => TMsg::Binary(data.to_vec().into()),
                Message::Text(text) => TMsg::Text(text.into()),
                Message::Close(_) => break,
                _ => continue,
            };
            if ttyd_tx.send(out).await.is_err() {
                break;
            }
        }
    });

    // ttyd → browser
    let t2b = tokio::spawn(async move {
        use tokio_tungstenite::tungstenite::Message as TMsg;
        while let Some(Ok(msg)) = ttyd_rx.next().await {
            let out = match msg {
                TMsg::Binary(data) => Message::Binary(data.to_vec()),
                TMsg::Text(text) => Message::Text(text.to_string()),
                TMsg::Close(_) => break,
                _ => continue,
            };
            if browser_tx.send(out).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = b2t => {}
        _ = t2b => {}
    }

    tracing::info!(session_id = %id, "Terminal WS disconnected");
}

async fn connect_ttyd(url: &str, timeout_secs: u64) -> anyhow::Result<TtydWs> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        match tokio_tungstenite::connect_async(url).await {
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
