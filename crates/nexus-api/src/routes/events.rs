use crate::auth::authenticate;
use crate::state::SharedState;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
struct AuthMessage {
    #[serde(rename = "type")]
    kind: String,
    token: String,
}

/// Live events. Browsers cannot set headers on WebSocket connections, so the
/// client authenticates with its first message: `{"type":"auth","token":"…"}`.
pub async fn events(State(s): State<SharedState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle(s, socket))
}

async fn handle(s: SharedState, mut socket: WebSocket) {
    let authed = match tokio::time::timeout(Duration::from_secs(10), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<AuthMessage>(&text) {
            Ok(msg) if msg.kind == "auth" => authenticate(&s, &msg.token).await.is_ok(),
            _ => false,
        },
        _ => false,
    };
    if !authed {
        let _ = socket
            .send(Message::Text(r#"{"type":"error","code":"unauthorized"}"#.into()))
            .await;
        return;
    }
    if socket.send(Message::Text(r#"{"type":"ready"}"#.into())).await.is_err() {
        return;
    }

    let mut rx = s.events.subscribe();
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(event) => {
                    let Ok(text) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if socket.send(Message::Text(r#"{"type":"resync"}"#.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}
