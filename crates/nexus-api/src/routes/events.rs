use crate::auth::{AuthUser, authenticate};
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
    let token = match tokio::time::timeout(Duration::from_secs(10), socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<AuthMessage>(&text) {
            Ok(msg) if msg.kind == "auth" => Some(msg.token),
            _ => None,
        },
        _ => None,
    };
    let mut user: Option<AuthUser> = match &token {
        Some(t) => authenticate(&s, t).await.ok(),
        None => None,
    };
    if user.is_none() {
        let _ = socket
            .send(Message::Text(r#"{"type":"error","code":"unauthorized"}"#.into()))
            .await;
        return;
    }
    if socket.send(Message::Text(r#"{"type":"ready"}"#.into())).await.is_err() {
        return;
    }

    let token = token.unwrap_or_default();
    let mut rx = s.events.subscribe();
    // Signing out, a removed account or changed limits take effect here too.
    let mut recheck = tokio::time::interval(Duration::from_secs(30));
    recheck.tick().await;
    loop {
        tokio::select! {
            _ = recheck.tick() => {
                user = authenticate(&s, &token).await.ok();
                if user.is_none() {
                    let _ = socket.send(Message::Text(r#"{"type":"error","code":"unauthorized"}"#.into())).await;
                    break;
                }
            }
            event = rx.recv() => match event {
                Ok(event) => {
                    if let (Some(u), Some(root)) = (&user, event.root_id())
                        && !u.sees_root(root) {
                            continue;
                        }
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
