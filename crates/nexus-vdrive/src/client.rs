//! Reads devices, storage locations, folders and file content from the local
//! node, through the same `/api/v1` calls the app's Files page makes: paired
//! devices are reached through the node's peer proxy, so the drive shows
//! exactly what the app shows.

use crate::tree::{Child, Identity, LOCAL, assign_names};
use axum::body::Body;
use axum::http::{HeaderValue, Request, Response, StatusCode, header};
use futures_util::future::BoxFuture;
use http_body_util::{BodyExt, Limited};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Sends one request to the node's API and returns its response.
pub type Responder = Arc<dyn Fn(Request<Body>) -> BoxFuture<'static, Response<Body>> + Send + Sync>;

/// Largest JSON reply read (a page of a large folder listing).
const MAX_JSON: usize = 64 * 1024 * 1024;
/// Items per listing request (the API's maximum).
const PAGE: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// The device, or the drive the files are on, cannot be reached now.
    Offline(String),
    /// The item no longer exists.
    NotFound(String),
    Other(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Offline(m) | ClientError::NotFound(m) | ClientError::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ClientError {}

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct RootStats {
    #[serde(default)]
    pub files: i64,
    #[serde(default)]
    pub dirs: i64,
    #[serde(default)]
    pub bytes: i64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct ScanInfo {
    #[serde(default)]
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RootInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub online: bool,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub stats: RootStats,
    #[serde(default)]
    pub scan: Option<ScanInfo>,
}

impl RootInfo {
    /// Changes whenever the device's index of this location changes in a
    /// way that can be seen from outside (files added, removed or resized,
    /// or a scan finished).
    pub fn fingerprint(&self) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.online,
            self.stats.files,
            self.stats.dirs,
            self.stats.bytes,
            self.scan.as_ref().and_then(|s| s.completed_at).unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EntryInfo {
    pub id: String,
    pub relative_path: String,
    pub filename: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub modified_at: Option<i64>,
}

#[derive(Deserialize)]
struct ListPage {
    items: Vec<EntryInfo>,
    total: i64,
}

#[derive(Deserialize)]
struct ErrorReply {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// Content links are reused this long (the device honours them for six
/// hours).
const TICKET_REUSE: std::time::Duration = std::time::Duration::from_secs(4 * 60 * 60);

#[derive(Deserialize)]
struct LinkReply {
    url: String,
}

/// Signed content links by (device, file): the link and when it was issued.
type Tickets = HashMap<(String, String), (String, Instant)>;

/// Talks to the local node as its owner.
#[derive(Clone)]
pub struct NodeClient {
    send: Responder,
    token: Arc<str>,
    /// Signed content links by device and file, with when they were issued.
    tickets: Arc<std::sync::Mutex<Tickets>>,
}

pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Where a device's API is reached: this computer's own, or a paired
/// device's through the node's proxy.
fn peer(dev: &str) -> String {
    if dev == LOCAL { "/api/v1".to_string() } else { format!("/api/v1/peers/{}/proxy", encode(dev)) }
}

/// The section folders at the top of the drive.
pub const MY_FILES: &str = "My Files";
pub const SHARED_WITH_ME: &str = "Shared With Me";

#[derive(Debug, Clone, Deserialize)]
pub struct SharedRoot {
    pub id: String,
}

/// A folder another device shared with this one.
#[derive(Debug, Clone, Deserialize)]
pub struct SharedFolderInfo {
    pub id: String,
    pub name: String,
    pub root: SharedRoot,
    pub path: String,
    #[serde(default)]
    pub sees_all: bool,
    #[serde(default)]
    pub access: String,
}

/// What a device answers when asked what changed.
#[derive(Debug, Clone, Deserialize)]
pub struct Changes {
    pub cursor: String,
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default)]
    pub reset: bool,
}

#[derive(Deserialize)]
struct DetailsReply {
    #[serde(flatten)]
    entry: EntryInfo,
}

/// Turns an error reply into the matching error.
async fn error_of(res: Response<Body>) -> ClientError {
    let status = res.status();
    let body = Limited::new(res.into_body(), 64 * 1024).collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let (code, message) = match serde_json::from_slice::<ErrorReply>(&body) {
        Ok(r) => (r.error.code, r.error.message),
        Err(_) => (String::new(), String::from_utf8_lossy(&body).trim().to_string()),
    };
    let message = if message.is_empty() { format!("The node answered {status}") } else { message };
    match status {
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT => {
            ClientError::Offline(message)
        }
        _ if code == "peer_unreachable" || code == "unavailable" => ClientError::Offline(message),
        StatusCode::NOT_FOUND => ClientError::NotFound(message),
        _ => ClientError::Other(message),
    }
}

impl NodeClient {
    pub fn new(send: Responder, token: &str) -> Self {
        Self { send, token: token.into(), tickets: Default::default() }
    }

    async fn request(&self, path: &str, range: Option<(u64, u64)>) -> Result<Response<Body>> {
        self.send_request(axum::http::Method::GET, path, range).await
    }

    async fn send_request(&self, method: axum::http::Method, path: &str, range: Option<(u64, u64)>) -> Result<Response<Body>> {
        self.send_body(method, path, range, None, Body::empty()).await
    }

    async fn send_body(
        &self,
        method: axum::http::Method,
        path: &str,
        range: Option<(u64, u64)>,
        content: Option<(&str, Option<u64>)>,
        body: Body,
    ) -> Result<Response<Body>> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1")
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some((kind, len)) = content {
            builder = builder.header(header::CONTENT_TYPE, kind);
            if let Some(len) = len {
                builder = builder.header(header::CONTENT_LENGTH, len);
            }
        }
        let mut req = builder.body(body).map_err(|e| ClientError::Other(format!("invalid request: {e}")))?;
        if let Some((start, len)) = range {
            let end = start + len.max(1) - 1;
            let value = HeaderValue::from_str(&format!("bytes={start}-{end}")).expect("a valid range header");
            req.headers_mut().insert(header::RANGE, value);
        }
        let res = (self.send)(req).await;
        if res.status().is_success() { Ok(res) } else { Err(error_of(res).await) }
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let res = self.request(path, None).await?;
        let body = Limited::new(res.into_body(), MAX_JSON)
            .collect()
            .await
            .map_err(|e| ClientError::Offline(format!("The reply was cut off: {e}")))?
            .to_bytes();
        serde_json::from_slice(&body).map_err(|e| ClientError::Other(format!("Unexpected reply from the node: {e}")))
    }

    /// Paired devices (not this computer).
    pub async fn devices(&self) -> Result<Vec<DeviceInfo>> {
        let all: Vec<DeviceInfo> = self.get_json("/api/v1/devices").await?;
        Ok(all.into_iter().filter(|d| !d.is_local && d.public_key.is_some()).collect())
    }

    /// This computer (as [`LOCAL`]) and the paired devices.
    pub async fn all_devices(&self) -> Result<Vec<DeviceInfo>> {
        let all: Vec<DeviceInfo> = self.get_json("/api/v1/devices").await?;
        Ok(all
            .into_iter()
            .filter(|d| d.is_local || d.public_key.is_some())
            .map(|mut d| {
                if d.is_local {
                    d.id = LOCAL.to_string();
                    d.online = true;
                    d.created_at = Some(0);
                }
                d
            })
            .collect())
    }

    /// Folders a device shared with this one (not those in storage this
    /// device sees whole anyway).
    pub async fn shared_with_me(&self, dev: &str) -> Result<Vec<SharedFolderInfo>> {
        let all: Vec<SharedFolderInfo> = self.get_json(&format!("{}/shared-folders/mine", peer(dev))).await?;
        Ok(all.into_iter().filter(|f| !f.sees_all).collect())
    }

    /// Waits up to `wait` seconds for a device's storage to change.
    pub async fn changes(&self, dev: &str, since: Option<&str>, wait: u64) -> Result<Changes> {
        let since = since.map(|s| format!("&since={}", encode(s))).unwrap_or_default();
        self.get_json(&format!("{}/storage/changes?wait={wait}{since}", peer(dev))).await
    }

    async fn details(res: Response<Body>) -> Result<EntryInfo> {
        let body = Limited::new(res.into_body(), 1024 * 1024)
            .collect()
            .await
            .map_err(|e| ClientError::Offline(format!("The reply was cut off: {e}")))?
            .to_bytes();
        serde_json::from_slice::<DetailsReply>(&body)
            .map(|d| d.entry)
            .map_err(|e| ClientError::Other(format!("Unexpected reply from the device: {e}")))
    }

    /// Moves a file or folder to the device's trash.
    pub async fn delete(&self, dev: &str, file_id: &str) -> Result<()> {
        self.send_request(axum::http::Method::DELETE, &format!("{}/files/{}", peer(dev), encode(file_id)), None).await?;
        Ok(())
    }

    /// Renames or moves a file or folder inside its storage location.
    pub async fn rename(&self, dev: &str, file_id: &str, name: &str, parent_path: &str) -> Result<EntryInfo> {
        let body = serde_json::json!({ "name": name, "parent_path": parent_path }).to_string();
        let res = self
            .send_body(
                axum::http::Method::PATCH,
                &format!("{}/files/{}", peer(dev), encode(file_id)),
                None,
                Some(("application/json", Some(body.len() as u64))),
                Body::from(body),
            )
            .await?;
        Self::details(res).await
    }

    /// Makes a folder.
    pub async fn create_folder(&self, dev: &str, root: &str, parent: &str, name: &str) -> Result<EntryInfo> {
        let body = serde_json::json!({ "root_id": root, "path": parent, "name": name }).to_string();
        let res = self
            .send_body(
                axum::http::Method::POST,
                &format!("{}/files/folder", peer(dev)),
                None,
                Some(("application/json", Some(body.len() as u64))),
                Body::from(body),
            )
            .await?;
        Self::details(res).await
    }

    /// Uploads a file's content, replacing what is there (the device keeps
    /// the old content as a version) or, with `replace` false, as a new file.
    pub async fn upload(&self, dev: &str, root: &str, parent: &str, name: &str, len: u64, body: Body, replace: bool) -> Result<EntryInfo> {
        let path = format!(
            "{}/files/upload?root_id={}&path={}&name={}&on_conflict={}",
            peer(dev),
            encode(root),
            encode(parent),
            encode(name),
            if replace { "replace" } else { "rename" }
        );
        let res = self
            .send_body(axum::http::Method::POST, &path, None, Some(("application/octet-stream", Some(len))), body)
            .await?;
        Self::details(res).await
    }

    /// A paired device's storage locations.
    pub async fn roots(&self, dev: &str) -> Result<Vec<RootInfo>> {
        let all: Vec<RootInfo> = self.get_json(&format!("{}/storage/roots", peer(dev))).await?;
        Ok(all.into_iter().filter(|r| r.enabled != Some(false)).collect())
    }

    /// Everything in one folder of a paired device's storage location.
    pub async fn list(&self, dev: &str, root: &str, path: &str) -> Result<Vec<EntryInfo>> {
        let mut items = Vec::new();
        loop {
            let page: ListPage = self
                .get_json(&format!(
                    "{}/files?root_id={}&path={}&sort=name&offset={}&limit={PAGE}",
                    peer(dev),
                    encode(root),
                    encode(path),
                    items.len()
                ))
                .await?;
            let got = page.items.len();
            items.extend(page.items);
            if got == 0 || items.len() as i64 >= page.total {
                return Ok(items);
            }
        }
    }

/// A signed link to a file's content on its device (devices accept
    /// content requests with a link they issued, as the app's previews do).
    async fn content_path(&self, dev: &str, file_id: &str, fresh: bool) -> Result<String> {
        let key = (dev.to_string(), file_id.to_string());
        if !fresh {
            let tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((url, at)) = tickets.get(&key)
                && at.elapsed() < TICKET_REUSE
            {
                return Ok(url.clone());
            }
        }
        let res = self
            .send_request(axum::http::Method::POST, &format!("{}/files/{}/link", peer(dev), encode(file_id)), None)
            .await?;
        let body = Limited::new(res.into_body(), 64 * 1024)
            .collect()
            .await
            .map_err(|e| ClientError::Offline(format!("The reply was cut off: {e}")))?
            .to_bytes();
        let reply: LinkReply = serde_json::from_slice(&body)
            .map_err(|e| ClientError::Other(format!("Unexpected reply from the device: {e}")))?;
        let rest = reply.url.strip_prefix("/api/v1").ok_or_else(|| ClientError::Other("Unexpected link".into()))?;
        let url = format!("{}{rest}", peer(dev));
        let mut tickets = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        tickets.retain(|_, (_, at)| at.elapsed() < TICKET_REUSE);
        tickets.insert(key, (url.clone(), Instant::now()));
        Ok(url)
    }

    /// Streams `len` bytes of a file from `offset`. Returns the file's total
    /// size as the device reports it, and the bytes.
    pub async fn read(&self, dev: &str, file_id: &str, offset: u64, len: u64) -> Result<(Option<u64>, Body)> {
        let path = self.content_path(dev, file_id, false).await?;
        let res = match self.request(&path, Some((offset, len))).await {
            // The device restarted (its links with it): get a new one.
            Err(ClientError::Other(_)) => {
                let path = self.content_path(dev, file_id, true).await?;
                self.request(&path, Some((offset, len))).await?
            }
            other => other?,
        };
        let total = match res.status() {
            StatusCode::PARTIAL_CONTENT => res
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit_once('/'))
                .and_then(|(_, total)| total.parse().ok()),
            // The whole file: only right when it was asked for from the start.
            _ if offset == 0 => res
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok()),
            status => return Err(ClientError::Other(format!("The device did not send part of the file ({status})"))),
        };
        Ok((total, res.into_body()))
    }

    /// What a folder of the drive contains: the two sections at the top,
    /// devices (or shared folders) in them, then storage locations, then files.
    pub async fn children(&self, of: Option<&Identity>) -> Result<Vec<Child>> {
        let items = match of {
            None => vec![
                (MY_FILES.to_string(), Identity::Section { s: "mine".into() }, Some(0)),
                (SHARED_WITH_ME.to_string(), Identity::Section { s: "shared".into() }, Some(0)),
            ],
            Some(Identity::Section { s }) if s == "mine" => {
                let mut devices = self.all_devices().await?;
                devices.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
                devices
                    .into_iter()
                    .map(|d| (d.name, Identity::Device { dev: d.id }, d.created_at))
                    .collect()
            }
            Some(Identity::Section { .. }) => {
                let mut out = Vec::new();
                for device in self.devices().await?.into_iter().filter(|d| d.online) {
                    // A device that cannot be asked now keeps nothing here
                    // until it can; its folders come back with it.
                    let Ok(folders) = self.shared_with_me(&device.id).await else { continue };
                    for f in folders {
                        let identity = Identity::Entry {
                            dev: device.id.clone(),
                            root: f.root.id,
                            path: f.path,
                            id: f.id,
                            dir: true,
                            size: 0,
                            mtime: 0,
                        };
                        out.push((format!("{} ({})", f.name, device.name), identity, device.created_at));
                    }
                }
                out
            }
            Some(Identity::Device { dev }) => {
                let mut roots = self.roots(dev).await?;
                roots.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
                roots
                    .into_iter()
                    .map(|r| (r.name, Identity::Root { dev: dev.clone(), root: r.id }, r.created_at))
                    .collect()
            }
            Some(folder) => {
                let (Some(root), Some(path)) = (folder.root(), folder.folder_path()) else {
                    return Ok(Vec::new());
                };
                let dev = folder.device();
                let mut entries = self.list(dev, root, path).await?;
                // Folders first, then by name, so clashing names are numbered
                // the same way every time.
                entries.sort_by(|a, b| (!a.is_dir, &a.filename, &a.relative_path).cmp(&(!b.is_dir, &b.filename, &b.relative_path)));
                entries
                    .into_iter()
                    .map(|e| {
                        let identity = Identity::Entry {
                            dev: dev.to_string(),
                            root: root.to_string(),
                            path: e.relative_path,
                            id: e.id,
                            dir: e.is_dir,
                            size: if e.is_dir { 0 } else { e.size.max(0) as u64 },
                            mtime: e.modified_at.unwrap_or_default(),
                        };
                        (e.filename, identity, e.created_at)
                    })
                    .collect()
            }
        };
        Ok(assign_names(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A node that answers from a table and records what was asked.
    fn fake(routes: Vec<(&'static str, StatusCode, &'static str)>) -> (NodeClient, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let send: Responder = Arc::new(move |req: Request<Body>| {
            let uri = req.uri().to_string();
            assert_eq!(req.headers()[header::AUTHORIZATION], "Bearer secret");
            log.lock().unwrap().push(uri.clone());
            let found = routes.iter().find(|(prefix, ..)| uri.starts_with(prefix)).cloned();
            Box::pin(async move {
                match found {
                    Some((_, status, body)) => Response::builder().status(status).body(Body::from(body)).unwrap(),
                    None => Response::builder().status(404).body(Body::from("{}")).unwrap(),
                }
            })
        });
        (NodeClient::new(send, "secret"), seen)
    }

    #[tokio::test]
    async fn the_top_holds_two_sections() {
        let (client, seen) = fake(vec![]);
        let children = client.children(None).await.unwrap();
        let names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, [MY_FILES, SHARED_WITH_ME]);
        assert!(seen.lock().unwrap().is_empty(), "nothing to ask for the top");
    }

    #[tokio::test]
    async fn my_files_lists_this_computer_and_paired_devices() {
        let (client, _) = fake(vec![(
            "/api/v1/devices",
            StatusCode::OK,
            r#"[{"id":"me","name":"This PC","is_local":true,"public_key":"k"},
                {"id":"b","name":"Phone","public_key":"k2","created_at":2},
                {"id":"a","name":"Laptop","public_key":"k1","created_at":1},
                {"id":"x","name":"Half-paired"}]"#,
        )]);
        let children = client.children(Some(&Identity::Section { s: "mine".into() })).await.unwrap();
        let names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["This PC", "Laptop", "Phone"]);
        assert_eq!(children[0].identity, Identity::Device { dev: LOCAL.into() });
        assert_eq!(children[1].identity, Identity::Device { dev: "a".into() });
    }

    #[tokio::test]
    async fn lists_a_devices_locations_through_the_proxy() {
        let (client, seen) = fake(vec![(
            "/api/v1/peers/dev%201/proxy/storage/roots",
            StatusCode::OK,
            r#"[{"id":"r1","name":"Photos","online":true,"stats":{"files":3,"dirs":1,"bytes":10}},
                {"id":"r2","name":"Old","enabled":false}]"#,
        )]);
        let children = client.children(Some(&Identity::Device { dev: "dev 1".into() })).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "Photos");
        assert_eq!(children[0].identity, Identity::Root { dev: "dev 1".into(), root: "r1".into() });
        assert_eq!(seen.lock().unwrap()[0], "/api/v1/peers/dev%201/proxy/storage/roots");
    }

    #[tokio::test]
    async fn lists_folders_page_by_page() {
        let (client, seen) = fake(vec![
            (
                "/api/v1/peers/d/proxy/files?root_id=r&path=a%2Fb&sort=name&offset=0&",
                StatusCode::OK,
                r#"{"items":[{"id":"1","relative_path":"a/b/z.txt","filename":"z.txt","is_dir":false,"size":5,"modified_at":7}],"total":2}"#,
            ),
            (
                "/api/v1/peers/d/proxy/files?root_id=r&path=a%2Fb&sort=name&offset=1&",
                StatusCode::OK,
                r#"{"items":[{"id":"2","relative_path":"a/b/sub","filename":"sub","is_dir":true,"size":4096}],"total":2}"#,
            ),
        ]);
        let folder = Identity::Entry {
            dev: "d".into(),
            root: "r".into(),
            path: "a/b".into(),
            id: "f".into(),
            dir: true,
            size: 0,
            mtime: 0,
        };
        let children = client.children(Some(&folder)).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);
        let names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["sub", "z.txt"], "folders come first");
        assert_eq!(children[0].size(), 0, "folders have no size");
        assert_eq!(children[1].size(), 5);
        assert_eq!(children[1].modified(), Some(7));
    }

    #[tokio::test]
    async fn this_computer_is_asked_directly() {
        let (client, seen) = fake(vec![("/api/v1/storage/roots", StatusCode::OK, r#"[{"id":"r1","name":"D"}]"#)]);
        let children = client.children(Some(&Identity::Device { dev: LOCAL.into() })).await.unwrap();
        assert_eq!(children[0].name, "D");
        assert_eq!(seen.lock().unwrap()[0], "/api/v1/storage/roots");
    }

    #[tokio::test]
    async fn shared_with_me_lists_folders_of_online_devices() {
        let (client, _) = fake(vec![
            ("/api/v1/devices", StatusCode::OK, r#"[{"id":"a","name":"Laptop","public_key":"k","online":true},{"id":"b","name":"Old","public_key":"k","online":false}]"#),
            (
                "/api/v1/peers/a/proxy/shared-folders/mine",
                StatusCode::OK,
                r#"[{"id":"s1","name":"Family","root":{"id":"r"},"path":"Family","sees_all":false},
                    {"id":"s2","name":"All","root":{"id":"r"},"path":"x","sees_all":true}]"#,
            ),
        ]);
        let children = client.children(Some(&Identity::Section { s: "shared".into() })).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "Family (Laptop)");
        assert_eq!(children[0].identity.folder_path(), Some("Family"));
    }

    #[tokio::test]
    async fn unreachable_devices_are_reported_offline() {
        let (client, _) = fake(vec![(
            "/api/v1/peers/d/proxy/storage/roots",
            StatusCode::BAD_GATEWAY,
            r#"{"error":{"code":"peer_unreachable","message":"Phone is not reachable right now"}}"#,
        )]);
        let err = client.children(Some(&Identity::Device { dev: "d".into() })).await.unwrap_err();
        assert_eq!(err, ClientError::Offline("Phone is not reachable right now".into()));

        let (client, _) = fake(vec![(
            "/api/v1/peers/d/proxy/files",
            StatusCode::NOT_FOUND,
            r#"{"error":{"code":"not_found","message":"Folder not found"}}"#,
        )]);
        let err = client.list("d", "r", "gone").await.unwrap_err();
        assert!(matches!(err, ClientError::NotFound(_)));
    }

    #[tokio::test]
    async fn reads_ranges() {
        let links = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let issued = links.clone();
        let send: Responder = Arc::new(move |req: Request<Body>| {
            if req.method() == axum::http::Method::POST {
                assert_eq!(req.uri().path(), "/api/v1/peers/d/proxy/files/f1/link");
                issued.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Box::pin(async {
                    Response::new(Body::from(r#"{"url":"/api/v1/files/f1/content?ticket=abc","expires_in":21600}"#))
                });
            }
            assert_eq!(req.uri().to_string(), "/api/v1/peers/d/proxy/files/f1/content?ticket=abc");
            assert_eq!(req.headers()[header::RANGE], "bytes=4096-8191");
            Box::pin(async {
                Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_RANGE, "bytes 4096-8191/10000")
                    .body(Body::from(vec![7u8; 4096]))
                    .unwrap()
            })
        });
        let client = NodeClient::new(send, "t");
        let (total, body) = client.read("d", "f1", 4096, 4096).await.unwrap();
        assert_eq!(total, Some(10000));
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(bytes.len(), 4096);
        let _ = client.read("d", "f1", 4096, 4096).await.unwrap();
        assert_eq!(links.load(std::sync::atomic::Ordering::SeqCst), 1, "the link is reused");
    }

    #[test]
    fn fingerprints_change_with_the_index() {
        let mut r = RootInfo {
            id: "r".into(),
            name: "n".into(),
            online: true,
            enabled: None,
            created_at: None,
            stats: RootStats { files: 1, dirs: 1, bytes: 1 },
            scan: None,
        };
        let before = r.fingerprint();
        r.stats.bytes = 2;
        assert_ne!(before, r.fingerprint());
    }
}
