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
    /// Phones: their shared storage and share inbox.
    media: std::path::PathBuf,
    inbox: std::path::PathBuf,
}

/// The update key test nodes trust: `(secret, public)`.
fn update_key() -> &'static (String, String) {
    static KEY: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
    KEY.get_or_init(nexus_devices::release::generate_key)
}

async fn node(name: &str) -> Node {
    node_with(name, false).await
}

/// A node; `phone` gives it a shared storage and a share inbox.
async fn node_with(name: &str, phone: bool) -> Node {
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
    let media = data.path().join("media");
    let inbox = data.path().join("inbox");
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
    )
    .with_phone_dirs(phone.then(|| media.clone()), phone.then(|| inbox.clone()))
    .with_update_keys(vec![update_key().1.clone()]));
    let app = nexus_api::router(state.clone(), None);
    nexus_api::p2p::spawn_server(state.clone(), app.clone());
    nexus_api::spawn_sync(state.clone());
    nexus_api::spawn_backup(state.clone());
    nexus_api::spawn_transfers(state.clone());
    nexus_api::spawn_snapshots(state.clone());
    nexus_api::spawn_health(state.clone());
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
    Node { base, state, token, storage, _data: data, media, inbox }
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

    /// Sends raw bytes and returns the status and JSON reply.
    async fn send_bytes(&self, method: reqwest::Method, path: &str, body: &'static [u8]) -> (u16, Value) {
        let res = reqwest::Client::new()
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .body(body)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.json().await.unwrap_or(Value::Null))
    }

    /// Adds a storage location and waits for its first scan.
    async fn add_root(&self, path: &std::path::Path) -> String {
        let (s, v) = self.call(Method::POST, "/storage/roots", Some(json!({"path": path.to_string_lossy()}))).await;
        assert_eq!(s, 201, "{v}");
        let root = v["id"].as_str().unwrap().to_string();
        for _ in 0..200 {
            let (_, v) = self.call(Method::GET, &format!("/storage/roots/{root}/stats"), None).await;
            if v["scanning"] == false && v["scan"]["status"] == "idle" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        root
    }

    /// Signs in as another account and returns a node handle acting as them.
    async fn login(&self, username: &str, password: &str) -> Node {
        let res: Value = reqwest::Client::new()
            .post(format!("{}/auth/login", self.base))
            .json(&json!({"username": username, "password": password}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        Node {
            base: self.base.clone(),
            state: self.state.clone(),
            token: res["token"].as_str().expect("login token").to_string(),
            storage: tempfile::tempdir().unwrap(),
            _data: tempfile::tempdir().unwrap(),
            media: self.media.clone(),
            inbox: self.inbox.clone(),
        }
    }

    /// Pairs `self` with `host`: `self` joins with an invitation and `host`
    /// approves it.
    async fn pair_with(&self, host: &Node) {
        let (_, invite) = host.call(Method::POST, "/devices/pair", None).await;
        let (s, join) = self.call(Method::POST, "/pairing/join", Some(json!({"link": invite["link"]}))).await;
        assert_eq!(s, 200, "{join}");
        let join_id = join["id"].as_str().unwrap().to_string();
        let mut request = String::new();
        for _ in 0..100 {
            let (_, r) = host.call(Method::GET, "/devices/pair/requests", None).await;
            if let Some(id) = r.as_array().and_then(|a| a.first()).and_then(|x| x["id"].as_str()) {
                request = id.to_string();
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (s, v) = host.call(Method::POST, &format!("/devices/pair/requests/{request}/approve"), None).await;
        assert_eq!(s, 200, "{v}");
        for _ in 0..100 {
            let (_, v) = self.call(Method::GET, &format!("/pairing/join/{join_id}"), None).await;
            if v["status"] == "approved" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        nexus_api::peers::check_all_peers(&self.state).await;
        nexus_api::peers::check_all_peers(&host.state).await;
    }

    /// Waits for a copy or sync job to end and returns it.
    async fn wait_job(&self, id: &str) -> Value {
        for _ in 0..300 {
            let (_, jobs) = self.call(Method::GET, "/transfers", None).await;
            if let Some(job) = jobs.as_array().unwrap().iter().find(|j| j["id"] == id)
                && ["done", "failed", "cancelled"].contains(&job["status"].as_str().unwrap()) {
                    return job.clone();
                }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("job {id} did not finish");
    }

    /// Asks a sync pair to run and waits for the run to end.
    async fn sync_now(&self, pair: &str) -> Value {
        let (_, before) = self.call(Method::GET, "/sync", None).await;
        let last = |v: &Value| v.as_array().unwrap().iter().find(|p| p["id"] == pair).map(|p| p["last_run_at"].as_i64().unwrap_or(0));
        let started = last(&before).unwrap();
        let (s, _) = self.call(Method::POST, &format!("/sync/{pair}/run"), None).await;
        assert_eq!(s, 202);
        for _ in 0..300 {
            let (_, now) = self.call(Method::GET, "/sync", None).await;
            let p = now.as_array().unwrap().iter().find(|p| p["id"] == pair).unwrap().clone();
            if p["last_run_at"].as_i64().unwrap_or(0) > started && p["running"] == false {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("sync {pair} did not run");
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
    let owner = || Some(Acting { user: "owner", role: nexus_common::types::Role::Owner, roots: None });
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

    // Copy between devices, streamed through B's node: A → B, then B → A.
    let (s, v) = b.call(Method::POST, "/storage/roots", Some(json!({"path": b.storage.path().to_string_lossy()}))).await;
    assert_eq!(s, 201, "{v}");
    let b_root = v["id"].as_str().unwrap().to_string();
    let (s, v) = b
        .call(Method::POST, "/transfers/copy", Some(json!({"from_device": a.device_id(), "file_id": file_id, "root_id": b_root})))
        .await;
    assert_eq!(s, 202, "{v}");
    let job = b.wait_job(v["id"].as_str().unwrap()).await;
    assert_eq!((job["status"].as_str(), job["files_done"].as_u64()), (Some("done"), Some(1)), "{job}");
    assert_eq!(std::fs::read(b.storage.path().join("plan.txt")).unwrap(), b"ship phase two");
    let copied = b.state.db.get_file_by_path(&b_root, "plan.txt").await.unwrap().unwrap();
    // The copy keeps the original's modification time.
    let original = a.state.db.get_file(&file_id).await.unwrap();
    assert_eq!(copied.modified_at, original.modified_at);
    let (s, v) = b
        .call(Method::POST, "/transfers/copy", Some(json!({"file_id": copied.id, "to_device": a.device_id(), "root_id": root})))
        .await;
    assert_eq!(s, 202, "{v}");
    b.wait_job(v["id"].as_str().unwrap()).await;
    assert_eq!(std::fs::read(a.storage.path().join("plan (1).txt")).unwrap(), b"ship phase two");

    // Folders are copied whole, with what is inside them.
    let (_, album) = a.call(Method::POST, "/files/folder", Some(json!({"root_id": root, "name": "album"}))).await;
    a.call(Method::POST, "/files/folder", Some(json!({"root_id": root, "path": "album", "name": "raw"}))).await;
    a.send_bytes(Method::POST, &format!("/files/upload?root_id={root}&path=album&name=one.jpg"), b"first").await;
    a.send_bytes(Method::POST, &format!("/files/upload?root_id={root}&path=album%2Fraw&name=two.cr2"), b"second").await;
    let (s, v) = b
        .call(Method::POST, "/transfers/copy", Some(json!({"from_device": a.device_id(), "file_ids": [album["id"]], "root_id": b_root})))
        .await;
    assert_eq!(s, 202, "{v}");
    let job = b.wait_job(v["id"].as_str().unwrap()).await;
    assert_eq!((job["files_total"].as_u64(), job["files_done"].as_u64()), (Some(2), Some(2)), "{job}");
    assert_eq!(std::fs::read(b.storage.path().join("album").join("one.jpg")).unwrap(), b"first");
    assert_eq!(std::fs::read(b.storage.path().join("album").join("raw").join("two.cr2")).unwrap(), b"second");
    // Copying it again makes "album (1)" instead of mixing the two.
    let (_, v) = b
        .call(Method::POST, "/transfers/copy", Some(json!({"from_device": a.device_id(), "file_ids": [album["id"]], "root_id": b_root})))
        .await;
    b.wait_job(v["id"].as_str().unwrap()).await;
    assert!(b.storage.path().join("album (1)").join("raw").join("two.cr2").exists());

    // Resumable upload through the proxy: send part, lose the rest, ask
    // where it stopped, continue, finish.
    let (s, up) = b
        .call(Method::POST, &format!("{proxy}/files/uploads"), Some(json!({"root_id": root, "name": "big.bin", "size": 10})))
        .await;
    assert_eq!(s, 201, "{up}");
    let up_id = up["id"].as_str().unwrap().to_string();
    let (s, v) = b.send_bytes(Method::PUT, &format!("{proxy}/files/uploads/{up_id}?offset=0"), b"0123").await;
    assert_eq!((s, v["offset"].as_u64()), (200, Some(4)), "{v}");
    let (s, _) = b.send_bytes(Method::PUT, &format!("{proxy}/files/uploads/{up_id}?offset=0"), b"zzzz").await;
    assert_eq!(s, 409, "a piece at the wrong offset is refused");
    let (s, _) = b.call(Method::POST, &format!("{proxy}/files/uploads/{up_id}/complete"), None).await;
    assert_eq!(s, 409, "an incomplete upload cannot be finished");
    let (_, v) = b.call(Method::GET, &format!("{proxy}/files/uploads/{up_id}"), None).await;
    assert_eq!(v["offset"].as_u64(), Some(4));
    let (s, _) = b.send_bytes(Method::PUT, &format!("{proxy}/files/uploads/{up_id}?offset=4"), b"4567890").await;
    assert_eq!(s, 400, "more than the declared size is refused");
    let (s, _) = b.send_bytes(Method::PUT, &format!("{proxy}/files/uploads/{up_id}?offset=4"), b"456789").await;
    assert_eq!(s, 200);
    let (s, big) = b.call(Method::POST, &format!("{proxy}/files/uploads/{up_id}/complete"), None).await;
    assert_eq!(s, 201, "{big}");
    assert_eq!(std::fs::read(a.storage.path().join("big.bin")).unwrap(), b"0123456789");
    assert_eq!(big["content_hash"].as_str(), Some(blake3::hash(b"0123456789").to_hex().as_str()));
    // Someone else cannot continue or see another person's upload.
    let (s, _) = a.call(Method::GET, &format!("/files/uploads/{up_id}"), None).await;
    assert_eq!(s, 404);

    // Replacing keeps the old content as a version, and the file keeps its id.
    let (_, up) = b
        .call(Method::POST, &format!("{proxy}/files/uploads"), Some(json!({"root_id": root, "name": "big.bin", "size": 10, "on_conflict": "replace"})))
        .await;
    let up_id = up["id"].as_str().unwrap().to_string();
    b.send_bytes(Method::PUT, &format!("{proxy}/files/uploads/{up_id}?offset=0"), b"ABCDEFGHIJ").await;
    let (s, replaced) = b.call(Method::POST, &format!("{proxy}/files/uploads/{up_id}/complete"), None).await;
    assert_eq!(s, 201, "{replaced}");
    assert_eq!(replaced["id"], big["id"]);
    assert_eq!(std::fs::read(a.storage.path().join("big.bin")).unwrap(), b"ABCDEFGHIJ");
    let big_id = big["id"].as_str().unwrap();
    let (s, versions) = b.call(Method::GET, &format!("{proxy}/files/{big_id}/versions"), None).await;
    assert_eq!((s, versions.as_array().unwrap().len()), (200, 1), "{versions}");
    let version = versions[0]["id"].as_str().unwrap();
    let (s, _) = b.call(Method::POST, &format!("{proxy}/files/{big_id}/versions/{version}/restore"), None).await;
    assert_eq!(s, 200);
    assert_eq!(std::fs::read(a.storage.path().join("big.bin")).unwrap(), b"0123456789");
    let (_, versions) = a.call(Method::GET, &format!("/files/{big_id}/versions"), None).await;
    assert_eq!(versions.as_array().unwrap().len(), 1, "the replaced content is now the version");
    let (s, link) = a.call(Method::POST, &format!("/files/{big_id}/versions/{}/link", versions[0]["id"].as_str().unwrap()), None).await;
    assert_eq!(s, 200);
    let old = reqwest::get(format!("{}{}", a.base.trim_end_matches("/api/v1"), link["url"].as_str().unwrap())).await.unwrap();
    assert_eq!(old.bytes().await.unwrap().as_ref(), b"ABCDEFGHIJ");

    // Two-way sync between B's "notes" and A's "notes".
    a.call(Method::POST, "/files/folder", Some(json!({"root_id": root, "name": "notes"}))).await;
    b.call(Method::POST, "/files/folder", Some(json!({"root_id": b_root, "name": "notes"}))).await;
    a.send_bytes(Method::POST, &format!("/files/upload?root_id={root}&path=notes&name=from-a.txt"), b"hello from A").await;
    b.send_bytes(Method::POST, &format!("/files/upload?root_id={b_root}&path=notes&name=from-b.txt"), b"hello from B").await;
    let (s, pair) = b
        .call(
            Method::POST,
            "/sync",
            Some(json!({"local_root_id": b_root, "local_path": "notes", "device_id": a.device_id(), "remote_root_id": root, "remote_path": "notes", "mode": "two_way"})),
        )
        .await;
    assert_eq!(s, 201, "{pair}");
    let pair_id = pair["id"].as_str().unwrap().to_string();
    let (s, _) = b
        .call(
            Method::POST,
            "/sync",
            Some(json!({"local_root_id": b_root, "local_path": "notes", "device_id": a.device_id(), "remote_root_id": root, "remote_path": "notes", "mode": "two_way"})),
        )
        .await;
    assert_eq!(s, 409, "the same folders cannot be paired twice");
    let run = b.sync_now(&pair_id).await;
    assert!(run["last_error"].is_null(), "{run}");
    assert_eq!(std::fs::read(b.storage.path().join("notes").join("from-a.txt")).unwrap(), b"hello from A");
    assert_eq!(std::fs::read(a.storage.path().join("notes").join("from-b.txt")).unwrap(), b"hello from B");
    // Nothing changed: nothing to do.
    let run = b.sync_now(&pair_id).await;
    assert_eq!(run["last_result"], "Up to date", "{run}");
    // A change on A reaches B, and B keeps its old content as a version.
    a.send_bytes(Method::POST, &format!("/files/upload?root_id={root}&path=notes&name=from-a.txt&on_conflict=replace"), b"edited on A").await;
    let run = b.sync_now(&pair_id).await;
    assert!(run["last_error"].is_null(), "{run}");
    assert_eq!(std::fs::read(b.storage.path().join("notes").join("from-a.txt")).unwrap(), b"edited on A");
    let on_b = b.state.db.get_file_by_path(&b_root, "notes/from-a.txt").await.unwrap().unwrap();
    assert_eq!(b.state.db.list_versions(&on_b.id).await.unwrap().len(), 1);
    // Both sides change the same file: both copies survive.
    a.send_bytes(Method::POST, &format!("/files/upload?root_id={root}&path=notes&name=from-b.txt&on_conflict=replace"), b"A's edit").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    b.send_bytes(Method::POST, &format!("/files/upload?root_id={b_root}&path=notes&name=from-b.txt&on_conflict=replace"), b"B's edit").await;
    let run = b.sync_now(&pair_id).await;
    assert!(run["last_error"].is_null(), "{run}");
    assert_eq!(std::fs::read(a.storage.path().join("notes").join("from-b.txt")).unwrap(), b"B's edit");
    let kept: Vec<_> = std::fs::read_dir(b.storage.path().join("notes"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("from-b (Studio's copy"))
        .collect();
    assert_eq!(kept.len(), 1, "A's edit is kept beside B's");
    assert_eq!(std::fs::read(b.storage.path().join("notes").join(&kept[0])).unwrap(), b"A's edit");
    // Deleting on B moves A's copy to A's trash.
    let gone = b.state.db.get_file_by_path(&b_root, "notes/from-a.txt").await.unwrap().unwrap();
    let (s, _) = b.call(Method::DELETE, &format!("/files/{}", gone.id), None).await;
    assert_eq!(s, 200);
    let run = b.sync_now(&pair_id).await;
    assert!(run["last_error"].is_null(), "{run}");
    assert!(!a.storage.path().join("notes").join("from-a.txt").exists());
    let (_, trash) = a.call(Method::GET, "/trash", None).await;
    assert!(trash.as_array().unwrap().iter().any(|t| t["filename"] == "from-a.txt"), "{trash}");
    let (s, _) = b.call(Method::DELETE, &format!("/sync/{pair_id}"), None).await;
    assert_eq!(s, 204);
    // A device cannot ask another node to copy for it.
    let res = b.state.peers.send(&target, Method::POST, "/api/v1/transfers/copy", Default::default(), empty(), owner(), t).await.unwrap();
    assert_eq!(p2p_status(res), 403);

    // A limits B to no storage at all: A's files disappear for B, then return.
    let (s, _) = a.call(Method::PATCH, &format!("/devices/{}", b.device_id()), Some(json!({"allowed_roots": []}))).await;
    assert_eq!(s, 204);
    let (s, roots) = b.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert_eq!((s, roots.as_array().unwrap().len()), (200, 0));
    let (s, _) = b.call(Method::GET, &format!("{proxy}/files?root_id={root}"), None).await;
    assert_eq!(s, 404);
    let (s, _) = b.call(Method::GET, &format!("{proxy}/files/{file_id}"), None).await;
    assert_eq!(s, 404);
    let (_, found) = b.call(Method::GET, &format!("{proxy}/search?q=plan"), None).await;
    assert_eq!(found["items"].as_array().unwrap().len(), 0);
    let res = reqwest::Client::new()
        .post(format!("{}{proxy}/files/upload?root_id={root}&path=&name=blocked.txt", b.base))
        .bearer_auth(&b.token)
        .body("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 404);
    let (s, _) = a.call(Method::PATCH, &format!("/devices/{}", b.device_id()), Some(json!({"allowed_roots": ["not-a-root"]}))).await;
    assert_eq!(s, 400);
    let (s, _) = a.call(Method::PATCH, &format!("/devices/{}", b.device_id()), Some(json!({"all_roots": true}))).await;
    assert_eq!(s, 204);
    let (_, roots) = b.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert_eq!(roots.as_array().unwrap().len(), 1);

    // A blocks B (a lost laptop): B's requests are refused and A sends B
    // nothing. Unblocked, everything works again.
    let (s, _) = a.call(Method::PATCH, &format!("/devices/{}", b.device_id()), Some(json!({"blocked": true}))).await;
    assert_eq!(s, 204);
    let (s, _) = b.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert!(s >= 400, "a blocked device is refused, got {s}");
    let (s, v) = a.call(Method::GET, &format!("/peers/{}/proxy/storage/roots", b.device_id()), None).await;
    assert_eq!(s, 403, "{v}");
    let (_, devices) = a.call(Method::GET, "/devices", None).await;
    assert!(devices.as_array().unwrap().iter().any(|d| d["id"] == b.device_id() && d["blocked_at"].is_i64()));
    let (s, _) = a.call(Method::PATCH, &format!("/devices/{}", b.device_id()), Some(json!({"blocked": false}))).await;
    assert_eq!(s, 204);
    let (s, _) = b.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert_eq!(s, 200);

    // Accounts: the owner adds a viewer, who can read but not manage people.
    let (s, viewer) = a.call(Method::POST, "/users", Some(json!({"username": "guest", "password": "guest password", "role": "viewer"}))).await;
    assert_eq!(s, 201, "{viewer}");
    let (s, _) = a.call(Method::POST, "/users", Some(json!({"username": "guest", "password": "guest password", "role": "viewer"}))).await;
    assert_eq!(s, 409);
    let login = || async {
        reqwest::Client::new()
            .post(format!("{}/auth/login", a.base))
            .json(&json!({"username": "guest", "password": "guest password"}))
            .send()
            .await
            .unwrap()
    };
    let res = login().await;
    assert_eq!(res.status().as_u16(), 200);
    let guest_token = res.json::<Value>().await.unwrap()["token"].as_str().unwrap().to_string();
    let as_guest = |method: Method, path: &str| reqwest::Client::new().request(method, format!("{}{path}", a.base)).bearer_auth(&guest_token);
    assert_eq!(as_guest(Method::GET, "/storage/roots").send().await.unwrap().status().as_u16(), 200);
    assert_eq!(as_guest(Method::GET, "/users").send().await.unwrap().status().as_u16(), 403);
    let res = as_guest(Method::POST, "/files/folder").json(&json!({"root_id": root, "name": "nope"})).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 403);
    // The only owner cannot be removed or demoted.
    let (_, users) = a.call(Method::GET, "/users", None).await;
    let owner_id = users.as_array().unwrap().iter().find(|u| u["role"] == "owner").unwrap()["id"].as_str().unwrap().to_string();
    let (s, _) = a.call(Method::PATCH, &format!("/users/{owner_id}"), Some(json!({"role": "viewer"}))).await;
    assert_eq!(s, 409);
    let (s, _) = a.call(Method::DELETE, &format!("/users/{owner_id}"), None).await;
    assert_eq!(s, 400);
    // Removing the viewer signs them out at once.
    let (s, _) = a.call(Method::DELETE, &format!("/users/{}", viewer["id"].as_str().unwrap()), None).await;
    assert_eq!(s, 204);
    assert_eq!(as_guest(Method::GET, "/storage/roots").send().await.unwrap().status().as_u16(), 401);
    assert_eq!(login().await.status().as_u16(), 401);

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


/// Ids of the storage locations a caller sees, sorted.
fn root_ids(v: &Value) -> Vec<String> {
    let mut ids: Vec<String> = v.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    ids.sort();
    ids
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn people_limits_apply_here_and_on_paired_devices() {
    let a = node("Studio").await;
    let b = node("Laptop").await;
    for n in [&a, &b] {
        for d in ["one", "two"] {
            std::fs::create_dir_all(n.storage.path().join(d)).unwrap();
            std::fs::write(n.storage.path().join(d).join(format!("{d}.txt")), d.as_bytes()).unwrap();
        }
    }
    let a1 = a.add_root(&a.storage.path().join("one")).await;
    let a2 = a.add_root(&a.storage.path().join("two")).await;
    let b1 = b.add_root(&b.storage.path().join("one")).await;
    let b2 = b.add_root(&b.storage.path().join("two")).await;
    b.pair_with(&a).await;
    let proxy = format!("/peers/{}/proxy", b.device_id());

    // A guest on Studio, limited to one location here and one on Laptop.
    let (s, guest) = a.call(Method::POST, "/users", Some(json!({"username": "guest", "password": "guest-password", "role": "editor"}))).await;
    assert_eq!(s, 201, "{guest}");
    let guest_id = guest["id"].as_str().unwrap().to_string();
    assert_eq!(guest["allowed_roots"], Value::Null);
    let (s, v) = a.call(Method::PATCH, &format!("/users/{guest_id}"), Some(json!({"allowed_roots": [a1, b1]}))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(sorted(serde_json::from_value(v["allowed_roots"].clone()).unwrap()), sorted(vec![a1.clone(), b1.clone()]));
    let g = a.login("guest", "guest-password").await;

    // Here: only the chosen location exists for them.
    let (_, roots) = g.call(Method::GET, "/storage/roots", None).await;
    assert_eq!(root_ids(&roots), vec![a1.clone()]);
    let (s, _) = g.call(Method::GET, &format!("/files?root_id={a2}"), None).await;
    assert_eq!(s, 404);
    let (_, found) = g.call(Method::GET, "/search?q=two", None).await;
    assert_eq!(found["items"].as_array().unwrap().len(), 0, "{found}");
    // Nor can they read a file there by its id.
    let (_, listing) = a.call(Method::GET, &format!("/files?root_id={a2}"), None).await;
    let hidden = listing["items"][0]["id"].as_str().unwrap().to_string();
    let (_, listing) = a.call(Method::GET, &format!("/files?root_id={a1}"), None).await;
    let shown = listing["items"][0]["id"].as_str().unwrap().to_string();
    let (s, _) = g.call(Method::GET, &format!("/files/{hidden}/content"), None).await;
    assert_eq!(s, 404);
    let (s, _) = g.call(Method::POST, &format!("/files/{hidden}/link"), None).await;
    assert_eq!(s, 404);
    let res = reqwest::Client::new().get(format!("{}/files/{shown}/content", g.base)).bearer_auth(&g.token).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 200);
    assert_eq!(res.bytes().await.unwrap().as_ref(), b"one");

    // On Laptop too: the limit travels with the signed request.
    let (s, roots) = g.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert_eq!(s, 200, "{roots}");
    assert_eq!(root_ids(&roots), vec![b1.clone()]);
    // Whatever a device answers is sandboxed on this node's origin.
    let res = reqwest::Client::new().get(format!("{}{proxy}/storage/roots", g.base)).bearer_auth(&g.token).send().await.unwrap();
    assert!(res.headers()["content-security-policy"].to_str().unwrap().starts_with("sandbox"));
    let (s, _) = g.call(Method::GET, &format!("{proxy}/files?root_id={b2}"), None).await;
    assert_eq!(s, 404);
    // The owner still sees everything there.
    let (_, roots) = a.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert_eq!(root_ids(&roots), sorted(vec![b1.clone(), b2.clone()]));

    // A forged, wider limit does not verify: the header is signed.
    let target = nexus_api::peers::Target::of(&a.state.db.get_device(b.device_id()).await.unwrap()).unwrap();
    let wide = [b1.clone(), b2.clone()];
    let (roots, _): (Value, _) = a
        .state
        .peers
        .call(&target, Method::GET, "/api/v1/storage/roots", None::<&()>, Some(Acting { user: "guest", role: nexus_common::types::Role::Editor, roots: Some(&wide[..1]) }), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(root_ids(&roots), vec![b1.clone()]);

    // They cannot lift their own limit.
    let (s, _) = g.call(Method::PATCH, &format!("/users/{guest_id}"), Some(json!({"all_roots": true}))).await;
    assert_eq!(s, 403);
    // Administrators always see everything.
    let (s, _) = a.call(Method::PATCH, &format!("/users/{guest_id}"), Some(json!({"role": "administrator", "allowed_roots": [a1]}))).await;
    assert_eq!(s, 400);
    // A changed limit applies to the next request.
    let (s, _) = a.call(Method::PATCH, &format!("/users/{guest_id}"), Some(json!({"allowed_roots": []}))).await;
    assert_eq!(s, 200);
    let (_, roots) = g.call(Method::GET, "/storage/roots", None).await;
    assert!(root_ids(&roots).is_empty());
    let (_, roots) = g.call(Method::GET, &format!("{proxy}/storage/roots"), None).await;
    assert!(root_ids(&roots).is_empty());
    let (s, _) = a.call(Method::PATCH, &format!("/users/{guest_id}"), Some(json!({"all_roots": true}))).await;
    assert_eq!(s, 200);
    let (_, roots) = g.call(Method::GET, "/storage/roots", None).await;
    assert_eq!(root_ids(&roots), sorted(vec![a1, a2]));
}

/// Writes a file whose modification time is an hour ago (backup skips
/// files changed in the last half minute).
fn old_file(path: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    let when = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
}

impl Node {
    /// Waits for the run that turning backup on started, and returns it.
    async fn backup_first_run(&self) -> Value {
        for _ in 0..400 {
            let (_, now) = self.call(Method::GET, "/backup", None).await;
            if now["backup"]["last_run_at"].is_i64() && now["running"] == false {
                return now["backup"].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("backup did not run");
    }

    /// Asks the backup to run and waits for the run to end.
    async fn backup_now(&self) -> Value {
        let (_, before) = self.call(Method::GET, "/backup", None).await;
        let started = before["backup"]["last_run_at"].as_i64().unwrap_or(0);
        let (s, v) = self.call(Method::POST, "/backup/run", None).await;
        assert_eq!(s, 202, "{v}");
        for _ in 0..400 {
            let (_, now) = self.call(Method::GET, "/backup", None).await;
            if now["backup"]["last_run_at"].as_i64().unwrap_or(0) > started && now["running"] == false {
                return now["backup"].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("backup did not run");
    }

    /// Paths of the files under a folder, relative to it.
    async fn files_under(&self, root: &str, path: &str) -> Vec<String> {
        let (s, v) = self.call(Method::GET, &format!("/files/tree?root_id={root}&path={}", path.replace(' ', "%20")), None).await;
        assert_eq!(s, 200, "{v}");
        let mut out: Vec<String> =
            v["items"].as_array().unwrap().iter().filter(|i| i["is_dir"] == false).map(|i| i["path"].as_str().unwrap().to_string()).collect();
        out.sort();
        out
    }
}

#[tokio::test]
async fn phone_backup_and_share() {
    let pc = node("PC").await;
    let phone = node_with("Phone", true).await;
    std::fs::create_dir_all(pc.storage.path().join("Phone backup/DCIM/Camera")).unwrap();
    // Already on the computer (the app was reinstalled, say): not sent again.
    old_file(&pc.storage.path().join("Phone backup/DCIM/Camera/c.jpg"), b"photo c");
    let root = pc.add_root(pc.storage.path()).await;
    phone.pair_with(&pc).await;

    let media = phone.media.clone();
    old_file(&media.join("DCIM/Camera/a.jpg"), b"photo a");
    old_file(&media.join("DCIM/Camera/b.mp4"), b"video b, first take");
    old_file(&media.join("DCIM/Camera/c.jpg"), b"photo c");
    old_file(&media.join("DCIM/.thumbnails/a.jpg"), b"thumbnail");
    old_file(&media.join("Pictures/Screenshots/s.png"), b"screenshot");
    // Still being written: left for the next run.
    std::fs::write(media.join("DCIM/Camera/new.jpg"), b"fresh").unwrap();

    // A computer that only lets the phone look gets a clear message.
    let (_, status) = phone.call(Method::GET, "/backup", None).await;
    assert_eq!(status["available"], true);
    assert_eq!(status["backup"], Value::Null);
    let (s, v) = phone
        .call(Method::PUT, "/backup", Some(json!({"device_id": pc.device_id(), "root_id": root, "path": "Phone backup", "sources": ["Nope"]})))
        .await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = phone
        .call(Method::PUT, "/backup", Some(json!({"device_id": pc.device_id(), "root_id": root, "path": "Phone backup", "sources": ["DCIM"]})))
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["backup"]["label"], format!("{} / Phone backup", pc.storage.path().file_name().unwrap().to_string_lossy()));

    let run = phone.backup_first_run().await;
    assert_eq!(run["last_error"], Value::Null, "{run}");
    assert_eq!(run["last_result"], "Backed up 2 new files", "{run}");
    assert_eq!(pc.files_under(&root, "Phone backup").await, ["DCIM/Camera/a.jpg", "DCIM/Camera/b.mp4", "DCIM/Camera/c.jpg"]);
    let copied = pc.storage.path().join("Phone backup/DCIM/Camera/a.jpg");
    assert_eq!(std::fs::read(&copied).unwrap(), b"photo a");
    let phone_time = std::fs::metadata(media.join("DCIM/Camera/a.jpg")).unwrap().modified().unwrap();
    let pc_time = std::fs::metadata(&copied).unwrap().modified().unwrap();
    let diff = phone_time.duration_since(pc_time).or_else(|e| Ok::<_, ()>(e.duration())).unwrap();
    assert!(diff < Duration::from_secs(2), "dates are kept");

    // Nothing new: nothing sent.
    let run = phone.backup_now().await;
    assert!(run["last_result"].as_str().unwrap().starts_with("Up to date"), "{run}");

    // Deleted on the computer: not sent again.
    let (_, listing) = pc.call(Method::GET, &format!("/files?root_id={root}&path=Phone%20backup/DCIM/Camera"), None).await;
    let a_id = listing["items"].as_array().unwrap().iter().find(|i| i["filename"] == "a.jpg").unwrap()["id"].as_str().unwrap().to_string();
    let (s, _) = pc.call(Method::DELETE, &format!("/files/{a_id}"), None).await;
    assert_eq!(s, 200);
    // Changed on the phone: replaced, and the computer keeps the old one.
    old_file(&media.join("DCIM/Camera/b.mp4"), b"video b, second take, longer");
    let run = phone.backup_now().await;
    assert_eq!(run["last_result"], "Backed up 1 new file", "{run}");
    assert_eq!(pc.files_under(&root, "Phone backup").await, ["DCIM/Camera/b.mp4", "DCIM/Camera/c.jpg"]);
    assert_eq!(std::fs::read(pc.storage.path().join("Phone backup/DCIM/Camera/b.mp4")).unwrap(), b"video b, second take, longer");
    let (_, listing) = pc.call(Method::GET, &format!("/files?root_id={root}&path=Phone%20backup/DCIM/Camera"), None).await;
    let b_id = listing["items"].as_array().unwrap().iter().find(|i| i["filename"] == "b.mp4").unwrap()["id"].as_str().unwrap().to_string();
    let (_, versions) = pc.call(Method::GET, &format!("/files/{b_id}/versions"), None).await;
    assert_eq!(versions.as_array().unwrap().len(), 1, "{versions}");

    // Pictures, once chosen, go in too.
    let (s, _) = phone
        .call(Method::PUT, "/backup", Some(json!({"device_id": pc.device_id(), "root_id": root, "path": "Phone backup", "sources": ["DCIM", "Pictures"]})))
        .await;
    assert_eq!(s, 200);
    // Changing what is backed up runs it at once.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let run = phone.backup_now().await;
    assert!(run["last_error"].is_null(), "{run}");
    assert!(pc.files_under(&root, "Phone backup").await.contains(&"Pictures/Screenshots/s.png".to_string()));

    // Computers have no backup.
    let (_, status) = pc.call(Method::GET, "/backup", None).await;
    assert_eq!(status["available"], false);

    // Share menu: a file waiting in the inbox is sent and then removed.
    std::fs::create_dir_all(&phone.inbox).unwrap();
    std::fs::write(phone.inbox.join("note.txt"), b"shared from another app").unwrap();
    std::fs::write(phone.inbox.join("half.pdf.part"), b"still copying").unwrap();
    let (_, waiting) = phone.call(Method::GET, "/shared", None).await;
    assert_eq!(waiting, json!([{"name": "note.txt", "size": 23}]));
    let (s, _) = phone
        .call(Method::POST, "/shared/send", Some(json!({"names": ["../nexus.db"], "device_id": pc.device_id(), "root_id": root, "path": ""})))
        .await;
    assert!(s == 400 || s == 404, "{s}");
    let (s, job) = phone
        .call(Method::POST, "/shared/send", Some(json!({"names": ["note.txt"], "device_id": pc.device_id(), "root_id": root, "path": ""})))
        .await;
    assert_eq!(s, 202, "{job}");
    let job = phone.wait_job(job["id"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(std::fs::read(pc.storage.path().join("note.txt")).unwrap(), b"shared from another app");
    assert!(!phone.inbox.join("note.txt").exists());
    let (_, waiting) = phone.call(Method::GET, "/shared", None).await;
    assert_eq!(waiting, json!([]));
}

/// Signs `package` as a release and adds it to `node`'s update store.
fn publish(node: &Node, platform: &str, version: &str, package: &[u8]) {
    use nexus_devices::release::{Release, SignedRelease};
    let release = Release {
        platform: platform.into(),
        version: version.into(),
        file: format!("NEXUS-Cloud_{version}"),
        size: package.len() as u64,
        blake3: blake3::hash(package).to_hex().to_string(),
        notes: "Better things".into(),
        published_at: 1,
    };
    let signed = SignedRelease::sign(&release, &update_key().0).unwrap();
    let file = node.storage.path().join(format!("{platform}-{version}"));
    std::fs::write(&file, package).unwrap();
    nexus_api::update_store::import(&node.state.data_dir, &signed, &file, &node.state.update_keys).unwrap();
}

#[tokio::test]
async fn updates_spread_between_paired_devices() {
    let pc = node("pc").await;
    let laptop = node("laptop").await;
    laptop.pair_with(&pc).await;

    let phone_package = b"pretend android package ".repeat(4000);
    publish(&pc, "android-aarch64", "9.9.9", &phone_package);
    let windows_package = b"pretend windows installer ".repeat(3000);
    publish(&pc, "windows-x86_64", "9.9.8", &windows_package);

    // Only paired devices may fetch packages.
    let (s, _) = pc.call(Method::GET, "/peer/updates", None).await;
    assert_eq!(s, 403);
    let (s, _) = pc.call(Method::GET, "/peer/updates/android-aarch64", None).await;
    assert_eq!(s, 403);

    // The laptop asks the computer and keeps both packages, to install
    // its own and pass the other on.
    nexus_api::check_updates(&laptop.state).await;
    let (_, status) = laptop.call(Method::GET, "/updates", None).await;
    assert_eq!(status["error"], Value::Null, "{status}");
    let carried: Vec<String> = status["carried"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| format!("{} {}", r["platform"].as_str().unwrap(), r["version"].as_str().unwrap()))
        .collect();
    assert_eq!(carried, ["windows-x86_64 9.9.8", "android-aarch64 9.9.9"]);
    let kept = |platform: &str| std::fs::read(nexus_api::update_store::package_path(&laptop.state.data_dir, platform)).unwrap();
    assert_eq!(kept("android-aarch64"), phone_package);
    assert_eq!(kept("windows-x86_64"), windows_package);

    if nexus_devices::release::this_platform() == Some("windows-x86_64") {
        assert_eq!(status["available"]["version"], "9.9.8");
        assert_eq!(status["available"]["notes"], "Better things");
        // A development build never installs anything.
        assert_eq!(status["install"], "none");
        let (s, _) = laptop.call(Method::POST, "/updates/install", None).await;
        assert_eq!(s, 400);
    }

    // Automatic installs can be turned off by the owner.
    let (s, v) = laptop.call(Method::PUT, "/updates/settings", Some(json!({"auto_install": false}))).await;
    assert_eq!((s, &v["auto_install"]), (200, &json!(false)));

    // Nothing newer: checking again changes nothing. A newer release on
    // the laptop flows back to the computer.
    nexus_api::check_updates(&laptop.state).await;
    publish(&laptop, "android-aarch64", "10.0.0", b"newer");
    nexus_api::check_updates(&pc.state).await;
    let (_, status) = pc.call(Method::GET, "/updates", None).await;
    assert!(status["carried"].as_array().unwrap().iter().any(|r| r["version"] == "10.0.0"), "{status}");

    // A package changed after it arrived is not passed on.
    std::fs::write(nexus_api::update_store::package_path(&pc.state.data_dir, "android-aarch64"), b"tampered and longer").unwrap();
    let (_, status) = pc.call(Method::GET, "/updates", None).await;
    assert!(!status["carried"].as_array().unwrap().iter().any(|r| r["platform"] == "android-aarch64"), "{status}");
}

/// The ids of the devices `node` lists, besides itself, and who introduced
/// each (`-` when paired directly).
async fn peers_of(node: &Node) -> Vec<(String, String, String)> {
    let (_, list) = node.call(Method::GET, "/devices", None).await;
    let mut out: Vec<(String, String, String)> = list
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["is_local"] == false)
        .map(|d| {
            (
                d["name"].as_str().unwrap().to_string(),
                d["introduced_by"].as_str().unwrap_or("-").to_string(),
                d["role"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn the_main_node_introduces_its_devices_to_each_other() {
    let hub = node("hub").await;
    let laptop = node("laptop").await;
    let phone = node_with("phone", true).await;
    laptop.pair_with(&hub).await;
    phone.pair_with(&hub).await;
    // The hub lets the phone only view its files.
    let (s, _) = hub.call(Method::PATCH, &format!("/devices/{}", phone.device_id()), Some(json!({"role": "viewer"}))).await;
    assert_eq!(s, 204);

    nexus_api::introduce_devices(&hub.state).await;
    let hub_id = hub.device_id().to_string();
    assert_eq!(peers_of(&laptop).await, [("hub".into(), "-".into(), "editor".into()), ("phone".into(), hub_id.clone(), "viewer".into())]);
    assert_eq!(peers_of(&phone).await, [("hub".into(), "-".into(), "editor".into()), ("laptop".into(), hub_id.clone(), "editor".into())]);

    // They now reach each other directly, with signed requests.
    nexus_api::peers::check_all_peers(&laptop.state).await;
    nexus_api::peers::check_all_peers(&phone.state).await;
    let (_, list) = laptop.call(Method::GET, "/devices", None).await;
    let phone_seen = list.as_array().unwrap().iter().find(|d| d["name"] == "phone").unwrap().clone();
    assert_eq!(phone_seen["online"], true, "{phone_seen}");
    let root = laptop.add_root(laptop.storage.path()).await;
    let (s, roots) = phone.call(Method::GET, &format!("/peers/{}/proxy/storage/roots", laptop.device_id()), None).await;
    assert_eq!(s, 200, "{roots}");
    assert!(roots.as_array().unwrap().iter().any(|r| r["id"] == root));

    // Introduced devices do not introduce others.
    nexus_api::introduce_devices(&laptop.state).await;
    assert_eq!(peers_of(&phone).await.len(), 2);

    // Removing a device on the hub removes it everywhere it was introduced.
    let (s, _) = hub.call(Method::DELETE, &format!("/devices/{}", phone.device_id()), None).await;
    assert_eq!(s, 204);
    nexus_api::introduce_devices(&hub.state).await;
    assert_eq!(peers_of(&laptop).await, [("hub".into(), "-".into(), "editor".into())]);
    for _ in 0..50 {
        if peers_of(&phone).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(peers_of(&phone).await.is_empty(), "the phone forgets the hub and what it introduced");

    // A device removed by hand is not brought back.
    let tablet = node("tablet").await;
    tablet.pair_with(&hub).await;
    nexus_api::introduce_devices(&hub.state).await;
    assert_eq!(peers_of(&laptop).await.len(), 2);
    let (s, _) = laptop.call(Method::DELETE, &format!("/devices/{}", tablet.device_id()), None).await;
    assert_eq!(s, 204);
    nexus_api::introduce_devices(&hub.state).await;
    assert_eq!(peers_of(&laptop).await, [("hub".into(), "-".into(), "editor".into())]);

    // And turning introductions off on the hub stops them.
    let (s, _) = hub.call(Method::PUT, "/devices/introductions", Some(json!({"enabled": false}))).await;
    assert_eq!(s, 200);
}

impl Node {
    /// Waits for a backup's next run (or check) to end and returns the set.
    async fn backup_set_after(&self, id: &str, field: &str, before: i64) -> Value {
        for _ in 0..400 {
            let (_, sets) = self.call(Method::GET, "/snapshots", None).await;
            let set = sets.as_array().unwrap().iter().find(|x| x["id"] == id).unwrap().clone();
            if set[field].as_i64().unwrap_or(0) > before && set["running"] == false {
                return set;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("backup {id} did not finish");
    }

    async fn backup_run(&self, id: &str) -> Value {
        let (_, sets) = self.call(Method::GET, "/snapshots", None).await;
        let before = sets.as_array().unwrap().iter().find(|x| x["id"] == id).unwrap()["last_run_at"].as_i64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(5)).await;
        let (s, _) = self.call(Method::POST, &format!("/snapshots/{id}/run"), None).await;
        assert_eq!(s, 202);
        self.backup_set_after(id, "last_run_at", before).await
    }
}

/// Files under `dir`, as paths relative to it.
fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            } else {
                out.push(e.path().strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn backups_keep_snapshots_check_and_restore() {
    let pc = node("pc").await;
    let nas = node("nas").await;
    pc.pair_with(&nas).await;

    let docs = pc.storage.path().join("docs");
    std::fs::create_dir_all(docs.join("sub")).unwrap();
    std::fs::write(docs.join("a.txt"), b"first version").unwrap();
    std::fs::write(docs.join("sub").join("b.txt"), b"same content").unwrap();
    std::fs::write(docs.join("sub").join("c.txt"), b"same content").unwrap();
    let here = pc.add_root(pc.storage.path()).await;
    let there = nas.add_root(nas.storage.path()).await;

    let (s, set) = pc
        .call(
            Method::POST,
            "/snapshots",
            Some(json!({
                "local_root_id": here, "local_path": "docs", "device_id": nas.device_id(), "remote_root_id": there, "remote_path": "",
                "encrypt": false
            })),
        )
        .await;
    assert_eq!(s, 201, "{set}");
    let id = set["id"].as_str().unwrap().to_string();
    let set = pc.backup_set_after(&id, "last_run_at", 0).await;
    assert!(set["last_error"].is_null(), "{set}");
    // Three files, two different contents: each content is stored once.
    assert_eq!(set["last_result"], "3 files, 2 new", "{set}");
    let store = nas.storage.path().join("pc - docs");
    let stored = files_in(&store);
    assert_eq!(stored.iter().filter(|p| p.starts_with("objects/")).count(), 2, "{stored:?}");
    assert_eq!(stored.iter().filter(|p| p.starts_with("snapshots/") && p.ends_with(".json")).count(), 1, "{stored:?}");
    // The sample read back after the run found nothing wrong.
    assert!(set["last_check_result"].as_str().unwrap().ends_with("intact"), "{set}");

    // One file changes: only its new content is sent.
    pc.send_bytes(Method::POST, &format!("/files/upload?root_id={here}&path=docs&name=a.txt&on_conflict=replace"), b"second version!").await;
    let set = pc.backup_run(&id).await;
    assert_eq!(set["last_result"], "3 files, 1 new", "{set}");
    let (_, history) = pc.call(Method::GET, &format!("/snapshots/{id}/history"), None).await;
    let history = history.as_array().unwrap().clone();
    assert_eq!(history.len(), 2);
    let first = history[1]["id"].as_str().unwrap().to_string();

    // Browsing the first snapshot.
    let (_, top) = pc.call(Method::GET, &format!("/snapshots/{id}/{first}/files?path="), None).await;
    let names: Vec<_> =
        top.as_array().unwrap().iter().map(|e| (e["name"].as_str().unwrap().to_string(), e["files"].as_u64().unwrap())).collect();
    assert_eq!(names, [("sub".to_string(), 2), ("a.txt".to_string(), 0)]);

    // Damage on the device is found and repaired from the files here.
    let shared = blake3::hash(b"same content").to_hex().to_string();
    let object = store.join("objects").join(&shared[..2]).join(&shared);
    std::fs::write(&object, b"same contenX").unwrap();
    let before = set["last_check_at"].as_i64().unwrap_or(0);
    let (s, _) = pc.call(Method::POST, &format!("/snapshots/{id}/check"), None).await;
    assert_eq!(s, 202);
    let set = pc.backup_set_after(&id, "last_check_at", before).await;
    let result = set["last_check_result"].as_str().unwrap();
    assert!(result.starts_with("All 3 contents read back: 1 damaged and 0 missing, all sent again"), "{result}");
    assert_eq!(std::fs::read(&object).unwrap(), b"same content");

    // Restoring the first snapshot beside the folder brings back the old file.
    let (s, started) = pc.call(Method::POST, &format!("/snapshots/{id}/{first}/restore"), Some(json!({"to": "beside"}))).await;
    assert_eq!(s, 202, "{started}");
    let job = pc.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["verified"], 3, "{job}");
    assert!(started["folder"].as_str().unwrap().starts_with("docs (restored "), "{started}");
    let restored = pc.storage.path().join(started["folder"].as_str().unwrap());
    assert_eq!(std::fs::read(restored.join("a.txt")).unwrap(), b"first version");
    assert_eq!(std::fs::read(restored.join("sub").join("c.txt")).unwrap(), b"same content");

    // Restoring one file over the original replaces it (keeping a version).
    let (s, started) =
        pc.call(Method::POST, &format!("/snapshots/{id}/{first}/restore"), Some(json!({"to": "original", "paths": ["a.txt"]}))).await;
    assert_eq!(s, 202, "{started}");
    let job = pc.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!((job["status"].as_str(), job["files_done"].as_u64()), (Some("done"), Some(1)), "{job}");
    assert_eq!(std::fs::read(docs.join("a.txt")).unwrap(), b"first version");
    let a = pc.state.db.get_file_by_path(&here, "docs/a.txt").await.unwrap().unwrap();
    // Kept as versions: "first version" (replaced by the upload) and
    // "second version!" (replaced by the restore).
    assert_eq!(pc.state.db.list_versions(&a.id).await.unwrap().len(), 2);

    // Keeping only the latest snapshot clears the old ones, and contents
    // only they used go to the trash on the device.
    let (s, v) = pc
        .call(
            Method::PATCH,
            &format!("/snapshots/{id}"),
            Some(json!({"schedule": {"every_hours": 24, "keep_last": 1, "keep_daily": 0, "keep_weekly": 0, "keep_monthly": 0}})),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    pc.send_bytes(Method::POST, &format!("/files/upload?root_id={here}&path=docs&name=a.txt&on_conflict=replace"), b"third").await;
    let set = pc.backup_run(&id).await;
    assert!(set["last_result"].as_str().unwrap().contains("2 old snapshots removed"), "{set}");
    let (_, history) = pc.call(Method::GET, &format!("/snapshots/{id}/history"), None).await;
    assert_eq!(history.as_array().unwrap().len(), 1);
    let stored = files_in(&store);
    assert_eq!(stored.iter().filter(|p| p.starts_with("objects/")).count(), 2, "{stored:?}");
    assert_eq!(stored.iter().filter(|p| p.starts_with("snapshots/")).count(), 1, "{stored:?}");

    // Bad keep rules are refused.
    let (s, _) = pc
        .call(
            Method::PATCH,
            &format!("/snapshots/{id}"),
            Some(json!({"schedule": {"every_hours": 24, "keep_last": 0, "keep_daily": 0, "keep_weekly": 0, "keep_monthly": 0}})),
        )
        .await;
    assert_eq!(s, 400);
}

#[tokio::test]
async fn encrypted_backups_open_on_another_computer() {
    let pc = node("pc").await;
    let nas = node("nas").await;
    let laptop = node("laptop").await;
    pc.pair_with(&nas).await;
    laptop.pair_with(&nas).await;

    let docs = pc.storage.path().join("docs");
    std::fs::create_dir_all(docs.join("sub")).unwrap();
    std::fs::write(docs.join("a.txt"), b"secret plans").unwrap();
    // Several chunks, so the format's chunking is used.
    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
    std::fs::write(docs.join("sub").join("big.bin"), &big).unwrap();
    std::fs::create_dir_all(pc.storage.path().join("other")).unwrap();
    std::fs::write(pc.storage.path().join("other").join("x.txt"), b"other things").unwrap();
    let here = pc.add_root(pc.storage.path()).await;
    let there = nas.add_root(nas.storage.path()).await;

    let create = |path: &'static str| {
        json!({"local_root_id": here, "local_path": path, "device_id": nas.device_id(), "remote_root_id": there, "remote_path": ""})
    };
    let (s, set) = pc.call(Method::POST, "/snapshots", Some(create("docs"))).await;
    assert_eq!(s, 201, "{set}");
    assert_eq!(set["encrypted"], true, "{set}");
    let key = set["recovery_key"].as_str().unwrap().to_string();
    assert_eq!(key.split('-').count(), 14, "{key}");
    let id = set["id"].as_str().unwrap().to_string();
    let set = pc.backup_set_after(&id, "last_run_at", 0).await;
    assert!(set["last_error"].is_null(), "{set}");
    assert_eq!(set["last_result"], "2 files, 2 new", "{set}");
    assert!(set["last_check_result"].as_str().unwrap().ends_with("intact"), "{set}");

    // A second backup, to have a recovery key that does not fit the first.
    let (s, other) = pc.call(Method::POST, "/snapshots", Some(create("other"))).await;
    assert_eq!(s, 201, "{other}");
    let other_key = other["recovery_key"].as_str().unwrap().to_string();
    pc.backup_set_after(other["id"].as_str().unwrap(), "last_run_at", 0).await;

    // On the device: nothing readable, and no plain hashes as names.
    let store = nas.storage.path().join("pc - docs");
    let stored = files_in(&store);
    let objects: Vec<_> = stored.iter().filter(|p| p.starts_with("objects/")).collect();
    assert_eq!(objects.len(), 2, "{stored:?}");
    let plain_names = [blake3::hash(b"secret plans").to_hex().to_string(), blake3::hash(&big).to_hex().to_string()];
    for p in &objects {
        assert!(!plain_names.iter().any(|h| p.ends_with(h.as_str())), "{p}");
        let bytes = std::fs::read(store.join(p)).unwrap();
        assert!(bytes.starts_with(b"NXB1"));
        assert!(!bytes.windows(12).any(|w| w == b"secret plans"));
    }
    let lists: Vec<_> = stored.iter().filter(|p| p.starts_with("snapshots/")).collect();
    assert_eq!(lists.len(), 1, "{stored:?}");
    assert!(lists[0].ends_with(".nxb"), "{lists:?}");
    let list = std::fs::read(store.join(lists[0])).unwrap();
    assert!(!list.windows(5).any(|w| w == b"a.txt"));

    // Damage is noticed (it no longer opens) and repaired.
    let damaged = store.join(objects[0]);
    let mut bytes = std::fs::read(&damaged).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&damaged, &bytes).unwrap();
    let before = set["last_check_at"].as_i64().unwrap_or(0);
    let (s, _) = pc.call(Method::POST, &format!("/snapshots/{id}/check"), None).await;
    assert_eq!(s, 202);
    let set = pc.backup_set_after(&id, "last_check_at", before).await;
    let result = set["last_check_result"].as_str().unwrap();
    assert!(result.starts_with("All 2 contents read back: 1 damaged and 0 missing, all sent again"), "{result}");
    assert_ne!(std::fs::read(&damaged).unwrap(), bytes);

    // The recovery key is shown again only with the password.
    let (s, _) = pc.call(Method::POST, &format!("/snapshots/{id}/recovery-key"), Some(json!({"password": "wrong"}))).await;
    assert_eq!(s, 403);
    let (s, shown) = pc.call(Method::POST, &format!("/snapshots/{id}/recovery-key"), Some(json!({"password": "correct horse"}))).await;
    assert_eq!(s, 200, "{shown}");
    assert_eq!(shown["recovery_key"], key.as_str());

    // Restoring here decrypts and checks every file.
    let (_, history) = pc.call(Method::GET, &format!("/snapshots/{id}/history"), None).await;
    let snap = history[0]["id"].as_str().unwrap().to_string();
    let (s, started) = pc.call(Method::POST, &format!("/snapshots/{id}/{snap}/restore"), Some(json!({"to": "beside"}))).await;
    assert_eq!(s, 202, "{started}");
    let job = pc.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!((job["status"].as_str(), job["verified"].as_u64()), (Some("done"), Some(2)), "{job}");
    let restored = pc.storage.path().join(started["folder"].as_str().unwrap());
    assert_eq!(std::fs::read(restored.join("sub").join("big.bin")).unwrap(), big);

    // Another computer opens the backup with the recovery key.
    let mine = laptop.add_root(laptop.storage.path()).await;
    let open = |key: &str| {
        json!({"device_id": nas.device_id(), "remote_root_id": there, "remote_path": "pc - docs", "recovery_key": key,
               "local_root_id": mine, "local_path": "Recovered"})
    };
    let (s, v) = laptop.call(Method::POST, "/snapshots/open", Some(open(""))).await;
    assert_eq!(s, 400, "{v}");
    assert!(v.to_string().contains("encrypted"), "{v}");
    let (s, v) = laptop.call(Method::POST, "/snapshots/open", Some(open(&key.replacen(&key[..1], if &key[..1] == "A" { "B" } else { "A" }, 1)))).await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = laptop.call(Method::POST, "/snapshots/open", Some(open(&other_key))).await;
    assert_eq!(s, 400, "{v}");
    assert!(v.to_string().contains("does not open"), "{v}");
    let (s, opened) = laptop.call(Method::POST, "/snapshots/open", Some(open(&key.to_lowercase()))).await;
    assert_eq!(s, 201, "{opened}");
    assert_eq!((opened["name"].as_str(), opened["enabled"].as_bool(), opened["snapshots"].as_u64()), (Some("docs"), Some(false), Some(1)), "{opened}");
    let (s, _) = laptop.call(Method::POST, "/snapshots/open", Some(open(&key))).await;
    assert_eq!(s, 409);

    let opened_id = opened["id"].as_str().unwrap().to_string();
    let (s, started) =
        laptop.call(Method::POST, &format!("/snapshots/{opened_id}/{snap}/restore"), Some(json!({"to": "original"}))).await;
    assert_eq!(s, 202, "{started}");
    let job = laptop.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!((job["status"].as_str(), job["verified"].as_u64()), (Some("done"), Some(2)), "{job}");
    let recovered = laptop.storage.path().join("Recovered");
    assert_eq!(std::fs::read(recovered.join("a.txt")).unwrap(), b"secret plans");
    assert_eq!(std::fs::read(recovered.join("sub").join("big.bin")).unwrap(), big);

    // Turned on, it continues the backup from the other computer.
    let (s, _) = laptop.call(Method::PATCH, &format!("/snapshots/{opened_id}"), Some(json!({"enabled": true}))).await;
    assert_eq!(s, 200);
    let set = laptop.backup_run(&opened_id).await;
    assert_eq!(set["last_result"], "2 files, 0 new", "{set}");
}

impl Node {
    /// The sync pair (or copy) as listed.
    async fn pair_view(&self, id: &str) -> Value {
        let (_, all) = self.call(Method::GET, "/sync", None).await;
        all.as_array().unwrap().iter().find(|p| p["id"] == id).unwrap().clone()
    }

    /// Asks for a full read-back of a copy and waits for it.
    async fn check_copy(&self, id: &str) -> Value {
        let before = self.pair_view(id).await["last_check_at"].as_i64().unwrap_or(0);
        let (s, v) = self.call(Method::POST, &format!("/sync/{id}/check"), None).await;
        assert_eq!(s, 202, "{v}");
        for _ in 0..400 {
            let p = self.pair_view(id).await;
            if p["last_check_at"].as_i64().unwrap_or(0) > before && p["checking"] == false {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("copy {id} was not checked");
    }

    /// Asks for a storage location to be verified and waits for it.
    async fn verify(&self, root: &str) -> Value {
        let before = |v: &Value| {
            v["roots"].as_array().unwrap().iter().find(|r| r["id"] == root).unwrap()["last_check"]["checked_at"].as_i64().unwrap_or(0)
        };
        let (_, h) = self.call(Method::GET, "/storage/health", None).await;
        let start = before(&h);
        let (s, v) = self.call(Method::POST, &format!("/storage/roots/{root}/verify"), None).await;
        assert_eq!(s, 202, "{v}");
        for _ in 0..400 {
            let (_, h) = self.call(Method::GET, "/storage/health", None).await;
            let r = h["roots"].as_array().unwrap().iter().find(|r| r["id"] == root).unwrap();
            if before(&h) > start && r["verifying"] == false {
                return h;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{root} was not verified");
    }
}

/// Writes new bytes over a file without changing its size or date, as
/// damage on a disk would.
fn damage(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    let when = std::fs::metadata(path).unwrap().modified().unwrap();
    let mut f = std::fs::File::options().write(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
    f.set_modified(when).unwrap();
}

#[tokio::test]
async fn extra_copies_keep_removed_files_and_repair_damage() {
    let pc = node("pc").await;
    let nas = node("nas").await;
    pc.pair_with(&nas).await;

    let docs = pc.storage.path().join("docs");
    old_file(&docs.join("a.txt"), b"alpha");
    old_file(&docs.join("sub").join("b.txt"), b"bravo");
    std::fs::create_dir_all(nas.storage.path().join("copy")).unwrap();
    let here = pc.add_root(pc.storage.path()).await;
    let there = nas.add_root(nas.storage.path()).await;

    let make = |keep: i64| {
        json!({"local_root_id": here, "local_path": "docs", "device_id": nas.device_id(), "remote_root_id": there,
               "remote_path": "copy", "mode": "copy", "keep_days": keep})
    };
    let (s, v) = pc.call(Method::POST, "/sync", Some(make(99999))).await;
    assert_eq!(s, 400, "{v}");
    let (s, pair) = pc.call(Method::POST, "/sync", Some(make(30))).await;
    assert_eq!(s, 201, "{pair}");
    let id = pair["id"].as_str().unwrap().to_string();
    let p = pc.sync_now(&id).await;
    assert!(p["last_error"].is_null(), "{p}");
    let copy = nas.storage.path().join("copy");
    assert_eq!(files_in(&copy), ["a.txt", "sub/b.txt"]);
    // A copy cannot be turned into a sync.
    let (s, _) = pc.call(Method::PATCH, &format!("/sync/{id}"), Some(json!({"mode": "two_way"}))).await;
    assert_eq!(s, 400);

    // Removed here: the copy keeps it, and it can be brought back.
    std::fs::remove_file(docs.join("a.txt")).unwrap();
    pc.state.indexer.reindex_path(&pc.state.db.get_root(&here).await.unwrap(), "docs/a.txt").await.unwrap();
    let p = pc.sync_now(&id).await;
    assert!(p["last_result"].as_str().unwrap().contains("1 removed here is kept on nas for 30 days"), "{p}");
    assert_eq!((p["held_files"].as_u64(), p["held_bytes"].as_i64()), (Some(1), Some(5)), "{p}");
    assert!(copy.join("a.txt").exists());
    let (_, held) = pc.call(Method::GET, &format!("/sync/{id}/held"), None).await;
    assert_eq!(held[0]["path"], "a.txt", "{held}");
    let (s, started) = pc.call(Method::POST, &format!("/sync/{id}/held/restore"), Some(json!({"paths": ["a.txt"]}))).await;
    assert_eq!(s, 202, "{started}");
    let job = pc.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!((job["status"].as_str(), job["files_done"].as_u64()), (Some("done"), Some(1)), "{job}");
    assert_eq!(std::fs::read(docs.join("a.txt")).unwrap(), b"alpha");
    let p = pc.sync_now(&id).await;
    assert_eq!(p["held_files"], 0, "{p}");

    // Damage on the copy is found by reading it back, and sent again.
    damage(&copy.join("sub").join("b.txt"), b"brXvo");
    let p = pc.check_copy(&id).await;
    let result = p["last_check_result"].as_str().unwrap();
    assert!(result.starts_with("2 files checked; 1 file damaged or missing on nas and sent again"), "{result}");
    assert_eq!(std::fs::read(copy.join("sub").join("b.txt")).unwrap(), b"bravo");
    let p = pc.check_copy(&id).await;
    assert_eq!(p["last_check_result"], "2 files checked, all intact", "{p}");

    // Damage here is found by verifying, and repaired from the copy.
    damage(&docs.join("sub").join("b.txt"), b"brav0");
    let health = pc.verify(&here).await;
    let root = health["roots"].as_array().unwrap().iter().find(|r| r["id"] == here).unwrap().clone();
    assert_eq!(root["last_check"]["damaged"], 1, "{root}");
    let bad = &root["damaged"][0];
    assert_eq!((bad["path"].as_str(), bad["unreadable"].as_bool()), (Some("docs/sub/b.txt"), Some(false)), "{bad}");
    assert_eq!(bad["repair_from"][0], "the copy on nas", "{bad}");
    assert!(health["problems"].as_array().unwrap().iter().any(|p| p["text"].as_str().unwrap().contains("damaged on the disk")), "{health}");
    let (s, started) = pc.call(Method::POST, &format!("/storage/health/damaged/{}/repair", bad["file_id"].as_str().unwrap()), None).await;
    assert_eq!(s, 202, "{started}");
    let job = pc.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!((job["status"].as_str(), job["files_done"].as_u64()), (Some("done"), Some(1)), "{job}");
    assert_eq!(std::fs::read(docs.join("sub").join("b.txt")).unwrap(), b"bravo");
    let (_, health) = pc.call(Method::GET, "/storage/health", None).await;
    let root = health["roots"].as_array().unwrap().iter().find(|r| r["id"] == here).unwrap().clone();
    assert_eq!(root["damaged"].as_array().unwrap().len(), 0, "{root}");

    // With nothing kept, a file removed here goes to the device's trash.
    let (s, v) = pc.call(Method::PATCH, &format!("/sync/{id}"), Some(json!({"keep_days": 0}))).await;
    assert_eq!(s, 200, "{v}");
    std::fs::remove_file(docs.join("a.txt")).unwrap();
    pc.state.indexer.reindex_path(&pc.state.db.get_root(&here).await.unwrap(), "docs/a.txt").await.unwrap();
    pc.sync_now(&id).await;
    assert!(!copy.join("a.txt").exists());
    assert_eq!(files_in(&copy), ["sub/b.txt"]);

    // Without the copy, a backup that holds the right content repairs it.
    let (s, _) = pc.call(Method::DELETE, &format!("/sync/{id}"), None).await;
    assert_eq!(s, 204);
    let (s, set) = pc
        .call(
            Method::POST,
            "/snapshots",
            Some(json!({"local_root_id": here, "local_path": "docs", "device_id": nas.device_id(), "remote_root_id": there,
                        "remote_path": "", "encrypt": false})),
        )
        .await;
    assert_eq!(s, 201, "{set}");
    let set = pc.backup_set_after(set["id"].as_str().unwrap(), "last_run_at", 0).await;
    assert!(set["last_error"].is_null(), "{set}");
    damage(&docs.join("sub").join("b.txt"), b"Bravo");
    let health = pc.verify(&here).await;
    let root = health["roots"].as_array().unwrap().iter().find(|r| r["id"] == here).unwrap().clone();
    let bad = &root["damaged"][0];
    assert_eq!(bad["repair_from"], json!([format!("the backup {}", set["name"].as_str().unwrap())]), "{bad}");
    let (s, started) = pc.call(Method::POST, &format!("/storage/health/damaged/{}/repair", bad["file_id"].as_str().unwrap()), None).await;
    assert_eq!(s, 202, "{started}");
    let job = pc.wait_job(started["job_id"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(std::fs::read(docs.join("sub").join("b.txt")).unwrap(), b"bravo");

    // Accepting a change instead: the file's current content becomes the right one.
    damage(&docs.join("sub").join("b.txt"), b"BRAVO");
    let health = pc.verify(&here).await;
    let root = health["roots"].as_array().unwrap().iter().find(|r| r["id"] == here).unwrap().clone();
    let (s, _) = pc.call(Method::POST, &format!("/storage/health/damaged/{}/accept", root["damaged"][0]["file_id"].as_str().unwrap()), None).await;
    assert_eq!(s, 204);
    let health = pc.verify(&here).await;
    let root = health["roots"].as_array().unwrap().iter().find(|r| r["id"] == here).unwrap().clone();
    assert_eq!(root["last_check"]["damaged"], 0, "{root}");
}
