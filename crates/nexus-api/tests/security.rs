//! Two-step sign-in, passkeys, sessions and locked accounts, end to end.

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
use nexus_security::totp;
use nexus_security::webauthn::soft::SoftPasskey;
use nexus_security::webauthn::unb64;
use nexus_storage::StorageManager;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

const LOCAL_TOKEN: &str = "test-local-token";
/// Where a browser on this computer would have the web interface open.
const PAGE: &str = "http://localhost:47100";

struct Harness {
    app: Router,
    _data: tempfile::TempDir,
}

async fn harness() -> Harness {
    let data = tempfile::tempdir().unwrap();
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
    ));
    let app = nexus_api::router(state, None).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50000))));
    Harness { app, _data: data }
}

/// A request from a page at `page` (or from a script at 127.0.0.1).
async fn send(app: &Router, method: Method, uri: &str, token: Option<&str>, body: Option<Value>, page: Option<&str>) -> (StatusCode, Value) {
    let host = page.map_or("127.0.0.1:47100", |p| p.trim_start_matches("http://"));
    let mut req = Request::builder().method(method).uri(format!("/api/v1{uri}")).header(header::HOST, host);
    if let Some(p) = page {
        req = req.header(header::ORIGIN, p);
    }
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

async fn call(app: &Router, method: Method, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    send(app, method, uri, token, body, None).await
}

fn unbase32(text: &str) -> Vec<u8> {
    let (mut buffer, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => panic!("not base32"),
        };
        buffer = (buffer << 5) | u32::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    out
}

fn code(secret: &[u8], steps_from_now: i64) -> String {
    let now = nexus_common::time::now_ms() / 1000 / totp::STEP_SECS;
    format!("{:06}", totp::code_at(secret, now + steps_from_now))
}

fn s(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_else(|| panic!("no {key} in {v}")).to_string()
}

/// Signs in with `key`, then sends the same answer again.
async fn passkey_sign_in(app: &Router, key: &mut SoftPasskey) -> ((StatusCode, Value), (StatusCode, Value)) {
    let (st, v) = send(app, Method::POST, "/auth/passkey/start", None, None, Some(PAGE)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let challenge = s(&v, "challenge");
    let a = key.sign(&unb64(&challenge).unwrap(), PAGE);
    let body = json!({"challenge": challenge, "credential_id": a.credential_id, "client_data": a.client_data,
        "authenticator_data": a.authenticator_data, "signature": a.signature, "user_handle": a.user_handle});
    let first = send(app, Method::POST, "/auth/passkey", None, Some(body.clone()), Some(PAGE)).await;
    let replay = send(app, Method::POST, "/auth/passkey", None, Some(body), Some(PAGE)).await;
    (first, replay)
}

#[tokio::test]
async fn two_step_sign_in_passkeys_sessions_and_locks() {
    let h = harness().await;
    let app = &h.app;
    let (st, v) = call(app, Method::POST, "/setup", None, Some(json!({"node_name": "Desk", "username": "owner", "password": "owner password 1"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let owner = s(&v, "token");
    let (st, v) = call(app, Method::POST, "/users", Some(&owner), Some(json!({"username": "anna", "password": "anna password 1", "role": "editor"}))).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let anna_id = s(&v, "id");
    let login = |password: &'static str| async move {
        call(app, Method::POST, "/auth/login", None, Some(json!({"username": "anna", "password": password}))).await
    };

    // No two-step sign-in yet: the password is enough.
    let (st, v) = login("anna password 1").await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let first_session = s(&v, "token");

    // Setting it up asks for the password, then a code from the app.
    let (st, _) = call(app, Method::POST, "/auth/2fa/setup", Some(&first_session), Some(json!({"password": "wrong"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(app, Method::POST, "/auth/2fa/setup", Some(&first_session), Some(json!({"password": "anna password 1"}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(s(&v, "uri").starts_with("otpauth://totp/NEXUS%20%28Desk%29:anna?secret="));
    let secret = unbase32(&s(&v, "secret"));
    let (st, _) = call(app, Method::POST, "/auth/2fa/enable", Some(&first_session), Some(json!({"code": "000000"}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let first_code = code(&secret, -1);
    let (st, v) = call(app, Method::POST, "/auth/2fa/enable", Some(&first_session), Some(json!({"code": first_code}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let recovery: Vec<String> = v["recovery_codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
    assert_eq!(recovery.len(), 10);

    // Now the password only leads to the code step.
    let (st, v) = login("anna password 1").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["two_factor"], true);
    assert!(v["token"].is_null());
    let ticket = s(&v, "ticket");
    let with_code = |ticket: String, code: String| async move {
        call(app, Method::POST, "/auth/login/code", None, Some(json!({"ticket": ticket, "code": code}))).await
    };
    let (st, _) = with_code(ticket.clone(), first_code.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a code works only once");
    let (st, _) = with_code(format!("{}x", ticket), code(&secret, 0)).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "a forged ticket is refused");
    let (st, v) = with_code(ticket.clone(), code(&secret, 0)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = with_code(ticket.clone(), recovery[0].to_uppercase()).await;
    assert_eq!(st, StatusCode::OK, "recovery codes work, in any case: {v}");
    let anna = s(&v, "token");
    let (st, _) = with_code(ticket.clone(), recovery[0].clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "recovery codes work once");

    // Her sessions, and signing out everywhere else.
    let (st, v) = call(app, Method::GET, "/auth/security", Some(&anna), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["two_factor"], true);
    assert_eq!(v["recovery_codes_left"], 9);
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 3);
    assert_eq!(sessions.iter().filter(|x| x["current"] == true).count(), 1);
    assert_eq!(sessions.iter().filter(|x| x["method"] == "password+recovery").count(), 1);
    assert!(sessions.iter().all(|x| x.get("token_hash").is_none()));
    let (st, v) = call(app, Method::POST, "/auth/sessions/end-others", Some(&anna), None).await;
    assert_eq!((st, v["ended"].as_i64()), (StatusCode::OK, Some(2)));
    let (st, _) = call(app, Method::GET, "/auth/me", Some(&first_session), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Adding a passkey: only from a page passkeys work on, and only after
    // the password and a code.
    let (st, _) = call(app, Method::POST, "/auth/passkeys/start", Some(&anna), Some(json!({"password": "anna password 1", "code": recovery[1]}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "127.0.0.1 cannot use passkeys");
    let (st, v) = send(app, Method::POST, "/auth/passkeys/start", Some(&anna), Some(json!({"password": "anna password 1"})), Some(PAGE)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a code is needed: {v}");
    let (st, v) = send(app, Method::POST, "/auth/passkeys/start", Some(&anna), Some(json!({"password": "anna password 1", "code": recovery[1]})), Some(PAGE)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["rp"]["id"], "localhost");
    let user_handle = unb64(&s(&v["user"], "id")).unwrap();
    assert_eq!(user_handle, anna_id.as_bytes());
    let challenge = unb64(&s(&v, "challenge")).unwrap();
    let mut key = SoftPasskey::new(&user_handle);
    let made = key.create(&challenge, PAGE);
    let new_passkey = json!({"challenge": s(&v, "challenge"), "client_data": made.client_data, "attestation_object": made.attestation_object, "name": "Laptop"});
    let (st, v) = send(app, Method::POST, "/auth/passkeys", Some(&anna), Some(new_passkey.clone()), Some(PAGE)).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["name"], "Laptop");
    let (st, _) = send(app, Method::POST, "/auth/passkeys", Some(&anna), Some(new_passkey), Some(PAGE)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "a challenge is answered once");

    // Signing in with it, no password or code.
    let ((st, v), (replay, _)) = passkey_sign_in(app, &mut key).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["user"]["username"], "anna");
    assert_eq!(replay, StatusCode::FORBIDDEN, "a sign-in cannot be replayed");
    let by_passkey = s(&v, "token");
    let (st, v) = call(app, Method::GET, "/auth/security", Some(&by_passkey), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["passkeys"].as_array().unwrap().len(), 1);
    assert!(v["passkeys"][0]["last_used_at"].is_i64());

    // People shows how she signs in.
    let (st, v) = call(app, Method::GET, "/users", Some(&owner), None).await;
    assert_eq!(st, StatusCode::OK);
    let row = v.as_array().unwrap().iter().find(|u| u["username"] == "anna").unwrap().clone();
    assert_eq!((row["two_factor"].clone(), row["passkeys"].clone(), row["locked_at"].clone()), (json!(true), json!(1), Value::Null));

    // Locking signs her out and keeps her out, whichever way she signs in.
    let (st, _) = call(app, Method::PATCH, &format!("/users/{anna_id}"), Some(&anna), Some(json!({"locked": true}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "editors cannot lock accounts");
    let (_, me) = call(app, Method::GET, "/auth/me", Some(&owner), None).await;
    let (st, _) = call(app, Method::PATCH, &format!("/users/{}", s(&me, "user_id")), Some(&owner), Some(json!({"locked": true}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "no one locks themselves out");
    let (st, v) = call(app, Method::PATCH, &format!("/users/{anna_id}"), Some(&owner), Some(json!({"locked": true}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["locked_at"].is_i64());
    for token in [&anna, &by_passkey] {
        let (st, _) = call(app, Method::GET, "/auth/me", Some(token), None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }
    let (st, v) = login("anna password 1").await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(v["error"]["message"].as_str().unwrap().contains("locked"), "{v}");
    let ((st, _), _) = passkey_sign_in(app, &mut key).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Unlocked, and two-step sign-in turned off for her (she lost her phone).
    let (st, v) = call(app, Method::PATCH, &format!("/users/{anna_id}"), Some(&owner), Some(json!({"locked": false, "two_factor": false}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!((v["two_factor"].clone(), v["locked_at"].clone()), (json!(false), Value::Null));
    let (st, v) = login("anna password 1").await;
    assert_eq!(st, StatusCode::OK);
    let anna = s(&v, "token");

    // The security log: everything for the owner, her own entries for her.
    let (st, v) = call(app, Method::GET, "/auth/security/log", Some(&owner), None).await;
    assert_eq!(st, StatusCode::OK);
    let actions: Vec<&str> = v.as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    for expected in ["auth.failed", "auth.2fa.enabled", "auth.passkey.added", "user.locked", "user.unlocked", "user.2fa.reset", "auth.session.ended"] {
        assert!(actions.contains(&expected), "{expected} missing from {actions:?}");
    }
    let failed = v.as_array().unwrap().iter().filter(|e| e["action"] == "auth.failed").map(|e| s(e, "resource")).collect::<Vec<_>>();
    assert!(failed.iter().any(|r| r == "account locked") && failed.iter().any(|r| r == "wrong code"), "{failed:?}");
    let (st, v) = call(app, Method::GET, "/auth/security/log", Some(&anna), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v.as_array().unwrap().iter().all(|e| e["username"] == "anna"), "{v}");
    assert!(!v.as_array().unwrap().is_empty());

    // Unknown names are logged as typed; guessing is slowed down.
    let (st, _) = call(app, Method::POST, "/auth/login", None, Some(json!({"username": "mallory", "password": "x"}))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (_, v) = call(app, Method::GET, "/auth/security/log?limit=1", Some(&owner), None).await;
    assert_eq!(v[0]["resource"], "wrong password (\"mallory\")");

    // Passkeys are removed one by one.
    let (_, v) = call(app, Method::GET, "/auth/security", Some(&anna), None).await;
    let passkey_id = s(&v["passkeys"][0], "id");
    let (st, _) = call(app, Method::DELETE, &format!("/auth/passkeys/{passkey_id}"), Some(&owner), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "only your own");
    let (st, _) = call(app, Method::DELETE, &format!("/auth/passkeys/{passkey_id}"), Some(&anna), None).await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let ((st, _), _) = passkey_sign_in(app, &mut key).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}
