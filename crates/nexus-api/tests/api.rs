//! End-to-end API tests against an in-memory database and a temp folder.

use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use nexus_api::{AppState, NodeInfo};
use nexus_common::events::EventBus;
use nexus_database::Database;
use nexus_indexer::Indexer;
use nexus_api::net::NetRuntime;
use nexus_devices::NodeIdentity;
use nexus_network::{P2pConfig, P2pNode};
use nexus_storage::StorageManager;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

const LOCAL_TOKEN: &str = "test-local-token";

struct Harness {
    app: Router,
    _data: tempfile::TempDir,
    storage: tempfile::TempDir,
}

async fn harness(peer: [u8; 4]) -> Harness {
    harness_with(peer, false).await
}

/// `setup_needs_token`: as on a phone, where any app can reach the port.
async fn harness_with(peer: [u8; 4], setup_needs_token: bool) -> Harness {
    let data = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let db = Database::open_in_memory().await.unwrap();
    let device = db.ensure_local_device("test-node", "windows").await.unwrap();
    let events = EventBus::default();
    let indexer = Indexer::new(db.clone(), events.clone(), vec![data.path().to_path_buf()]);
    let identity = Arc::new(NodeIdentity::generate());
    let p2p = Arc::new(P2pNode::start(identity.secret_bytes(), P2pConfig::loopback()).await.unwrap());
    let state = Arc::new(AppState::new(
        db.clone(),
        StorageManager::new(db.clone(), device.id.clone(), vec![data.path().to_path_buf()]),
        indexer,
        events,
        blake3::hash(LOCAL_TOKEN.as_bytes()),
        NodeInfo { device_id: device.id, version: "test", hostname: "test".into() },
        data.path().to_path_buf(),
        identity,
        NetRuntime::new(p2p, true),
    ).with_setup_token_required(setup_needs_token));
    let app = nexus_api::router(state, None).layer(MockConnectInfo(SocketAddr::from((peer, 50000))));
    Harness { app, _data: data, storage }
}

async fn call(app: &Router, method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri).header(header::HOST, "127.0.0.1:47100");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
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

async fn upload(app: &Router, token: &str, root: &str, path: &str, name: &str, data: &[u8]) -> (StatusCode, Value) {
    let uri = format!("/api/v1/files/upload?root_id={root}&path={path}&name={name}");
    let req = Request::post(uri)
        .header(header::HOST, "127.0.0.1:47100")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_LENGTH, data.len())
        .body(Body::from(data.to_vec()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn wait_for_scan(app: &Router, token: &str, root: &str) {
    for _ in 0..100 {
        let (_, v) = call(app, Method::GET, &format!("/api/v1/storage/roots/{root}/stats"), Some(token), None).await;
        if v["scanning"] == false && v["scan"]["status"] == "idle" {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("scan did not finish");
}

#[tokio::test]
async fn full_local_file_lifecycle() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    std::fs::create_dir(h.storage.path().join("docs")).unwrap();
    std::fs::write(h.storage.path().join("docs").join("report.txt"), b"quarterly numbers").unwrap();
    std::fs::write(h.storage.path().join("photo.jpg"), [0xFF, 0xD8, 0xFF]).unwrap();

    // Unauthenticated requests are refused.
    let (s, _) = call(app, Method::GET, "/api/v1/storage/roots", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // First-run setup.
    let (s, v) = call(app, Method::GET, "/api/v1/setup/status", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["needs_setup"], true);
    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "short"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "X", "username": "intruder", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "setup must only run once");

    // Login with the password; wrong password fails.
    let (s, _) = call(app, Method::POST, "/api/v1/auth/login", None, Some(json!({"username": "owner", "password": "nope"}))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = call(app, Method::POST, "/api/v1/auth/login", None, Some(json!({"username": "OWNER", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let token = v["token"].as_str().unwrap().to_string();
    let t = token.as_str();

    // The desktop local token also works.
    let (s, v) = call(app, Method::GET, "/api/v1/auth/me", Some(LOCAL_TOKEN), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["username"], "owner");

    // Add a storage root and wait for indexing.
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(t),
        Some(json!({"path": h.storage.path().to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, t, &root).await;

    // Overlapping roots are rejected.
    let (s, _) = call(app, Method::POST, "/api/v1/storage/roots", Some(t),
        Some(json!({"path": h.storage.path().join("docs").to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Browse.
    let (s, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}"), Some(t), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["total"], 2);
    assert_eq!(v["items"][0]["filename"], "docs", "folders sort first");
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}&path=docs"), Some(t), None).await;
    let report_id = v["items"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(v["items"][0]["mime_type"], "text/plain");

    // Traversal is rejected.
    let (s, _) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}&path=..%2F.."), Some(t), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = upload(app, t, &root, "..", "evil.txt", b"x").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = upload(app, t, &root, "", "..%5Cevil.txt", b"x").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Search is case-insensitive.
    let (_, v) = call(app, Method::GET, "/api/v1/search?q=REPORT", Some(t), None).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1);

    // Upload, then upload again with the same name: never overwrites.
    let (s, v) = upload(app, t, &root, "docs", "notes.md", b"# hello").await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["content_hash"], blake3::hash(b"# hello").to_hex().as_str());
    let (_, v) = upload(app, t, &root, "docs", "notes.md", b"# second").await;
    assert_eq!(v["filename"], "notes (1).md");
    assert_eq!(std::fs::read(h.storage.path().join("docs").join("notes.md")).unwrap(), b"# hello");

    // Download through a signed ticket, with range support.
    let (_, v) = call(app, Method::POST, &format!("/api/v1/files/{report_id}/link"), Some(t), None).await;
    let url = v["url"].as_str().unwrap().to_string();
    let res = app.clone().oneshot(
        Request::get(&url).header(header::HOST, "127.0.0.1:47100").header(header::RANGE, "bytes=0-8").body(Body::empty()).unwrap(),
    ).await.unwrap();
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert!(res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap().starts_with("sandbox"));
    assert_eq!(res.into_body().collect().await.unwrap().to_bytes().as_ref(), b"quarterly");
    let tampered = url.replace("ticket=", "ticket=1");
    let res = app.clone().oneshot(Request::get(&tampered).header(header::HOST, "127.0.0.1:47100").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Rename keeps the id; moving a folder carries its children.
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/files/{report_id}"), Some(t), Some(json!({"name": "report-2025.txt"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["id"], report_id.as_str());
    assert!(h.storage.path().join("docs").join("report-2025.txt").exists());
    let (_, v) = call(app, Method::POST, "/api/v1/files/folder", Some(t), Some(json!({"root_id": root, "name": "archive"}))).await;
    assert_eq!(v["relative_path"], "archive");
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}"), Some(t), None).await;
    let docs_id = v["items"].as_array().unwrap().iter().find(|i| i["filename"] == "docs").unwrap()["id"].as_str().unwrap().to_string();
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/files/{docs_id}"), Some(t), Some(json!({"parent_path": "archive"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files/{report_id}"), Some(t), None).await;
    assert_eq!(v["relative_path"], "archive/docs/report-2025.txt");
    let (s, _) = call(app, Method::PATCH, &format!("/api/v1/files/{docs_id}"), Some(t), Some(json!({"parent_path": "archive/docs"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "cannot move a folder into itself");

    // Delete moves to trash; restore brings it back with the same id.
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/files/{report_id}"), Some(t), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!h.storage.path().join("archive/docs/report-2025.txt").exists());
    let (_, v) = call(app, Method::GET, "/api/v1/trash", Some(t), None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    let (s, v) = call(app, Method::POST, &format!("/api/v1/files/{report_id}/restore"), Some(t), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["id"], report_id.as_str());
    assert_eq!(std::fs::read(h.storage.path().join("archive/docs/report-2025.txt")).unwrap(), b"quarterly numbers");

    // Removing a root never deletes files.
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/storage/roots/{root}"), Some(t), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(h.storage.path().join("photo.jpg").exists());

    // Logout revokes the session immediately.
    let (s, _) = call(app, Method::POST, "/api/v1/auth/logout", Some(t), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(app, Method::GET, "/api/v1/auth/me", Some(t), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn remote_peers_cannot_run_setup_or_add_roots() {
    let h = harness([100, 64, 0, 7]).await;
    let (s, _) = call(&h.app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&h.app, Method::POST, "/api/v1/storage/roots", Some(LOCAL_TOKEN),
        Some(json!({"path": h.storage.path().to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn on_phones_only_the_app_can_run_setup() {
    let h = harness_with([127, 0, 0, 1], true).await;
    let body = json!({"node_name": "Phone", "username": "owner", "password": "correct horse"});
    // Another app on the phone reaches the port but has no token.
    let (s, _) = call(&h.app, Method::POST, "/api/v1/setup", None, Some(body.clone())).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&h.app, Method::POST, "/api/v1/setup", Some("guessed"), Some(body.clone())).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(&h.app, Method::POST, "/api/v1/setup", Some(LOCAL_TOKEN), Some(body)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn web_pages_cannot_open_the_event_stream() {
    let h = harness([127, 0, 0, 1]).await;
    let upgrade = |origin: Option<&str>| {
        let mut req = Request::get("/api/v1/events")
            .header(header::HOST, "127.0.0.1:47100")
            .header(header::CONNECTION, "upgrade")
            .header(header::UPGRADE, "websocket")
            .header(header::SEC_WEBSOCKET_VERSION, "13")
            .header(header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(o) = origin {
            req = req.header(header::ORIGIN, o);
        }
        req.body(Body::empty()).unwrap()
    };
    let res = h.app.clone().oneshot(upgrade(Some("https://evil.example"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = h.app.clone().oneshot(upgrade(Some("http://localhost:8080"))).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    // The app and the interface this node serves get past the origin check.
    for origin in ["http://tauri.localhost", "http://127.0.0.1:47100"] {
        let res = h.app.clone().oneshot(upgrade(Some(origin))).await.unwrap();
        assert_ne!(res.status(), StatusCode::FORBIDDEN, "{origin}");
    }
}

#[tokio::test]
async fn the_web_interface_has_a_content_security_policy() {
    let h = harness([127, 0, 0, 1]).await;
    let res = h.app.clone().oneshot(Request::get("/api/v1/health").header(header::HOST, "127.0.0.1:47100").body(Body::empty()).unwrap()).await.unwrap();
    let csp = res.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
}

#[tokio::test]
async fn rejects_foreign_host_headers() {
    let h = harness([127, 0, 0, 1]).await;
    let req = Request::get("/api/v1/health").header(header::HOST, "evil.example:47100").body(Body::empty()).unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::MISDIRECTED_REQUEST);
}

#[cfg(windows)]
#[tokio::test]
async fn refuses_system_locations() {
    let h = harness([127, 0, 0, 1]).await;
    let (s, _) = call(&h.app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    for path in ["C:\\", "C:\\Windows\\System32"] {
        let (s, v) = call(&h.app, Method::POST, "/api/v1/storage/roots", Some(LOCAL_TOKEN), Some(json!({"path": path}))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}: {v}");
    }
}

#[tokio::test]
async fn unknown_api_addresses_answer_with_an_error_not_the_web_page() {
    let h = harness([127, 0, 0, 1]).await;
    let (s, v) = call(&h.app, Method::GET, "/api/v1/no-such-thing", Some(LOCAL_TOKEN), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "unknown_route");
}
