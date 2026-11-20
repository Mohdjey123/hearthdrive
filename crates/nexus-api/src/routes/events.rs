use crate::auth::{AuthUser, Need, authenticate};
use crate::state::SharedState;
use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
struct AuthMessage {
    #[serde(rename = "type")]
    kind: String,
    token: String,
}

/// Whether the app's window is showing: `{"type":"window","visible":true}`.
#[derive(Deserialize)]
struct WindowMessage {
    #[serde(rename = "type")]
    kind: String,
    visible: bool,
}

/// Live events. Browsers cannot set headers on WebSocket connections, so the
/// client authenticates with its first message: `{"type":"auth","token":"…"}`.
///
/// WebSockets are not covered by CORS, so the origin is checked here: only
/// the app, the web interface this node serves, and programs that send no
/// origin may connect. Other web pages are turned away before any message.
pub async fn events(
    State(s): State<SharedState>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if !origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "Origin not allowed").into_response();
    }
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| handle(s, socket)),
        Err(rejection) => rejection.into_response(),
    }
}

fn origin_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else { return true };
    let Ok(origin) = origin.to_str() else { return false };
    if crate::allowed_origins().any(|o| o == origin) {
        return true;
    }
    // The web interface served by this node: same scheme, host and port.
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    !host.is_empty() && origin == format!("http://{host}")
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
    // An open app window: updates wait until it is closed. The desktop app
    // says when its window is hidden in the tray (`{"type":"window",
    // "visible":false}`); a hidden window does not hold updates back.
    let mut viewer = Some(s.updates.viewer());
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
                        && !u.sees_root(root)
                        && !event.paths().iter().any(|p| u.may(root, p, Need::Read)) {
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
                Some(Ok(Message::Text(text))) => {
                    if let Ok(WindowMessage { kind, visible }) = serde_json::from_str(&text)
                        && kind == "window"
                    {
                        match (visible, viewer.is_some()) {
                            (true, false) => {
                                viewer = Some(s.updates.viewer());
                                tracing::info!("app window shown: updates wait");
                            }
                            (false, true) => {
                                viewer = None;
                                tracing::info!("app window hidden in the tray");
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            },
        }
    }
}
