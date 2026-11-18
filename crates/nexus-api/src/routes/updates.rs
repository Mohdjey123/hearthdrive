//! Updates that spread between your own devices.
//!
//! A release is published by dropping a signed package into one node's
//! update store (`<data>/updates/<platform>/`, see the `nexus-release`
//! tool). Every node asks its online paired devices what they hold, fetches
//! anything newer and keeps it, so a package reaches every device that can
//! reach one that already has it. Computers keep packages for every
//! platform, to pass on; phones only their own.
//!
//! Nothing is trusted on the way: a release must be signed with an update
//! key built into the app ([`nexus_devices::release`]), and the package must
//! match the signed size and BLAKE3 hash, both when it arrives and again
//! right before it is installed.
//!
//! Installing: on Windows this service starts the signed installer, which
//! stops the service and the app, replaces them, and starts them again.
//! With automatic installs on, that happens once the app's window is closed
//! and no copy or sync is running. On Android the app opens the system
//! installer, which always asks the person.

use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::peers::{Acting, CALL_TIMEOUT, TRANSFER_TIMEOUT, Target, peer_error, read_body};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::future::join_all;
use http_body_util::BodyExt;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::{NexusError, Result};
use nexus_devices::release::{self, PLATFORMS, Release, SignedRelease, is_newer};
use nexus_security::Action;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;

/// Whether this node installs updates by itself (Windows). On by default.
pub const AUTO_INSTALL_SETTING: &str = "updates.auto_install";

/// How often paired devices are asked for newer packages.
const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);
/// How often a waiting update is looked at for installing.
const TICK: Duration = Duration::from_secs(5 * 60);
/// The first check, once the device has had time to find its peers.
const FIRST_CHECK: Duration = Duration::from_secs(60);
/// An automatic install of the same version is not retried sooner.
const RETRY_INSTALL_MS: i64 = 6 * 60 * 60 * 1000;
/// A paired device listing more releases than this is ignored.
const MAX_LISTED: usize = 32;

// ---------------------------------------------------------------------------
// State

#[derive(Default)]
pub struct UpdateControl {
    wake: tokio::sync::Notify,
    /// One check at a time.
    checking: tokio::sync::Mutex<()>,
    status: Mutex<CheckStatus>,
    /// App windows connected to this node's live events.
    viewers: AtomicUsize,
    /// The last automatic install: version and when.
    last_install: Mutex<Option<(String, i64)>>,
}

#[derive(Default, Clone, Serialize)]
struct CheckStatus {
    checking: bool,
    /// `platform version` being fetched.
    downloading: Option<String>,
    last_checked: Option<i64>,
    error: Option<String>,
}

impl UpdateControl {
    /// Counts an app window as open until the returned guard is dropped.
    pub fn viewer(&self) -> ViewerGuard<'_> {
        self.viewers.fetch_add(1, Ordering::SeqCst);
        ViewerGuard(self)
    }

    fn set(&self, f: impl FnOnce(&mut CheckStatus)) {
        f(&mut self.status.lock().unwrap());
    }

    fn snapshot(&self) -> CheckStatus {
        self.status.lock().unwrap().clone()
    }
}

pub struct ViewerGuard<'a>(&'a UpdateControl);

impl Drop for ViewerGuard<'_> {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Platforms this node keeps packages for.
fn carried() -> Vec<&'static str> {
    if cfg!(target_os = "android") { release::this_platform().into_iter().collect() } else { PLATFORMS.to_vec() }
}

// ---------------------------------------------------------------------------
// The store

fn store_dir(data_dir: &FsPath, platform: &str) -> PathBuf {
    data_dir.join("updates").join(platform)
}

/// Where a platform's verified package is kept.
pub fn package_path(data_dir: &FsPath, platform: &str) -> PathBuf {
    let name = if platform.starts_with("windows") { "package.exe" } else { "package.apk" };
    store_dir(data_dir, platform).join(name)
}

fn release_path(data_dir: &FsPath, platform: &str) -> PathBuf {
    store_dir(data_dir, platform).join("release.json")
}

/// The stored release for `platform`, if a trusted key signed it and its
/// package is there at the signed size. (The hash is checked on arrival and
/// before installing.)
pub fn stored(data_dir: &FsPath, platform: &str, keys: &[String]) -> Option<(SignedRelease, Release)> {
    let text = std::fs::read(release_path(data_dir, platform)).ok()?;
    let signed: SignedRelease = serde_json::from_slice(&text).ok()?;
    let release = signed.verify(keys).filter(|r| r.platform == platform)?;
    let size = std::fs::metadata(package_path(data_dir, platform)).ok()?.len();
    (size == release.size).then_some((signed, release))
}

/// The BLAKE3 hash of a file, in hex.
fn hash_file(path: &FsPath) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(std::fs::File::open(path)?)?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Checks `staged` (a file inside the platform's store folder) against the
/// signed release and makes it the stored package. Blocking.
fn place(data_dir: &FsPath, signed: &SignedRelease, release: &Release, staged: &FsPath) -> Result<()> {
    let size = std::fs::metadata(staged)?.len();
    if size != release.size || hash_file(staged)? != release.blake3 {
        let _ = std::fs::remove_file(staged);
        return Err(NexusError::invalid("The package does not match its signed release"));
    }
    let release_file = release_path(data_dir, &release.platform);
    let tmp = release_file.with_extension("json.part");
    std::fs::write(&tmp, serde_json::to_vec_pretty(signed).map_err(NexusError::internal)?)?;
    // Without its release the old package is never offered half-replaced.
    let _ = std::fs::remove_file(&release_file);
    std::fs::rename(staged, package_path(data_dir, &release.platform))?;
    std::fs::rename(&tmp, &release_file)?;
    Ok(())
}

/// Adds a signed package to a node's store (the `nexus-release` tool).
/// Refuses anything not signed by `keys`, not matching its release, or not
/// newer than what the store has. Blocking.
pub fn import(data_dir: &FsPath, signed: &SignedRelease, package: &FsPath, keys: &[String]) -> Result<Release> {
    let release = signed.verify(keys).ok_or_else(|| NexusError::invalid("The release is not signed with a trusted update key"))?;
    if let Some((_, current)) = stored(data_dir, &release.platform, keys)
        && !is_newer(&release.version, &current.version)
    {
        return Err(NexusError::Conflict(format!(
            "The store already has {} {}",
            current.platform, current.version
        )));
    }
    let dir = store_dir(data_dir, &release.platform);
    std::fs::create_dir_all(&dir)?;
    let staged = dir.join("import.part");
    std::fs::copy(package, &staged)?;
    place(data_dir, signed, &release, &staged)?;
    Ok(release)
}

/// The newest version this node already has for `platform`: its stored
/// package, or for its own platform the running version if that is later.
fn have(s: &AppState, platform: &str) -> Option<String> {
    let stored = stored(&s.data_dir, platform, &s.update_keys).map(|(_, r)| r.version);
    let running = (release::this_platform() == Some(platform)).then(|| s.node.version.to_string());
    match (stored, running) {
        (Some(a), Some(b)) => Some(if is_newer(&a, &b) { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// The update for this device, if one newer than the running version is
/// stored.
fn waiting(s: &AppState) -> Option<Release> {
    let platform = release::this_platform()?;
    stored(&s.data_dir, platform, &s.update_keys).map(|(_, r)| r).filter(|r| is_newer(&r.version, s.node.version))
}

// ---------------------------------------------------------------------------
// Fetching from paired devices

/// Asks every online paired device what it holds, and fetches whatever is
/// newer than what this node has.
pub async fn check(s: &SharedState) {
    let Ok(_one) = s.updates.checking.try_lock() else { return };
    s.updates.set(|st| {
        st.checking = true;
        st.error = None;
    });
    s.events.emit(Event::UpdatesChanged);
    let result = fetch_newer(s).await;
    s.updates.set(|st| {
        st.checking = false;
        st.downloading = None;
        st.last_checked = Some(now_ms());
        st.error = result.err().map(|e| e.to_string());
    });
    s.events.emit(Event::UpdatesChanged);
}

async fn fetch_newer(s: &SharedState) -> Result<()> {
    let peers: Vec<Target> = s
        .db
        .list_peer_devices()
        .await?
        .iter()
        .filter(|d| d.status == "online")
        .filter_map(|d| Target::of(d).ok())
        .collect();
    let listed = join_all(peers.iter().map(|t| async move {
        let reply = s
            .peers
            .call::<Vec<SignedRelease>>(t, Method::GET, "/api/v1/peer/updates", None::<&()>, Some(Acting::NODE), CALL_TIMEOUT)
            .await;
        (t, reply)
    }))
    .await;

    // The newest trusted release per platform, and who has it.
    let platforms = carried();
    let mut best: HashMap<String, (SignedRelease, Release, Vec<&Target>)> = HashMap::new();
    for (target, reply) in listed {
        let Ok((list, _)) = reply else { continue };
        for signed in list.into_iter().take(MAX_LISTED) {
            let Some(release) = signed.verify(&s.update_keys) else { continue };
            if !platforms.contains(&release.platform.as_str()) {
                continue;
            }
            if have(s, &release.platform).is_some_and(|v| !is_newer(&release.version, &v)) {
                continue;
            }
            match best.get_mut(&release.platform) {
                Some(entry) if entry.1.version == release.version => entry.2.push(target),
                Some(entry) if !is_newer(&release.version, &entry.1.version) => {}
                _ => {
                    best.insert(release.platform.clone(), (signed, release, vec![target]));
                }
            }
        }
    }

    let mut last_error = None;
    for (signed, release, sources) in best.into_values() {
        s.updates.set(|st| st.downloading = Some(format!("{} {}", release.platform, release.version)));
        s.events.emit(Event::UpdatesChanged);
        let mut fetched = false;
        for target in sources {
            match download(s, target, &signed, &release).await {
                Ok(()) => {
                    tracing::info!(platform = %release.platform, version = %release.version, from = %target.name, "update received");
                    let _ = s.db.log_activity("update.received", Some(&format!("{} {}", release.platform, release.version)), None, Some(&target.device_id)).await;
                    fetched = true;
                    break;
                }
                Err(e) => {
                    tracing::warn!(platform = %release.platform, from = %target.name, error = %e, "could not fetch an update");
                    last_error = Some(e);
                }
            }
        }
        if fetched {
            s.events.emit(Event::UpdatesChanged);
        }
    }
    match last_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Streams a package from a paired device into the store, checking its
/// size as it arrives and its hash before keeping it.
async fn download(s: &AppState, target: &Target, signed: &SignedRelease, release: &Release) -> Result<()> {
    let path = format!("/api/v1/peer/updates/{}", release.platform);
    let (res, _) = s
        .peers
        .send(target, Method::GET, &path, HeaderMap::new(), Body::empty(), Some(Acting::NODE), TRANSFER_TIMEOUT)
        .await?;
    let status = res.status();
    if !status.is_success() {
        let bytes = read_body(res).await.unwrap_or_default();
        return Err(peer_error(&target.name, status, &bytes));
    }
    let dir = store_dir(&s.data_dir, &release.platform);
    tokio::fs::create_dir_all(&dir).await?;
    let staged = dir.join("download.part");
    let mut file = tokio::fs::File::create(&staged).await?;
    let mut body = res.into_body();
    let mut received = 0u64;
    let result: Result<()> = async {
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| NexusError::PeerUnreachable(format!("{}: {e}", target.name)))?;
            if let Ok(data) = frame.into_data() {
                received += data.len() as u64;
                if received > release.size {
                    return Err(NexusError::invalid("The package is larger than its signed release"));
                }
                file.write_all(&data).await?;
            }
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(e);
    }
    let (data_dir, signed, release) = (s.data_dir.clone(), signed.clone(), release.clone());
    tokio::task::spawn_blocking(move || place(&data_dir, &signed, &release, &staged))
        .await
        .map_err(NexusError::internal)?
}

// ---------------------------------------------------------------------------
// Installing (Windows)

/// How this device installs updates: `service` (this service starts the
/// installer), `app` (the app opens the system installer) or `none`, and
/// why not when it cannot.
fn install_method() -> (&'static str, Option<String>) {
    if cfg!(target_os = "android") {
        return ("app", None);
    }
    if !cfg!(windows) || release::this_platform().is_none() {
        return ("none", Some("Updates are not made for this system yet.".into()));
    }
    if cfg!(debug_assertions) {
        return ("none", Some("This is a development build; it is not updated.".into()));
    }
    let installed = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("uninstall.exe").is_file()));
    if installed != Some(true) {
        return ("none", Some("NEXUS was installed from the MSI package. Install the newest MSI to update.".into()));
    }
    ("service", None)
}

/// Checks the waiting package once more and starts its installer. With
/// `reopen`, the installer opens the app again when it finishes.
async fn start_installer(s: &AppState, reopen: bool) -> Result<Release> {
    if install_method().0 != "service" {
        return Err(NexusError::invalid(install_method().1.unwrap_or_else(|| "This device cannot install updates by itself".into())));
    }
    let release = waiting(s).ok_or_else(|| NexusError::NotFound("An update".into()))?;
    let package = package_path(&s.data_dir, &release.platform);
    let check = package.clone();
    let hash = tokio::task::spawn_blocking(move || hash_file(&check)).await.map_err(NexusError::internal)??;
    if hash != release.blake3 {
        let _ = std::fs::remove_file(&package);
        return Err(NexusError::invalid("The update package was changed on disk, so it was removed"));
    }
    let mut args = vec!["/P", "/UPDATE"];
    if reopen {
        args.push("/R");
    }
    launch_detached(&package, &args)?;
    tracing::info!(version = %release.version, reopen, "starting the update installer");
    Ok(release)
}

#[cfg(windows)]
fn launch_detached(program: &FsPath, args: &[&str]) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    // The installer stops this service, so it must not be tied to it.
    let spawn = |flags: u32| {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags)
            .spawn()
    };
    spawn(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
        .or_else(|_| spawn(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP))
        .map(|_| ())
        .map_err(|e| NexusError::internal(format!("Could not start the installer: {e}")))
}

#[cfg(not(windows))]
fn launch_detached(_program: &FsPath, _args: &[&str]) -> Result<()> {
    Err(NexusError::invalid("This device cannot install updates by itself"))
}

async fn auto_install_on(s: &AppState) -> bool {
    s.db.get_setting(AUTO_INSTALL_SETTING).await.ok().flatten().map(|v| v != "0").unwrap_or(true)
}

/// Installs a waiting update by itself when that is allowed and nothing
/// would be interrupted: no app window open, no copy or sync running.
async fn maybe_install(s: &AppState) {
    if install_method().0 != "service" || !auto_install_on(s).await {
        return;
    }
    let Some(release) = waiting(s) else { return };
    if s.updates.viewers.load(Ordering::SeqCst) > 0 || s.jobs.busy() {
        return;
    }
    {
        let mut last = s.updates.last_install.lock().unwrap();
        if last.as_ref().is_some_and(|(v, at)| *v == release.version && now_ms() - at < RETRY_INSTALL_MS) {
            return;
        }
        *last = Some((release.version.clone(), now_ms()));
    }
    match start_installer(s, false).await {
        Ok(r) => {
            let _ = s.db.log_activity("update.installing", Some(&r.version), None, Some(&s.node.device_id)).await;
        }
        Err(e) => tracing::warn!(error = %e, "could not install the update"),
    }
}

pub fn spawn(s: SharedState) {
    tokio::spawn(async move {
        // Leftovers from a download or import that did not finish.
        for platform in PLATFORMS {
            let dir = store_dir(&s.data_dir, platform);
            for name in ["download.part", "import.part"] {
                let _ = tokio::fs::remove_file(dir.join(name)).await;
            }
        }
        tokio::time::sleep(FIRST_CHECK).await;
        let mut last_check: Option<tokio::time::Instant> = None;
        loop {
            let woken = tokio::select! {
                _ = s.updates.wake.notified() => true,
                _ = tokio::time::sleep(if last_check.is_none() { Duration::ZERO } else { TICK }) => false,
            };
            if woken || last_check.is_none_or(|t| t.elapsed() >= CHECK_EVERY) {
                last_check = Some(tokio::time::Instant::now());
                check(&s).await;
            }
            maybe_install(&s).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Endpoints for paired devices

/// `GET /peer/updates`: the signed releases this node can pass on.
pub async fn peer_list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<SignedRelease>>> {
    user.require_device()?;
    let list = PLATFORMS.iter().filter_map(|p| stored(&s.data_dir, p, &s.update_keys).map(|(signed, _)| signed)).collect();
    Ok(Json(list))
}

/// `GET /peer/updates/{platform}`: a stored package.
pub async fn peer_package(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(platform): Path<String>,
    req: Request,
) -> ApiResult<Response> {
    user.require_device()?;
    if !PLATFORMS.contains(&platform.as_str()) || stored(&s.data_dir, &platform, &s.update_keys).is_none() {
        return Err(NexusError::NotFound("That update".into()).into());
    }
    let path = package_path(&s.data_dir, &platform);
    let response = tower_http::services::ServeFile::new_with_mime(&path, &"application/octet-stream".parse().unwrap())
        .oneshot(req)
        .await
        .map_err(NexusError::internal)?;
    Ok(response.into_response())
}

// ---------------------------------------------------------------------------
// Endpoints for this device's app

#[derive(Serialize)]
pub struct ReleaseView {
    platform: String,
    version: String,
    file: String,
    size: u64,
    notes: String,
    published_at: i64,
}

impl From<Release> for ReleaseView {
    fn from(r: Release) -> Self {
        Self { platform: r.platform, version: r.version, file: r.file, size: r.size, notes: r.notes, published_at: r.published_at }
    }
}

#[derive(Serialize)]
pub struct UpdateStatus {
    version: &'static str,
    platform: Option<&'static str>,
    /// A newer version for this device, ready to install.
    available: Option<ReleaseView>,
    /// `service`, `app` or `none`.
    install: &'static str,
    install_note: Option<String>,
    /// Phones: the verified package the app hands to Android's installer.
    package_path: Option<String>,
    auto_install: bool,
    can_configure: bool,
    #[serde(flatten)]
    check: CheckStatus,
    /// Packages this node keeps and passes on to other devices.
    carried: Vec<ReleaseView>,
}

async fn status_for(s: &AppState, user: &AuthUser) -> UpdateStatus {
    let (install, install_note) = install_method();
    let available = waiting(s);
    let package_path = (install == "app")
        .then_some(available.as_ref())
        .flatten()
        .map(|r| package_path(&s.data_dir, &r.platform).to_string_lossy().into_owned());
    UpdateStatus {
        version: s.node.version,
        platform: release::this_platform(),
        available: available.map(Into::into),
        install,
        install_note,
        package_path,
        auto_install: auto_install_on(s).await,
        can_configure: user.require(Action::ManageStorage).is_ok(),
        check: s.updates.snapshot(),
        carried: PLATFORMS.iter().filter_map(|p| stored(&s.data_dir, p, &s.update_keys)).map(|(_, r)| r.into()).collect(),
    }
}

/// `GET /updates`
pub async fn status(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<UpdateStatus>> {
    user.require_person()?;
    Ok(Json(status_for(&s, &user).await))
}

/// `POST /updates/check`: asks paired devices now (in the background).
pub async fn check_now(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<UpdateStatus>> {
    user.require_person()?;
    s.updates.set(|st| st.checking = true);
    s.updates.wake.notify_one();
    Ok(Json(status_for(&s, &user).await))
}

#[derive(Deserialize)]
pub struct Settings {
    auto_install: bool,
}

/// `PUT /updates/settings`
pub async fn set_settings(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<Settings>,
) -> ApiResult<Json<UpdateStatus>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    s.db.set_setting(AUTO_INSTALL_SETTING, if body.auto_install { "1" } else { "0" }).await?;
    audit(&s, &user, if body.auto_install { "updates.auto.enabled" } else { "updates.auto.disabled" }, None).await;
    Ok(Json(status_for(&s, &user).await))
}

/// `POST /updates/install`: installs the waiting update now and opens the
/// app again afterwards (Windows). Only from the computer itself.
pub async fn install(
    State(s): State<SharedState>,
    user: AuthUser,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> ApiResult<(StatusCode, Json<ReleaseView>)> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    if !peer.ip().is_loopback() {
        return Err(NexusError::PermissionDenied("Updates can only be installed on the device itself".into()).into());
    }
    let release = start_installer(&s, true).await?;
    audit(&s, &user, "update.installing", Some(&release.version)).await;
    Ok((StatusCode::ACCEPTED, Json(release.into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(secret: &str, version: &str, package: &[u8]) -> SignedRelease {
        let release = Release {
            platform: "android-aarch64".into(),
            version: version.into(),
            file: format!("NEXUS-Cloud_{version}_arm64.apk"),
            size: package.len() as u64,
            blake3: blake3::hash(package).to_hex().to_string(),
            notes: String::new(),
            published_at: 1,
        };
        SignedRelease::sign(&release, secret).unwrap()
    }

    #[test]
    fn the_store_keeps_only_signed_newer_matching_packages() {
        let (secret, public) = release::generate_key();
        let (stranger, _) = release::generate_key();
        let keys = vec![public];
        let data = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let file = src.path().join("app.apk");
        std::fs::write(&file, b"version two").unwrap();

        // Signed by someone else: refused.
        assert!(import(data.path(), &signed(&stranger, "0.2.0", b"version two"), &file, &keys).is_err());
        // Signed, but for different bytes: refused, nothing kept.
        assert!(import(data.path(), &signed(&secret, "0.2.0", b"something else"), &file, &keys).is_err());
        assert!(stored(data.path(), "android-aarch64", &keys).is_none());

        let release = import(data.path(), &signed(&secret, "0.2.0", b"version two"), &file, &keys).unwrap();
        assert_eq!(release.version, "0.2.0");
        let (_, kept) = stored(data.path(), "android-aarch64", &keys).unwrap();
        assert_eq!(kept.version, "0.2.0");
        assert_eq!(std::fs::read(package_path(data.path(), "android-aarch64")).unwrap(), b"version two");

        // The same or an older version is not taken again.
        std::fs::write(&file, b"version one").unwrap();
        assert!(import(data.path(), &signed(&secret, "0.1.0", b"version one"), &file, &keys).is_err());
        assert!(import(data.path(), &signed(&secret, "0.2.0", b"version one"), &file, &keys).is_err());

        // A package changed on disk is no longer offered.
        std::fs::write(package_path(data.path(), "android-aarch64"), b"version 2!!").unwrap();
        assert!(stored(data.path(), "android-aarch64", &keys).is_some(), "same size: caught by the hash check");
        assert_ne!(hash_file(&package_path(data.path(), "android-aarch64")).unwrap(), kept.blake3);
        std::fs::write(package_path(data.path(), "android-aarch64"), b"short").unwrap();
        assert!(stored(data.path(), "android-aarch64", &keys).is_none());
    }
}
