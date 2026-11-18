//! Introductions: the devices paired with one node pair with each other
//! without scanning a code.
//!
//! Every node tells each device it paired with directly about its other
//! directly paired devices (name, key, addresses, and what that device may
//! do here). The receiving device adds them, trusted no further than the
//! device that introduced them:
//!
//! * its role there is the lesser of the role it has on the introducer and
//!   the role the introducer itself has there;
//! * it sees no more storage there than the introducer does;
//! * introduced devices never introduce others (one hop only);
//! * when the introducer stops listing a device (it was unpaired there), or
//!   the introducer itself is removed, the device is forgotten again;
//! * a device you remove yourself is not added again by an introduction.
//!
//! So an introduction never gives any device more than the introducer could
//! already do itself. Turning introductions off (`devices.introduce`) stops
//! both sending and accepting them.

use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::peers::{Acting, CALL_TIMEOUT, Target};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::State;
use axum::http::Method;
use futures_util::future::join_all;
use nexus_common::events::Event;
use nexus_common::types::{Device, Role};
use nexus_common::Result;
use nexus_database::NewPeer;
use nexus_security::Action;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Whether this node sends and accepts introductions. On by default.
pub const INTRODUCE_SETTING: &str = "devices.introduce";
/// Devices removed by hand here, which introductions must not add again.
const DECLINED_SETTING: &str = "devices.declined";

/// How often the list is looked at; an unchanged list is sent again only
/// after [`RESEND`].
const EVERY: Duration = Duration::from_secs(60);
const RESEND: Duration = Duration::from_secs(10 * 60);
const MAX_INTRODUCED: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Introduced {
    pub id: String,
    pub name: String,
    pub device_type: String,
    pub public_key: String,
    #[serde(default)]
    pub addresses: Vec<String>,
    /// What the device may do on the introducer.
    pub role: Role,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Introductions {
    pub devices: Vec<Introduced>,
}

pub async fn enabled(s: &AppState) -> bool {
    s.db.get_setting(INTRODUCE_SETTING).await.ok().flatten().map(|v| v != "0").unwrap_or(true)
}

async fn declined(s: &AppState) -> Vec<String> {
    s.db.get_setting(DECLINED_SETTING).await.ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

/// Remembers that a device was removed by hand, so introductions leave it
/// out from now on.
pub(crate) async fn decline(s: &AppState, device_id: &str) {
    let mut list = declined(s).await;
    if !list.iter().any(|d| d == device_id) {
        list.push(device_id.to_string());
        if let Ok(json) = serde_json::to_string(&list) {
            let _ = s.db.set_setting(DECLINED_SETTING, &json).await;
        }
    }
}

/// Lets a device be introduced again (it was paired directly).
pub(crate) async fn undecline(s: &AppState, device_id: &str) {
    let mut list = declined(s).await;
    let before = list.len();
    list.retain(|d| d != device_id);
    if list.len() != before
        && let Ok(json) = serde_json::to_string(&list)
    {
        let _ = s.db.set_setting(DECLINED_SETTING, &json).await;
    }
}

/// Forgets every device `by` introduced (it was removed here).
pub(crate) async fn forget_all_from(s: &AppState, by: &str) {
    if let Ok(gone) = s.db.forget_introduced(by, &[]).await {
        for id in gone {
            s.events.emit(Event::DeviceRemoved { device_id: id });
        }
    }
}

fn directly_paired(device: &Device) -> bool {
    !device.is_local && device.introduced_by.is_none() && device.public_key.is_some()
}

/// What `peer` should hear: every other device paired directly here.
fn list_for(peer: &Device, all: &[Device]) -> Introductions {
    let devices = all
        .iter()
        .filter(|d| directly_paired(d) && d.id != peer.id)
        .filter_map(|d| {
            Some(Introduced {
                id: d.id.clone(),
                name: d.name.clone(),
                device_type: d.device_type.clone(),
                public_key: d.public_key.clone()?,
                addresses: d.address_list(),
                role: d.role,
                version: d.version.clone(),
            })
        })
        .take(MAX_INTRODUCED)
        .collect();
    Introductions { devices }
}

/// Sends the current introductions to every online, directly paired device
/// whose list changed (or was last sent a while ago). `sent` remembers what
/// went where.
async fn introduce(s: &SharedState, sent: &mut HashMap<String, (u64, Instant)>) -> Result<()> {
    if !enabled(s).await {
        return Ok(());
    }
    let all = s.db.list_peer_devices().await?;
    let due: Vec<(Target, Introductions, u64)> = all
        .iter()
        .filter(|d| directly_paired(d) && d.status == "online")
        .filter_map(|d| {
            let list = list_for(d, &all);
            let digest = digest(&list);
            let fresh = sent.get(&d.id).is_some_and(|(h, at)| *h == digest && at.elapsed() < RESEND);
            if fresh { None } else { Some((Target::of(d).ok()?, list, digest)) }
        })
        .collect();
    let results = join_all(due.iter().map(|(target, list, _)| async move {
        s.peers
            .call::<serde_json::Value>(target, Method::POST, "/api/v1/peer/introductions", Some(list), Some(Acting::NODE), CALL_TIMEOUT)
            .await
    }))
    .await;
    for ((target, _, digest), result) in due.into_iter().zip(results) {
        match result {
            Ok(_) => {
                sent.insert(target.device_id, (digest, Instant::now()));
            }
            // An older version answers `unknown_route`; it is asked again
            // later, by when it may have been updated.
            Err(e) => tracing::debug!(device = %target.name, error = %e, "introductions not delivered"),
        }
    }
    Ok(())
}

fn digest(list: &Introductions) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(list).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// Runs introductions now and then, and soon after pairing changes.
pub fn spawn(s: SharedState) {
    tokio::spawn(async move {
        let mut sent = HashMap::new();
        tokio::time::sleep(Duration::from_secs(20)).await;
        loop {
            if let Err(e) = introduce(&s, &mut sent).await {
                tracing::debug!(error = %e, "introductions skipped");
            }
            tokio::select! {
                _ = s.introductions.notified() => {
                    // Pairing changed: send the new lists, even if a peer
                    // just had one.
                    sent.clear();
                    // Let the peer monitor see a new device online first.
                    tokio::time::sleep(Duration::from_secs(3)).await;
                }
                _ = tokio::time::sleep(EVERY) => {}
            }
        }
    });
}

/// Sends introductions now (tests).
pub async fn introduce_now(s: &SharedState) {
    let _ = introduce(s, &mut HashMap::new()).await;
}

// ---------------------------------------------------------------------------
// Receiving

#[derive(Serialize)]
pub struct Received {
    accepted: bool,
    added: usize,
    removed: usize,
}

/// `POST /peer/introductions`: a paired device lists the other devices it is
/// paired with.
pub async fn receive(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<Introductions>,
) -> ApiResult<Json<Received>> {
    let caller_id = user.require_device()?.to_string();
    let caller = s.db.get_device(&caller_id).await?;
    if !enabled(&s).await || caller.introduced_by.is_some() {
        return Ok(Json(Received { accepted: false, added: 0, removed: 0 }));
    }
    let declined = declined(&s).await;
    let roots = caller.allowed_root_list();
    let mut keep = Vec::new();
    let mut added = 0;
    for d in body.devices.into_iter().take(MAX_INTRODUCED) {
        if d.id == s.node.device_id || d.id == caller.id || declined.contains(&d.id) {
            continue;
        }
        let name = d.name.trim();
        if name.is_empty() || name.len() > 64 || d.id.len() > 64 || d.device_type.len() > 16 {
            continue;
        }
        if nexus_network::public_key(&d.public_key).is_none() {
            continue;
        }
        let addresses: Vec<String> = d.addresses.into_iter().filter(|a| nexus_network::valid_hint(a)).take(8).collect();
        // Never more than the introducer has here, and never an owner.
        let role = match d.role.least(caller.role) {
            Role::Owner => Role::Administrator,
            r => r,
        };
        let peer = NewPeer {
            id: &d.id,
            name,
            device_type: &d.device_type,
            public_key: &d.public_key,
            addresses: &addresses,
            role,
            version: d.version.as_deref(),
        };
        keep.push(d.id.clone());
        if s.db.upsert_introduced_device(&peer, &caller.id, roots.as_deref()).await? {
            added += 1;
            let _ = s.db.log_activity("device.introduced", Some(&format!("{name} by {}", caller.name)), None, Some(&caller.id)).await;
            tracing::info!(device = %name, by = %caller.name, "device introduced");
        }
        s.events.emit(Event::DeviceChanged { device_id: d.id.clone() });
    }
    let gone = s.db.forget_introduced(&caller.id, &keep).await?;
    for id in &gone {
        s.events.emit(Event::DeviceRemoved { device_id: id.clone() });
    }
    if added > 0 {
        s.net.peer_wake.notify_one();
    }
    Ok(Json(Received { accepted: true, added, removed: gone.len() }))
}

// ---------------------------------------------------------------------------
// Settings

#[derive(Serialize, Deserialize)]
pub struct IntroSettings {
    enabled: bool,
}

/// `GET /devices/introductions`
pub async fn settings(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<IntroSettings>> {
    user.require_person()?;
    Ok(Json(IntroSettings { enabled: enabled(&s).await }))
}

/// `PUT /devices/introductions`
pub async fn set_settings(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<IntroSettings>,
) -> ApiResult<Json<IntroSettings>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    s.db.set_setting(INTRODUCE_SETTING, if body.enabled { "1" } else { "0" }).await?;
    audit(&s, &user, if body.enabled { "devices.introduce.enabled" } else { "devices.introduce.disabled" }, None).await;
    if body.enabled {
        s.introductions.notify_one();
    } else {
        // Without introductions, devices that came from them go too.
        for d in s.db.list_peer_devices().await? {
            if d.introduced_by.is_some() && s.db.delete_peer_device(&d.id).await? {
                s.events.emit(Event::DeviceRemoved { device_id: d.id });
            }
        }
    }
    Ok(Json(body))
}
