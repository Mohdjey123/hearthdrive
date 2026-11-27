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
    state: nexus_api::SharedState,
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
    let app = nexus_api::router(state.clone(), None).layer(MockConnectInfo(SocketAddr::from((peer, 50000))));
    Harness { app, state, _data: data, storage }
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

/// Signs someone in and returns their session token.
async fn sign_in(app: &Router, username: &str, password: &str) -> String {
    let (s, v) = call(app, Method::POST, "/api/v1/auth/login", None, Some(json!({"username": username, "password": password}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["token"].as_str().unwrap().to_string()
}

/// Adds a person limited to no storage location and signs them in.
async fn limited_person(app: &Router, owner: &str, username: &str, role: &str) -> (String, String) {
    let password = format!("{username} password 1");
    let (s, v) = call(app, Method::POST, "/api/v1/users", Some(owner), Some(json!({"username": username, "password": password, "role": role}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/users/{id}"), Some(owner), Some(json!({"allowed_roots": []}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    (id, sign_in(app, username, &password).await)
}

fn names(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v["items"].as_array().unwrap().iter().map(|i| i["filename"].as_str().unwrap().to_string()).collect();
    out.sort();
    out
}

#[tokio::test]
async fn shared_folders_drop_folders_and_comments() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let base = h.storage.path();
    std::fs::create_dir_all(base.join("Family").join("Inbox")).unwrap();
    std::fs::create_dir(base.join("Private")).unwrap();
    std::fs::write(base.join("Family").join("photo.jpg"), [0xFF, 0xD8, 0xFF]).unwrap();
    std::fs::write(base.join("Private").join("secret.txt"), b"keep out").unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": base.to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, &owner, &root).await;

    let (anna_id, anna) = limited_person(app, &owner, "anna", "editor").await;
    let (bob_id, bob) = limited_person(app, &owner, "bob", "viewer").await;
    let (carl_id, carl) = limited_person(app, &owner, "carl", "contributor").await;
    let list = |token: String, path: &'static str| {
        let root = root.clone();
        async move { call(app, Method::GET, &format!("/api/v1/files?root_id={root}&path={path}"), Some(&token), None).await }
    };

    // Nothing is shared yet: the storage location looks missing.
    assert_eq!(list(anna.clone(), "Family").await.0, StatusCode::NOT_FOUND);

    // Only owners and administrators share folders.
    let (s, _) = call(app, Method::POST, "/api/v1/shared-folders", Some(&anna), Some(json!({"root_id": root, "path": "Family"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(app, Method::POST, "/api/v1/shared-folders", Some(&owner), Some(json!({"root_id": root, "path": "Nope"}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{v}");
    let (s, v) = call(app, Method::POST, "/api/v1/shared-folders", Some(&owner), Some(json!({"root_id": root, "path": "Family",
        "members": [{"member": "user:nobody", "access": "view"}]}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");

    let (s, family) = call(app, Method::POST, "/api/v1/shared-folders", Some(&owner), Some(json!({"root_id": root, "path": "Family",
        "members": [{"member": format!("user:{anna_id}"), "access": "edit"}, {"member": format!("user:{bob_id}"), "access": "edit"}]}))).await;
    assert_eq!(s, StatusCode::CREATED, "{family}");
    assert_eq!(family["name"], "Family");
    let bob_view = family["members"].as_array().unwrap().iter().find(|m| m["name"] == "bob").unwrap();
    assert_eq!(bob_view["note"], "Their role only lets them view");
    let (s, v) = call(app, Method::POST, "/api/v1/shared-folders", Some(&owner), Some(json!({"root_id": root, "path": "family"}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, inbox) = call(app, Method::POST, "/api/v1/shared-folders", Some(&owner), Some(json!({"root_id": root, "path": "Family/Inbox",
        "name": "Drop box", "members": [{"member": format!("user:{carl_id}"), "access": "add"}]}))).await;
    assert_eq!(s, StatusCode::CREATED, "{inbox}");

    // Anna edits inside Family, and sees nothing else.
    let (s, v) = call(app, Method::GET, "/api/v1/shared-folders/mine", Some(&anna), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v.as_array().unwrap().len(), 1);
    assert_eq!(v[0]["access"], "edit");
    assert_eq!(v[0]["path"], "Family");
    let (s, v) = list(anna.clone(), "Family").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(names(&v), ["Inbox", "photo.jpg"]);
    let family_dir = v["items"].as_array().unwrap().iter().find(|i| i["filename"] == "Inbox").unwrap()["parent_path"].clone();
    assert_eq!(family_dir, "Family");
    assert_eq!(list(anna.clone(), "").await.0, StatusCode::NOT_FOUND);
    assert_eq!(list(anna.clone(), "Private").await.0, StatusCode::NOT_FOUND);
    let (s, new) = upload(app, &anna, &root, "Family", "notes.txt", b"hello").await;
    assert_eq!(s, StatusCode::CREATED, "{new}");
    let (s, _) = upload(app, &anna, &root, "Private", "x.txt", b"no").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, v) = call(app, Method::GET, "/api/v1/search?q=secret", Some(&anna), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["items"].as_array().unwrap().is_empty(), "{v}");
    let (_, v) = call(app, Method::GET, "/api/v1/search?q=photo", Some(&anna), None).await;
    assert_eq!(v["items"].as_array().unwrap().len(), 1, "{v}");

    // The shared folder itself stays put.
    let (_, root_list) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}"), Some(&owner), None).await;
    let family_id = root_list["items"].as_array().unwrap().iter().find(|i| i["filename"] == "Family").unwrap()["id"].as_str().unwrap().to_string();
    let (s, _) = call(app, Method::PATCH, &format!("/api/v1/files/{family_id}"), Some(&anna), Some(json!({"name": "Mine"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/files/{family_id}"), Some(&anna), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let notes_id = new["id"].as_str().unwrap().to_string();
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/files/{notes_id}"), Some(&anna), Some(json!({"parent_path": "Private"}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "moving out of the shared folder: {v}");
    let (s, v) = call(app, Method::DELETE, &format!("/api/v1/files/{notes_id}"), Some(&anna), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = call(app, Method::GET, "/api/v1/trash", Some(&anna), None).await;
    assert_eq!(v.as_array().unwrap().len(), 1, "her deleted file is in her trash: {v}");

    // Bob's role caps the share: he views.
    assert_eq!(list(bob.clone(), "Family").await.0, StatusCode::OK);
    let (s, _) = upload(app, &bob, &root, "Family", "b.txt", b"no").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, v) = call(app, Method::GET, "/api/v1/shared-folders/mine", Some(&bob), None).await;
    assert_eq!(v[0]["access"], "view");

    // Carl may only add files to the drop folder.
    let (s, v) = list(carl.clone(), "Family/Inbox").await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(list(carl.clone(), "Family").await.0, StatusCode::NOT_FOUND);
    let (s, first) = upload(app, &carl, &root, "Family/Inbox", "scan.pdf", b"one").await;
    assert_eq!(s, StatusCode::CREATED, "{first}");
    let replace = format!("/api/v1/files/upload?root_id={root}&path=Family/Inbox&name=scan.pdf&on_conflict=replace");
    let req = Request::post(replace)
        .header(header::HOST, "127.0.0.1:47100")
        .header(header::AUTHORIZATION, format!("Bearer {carl}"))
        .header(header::CONTENT_LENGTH, 3)
        .body(Body::from(b"two".to_vec()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let second: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(second["filename"], "scan (1).pdf", "a drop folder never replaces: {second}");
    let (s, _) = call(app, Method::GET, &format!("/api/v1/files/{}", first["id"].as_str().unwrap()), Some(&carl), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/files/{}", first["id"].as_str().unwrap()), Some(&carl), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(app, Method::POST, "/api/v1/files/folder", Some(&carl), Some(json!({"root_id": root, "path": "Family/Inbox", "name": "March"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let (_, v) = list(anna.clone(), "Family/Inbox").await;
    assert_eq!(names(&v), ["March", "scan (1).pdf", "scan.pdf"]);

    // Renaming a shared folder in HearthDrive keeps it shared.
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/files/{family_id}"), Some(&owner), Some(json!({"name": "Family-2026"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, v) = call(app, Method::GET, "/api/v1/shared-folders/mine", Some(&anna), None).await;
    assert_eq!(v[0]["path"], "Family-2026");
    assert_eq!(list(anna.clone(), "Family-2026").await.0, StatusCode::OK);
    let (_, v) = call(app, Method::GET, "/api/v1/shared-folders/mine", Some(&carl), None).await;
    assert_eq!(v[0]["path"], "Family-2026/Inbox");
    assert_eq!(v[0]["name"], "Drop box");

    // Comments: whoever may open a file.
    let (_, v) = list(anna.clone(), "Family-2026").await;
    let photo = v["items"].as_array().unwrap().iter().find(|i| i["filename"] == "photo.jpg").unwrap()["id"].as_str().unwrap().to_string();
    let comments = format!("/api/v1/files/{photo}/comments");
    let (s, _) = call(app, Method::POST, &comments, Some(&anna), Some(json!({"body": "  "}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, mine) = call(app, Method::POST, &comments, Some(&anna), Some(json!({"body": "Lovely light here"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{mine}");
    assert_eq!(mine["author"], "anna");
    let (s, theirs) = call(app, Method::POST, &comments, Some(&bob), Some(json!({"body": "Print this one"}))).await;
    assert_eq!(s, StatusCode::CREATED, "viewers comment too: {theirs}");
    let (s, v) = call(app, Method::GET, &comments, Some(&bob), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v.as_array().unwrap().len(), 2);
    assert_eq!(v[0]["body"], "Lovely light here");
    assert_eq!(v[0]["can_delete"], false);
    assert_eq!(v[1]["can_delete"], true);
    let (s, _) = call(app, Method::GET, &comments, Some(&carl), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(app, Method::DELETE, &format!("{comments}/{}", mine["id"].as_str().unwrap()), Some(&bob), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::DELETE, &format!("{comments}/{}", theirs["id"].as_str().unwrap()), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = call(app, Method::DELETE, &format!("{comments}/{}", mine["id"].as_str().unwrap()), Some(&anna), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Changing members, removing a person, and stopping sharing.
    let family_share = family["id"].as_str().unwrap();
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/shared-folders/{family_share}"), Some(&owner),
        Some(json!({"members": [{"member": format!("user:{anna_id}"), "access": "view"}]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["members"].as_array().unwrap().len(), 1);
    assert_eq!(list(bob.clone(), "Family-2026").await.0, StatusCode::NOT_FOUND);
    let (s, _) = upload(app, &anna, &root, "Family-2026", "late.txt", b"no").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/users/{carl_id}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, v) = call(app, Method::GET, "/api/v1/shared-folders", Some(&owner), None).await;
    let drop_box = v.as_array().unwrap().iter().find(|f| f["name"] == "Drop box").unwrap();
    assert!(drop_box["members"].as_array().unwrap().is_empty(), "{drop_box}");
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/shared-folders/{family_share}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(list(anna.clone(), "Family-2026").await.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rules_sort_files_once() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let base = h.storage.path();
    std::fs::create_dir(base.join("Downloads")).unwrap();
    std::fs::write(base.join("Downloads").join("beach.jpg"), [0xFF, 0xD8, 0xFF]).unwrap();
    std::fs::write(base.join("Downloads").join("notes.txt"), b"shopping list").unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": base.to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, &owner, &root).await;

    // A trash rule without conditions is refused.
    let (s, _) = call(app, Method::POST, "/api/v1/rules", Some(&owner), Some(json!({
        "name": "Empty", "trigger": "daily", "root_id": root, "path": "Downloads", "then": {"type": "trash"}
    }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let (s, v) = call(app, Method::POST, "/api/v1/rules", Some(&owner), Some(json!({
        "name": "Photos by year", "trigger": "added", "root_id": root, "path": "Downloads",
        "when": {"kinds": ["image"]}, "then": {"type": "move", "folder": "Photos/{year}"}
    }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let id = v["id"].as_str().unwrap().to_string();

    let (s, v) = call(app, Method::GET, &format!("/api/v1/rules/{id}/preview"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let items = v.as_array().unwrap();
    assert_eq!(items.len(), 1, "{v}");
    assert_eq!(items[0]["file"]["filename"], "beach.jpg");
    let target = items[0]["target"].as_str().unwrap().to_string();
    assert!(target.starts_with("Photos/20"), "{target}");

    let (s, v) = call(app, Method::POST, &format!("/api/v1/rules/{id}/run"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["done"], 1, "{v}");
    assert!(base.join(&target).join("beach.jpg").exists());
    assert!(base.join("Downloads").join("notes.txt").exists());
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}&path={target}"), Some(&owner), None).await;
    assert_eq!(names(&v), ["beach.jpg"]);

    // The same file is never dealt with twice.
    let (_, v) = call(app, Method::POST, &format!("/api/v1/rules/{id}/run"), Some(&owner), None).await;
    assert_eq!(v["matched"], 0, "{v}");
    let (_, list) = call(app, Method::GET, "/api/v1/rules", Some(&owner), None).await;
    assert_eq!(list[0]["files_done"], 1, "{list}");

    // Only owners and administrators manage rules.
    let (_, editor) = limited_person(app, &owner, "erin", "editor").await;
    let (s, _) = call(app, Method::GET, "/api/v1/rules", Some(&editor), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, v) = call(app, Method::GET, "/api/v1/storage/suggestions", Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v.is_array());

    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/rules/{id}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}

/// Sends a request to the address for programs.
async fn program_call(app: &Router, method: &str, uri: &str, headers: &[(&str, String)], body: Vec<u8>) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri).header(header::HOST, "127.0.0.1:47180");
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let res = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
    let status = res.status();
    (status, res.into_body().collect().await.unwrap().to_bytes().to_vec())
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hmac256(key: &[u8], data: &str) -> Vec<u8> {
    ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), data.as_bytes()).as_ref().to_vec()
}

/// An S3 request signed with Signature Version 4, as S3 tools send it.
async fn s3(app: &Router, access: &str, secret: &str, method: &str, path: &str, query: &str, body: &[u8]) -> (StatusCode, String) {
    let now = chrono::Utc::now();
    let date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let day = now.format("%Y%m%d").to_string();
    let payload = hexs(ring::digest::digest(&ring::digest::SHA256, body).as_ref());
    let host = "127.0.0.1:47180";
    let canonical = format!(
        "{method}\n{path}\n{query}\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{date}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload}"
    );
    let scope = format!("{day}/us-east-1/s3/aws4_request");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
        hexs(ring::digest::digest(&ring::digest::SHA256, canonical.as_bytes()).as_ref())
    );
    let mut key = hmac256(format!("AWS4{secret}").as_bytes(), &day);
    for p in ["us-east-1", "s3", "aws4_request"] {
        key = hmac256(&key, p);
    }
    let sig = hexs(&hmac256(&key, &to_sign));
    let auth = format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature={sig}");
    let uri = if query.is_empty() { path.to_string() } else { format!("{path}?{query}") };
    let (s, b) = program_call(app, method, &uri, &[
        ("authorization", auth),
        ("x-amz-date", date),
        ("x-amz-content-sha256", payload),
    ], body.to_vec()).await;
    (s, String::from_utf8_lossy(&b).into_owned())
}

#[tokio::test]
async fn programs_use_tokens_for_the_api_webdav_and_s3() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let base = h.storage.path();
    std::fs::create_dir(base.join("docs")).unwrap();
    std::fs::write(base.join("docs").join("a.txt"), b"alpha").unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": base.to_string_lossy(), "name": "Main Disk"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, &owner, &root).await;
    let bucket = nexus_api::programs::slug(v["name"].as_str().unwrap());

    let (s, v) = call(app, Method::POST, "/api/v1/tokens", Some(&owner), Some(json!({"name": "backup script"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let token = v["secret_token"].as_str().unwrap().to_string();
    let access = v["access_key"].as_str().unwrap().to_string();
    let secret = v["secret_key"].as_str().unwrap().to_string();
    assert!(v.get("secret").is_none(), "the sealed secret is never sent");

    // The token works on the API, but not to manage people or tokens.
    let (s, _) = call(app, Method::GET, "/api/v1/storage/roots", Some(&token), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(app, Method::GET, "/api/v1/users", Some(&token), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::POST, "/api/v1/tokens", Some(&token), Some(json!({"name": "more"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // The address for programs takes only these tokens.
    let programs = nexus_api::programs::router(h.state.clone());
    let p = &programs;
    let (s, _) = program_call(p, "GET", "/api/v1/storage/roots", &[("authorization", format!("Bearer {owner}"))], vec![]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = program_call(p, "GET", "/api/v1/storage/roots", &[("authorization", format!("Bearer {token}"))], vec![]).await;
    assert_eq!(s, StatusCode::OK);

    // WebDAV.
    use base64::Engine;
    let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("me:{token}")));
    let (s, _) = program_call(p, "PROPFIND", "/dav/", &[], vec![]).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, b) = program_call(p, "PROPFIND", "/dav/", &[("authorization", basic.clone()), ("depth", "1".into())], vec![]).await;
    assert_eq!(s, StatusCode::MULTI_STATUS);
    assert!(String::from_utf8_lossy(&b).contains(&format!("/dav/{bucket}/")));
    let (s, b) = program_call(p, "GET", &format!("/dav/{bucket}/docs/a.txt"), &[("authorization", basic.clone())], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b, b"alpha");
    let (s, _) = program_call(p, "PUT", &format!("/dav/{bucket}/docs/b.txt"), &[("authorization", basic.clone())], b"bravo".to_vec()).await;
    assert!(s.is_success(), "{s}");
    assert_eq!(std::fs::read(base.join("docs").join("b.txt")).unwrap(), b"bravo");
    let (s, _) = program_call(p, "PROPFIND", &format!("/dav/{bucket}/.nexus/"), &[("authorization", basic.clone())], vec![]).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, b) = program_call(p, "PROPFIND", &format!("/dav/{bucket}/"), &[("authorization", basic.clone()), ("depth", "1".into())], vec![]).await;
    assert_eq!(s, StatusCode::MULTI_STATUS);
    assert!(!String::from_utf8_lossy(&b).contains(".nexus"));

    // S3.
    let (s, b) = s3(p, &access, &secret, "GET", "/", "", b"").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(b.contains(&format!("<Name>{bucket}</Name>")), "{b}");
    let (s, b) = s3(p, &access, &secret, "PUT", &format!("/{bucket}/new/c.txt"), "", b"charlie").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(std::fs::read(base.join("new").join("c.txt")).unwrap(), b"charlie");
    let (s, b) = s3(p, &access, &secret, "GET", &format!("/{bucket}/new/c.txt"), "", b"").await;
    assert_eq!((s, b.as_str()), (StatusCode::OK, "charlie"));
    let (s, b) = s3(p, &access, &secret, "GET", &format!("/{bucket}"), "list-type=2&prefix=docs%2F", b"").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert!(b.contains("<Key>docs/a.txt</Key>") && b.contains("<Key>docs/b.txt</Key>"), "{b}");
    let (s, b) = s3(p, &access, &secret, "GET", &format!("/{bucket}"), "delimiter=%2F&list-type=2", b"").await;
    assert!(b.contains("<Prefix>docs/</Prefix>") && b.contains("<Prefix>new/</Prefix>"), "{s} {b}");
    let (s, _) = s3(p, &access, "wrong-secret-wrong-secret-wrong-secret!", "GET", "/", "", b"").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = s3(p, &access, &secret, "DELETE", &format!("/{bucket}/new/c.txt"), "", b"").await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(!base.join("new").join("c.txt").exists());

    // Multipart upload.
    let (s, b) = s3(p, &access, &secret, "POST", &format!("/{bucket}/big.bin"), "uploads=", b"").await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let upload = b.split("<UploadId>").nth(1).unwrap().split("</UploadId>").next().unwrap().to_string();
    for (n, part) in [(1, b"one-".as_slice()), (2, b"two".as_slice())] {
        let (s, b) = s3(p, &access, &secret, "PUT", &format!("/{bucket}/big.bin"), &format!("partNumber={n}&uploadId={upload}"), part).await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    let done = b"<CompleteMultipartUpload><Part><PartNumber>1</PartNumber></Part><Part><PartNumber>2</PartNumber></Part></CompleteMultipartUpload>";
    let (s, b) = s3(p, &access, &secret, "POST", &format!("/{bucket}/big.bin"), &format!("uploadId={upload}"), done).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(std::fs::read(base.join("big.bin")).unwrap(), b"one-two");

    // A read-only token cannot change anything.
    let (_, v) = call(app, Method::POST, "/api/v1/tokens", Some(&owner), Some(json!({"name": "reader", "read_only": true}))).await;
    let (ra, rs) = (v["access_key"].as_str().unwrap().to_string(), v["secret_key"].as_str().unwrap().to_string());
    let (s, _) = s3(p, &ra, &rs, "PUT", &format!("/{bucket}/nope.txt"), "", b"x").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = s3(p, &ra, &rs, "GET", &format!("/{bucket}/docs/a.txt"), "", b"").await;
    assert_eq!(s, StatusCode::OK);

    // Removed tokens stop working.
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/tokens/{access}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = s3(p, &access, &secret, "GET", "/", "", b"").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn groups_share_folders_with_everyone_in_them() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let base = h.storage.path();
    std::fs::create_dir(base.join("Family")).unwrap();
    std::fs::write(base.join("Family").join("list.txt"), b"milk").unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": base.to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, &owner, &root).await;
    let (anna_id, anna) = limited_person(app, &owner, "anna", "editor").await;
    let (bob_id, bob) = limited_person(app, &owner, "bob", "editor").await;
    let list = |token: String| {
        let root = root.clone();
        async move { call(app, Method::GET, &format!("/api/v1/files?root_id={root}&path=Family"), Some(&token), None).await.0 }
    };

    // Only owners and administrators make groups, of people that exist.
    let (s, _) = call(app, Method::POST, "/api/v1/groups", Some(&anna), Some(json!({"name": "Family"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::POST, "/api/v1/groups", Some(&owner), Some(json!({"name": "Family", "members": ["nobody"]}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, group) = call(app, Method::POST, "/api/v1/groups", Some(&owner), Some(json!({"name": "Family", "members": [anna_id]}))).await;
    assert_eq!(s, StatusCode::CREATED, "{group}");
    let group_id = group["id"].as_str().unwrap().to_string();
    let (s, _) = call(app, Method::POST, "/api/v1/groups", Some(&owner), Some(json!({"name": "family"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Shared with the group: Anna (in it) gets the folder, Bob does not.
    let (s, folder) = call(app, Method::POST, "/api/v1/shared-folders", Some(&owner), Some(json!({"root_id": root, "path": "Family",
        "members": [{"member": format!("group:{group_id}"), "access": "edit"}]}))).await;
    assert_eq!(s, StatusCode::CREATED, "{folder}");
    assert_eq!(folder["members"][0]["kind"], "group");
    assert_eq!(folder["members"][0]["name"], "Family");
    assert_eq!(folder["members"][0]["note"], "anna");
    assert_eq!(list(anna.clone()).await, StatusCode::OK);
    assert_eq!(list(bob.clone()).await, StatusCode::NOT_FOUND);
    let (_, mine) = call(app, Method::GET, "/api/v1/shared-folders/mine", Some(&anna), None).await;
    assert_eq!(mine[0]["access"], "edit", "{mine}");

    // Shared with Anna directly too, to view only: the group's edit wins.
    let folder_id = folder["id"].as_str().unwrap().to_string();
    let (s, _) = call(app, Method::PATCH, &format!("/api/v1/shared-folders/{folder_id}"), Some(&owner), Some(json!({"members": [
        {"member": format!("group:{group_id}"), "access": "edit"}, {"member": format!("user:{anna_id}"), "access": "view"}]}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = upload(app, &anna, &root, "Family", "more.txt", b"eggs").await;
    assert_eq!(s, StatusCode::CREATED, "{v}");

    // Bob joins: he gets it at once. Anna leaves: only her own view is left.
    let (s, v) = call(app, Method::PATCH, &format!("/api/v1/groups/{group_id}"), Some(&owner), Some(json!({"members": [bob_id]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["folders"][0], "Family");
    assert_eq!(list(bob.clone()).await, StatusCode::OK);
    let (s, _) = upload(app, &anna, &root, "Family", "late.txt", b"x").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(list(anna.clone()).await, StatusCode::OK);

    // Removing the group takes it off the folder.
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/groups/{group_id}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(list(bob).await, StatusCode::NOT_FOUND);
    let (_, v) = call(app, Method::GET, "/api/v1/shared-folders", Some(&owner), None).await;
    assert_eq!(v[0]["members"].as_array().unwrap().len(), 1, "{v}");
}

#[tokio::test]
async fn upload_links_take_files_from_anyone_with_the_link() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let base = h.storage.path();
    std::fs::create_dir(base.join("Inbox")).unwrap();
    std::fs::write(base.join("Inbox").join("photo.jpg"), b"old").unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": base.to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, &owner, &root).await;
    let (_, viewer) = limited_person(app, &owner, "vic", "viewer").await;

    // Viewers cannot ask for files; a folder must exist.
    let (s, _) = call(app, Method::POST, "/api/v1/upload-links", Some(&viewer), Some(json!({"root_id": root, "path": "Inbox"}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::POST, "/api/v1/upload-links", Some(&owner), Some(json!({"root_id": root, "path": "Nope"}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, link) = call(app, Method::POST, "/api/v1/upload-links", Some(&owner), Some(json!({
        "root_id": root, "path": "Inbox", "title": "Holiday <photos>", "message": "Thanks!", "max_mb": 1}))).await;
    assert_eq!(s, StatusCode::CREATED, "{link}");
    assert!(link.get("secret").is_none(), "the sealed secret is never sent");
    assert_eq!(link["urls"].as_array().unwrap().len(), 0, "the address for programs is off");
    let page_path = link["link_path"].as_str().unwrap().to_string();

    let programs = nexus_api::programs::router(h.state.clone());
    let p = &programs;

    // The page, escaped, with a strict policy; a wrong secret gets nothing.
    let (s, html) = program_call(p, "GET", &page_path, &[], vec![]).await;
    assert_eq!(s, StatusCode::OK);
    let html = String::from_utf8(html).unwrap();
    assert!(html.contains("Holiday &lt;photos&gt;") && !html.contains("<photos>"), "{html}");
    assert!(html.contains("owner asks you to add files here"));
    let wrong = format!("{}x", page_path);
    let (s, _) = program_call(p, "GET", &wrong, &[], vec![]).await;
    assert_eq!(s, StatusCode::GONE);

    // Files arrive next to what is there, never replacing it.
    let up = |name: &str, body: Vec<u8>| {
        let uri = format!("{page_path}/files?name={name}&from=Grandma");
        async move {
            let len = body.len().to_string();
            program_call(p, "POST", &uri, &[("content-length", len)], body).await
        }
    };
    let (s, v) = up("photo.jpg", b"new picture".to_vec()).await;
    assert_eq!(s, StatusCode::OK, "{}", String::from_utf8_lossy(&v));
    assert_eq!(std::fs::read(base.join("Inbox").join("photo.jpg")).unwrap(), b"old");
    let added: serde_json::Value = serde_json::from_slice(&v).unwrap();
    let added_name = added["name"].as_str().unwrap();
    assert_ne!(added_name, "photo.jpg");
    assert_eq!(std::fs::read(base.join("Inbox").join(added_name)).unwrap(), b"new picture");
    let (s, _) = up("..%2Fescape.txt", b"x".to_vec()).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // The limit holds.
    let (s, _) = up("big.bin", vec![0u8; 1024 * 1024]).await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    let (_, v) = call(app, Method::GET, "/api/v1/upload-links", Some(&owner), None).await;
    assert_eq!(v[0]["received_files"], 1, "{v}");
    assert_eq!(v[0]["received_bytes"], 11, "{v}");

    // Taken down: the page and uploads stop at once.
    let id = link["id"].as_str().unwrap();
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/upload-links/{id}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = program_call(p, "GET", &page_path, &[], vec![]).await;
    assert_eq!(s, StatusCode::GONE);
    let (s, _) = up("late.txt", b"late".to_vec()).await;
    assert_eq!(s, StatusCode::GONE);
}

#[tokio::test]
async fn plugin_store_installs_runs_and_keeps_to_what_was_allowed() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let base = h.storage.path();
    std::fs::create_dir(base.join("Docs")).unwrap();
    std::fs::write(base.join("Docs").join("a.txt"), b"alpha").unwrap();
    std::fs::write(base.join("Docs").join("b.txt"), b"beta").unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir(other.path().join("Elsewhere")).unwrap();
    std::fs::write(other.path().join("Elsewhere").join("c.txt"), b"gamma").unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": base.to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(&owner), Some(json!({"path": other.path().to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root_b = v["id"].as_str().unwrap().to_string();
    wait_for_scan(app, &owner, &root).await;
    wait_for_scan(app, &owner, &root_b).await;
    let folder_id = |v: &Value, name: &str| {
        v["items"].as_array().unwrap().iter().find(|i| i["filename"] == name).unwrap()["id"].as_str().unwrap().to_string()
    };
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}"), Some(&owner), None).await;
    let docs = folder_id(&v, "Docs");
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root_b}"), Some(&owner), None).await;
    let elsewhere = folder_id(&v, "Elsewhere");

    // The store lists its plugins, none installed.
    let (s, v) = call(app, Method::GET, "/api/v1/plugins", Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let checksums = v["store"].as_array().unwrap().iter().find(|p| p["id"] == "checksums").unwrap().clone();
    assert!(checksums["installed"].is_null());
    assert_eq!(checksums["needs"], json!(["read", "add"]));

    // Not installed: nothing to run.
    let run = "/api/v1/plugins/store/checksums/commands/write";
    let (s, _) = call(app, Method::POST, run, Some(&owner), Some(json!({"file_id": docs}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Installed for the first storage location only.
    let (s, v) = call(app, Method::POST, "/api/v1/plugins/store/checksums", Some(&owner), Some(json!({"roots": [root]}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["installed"]["enabled"], true);
    let (s, _) = call(app, Method::POST, "/api/v1/plugins/store/nonsense", Some(&owner), Some(json!({}))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let (_, v) = call(app, Method::GET, &format!("/api/v1/plugins/commands?file_id={docs}"), Some(&owner), None).await;
    assert!(v.as_array().unwrap().iter().any(|c| c["plugin"] == "checksums" && c["id"] == "write" && c["store"] == true), "{v}");
    let (_, v) = call(app, Method::GET, &format!("/api/v1/plugins/commands?file_id={elsewhere}"), Some(&owner), None).await;
    assert!(v.as_array().unwrap().is_empty(), "{v}");

    let (s, v) = call(app, Method::POST, run, Some(&owner), Some(json!({"file_id": docs}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["exit_code"], 0, "{v}");
    let made: Vec<_> = std::fs::read_dir(base.join("Docs")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(made.iter().any(|n| n.starts_with("Checksums ") && n.ends_with(".sha256")), "{made:?}");

    // Not in the storage location it was not given.
    let (s, _) = call(app, Method::POST, run, Some(&owner), Some(json!({"file_id": elsewhere}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(std::fs::read_dir(other.path().join("Elsewhere")).unwrap().count(), 1);

    // Settings are checked against what the plugin has.
    let change = "/api/v1/plugins/store/checksums";
    let (s, _) = call(app, Method::PATCH, change, Some(&owner), Some(json!({"settings": {"nonsense": true}}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(app, Method::PATCH, change, Some(&owner), Some(json!({"settings": {"subfolders": "yes"}}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(app, Method::PATCH, change, Some(&owner), Some(json!({"settings": {"subfolders": false}}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["installed"]["settings"]["subfolders"], false);

    // Turned off, it does not run.
    let (s, _) = call(app, Method::PATCH, change, Some(&owner), Some(json!({"enabled": false}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(app, Method::POST, run, Some(&owner), Some(json!({"file_id": docs}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Everywhere, and on again.
    let (s, v) = call(app, Method::PATCH, change, Some(&owner), Some(json!({"enabled": true, "roots": null}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["installed"]["roots"].is_null(), "{v}");

    // Administrators run plugins but do not install them; others do neither.
    let (s, v) = call(app, Method::POST, "/api/v1/users", Some(&owner), Some(json!({"username": "ada", "password": "ada password 1", "role": "administrator"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let admin = sign_in(app, "ada", "ada password 1").await;
    let (s, _) = call(app, Method::POST, "/api/v1/plugins/store/folder-list", Some(&admin), Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::PATCH, change, Some(&admin), Some(json!({"enabled": false}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, v) = call(app, Method::POST, run, Some(&admin), Some(json!({"file_id": elsewhere}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (_, editor) = limited_person(app, &owner, "erin", "editor").await;
    for (method, uri, body) in [
        (Method::GET, "/api/v1/plugins", None),
        (Method::POST, "/api/v1/plugins/store/folder-list", Some(json!({}))),
        (Method::POST, run, Some(json!({"file_id": docs}))),
    ] {
        let (s, _) = call(app, method, uri, Some(&editor), body).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}");
    }

    // Removed: gone from the menu; what it made stays.
    let (s, v) = call(app, Method::DELETE, change, Some(&owner), None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["installed"].is_null());
    let (_, v) = call(app, Method::GET, &format!("/api/v1/plugins/commands?file_id={docs}"), Some(&owner), None).await;
    assert!(v.as_array().unwrap().is_empty(), "{v}");
    assert!(std::fs::read_dir(base.join("Docs")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".sha256")));
}

#[tokio::test]
async fn the_owner_runs_commands_in_a_terminal_that_keeps_its_folder() {
    let h = harness([127, 0, 0, 1]).await;
    let app = &h.app;
    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({"node_name": "Desk", "username": "owner", "password": "correct horse"}))).await;
    assert_eq!(s, StatusCode::OK);
    let owner = sign_in(app, "owner", "correct horse").await;
    let place = tempfile::tempdir().unwrap();
    std::fs::create_dir(place.path().join("inner")).unwrap();

    let (s, v) = call(app, Method::POST, "/api/v1/terminal/sessions", Some(&owner), Some(json!({"cwd": place.path().to_string_lossy()}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();

    // Reads until the shell is idle again, gathering what it printed.
    async fn settle(app: &Router, token: &str, id: &str, after: &mut u64) -> (String, Value) {
        let mut text = String::new();
        for _ in 0..40 {
            let (s, v) = call(app, Method::GET, &format!("/api/v1/terminal/sessions/{id}?after={after}"), Some(token), None).await;
            assert_eq!(s, StatusCode::OK, "{v}");
            for c in v["chunks"].as_array().unwrap() {
                if c["kind"] == "out" || c["kind"] == "err" {
                    text.push_str(c["text"].as_str().unwrap());
                }
            }
            *after = v["next"].as_u64().unwrap();
            if !v["busy"].as_bool().unwrap() {
                return (text, v);
            }
        }
        panic!("the shell did not finish: {text}");
    }
    let mut after = 0;
    let (_, v) = settle(app, &owner, &id, &mut after).await;
    assert!(v["cwd"].as_str().unwrap().ends_with(place.path().file_name().unwrap().to_str().unwrap()), "{v}");

    let (s, _) = call(app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/input"), Some(&owner), Some(json!({"text": "cd inner"}))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, v) = settle(app, &owner, &id, &mut after).await;
    assert!(v["cwd"].as_str().unwrap().ends_with("inner"), "the folder carries over: {v}");
    call(app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/input"), Some(&owner), Some(json!({"text": "echo hello-terminal"}))).await;
    let (text, _) = settle(app, &owner, &id, &mut after).await;
    assert!(text.contains("hello-terminal"), "{text}");
    assert!(!text.contains("__hearthdrive_"), "markers stay hidden: {text}");

    // Someone else's session is not theirs to read; another person cannot open one.
    let (_, adm) = call(app, Method::POST, "/api/v1/users", Some(&owner),
        Some(json!({"username": "ada", "password": "ada password 1", "role": "administrator"}))).await;
    assert!(adm.is_object());
    let ada = sign_in(app, "ada", "ada password 1").await;
    let (s, _) = call(app, Method::POST, "/api/v1/terminal/sessions", Some(&ada), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::GET, &format!("/api/v1/terminal/sessions/{id}"), Some(&ada), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, _) = call(app, Method::POST, &format!("/api/v1/terminal/sessions/{id}/stop"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let mut closed = Value::Null;
    for _ in 0..10 {
        let (_, v) = call(app, Method::GET, &format!("/api/v1/terminal/sessions/{id}?after={after}"), Some(&owner), None).await;
        if !v["closed"].is_null() {
            closed = v;
            break;
        }
    }
    assert!(!closed.is_null(), "stopping ends the shell");
    assert_eq!(closed["closed"], "The terminal was stopped.");
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/terminal/sessions/{id}"), Some(&owner), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
}
