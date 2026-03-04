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

pub async fn terminal_ws(
    ws: WebSocketUpgrade,
    Path(id): Path<Uuid>,
    State(manager): State<SharedSessionManager>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, id, manager))
}

async fn handle_socket(socket: WebSocket, id: Uuid, manager: SharedSessionManager) {
    let channels = manager.terminal_channels(id).await;
    let (scrollback, mut output_rx, input_tx) = match channels {
        Some(c) => c,
        None => {
            tracing::warn!(session_id = %id, "Terminal connect: session not found or not running");
            return;
        }
    };

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Replay scrollback so late subscribers see prior output.
    if !scrollback.is_empty() {
        let _ = ws_tx.send(Message::Binary(scrollback.into())).await;
    }

    // Task: VM output → WebSocket
    let send_task = tokio::spawn(async move {
        loop {
            match output_rx.recv().await {
                Ok(data) => {
                    if ws_tx.send(Message::Binary(data.into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(session_id = %id, skipped = n, "Terminal output lagged");
                }
            }
        }
    });

    // Main loop: WebSocket → VM input
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            Message::Binary(data) => {
                if input_tx.send(bytes::Bytes::from(data)).is_err() {
                    break;
                }
            }
            Message::Text(text) => {
                // Handle resize events: {"type":"resize","cols":80,"rows":24}
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    if val.get("type").and_then(|t| t.as_str()) == Some("resize") {
                        // Serial console doesn't support TIOCSWINSZ from outside;
                        // acknowledge but take no action for now.
                        tracing::debug!(session_id = %id, "Terminal resize event received");
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
    tracing::debug!(session_id = %id, "Terminal WebSocket closed");
}
