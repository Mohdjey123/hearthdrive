//! Two real nodes, each with its own peer-to-peer endpoint on loopback: pair
//! them, browse and upload through the proxy, check that only signed device
//! requests over the right connection are accepted, then revoke.

use nexus_api::net::NetRuntime;
use nexus_api::{AppState, NodeInfo, SharedState};
use nexus_common::events::EventBus;
use nexus_database::Database;
use nexus_devices::NodeIdentity;
use nexus_indexer::Indexer;
use nexus_api::peers::{Acting, Target};
use nexus_network::{P2pConfig, P2pNode};
use nexus_storage::StorageManager;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

struct Node {
    base: String,
    state: SharedState,
    token: String,
    storage: tempfile::TempDir,
    _data: tempfile::TempDir,
}

async fn node(name: &str) -> Node {
    let data = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let db = Database::open_in_memory().await.unwrap();
    let device = db.ensure_local_device(name, "windows").await.unwrap();
    let identity = Arc::new(NodeIdentity::generate());
    db.set_local_identity(&device.id, identity.public_key(), "test").await.unwrap();
    let p2p = Arc::new(P2pNode::start(identity.secret_bytes(), P2pConfig::loopback()).await.unwrap());
    let events = EventBus::default();
    let indexer = Indexer::new(db.clone(), events.clone(), vec![data.path().to_path_buf()]);
    let state: SharedState = Arc::new(AppState::new(
        db.clone(),
        StorageManager::new(db.clone(), device.id.clone(), vec![data.path().to_path_buf()]),
        indexer,
        events,
        blake3::hash(b"unused-local-token"),
        NodeInfo { device_id: device.id, version: "test", hostname: name.into() },
        data.path().to_path_buf(),
        identity,
        NetRuntime::new(p2p, true),
    ));
    let app = nexus_api::router(state.clone(), None);
    nexus_api::p2p::spawn_server(state.clone(), app.clone());
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap();
    });

    let base = format!("http://{addr}/api/v1");
    let res: Value = reqwest::Client::new()
        .post(format!("{base}/setup"))
        .json(&json!({"node_name": name, "username": "owner", "password": "correct horse"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = res["token"].as_str().expect("setup token").to_string();
    Node { base, state, token, storage, _data: data }
}

impl Node {
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = reqwest::Client::new().request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    fn device_id(&self) -> &str {
        &self.state.node.device_id
    }
}

use reqwest::Method;

#[tokio::test]
async fn two_nodes_pair_browse_and_revoke() {
    let a = node("Studio").await;
    let b = node("Laptop").await;

    // A shares some files.
    std::fs::write(a.storage.path().join("plan.txt"), b"ship phase two").unwrap();
    let (s, v) = a.call(Method::POST, "/storage/roots", Some(json!({"path": a.storage.path().to_string_lossy()}))).await;
    assert_eq!(s, 201, "{v}");
    let root = v["id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        let (_, v) = a.call(Method::GET, &format!("/storage/roots/{root}/stats"), None).await;
        if v["scanning"] == false && v["scan"]["status"] == "idle" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A creates an invitation; it carries A's key and its endpoint's
    // addresses, never a secret key.
    let (s, invite) = a.call(Method::POST, "/devices/pair", None).await;
    assert_eq!(s, 200, "{invite}");
    let link = invite["link"].as_str().unwrap().to_string();
    let parsed = nexus_devices::PairingInvite::parse(&link).unwrap();
    assert_eq!(parsed.public_key, a.state.identity.public_key());
    assert!(!parsed.addrs.is_empty());
    assert_eq!(invite["fingerprint"], a.state.identity.fingerprint());

    // A wrong code is refused.
    let mut forged = parsed.clone();
    forged.code = nexus_devices::random_code(16);
    let (s, _) = b.call(Method::POST, "/pairing/join", Some(json!({"link": forged.to_link()}))).await;
    assert_eq!(s, 403);

    // B joins with the real link and waits.
    let (s, join) = b.call(Method::POST, "/pairing/join", Some(json!({"link": link}))).await;
    assert_eq!(s, 200, "{join}");
    assert_eq!(join["status"], "waiting");
    assert_eq!(join["host_fingerprint"], a.state.identity.fingerprint());
    let join_id = join["id"].as_str().unwrap().to_string();

    // A sees the request with B's fingerprint, and approves it.
    let (_, requests) = a.call(Method::GET, "/devices/pair/requests", None).await;
    let requests = requests.as_array().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["name"], "Laptop");
    assert_eq!(requests[0]["fingerprint"], b.state.identity.fingerprint());
    let request_id = requests[0]["id"].as_str().unwrap();

    // Before approval, B's signed requests are not accepted by A.
    let (s, _) = b.call(Method::GET, &format!("/peers/{}/proxy/storage/roots", a.device_id()), None).await;
    assert_eq!(s, 404, "A is not a paired device on B yet");

    let (s, v) = a.call(Method::POST, &format!("/devices/pair/requests/{request_id}/approve"), None).await;
    assert_eq!(s, 200, "{v}");
    // The invitation is single-use.
    let (s, _) = b.call(Method::POST, "/pairing/join", Some(json!({"link": link}))).await;
    assert_eq!(s, 403);

    // B learns of the approval and pairs A.
    let mut status = Value::Null;
    for _ in 0..50 {
        let (_, v) = b.call(Method::GET, &format!("/pairing/join/{join_id}"), None).await;
        status = v["status"].clone();
        if status != "waiting" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(status, "approved");

    // Presence: both see each other online.
    nexus_api::peers::check_all_peers(&b.state).await;
    nexus_api::peers::check_all_peers(&a.state).await;
    let (_, devices) = b.call(Method::GET, "/devices", None).await;
    let peer = devices.as_array().unwrap().iter().find(|d| d["id"] == a.device_id()).expect("A paired on B").clone();
    assert_eq!(peer["status"], "online", "{peer}");
    assert_eq!(peer["role"], "editor");
    assert_eq!(peer["fingerprint"], a.state.identity.fingerprint());
    assert!(peer["latency_ms"].is_number());
    assert!(peer["route"].as_str().unwrap().starts_with("direct "), "{peer}");
    let (_, devices) = a.call(Method::GET, "/devices", None).await;
    assert!(devices.as_array().unwrap().iter().any(|d| d["id"] == b.device_id() && d["status"] == "online"));

    // B browses A's files through its own node.
    let proxy = format!("/peers/{}/proxy", a.device_id());
    let (s, roots) = b.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert_eq!(s, 200, "{roots}");
    assert_eq!(roots[0]["id"], root.as_str());
    let (s, listing) = b.call(Method::GET, &format!("{proxy}/files?root_id={root}"), None).await;
    assert_eq!(s, 200);
    assert_eq!(listing["items"][0]["filename"], "plan.txt");
    let file_id = listing["items"][0]["id"].as_str().unwrap().to_string();

    // Media through a ticket issued by A, fetched without a session.
    let (_, link) = b.call(Method::POST, &format!("{proxy}/files/{file_id}/link"), None).await;
    let url = link["url"].as_str().unwrap().replacen("/api/v1", &format!("/api/v1{proxy}"), 1);
    let base_origin = b.base.trim_end_matches("/api/v1");
    let res = reqwest::Client::new()
        .get(format!("{base_origin}{url}"))
        .header("range", "bytes=0-3")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 206);
    assert_eq!(res.bytes().await.unwrap().as_ref(), b"ship");
    // Without a ticket or session, the proxy refuses.
    let res = reqwest::get(format!("{}{proxy}/files?root_id={root}", b.base)).await.unwrap();
    assert_eq!(res.status().as_u16(), 401);

    // Upload from B lands on A's disk.
    let res = reqwest::Client::new()
        .post(format!("{}{proxy}/files/upload?root_id={root}&path=&name=from-laptop.txt", b.base))
        .bearer_auth(&b.token)
        .header("content-length", "5")
        .body("hello")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 201);
    assert_eq!(std::fs::read(a.storage.path().join("from-laptop.txt")).unwrap(), b"hello");

    // Requests outside the browsable API are not relayed.
    let (s, _) = b.call(Method::GET, &format!("{proxy}/devices"), None).await;
    assert_eq!(s, 403);
    let (s, _) = b.call(Method::GET, &format!("{proxy}/files/../devices"), None).await;
    assert!(s == 403 || s == 404, "{s}");

    // Device headers on A's local HTTP port are refused: device requests
    // only count when they arrive over a peer-to-peer connection.
    let res = reqwest::Client::new()
        .get(format!("{}/storage/roots", a.base))
        .header("x-nexus-device", b.device_id())
        .header("x-nexus-timestamp", nexus_common::time::now_ms().to_string())
        .header("x-nexus-nonce", "0123456789abcdef0123")
        .header("x-nexus-signature", nexus_devices::b64_encode(&[0u8; 64]))
        .header("x-nexus-acting-role", "owner")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 401);

    let device_a = a.state.db.get_device(a.device_id()).await.unwrap();
    let mut target = Target::of(&device_a).unwrap();
    target.hints = a.state.net.advertised();
    let p2p_status = |res: (axum::http::Response<hyper::body::Incoming>, String)| res.0.status().as_u16();
    let empty = || axum::body::Body::empty();
    let owner = || Some(Acting { user: "owner", role: nexus_common::types::Role::Owner });
    let t = Duration::from_secs(5);

    // Over the peer-to-peer connection, a forged signature is refused.
    let mut forged = axum::http::HeaderMap::new();
    forged.insert("x-nexus-device", b.device_id().parse().unwrap());
    forged.insert("x-nexus-timestamp", nexus_common::time::now_ms().to_string().parse().unwrap());
    forged.insert("x-nexus-nonce", "0123456789abcdef0123".parse().unwrap());
    forged.insert("x-nexus-signature", nexus_devices::b64_encode(&[0u8; 64]).parse().unwrap());
    forged.insert("x-nexus-acting-role", "owner".parse().unwrap());
    let res = b.state.peers.send(&target, Method::GET, "/api/v1/storage/roots", forged, empty(), None, t).await.unwrap();
    assert_eq!(p2p_status(res), 401);
    // A correctly signed request works.
    let res = b.state.peers.send(&target, Method::GET, "/api/v1/storage/roots", Default::default(), empty(), owner(), t).await.unwrap();
    assert_eq!(p2p_status(res), 200);
    // A paired device cannot manage A's devices, sign in, or relay onwards.
    for (method, path) in [
        (Method::POST, "/api/v1/devices/pair"),
        (Method::POST, "/api/v1/auth/login"),
        (Method::GET, &*format!("/api/v1/peers/{}/proxy/files", b.device_id())),
    ] {
        let res = b.state.peers.send(&target, method, path, Default::default(), empty(), owner(), t).await.unwrap();
        assert_eq!(p2p_status(res), 403, "{path}");
    }

    // A removes B: B's requests stop working at once, and B forgets A.
    let (s, _) = a.call(Method::DELETE, &format!("/devices/{}", b.device_id()), None).await;
    assert_eq!(s, 204);
    let (s, _) = b.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert!(s == 401 || s == 404, "revoked device must be refused, got {s}");
    let mut forgotten = false;
    for _ in 0..50 {
        let (_, devices) = b.call(Method::GET, "/devices", None).await;
        if !devices.as_array().unwrap().iter().any(|d| d["id"] == a.device_id()) {
            forgotten = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(forgotten, "B should drop A after being removed");
}
