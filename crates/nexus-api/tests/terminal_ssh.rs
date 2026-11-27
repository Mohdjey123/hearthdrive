//! The Termux terminal against a small SSH server that plays Termux's shell:
//! it lets in only HearthDrive's key, answers the lines HearthDrive sends
//! and reports errors on the error stream.

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use nexus_api::net::NetRuntime;
use nexus_api::{AppState, NodeInfo};
use nexus_common::events::EventBus;
use nexus_database::Database;
use nexus_devices::NodeIdentity;
use nexus_indexer::Indexer;
use nexus_network::{P2pConfig, P2pNode};
use nexus_storage::StorageManager;
use russh::keys::ssh_key::private::Ed25519Keypair;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

async fn app() -> (Router, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let db = Database::open_in_memory().await.unwrap();
    let device = db.ensure_local_device("phone", "android").await.unwrap();
    let events = EventBus::default();
    let indexer = Indexer::new(db.clone(), events.clone(), vec![data.path().to_path_buf()]);
    let identity = Arc::new(NodeIdentity::generate());
    let p2p = Arc::new(P2pNode::start(identity.secret_bytes(), P2pConfig::loopback()).await.unwrap());
    let state = Arc::new(AppState::new(
        db.clone(),
        StorageManager::new(db.clone(), device.id.clone(), vec![data.path().to_path_buf()]),
        indexer,
        events,
        blake3::hash(b"local"),
        NodeInfo { device_id: device.id, version: "test", hostname: "test".into() },
        data.path().to_path_buf(),
        identity,
        NetRuntime::new(p2p, true),
    ));
    let app = nexus_api::router(state, None).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50000))));
    (app, data)
}

async fn call(app: &Router, method: Method, uri: &str, token: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:47100")
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let req = match body {
        Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Plays Termux's shell, one connection per handler.
#[derive(Clone)]
struct FakeTermux {
    allowed: PublicKey,
    cwd: String,
    pending: String,
}

impl russh::server::Server for FakeTermux {
    type Handler = Self;
    fn new_client(&mut self, _: Option<SocketAddr>) -> Self {
        self.clone()
    }
}

impl russh::server::Handler for FakeTermux {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, _: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if key.public_key.key_data() == self.allowed.key_data() { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(&mut self, _: Channel<Msg>, reply: russh::server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        assert_eq!(data, b"uname -sm");
        session.channel_success(channel)?;
        session.data(channel, b"Linux aarch64\n".to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        self.pending.push_str(&String::from_utf8_lossy(data));
        while let Some(end) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=end).collect();
            let line = line.trim_end();
            if let Some(dir) = line.strip_prefix("cd ").map(|r| r.trim_end_matches(" 2>/dev/null")) {
                self.cwd = if dir == "~" { "/data/data/com.termux/files/home".into() } else { dir.trim_matches('\'').to_string() };
            } else if let Some(said) = line.strip_prefix("echo \"").and_then(|r| r.strip_suffix('"')) {
                session.data(channel, format!("{}\n", said.replace("$PWD", &self.cwd)).into_bytes())?;
            } else if let Some(said) = line.strip_prefix("say ") {
                session.data(channel, format!("{said}\n").into_bytes())?;
            } else if line == "exit" {
                session.exit_status_request(channel, 0)?;
                session.eof(channel)?;
                session.close(channel)?;
            } else {
                session.extended_data(channel, 1, format!("{line}: not found\n").into_bytes())?;
            }
        }
        Ok(())
    }
}

/// Starts a fake Termux on a free port with its own host key.
async fn termux(allowed: PublicKey, host_seed: u8) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let config = Arc::new(russh::server::Config {
        keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&[host_seed; 32]))],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });
    let mut server = FakeTermux { allowed, cwd: "/".into(), pending: String::new() };
    let task = tokio::spawn(async move {
        let _ = server.run_on_socket(config, &listener).await;
    });
    (port, task)
}

/// Reads until the shell is idle again, gathering output and errors.
async fn settle(app: &Router, token: &str, id: &str, after: &mut u64) -> (String, String, Value) {
    let (mut out, mut err) = (String::new(), String::new());
    for _ in 0..20 {
        let (s, v) = call(app, Method::GET, &format!("/api/v1/terminal/sessions/{id}?after={after}"), token, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        for c in v["chunks"].as_array().unwrap() {
            match c["kind"].as_str().unwrap() {
                "out" => out.push_str(c["text"].as_str().unwrap()),
                "err" => err.push_str(c["text"].as_str().unwrap()),
                _ => {}
            }
        }
        *after = v["next"].as_u64().unwrap();
        if !v["busy"].as_bool().unwrap() {
            return (out, err, v);
        }
    }
    panic!("the shell did not finish: {out} {err}");
}

#[tokio::test]
async fn a_phone_terminal_can_be_termux_over_ssh() {
    let (app, _data) = app().await;
    let (s, _) = call(&app, Method::POST, "/api/v1/setup", "local",
        Some(json!({"node_name": "Pixel", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = call(&app, Method::POST, "/api/v1/auth/login", "", Some(json!({"username": "owner", "password": "correct horse"}))).await;
    let owner = v["token"].as_str().unwrap().to_string();

    // HearthDrive's key, to put in Termux.
    let (s, info) = call(&app, Method::GET, "/api/v1/terminal", &owner, None).await;
    assert_eq!(s, StatusCode::OK, "{info}");
    assert_eq!(info["ssh"]["on"], false);
    let line = info["ssh"]["public_key"].as_str().unwrap();
    assert!(line.starts_with("ssh-ed25519 ") && line.ends_with(" hearthdrive"), "{line}");
    let key = PublicKey::from_openssh(line).unwrap();

    // Nothing listening yet: a clear message.
    let closed_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    call(&app, Method::PUT, "/api/v1/terminal/settings", &owner, Some(json!({"ssh": {"on": true, "port": closed_port}}))).await;
    let (s, v) = call(&app, Method::POST, "/api/v1/terminal/settings/test", &owner, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"]["message"].as_str().unwrap().contains("run sshd"), "{v}");

    // A server that does not know HearthDrive's key.
    let stranger = PrivateKey::from(Ed25519Keypair::from_seed(&[9; 32])).public_key().clone();
    let (port, other) = termux(stranger, 1).await;
    call(&app, Method::PUT, "/api/v1/terminal/settings", &owner, Some(json!({"ssh": {"port": port}}))).await;
    let (s, v) = call(&app, Method::POST, "/api/v1/terminal/settings/test", &owner, None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"]["message"].as_str().unwrap().contains("did not accept"), "{v}");
    other.abort();

    let (port, server) = termux(key.clone(), 2).await;
    let (s, _) = call(&app, Method::PUT, "/api/v1/terminal/settings", &owner, Some(json!({"ssh": {"port": port}}))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, v) = call(&app, Method::POST, "/api/v1/terminal/settings/test", &owner, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["message"], "Connected to Linux (SSH) (Linux aarch64)");
    let (_, info) = call(&app, Method::GET, "/api/v1/terminal", &owner, None).await;
    assert_eq!(info["ssh"]["trusted"], true, "the server's key is remembered");
    assert_eq!(info["shell"], "Linux (SSH)");

    let (s, v) = call(&app, Method::POST, "/api/v1/terminal/sessions", &owner, Some(json!({}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let mut after = 0;
    let (_, _, v) = settle(&app, &owner, &id, &mut after).await;
    assert_eq!(v["cwd"], "/data/data/com.termux/files/home");

    call(&app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/input"), &owner, Some(json!({"text": "cd /sdcard"}))).await;
    let (_, _, v) = settle(&app, &owner, &id, &mut after).await;
    assert_eq!(v["cwd"], "/sdcard");
    call(&app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/input"), &owner, Some(json!({"text": "say hello from termux"}))).await;
    let (out, err, _) = settle(&app, &owner, &id, &mut after).await;
    assert_eq!(out, "hello from termux\n");
    assert!(err.is_empty() && !out.contains("__hearthdrive_"));
    call(&app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/input"), &owner, Some(json!({"text": "nonsense"}))).await;
    let (_, err, _) = settle(&app, &owner, &id, &mut after).await;
    assert_eq!(err, "nonsense: not found\n");

    // `exit` ends it like a shell does.
    call(&app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/input"), &owner, Some(json!({"text": "exit"}))).await;
    let mut ended = Value::Null;
    for _ in 0..10 {
        let (_, v) = call(&app, Method::GET, &format!("/api/v1/terminal/sessions/{id}?after={after}"), &owner, None).await;
        if !v["closed"].is_null() {
            ended = v;
            break;
        }
    }
    assert_eq!(ended["closed"], "The terminal ended.");

    // Stop hangs up.
    let (_, v) = call(&app, Method::POST, "/api/v1/terminal/sessions", &owner, Some(json!({"cwd": "/sdcard/Download"}))).await;
    let id = v["id"].as_str().unwrap().to_string();
    let mut after = 0;
    let (_, _, v) = settle(&app, &owner, &id, &mut after).await;
    assert_eq!(v["cwd"], "/sdcard/Download", "Start again opens where it was");
    call(&app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/stop"), &owner, None).await;
    let mut ended = Value::Null;
    for _ in 0..10 {
        let (_, v) = call(&app, Method::GET, &format!("/api/v1/terminal/sessions/{id}?after={after}"), &owner, None).await;
        if !v["closed"].is_null() {
            ended = v;
            break;
        }
    }
    assert_eq!(ended["closed"], "The terminal was stopped.");
    server.abort();

    // Another server on the same port, with another key: refused until the
    // owner forgets the old one.
    let listener = loop {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => break l,
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    };
    let config = Arc::new(russh::server::Config {
        keys: vec![PrivateKey::from(Ed25519Keypair::from_seed(&[3; 32]))],
        auth_rejection_time: std::time::Duration::from_millis(10),
        auth_rejection_time_initial: Some(std::time::Duration::ZERO),
        ..Default::default()
    });
    let mut impostor = FakeTermux { allowed: key, cwd: "/".into(), pending: String::new() };
    let task = tokio::spawn(async move {
        let _ = impostor.run_on_socket(config, &listener).await;
    });
    let (s, v) = call(&app, Method::POST, "/api/v1/terminal/sessions", &owner, Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(v["error"]["message"].as_str().unwrap().contains("key has changed"), "{v}");
    call(&app, Method::PUT, "/api/v1/terminal/settings", &owner, Some(json!({"ssh": {"forget_host": true}}))).await;
    let (s, _) = call(&app, Method::POST, "/api/v1/terminal/settings/test", &owner, None).await;
    assert_eq!(s, StatusCode::OK);
    task.abort();
}
