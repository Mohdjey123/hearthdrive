//! Share links end to end, against a stand-in Send service on this computer
//! that keeps what it receives, so the test can decrypt it like a recipient.

use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{Path, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::Json;
use http_body_util::BodyExt;
use nexus_api::net::NetRuntime;
use nexus_api::share;
use nexus_api::{AppState, NodeInfo};
use nexus_common::events::EventBus;
use nexus_database::Database;
use nexus_devices::NodeIdentity;
use nexus_indexer::Indexer;
use nexus_network::{P2pConfig, P2pNode};
use nexus_storage::StorageManager;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// A stand-in Send service

#[derive(Default, Clone)]
struct Stored {
    meta: String,
    auth: String,
    dlimit: u64,
    data: Vec<u8>,
}

#[derive(Clone)]
struct Mock {
    base: String,
    files: Arc<Mutex<HashMap<String, Stored>>>,
}

const OWNER: &str = "owner-token";

async fn mock_ws(State(m): State<Mock>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |sock| mock_upload(m, sock))
}

async fn mock_upload(m: Mock, mut sock: WebSocket) {
    let Some(Ok(Message::Text(hello))) = sock.recv().await else { return };
    let hello: Value = serde_json::from_str(&hello).unwrap();
    let id = format!("f{}", m.files.lock().unwrap().len() + 1);
    let reply = json!({ "url": format!("{}/download/{id}/", m.base), "ownerToken": OWNER, "id": id });
    sock.send(Message::Text(reply.to_string().into())).await.unwrap();
    let mut data = Vec::new();
    while let Some(Ok(msg)) = sock.recv().await {
        if let Message::Binary(b) = msg {
            if b.len() == 1 && b[0] == 0 {
                break;
            }
            data.extend_from_slice(&b);
        }
    }
    let stored = Stored {
        meta: hello["fileMetadata"].as_str().unwrap().to_string(),
        auth: hello["authorization"].as_str().unwrap().trim_start_matches("send-v1 ").to_string(),
        dlimit: hello["dlimit"].as_u64().unwrap(),
        data,
    };
    m.files.lock().unwrap().insert(id, stored);
    sock.send(Message::Text(r#"{"ok":true}"#.into())).await.unwrap();
}

fn owned(m: &Mock, id: &str, body: &Value) -> Option<Stored> {
    (body["owner_token"] == OWNER).then(|| m.files.lock().unwrap().get(id).cloned()).flatten()
}

async fn mock_owner(State(m): State<Mock>, Path((what, id)): Path<(String, String)>, Json(body): Json<Value>) -> axum::response::Response {
    let Some(file) = owned(&m, &id, &body) else { return StatusCode::NOT_FOUND.into_response() };
    match what.as_str() {
        "info" => Json(json!({ "dlimit": file.dlimit, "dtotal": 0, "ttl": 3_000_000 })).into_response(),
        "delete" => {
            m.files.lock().unwrap().remove(&id);
            StatusCode::OK.into_response()
        }
        "password" => {
            m.files.lock().unwrap().get_mut(&id).unwrap().auth = body["auth"].as_str().unwrap().to_string();
            StatusCode::OK.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn mock_send() -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let m = Mock { base: format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()), files: Default::default() };
    let app = Router::new()
        .route(
            "/config",
            get(|| async {
                Json(json!({ "LIMITS": { "MAX_FILE_SIZE": 1_000_000, "MAX_DOWNLOADS": 20, "MAX_EXPIRE_SECONDS": 86400 } }))
            }),
        )
        .route("/api/ws", any(mock_ws))
        .route("/api/{what}/{id}", post(mock_owner))
        .with_state(m.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    m
}

// ---------------------------------------------------------------------------
// The node

struct Harness {
    app: Router,
    _data: tempfile::TempDir,
    storage: tempfile::TempDir,
}

async fn harness() -> Harness {
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
        blake3::hash(b"test-local-token"),
        NodeInfo { device_id: device.id, version: "test", hostname: "test".into() },
        data.path().to_path_buf(),
        identity,
        NetRuntime::new(p2p, true),
    ));
    let app = nexus_api::router(state, None).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50000))));
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

async fn login(app: &Router, username: &str, password: &str) -> String {
    let (s, v) = call(app, Method::POST, "/api/v1/auth/login", None, Some(json!({ "username": username, "password": password }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["token"].as_str().unwrap().to_string()
}

async fn wait_for(app: &Router, token: &str, id: &str) -> Value {
    for _ in 0..200 {
        let (_, list) = call(app, Method::GET, "/api/v1/shares", Some(token), None).await;
        let link = list.as_array().unwrap().iter().find(|l| l["id"] == id).cloned().unwrap();
        if link["status"] != "uploading" {
            return link;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the upload did not finish");
}

async fn item(app: &Router, token: &str, root: &str, path: &str, name: &str) -> String {
    let (_, v) = call(app, Method::GET, &format!("/api/v1/files?root_id={root}&path={path}"), Some(token), None).await;
    v["items"].as_array().unwrap().iter().find(|i| i["filename"] == name).unwrap()["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn links_are_encrypted_and_can_be_taken_down() {
    let send = mock_send().await;
    let h = harness().await;
    let app = &h.app;

    let content: Vec<u8> = (0..(share::CHUNK * 2 + 777)).map(|i| (i % 253) as u8).collect();
    std::fs::write(h.storage.path().join("report.bin"), &content).unwrap();
    std::fs::write(h.storage.path().join("empty.txt"), b"").unwrap();
    std::fs::create_dir(h.storage.path().join("pics")).unwrap();
    std::fs::write(h.storage.path().join("pics").join("a.txt"), b"first picture").unwrap();
    std::fs::create_dir(h.storage.path().join("pics").join("more")).unwrap();
    std::fs::write(h.storage.path().join("pics").join("more").join("b.jpg"), [0xFF, 0xD8, 0xFF]).unwrap();

    let (s, _) = call(app, Method::POST, "/api/v1/setup", None,
        Some(json!({ "node_name": "Desk", "username": "owner", "password": "correct horse" }))).await;
    assert_eq!(s, StatusCode::OK);
    let token = login(app, "owner", "correct horse").await;
    let t = token.as_str();
    let (s, v) = call(app, Method::POST, "/api/v1/storage/roots", Some(t), Some(json!({ "path": h.storage.path().to_string_lossy() }))).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        let (_, v) = call(app, Method::GET, &format!("/api/v1/storage/roots/{root}/stats"), Some(t), None).await;
        if v["scanning"] == false && v["scan"]["status"] == "idle" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let report = item(app, t, &root, "", "report.bin").await;
    let empty = item(app, t, &root, "", "empty.txt").await;
    let pics = item(app, t, &root, "", "pics").await;

    // Only Send services: an address that is not HTTPS is refused.
    let (s, _) = call(app, Method::PUT, "/api/v1/shares/services", Some(t), Some(json!({ "send_url": "http://example.com" }))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = call(app, Method::PUT, "/api/v1/shares/services", Some(t), Some(json!({ "send_url": send.base }))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["send"]["MAX_DOWNLOADS"], 20);

    // Requests that cannot work are refused before anything is uploaded.
    for body in [
        json!({ "file_id": report, "provider": "dropbox" }),
        json!({ "file_id": report, "provider": "tempsh", "password": "pw" }),
        json!({ "file_id": empty }),
    ] {
        let (s, v) = call(app, Method::POST, "/api/v1/shares", Some(t), Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    }
    assert!(send.files.lock().unwrap().is_empty());

    // A file with a password.
    let (s, v) = call(app, Method::POST, "/api/v1/shares", Some(t),
        Some(json!({ "file_id": report, "expires_secs": 3600, "downloads": 2, "password": "open sesame" }))).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["status"], "uploading");
    let link = wait_for(app, t, v["id"].as_str().unwrap()).await;
    assert_eq!(link["status"], "active", "{link}");
    assert!(link.get("owner_token").is_none() && link.get("remote_id").is_none(), "secrets stay on the node");
    let url = link["url"].as_str().unwrap();
    let (address, secret) = url.split_once('#').unwrap();
    assert!(address.starts_with(&send.base));
    let stored = send.files.lock().unwrap().get("f1").cloned().unwrap();
    assert_eq!(stored.dlimit, 2);
    // The service holds only encrypted data; the link's secret opens it.
    assert_ne!(&stored.data[..64], &content[..64]);
    assert_eq!(share::decrypt(secret, &stored.data).unwrap(), content);
    let meta = share::decrypt_metadata(secret, &share::b64_decode(&stored.meta)).unwrap();
    assert_eq!(meta["name"], "report.bin");
    assert_eq!(meta["size"], content.len());
    // With a password, downloading needs a key made from it, not the link alone.
    assert_eq!(stored.auth, share::b64(&share::password_auth_key("open sesame", url)));

    // A folder becomes one zip file.
    let (s, v) = call(app, Method::POST, "/api/v1/shares", Some(t), Some(json!({ "file_id": pics }))).await;
    assert_eq!(s, StatusCode::ACCEPTED, "{v}");
    let folder = wait_for(app, t, v["id"].as_str().unwrap()).await;
    assert_eq!(folder["status"], "active", "{folder}");
    assert_eq!(folder["name"], "pics.zip");
    let secret = folder["url"].as_str().unwrap().split_once('#').unwrap().1.to_string();
    let stored = send.files.lock().unwrap().get("f2").cloned().unwrap();
    let zip = share::decrypt(&secret, &stored.data).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
    let mut a = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("pics/a.txt").unwrap(), &mut a).unwrap();
    assert_eq!(a, "first picture");
    assert!(archive.by_name("pics/more/b.jpg").is_ok());

    // Checking with the service.
    let (s, list) = call(app, Method::POST, "/api/v1/shares/refresh", Some(t), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2);

    // Someone who may only look cannot make links or see them.
    let (s, _) = call(app, Method::POST, "/api/v1/users", Some(t),
        Some(json!({ "username": "guest", "password": "guest password", "role": "viewer" }))).await;
    assert_eq!(s, StatusCode::CREATED);
    let guest = login(app, "guest", "guest password").await;
    let (s, _) = call(app, Method::GET, "/api/v1/shares", Some(&guest), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(app, Method::POST, "/api/v1/shares", Some(&guest), Some(json!({ "file_id": report }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // An editor sees only their own links.
    let (s, _) = call(app, Method::POST, "/api/v1/users", Some(t),
        Some(json!({ "username": "editor", "password": "editor password", "role": "editor" }))).await;
    assert_eq!(s, StatusCode::CREATED);
    let editor = login(app, "editor", "editor password").await;
    let (_, list) = call(app, Method::GET, "/api/v1/shares", Some(&editor), None).await;
    assert_eq!(list, json!([]));
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/shares/{}", link["id"].as_str().unwrap()), Some(&editor), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(app, Method::PUT, "/api/v1/shares/services", Some(&editor), Some(json!({ "send_url": send.base }))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Taking a link down deletes the file from the service.
    let (s, _) = call(app, Method::DELETE, &format!("/api/v1/shares/{}", link["id"].as_str().unwrap()), Some(t), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(!send.files.lock().unwrap().contains_key("f1"));
    let (_, list) = call(app, Method::GET, "/api/v1/shares", Some(t), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}
