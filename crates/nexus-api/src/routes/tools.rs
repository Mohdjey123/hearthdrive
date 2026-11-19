//! File tools: duplicate files, opening and extracting archives, and
//! downloading a file from a web link straight into a storage location.

use super::audit;
use super::files::{FoundItem, with_roots};
use super::transfers::{Job, set_status};
use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use futures_util::StreamExt;
use nexus_common::time::now_ms;
use nexus_common::types::{FileEntry, Role, StorageRoot};
use nexus_common::NexusError;
use nexus_security::Action;
use nexus_storage::{ConflictPolicy, paths};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::net::IpAddr;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

// ---------------------------------------------------------------------------
// Duplicates

#[derive(Deserialize)]
pub struct DuplicateQuery {
    root_id: Option<String>,
    /// Smallest file size to look at, in bytes (default 1 MB).
    min_size: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub struct DuplicateGroup {
    hash: String,
    size: i64,
    /// Space the extra copies take.
    wasted: i64,
    files: Vec<FoundItem>,
}

/// Files with exactly the same content, biggest savings first. Only files
/// whose hash is known count (hashing runs in the background).
pub async fn duplicates(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<DuplicateQuery>,
) -> ApiResult<Json<Vec<DuplicateGroup>>> {
    user.require(Action::Read)?;
    let limit = super::clamp_limit(q.limit, 100, 500);
    let groups = s.db.duplicate_groups(q.root_id.as_deref(), q.min_size.unwrap_or(1 << 20), limit).await?;
    let mut out = Vec::new();
    for mut files in groups {
        files.retain(|f| user.may(&f.root_id, &f.relative_path, Need::Read));
        if files.len() < 2 {
            continue;
        }
        let size = files[0].size;
        let hash = files[0].content_hash.clone().unwrap_or_default();
        let wasted = size * (files.len() as i64 - 1);
        out.push(DuplicateGroup { hash, size, wasted, files: with_roots(&s, files).await? });
    }
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Archives

const MAX_LISTED: usize = 5000;
const MAX_ENTRIES: usize = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    SevenZip,
}

impl Format {
    pub fn of(name: &str) -> Option<Self> {
        let n = name.to_ascii_lowercase();
        if n.ends_with(".zip") || n.ends_with(".cbz") {
            Some(Self::Zip)
        } else if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if n.ends_with(".tar") {
            Some(Self::Tar)
        } else if n.ends_with(".7z") || n.ends_with(".cb7") {
            Some(Self::SevenZip)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::SevenZip => "7z",
        }
    }

    /// The folder an archive is extracted into: its name without the extension.
    fn stem(self, name: &str) -> String {
        let n = name.to_ascii_lowercase();
        let cut = [".tar.gz", ".tgz", ".tar", ".zip", ".cbz", ".7z", ".cb7"]
            .iter()
            .find(|e| n.ends_with(*e))
            .map(|e| e.len())
            .unwrap_or(0);
        let stem = name[..name.len() - cut].trim().to_string();
        if stem.is_empty() { "Extracted".into() } else { stem }
    }
}

#[derive(Serialize, Clone)]
pub struct ArchiveEntry {
    path: String,
    size: u64,
    is_dir: bool,
}

#[derive(Serialize)]
pub struct ArchiveListing {
    format: &'static str,
    entries: Vec<ArchiveEntry>,
    total_entries: usize,
    total_size: u64,
    truncated: bool,
}

fn archive_error(e: impl std::fmt::Display) -> NexusError {
    NexusError::invalid(format!("This archive could not be read: {e}"))
}

/// A path inside an archive as a safe relative path: no absolute paths,
/// drive letters or `..`, and names Windows accepts.
pub fn safe_entry_path(raw: &str) -> Option<String> {
    let raw = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for c in FsPath::new(&raw).components() {
        match c {
            Component::Normal(p) => {
                let p = p.to_string_lossy();
                let clean: String = p
                    .chars()
                    .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
                    .collect();
                let clean = clean.trim_end_matches(['.', ' ']).to_string();
                if clean.is_empty() || paths::validate_name(&clean).is_err() {
                    return None;
                }
                parts.push(clean);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Calls `each` for every entry: its name, size, whether it is a folder,
/// and (when `with_data`) a reader for its contents.
fn walk(
    file: &FsPath,
    format: Format,
    with_data: bool,
    each: &mut dyn FnMut(&str, u64, bool, Option<&mut dyn Read>) -> Result<bool, NexusError>,
) -> Result<(), NexusError> {
    match format {
        Format::Zip => {
            let mut zip = zip::ZipArchive::new(std::fs::File::open(file)?).map_err(archive_error)?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).map_err(archive_error)?;
                let name = entry.name().map_err(archive_error)?.to_string();
                let (size, dir) = (entry.size(), entry.is_dir());
                let go = if with_data && !dir {
                    each(&name, size, dir, Some(&mut entry))?
                } else {
                    each(&name, size, dir, None)?
                };
                if !go {
                    break;
                }
            }
        }
        Format::Tar | Format::TarGz => {
            let raw = std::fs::File::open(file)?;
            let reader: Box<dyn Read> =
                if format == Format::TarGz { Box::new(flate2::read::GzDecoder::new(raw)) } else { Box::new(raw) };
            let mut tar = tar::Archive::new(reader);
            for entry in tar.entries().map_err(archive_error)? {
                let mut entry = entry.map_err(archive_error)?;
                let kind = entry.header().entry_type();
                if !(kind.is_file() || kind.is_dir()) {
                    continue; // links and special files are skipped
                }
                let name = entry.path().map_err(archive_error)?.to_string_lossy().to_string();
                let size = entry.header().size().unwrap_or(0);
                let dir = kind.is_dir();
                let go = if with_data && !dir { each(&name, size, dir, Some(&mut entry))? } else { each(&name, size, dir, None)? };
                if !go {
                    break;
                }
            }
        }
        Format::SevenZip => {
            if !with_data {
                let archive = sevenz_rust2::Archive::open(file).map_err(archive_error)?;
                for e in &archive.files {
                    if !each(e.name(), e.size(), e.is_directory(), None)? {
                        break;
                    }
                }
                return Ok(());
            }
            let mut reader =
                sevenz_rust2::ArchiveReader::open(file, sevenz_rust2::Password::empty()).map_err(archive_error)?;
            let mut failure = None;
            reader
                .for_each_entries(|e, r| {
                    let dir = e.is_directory();
                    match each(e.name(), e.size(), dir, if dir { None } else { Some(r) }) {
                        Ok(go) => Ok(go),
                        Err(err) => {
                            failure = Some(err);
                            Ok(false)
                        }
                    }
                })
                .map_err(archive_error)?;
            if let Some(e) = failure {
                return Err(e);
            }
        }
    }
    Ok(())
}

pub fn list_archive(file: &FsPath, format: Format) -> Result<ArchiveListing, NexusError> {
    let mut entries = Vec::new();
    let (mut total_entries, mut total_size) = (0usize, 0u64);
    walk(file, format, false, &mut |name, size, dir, _| {
        total_entries += 1;
        total_size += size;
        if entries.len() < MAX_LISTED {
            entries.push(ArchiveEntry { path: name.trim_end_matches('/').to_string(), size, is_dir: dir });
        }
        Ok(true)
    })?;
    Ok(ArchiveListing { format: format.name(), truncated: total_entries > entries.len(), entries, total_entries, total_size })
}

/// Writes every entry under `dest`. Calls `progress` with the bytes written
/// so far; stops when `stop` returns true.
pub fn extract_archive(
    file: &FsPath,
    format: Format,
    dest: &FsPath,
    progress: &dyn Fn(u64),
    stop: &dyn Fn() -> bool,
) -> Result<(u64, u64), NexusError> {
    let (mut files, mut bytes, mut seen) = (0u64, 0u64, 0usize);
    let mut buf = vec![0u8; 256 * 1024];
    walk(file, format, true, &mut |name, _size, dir, data| {
        seen += 1;
        if seen > MAX_ENTRIES {
            return Err(NexusError::invalid("This archive has too many files to extract"));
        }
        if stop() {
            return Ok(false);
        }
        let Some(rel) = safe_entry_path(name) else { return Ok(true) };
        let target = dest.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        if dir {
            std::fs::create_dir_all(&target)?;
            return Ok(true);
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let Some(reader) = data else { return Ok(true) };
        // Never overwrite: a repeated name inside the archive gets a number.
        let mut out_path = target.clone();
        let mut n = 1;
        while out_path.exists() {
            let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            out_path = target.with_file_name(paths::numbered_name(&name, n, false));
            n += 1;
        }
        let mut out = std::fs::File::create(&out_path)?;
        loop {
            if stop() {
                drop(out);
                let _ = std::fs::remove_file(&out_path);
                return Ok(false);
            }
            let read = reader.read(&mut buf).map_err(archive_error)?;
            if read == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..read])?;
            bytes += read as u64;
            progress(bytes);
        }
        files += 1;
        Ok(true)
    })?;
    Ok((files, bytes))
}

async fn archive_file(s: &AppState, user: &AuthUser, id: &str) -> ApiResult<(FileEntry, StorageRoot, PathBuf, Format)> {
    user.require(Action::Read)?;
    let file = s.db.get_file(id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    let format = Format::of(&file.filename)
        .ok_or_else(|| NexusError::invalid("HearthDrive can open .zip, .7z, .tar and .tar.gz archives"))?;
    let root = s.db.get_root(&file.root_id).await?;
    s.storage.ensure_online(&root)?;
    let path = s.storage.resolve(&root, &file.relative_path)?;
    Ok((file, root, path, format))
}

/// What is inside an archive.
pub async fn archive(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<ArchiveListing>> {
    let (_, _, path, format) = archive_file(&s, &user, &id).await?;
    let listing = tokio::task::spawn_blocking(move || list_archive(&path, format))
        .await
        .map_err(|e| NexusError::internal(e.to_string()))??;
    Ok(Json(listing))
}

#[derive(Serialize)]
pub struct Started {
    job_id: String,
    /// Where the files go.
    path: String,
}

/// Extracts an archive into a new folder next to it, as a transfer job.
pub async fn extract(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Started>)> {
    user.require(Action::Write)?;
    let (file, root, path, format) = archive_file(&s, &user, &id).await?;
    let (parent, _) = paths::split_parent(&file.relative_path);
    user.require_access(&root.id, &parent, Need::Add)?;
    if root.read_only {
        return Err(NexusError::PermissionDenied(format!("{} is read-only", root.name)).into());
    }
    let listing = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || list_archive(&path, format))
            .await
            .map_err(|e| NexusError::internal(e.to_string()))??
    };
    if let Some(space) = nexus_storage::drives::space_for(&path)
        && listing.total_size.saturating_add(256 << 20) > space.available_bytes
    {
        return Err(NexusError::InsufficientStorage(format!(
            "Extracting needs {} MB and {} has {} MB free",
            listing.total_size >> 20,
            root.name,
            space.available_bytes >> 20
        ))
        .into());
    }
    // A new folder named after the archive ("Photos (1)" if taken).
    let stem = format.stem(&file.filename);
    let mut name = stem.clone();
    let mut n = 1;
    while s.db.get_file_by_path(&root.id, &paths::join(&parent, &name)).await?.is_some()
        || s.storage.resolve(&root, &paths::join(&parent, &name))?.exists()
    {
        name = paths::numbered_name(&stem, n, true);
        n += 1;
    }
    let (root, folder) = s.storage.create_folder(&root.id, &parent, &name).await?;
    let dest = s.storage.resolve(&root, &folder)?;
    let job = s.jobs.add("extract", format!("Extract {}", file.filename), root.name.clone(), user.username.clone());
    job.update(|i| {
        i.bytes_total = listing.total_size;
        i.files_total = listing.total_entries as u64;
    });
    audit(&s, &user, "archive.extracted", Some(&format!("{}/{}", root.name, file.relative_path))).await;
    let started = Started { job_id: job.id.clone(), path: folder.clone() };
    let state = s.clone();
    tokio::spawn(async move {
        set_status(&state, &job, "running");
        let result = {
            let job = job.clone();
            let src = path.clone();
            tokio::task::spawn_blocking(move || {
                extract_archive(&src, format, &dest, &|b| job.set_bytes(b), &|| job.is_cancelled())
            })
            .await
            .map_err(|e| NexusError::internal(e.to_string()))
            .and_then(|r| r)
        };
        match result {
            Ok((files, _)) => job.update(|i| i.files_done = files),
            Err(e) => job.fail_file(&file.filename, &e),
        }
        let _ = state.indexer.reindex_path(&root, &folder).await;
        finish(&state, &job);
    });
    Ok((StatusCode::ACCEPTED, Json(started)))
}

fn finish(s: &AppState, job: &Arc<Job>) {
    super::transfers::finish_job(s, job);
}

// ---------------------------------------------------------------------------
// Downloading from a link

#[derive(Deserialize)]
pub struct FetchRequest {
    root_id: String,
    #[serde(default)]
    path: String,
    url: String,
    /// File name; taken from the link when empty.
    name: Option<String>,
}

/// Addresses no download may reach: this computer and link-local
/// addresses. Home-network addresses only for owners and administrators.
fn blocked(ip: IpAddr, may_reach_lan: bool) -> bool {
    let lan = match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.is_multicast() {
                return true;
            }
            v4.is_private() || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return blocked(IpAddr::V4(v4), may_reach_lan);
            }
            if v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    };
    lan && !may_reach_lan
}

async fn check_host(url: &reqwest::Url, may_reach_lan: bool) -> Result<(), NexusError> {
    let host = url.host_str().ok_or_else(|| NexusError::invalid("That link has no address"))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> = tokio::net::lookup_host((host.trim_start_matches('[').trim_end_matches(']'), port))
        .await
        .map_err(|_| NexusError::invalid(format!("{host} could not be found")))?
        .collect();
    if addrs.is_empty() || addrs.iter().any(|a| blocked(a.ip(), may_reach_lan)) {
        return Err(NexusError::PermissionDenied(format!("HearthDrive does not download from {host}")));
    }
    Ok(())
}

fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .tls_backend_preconfigured(crate::share::tls())
            .user_agent(concat!("HearthDrive/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            // Redirects are followed by hand, checking each address.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client")
    })
}

/// A file name from the link or the server's Content-Disposition.
fn name_from(url: &reqwest::Url, headers: &reqwest::header::HeaderMap) -> String {
    let from_header = headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split(';').map(str::trim).find_map(|p| {
                p.strip_prefix("filename*=UTF-8''")
                    .map(percent_decode)
                    .or_else(|| p.strip_prefix("filename=").map(|n| n.trim_matches('"').to_string()))
            })
        });
    let from_url = url
        .path_segments()
        .and_then(|mut s| s.next_back().map(percent_decode))
        .filter(|s| !s.is_empty());
    let name = from_header.or(from_url).unwrap_or_else(|| "download".into());
    safe_entry_path(&name).and_then(|p| p.rsplit('/').next().map(str::to_string)).unwrap_or_else(|| "download".into())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(b);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Starts downloading a link into a folder, as a transfer job.
pub async fn fetch(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<FetchRequest>,
) -> ApiResult<(StatusCode, Json<Started>)> {
    user.require(Action::Write)?;
    let parent = paths::normalize_relative(&body.path)?;
    let policy = super::files::upload_policy(&user, &body.root_id, &parent, ConflictPolicy::Rename)?;
    let url = reqwest::Url::parse(body.url.trim()).map_err(|_| NexusError::invalid("That is not a web link"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(NexusError::invalid("Only http and https links can be downloaded").into());
    }
    let lan = matches!(user.role, Role::Owner | Role::Administrator);
    check_host(&url, lan).await?;
    let root = s.db.get_root(&body.root_id).await?;
    let title = body.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| name_from(&url, &Default::default()));
    let job = s.jobs.add("download", format!("Download {title}"), format!("{} → {}", url.host_str().unwrap_or("web"), root.name), user.username.clone());
    job.update(|i| i.files_total = 1);
    audit(&s, &user, "file.fetch", Some(url.as_str())).await;
    let started = Started { job_id: job.id.clone(), path: parent.clone() };
    let state = s.clone();
    tokio::spawn(async move {
        let Some(_slot) = state.jobs.start(&state, &job).await else { return };
        let result = run_fetch(&state, &user, &job, url, &body.root_id, &parent, body.name, lan, policy).await;
        match result {
            Ok(name) => {
                job.update(|i| {
                    i.files_done = 1;
                    i.current = Some(name);
                });
            }
            Err(e) => job.fail_file(&title, &e),
        }
        finish(&state, &job);
    });
    Ok((StatusCode::ACCEPTED, Json(started)))
}

#[allow(clippy::too_many_arguments)]
async fn run_fetch(
    s: &AppState,
    user: &AuthUser,
    job: &Arc<Job>,
    mut url: reqwest::Url,
    root_id: &str,
    parent: &str,
    name: Option<String>,
    lan: bool,
    policy: ConflictPolicy,
) -> Result<String, NexusError> {
    let mut response = None;
    for _ in 0..8 {
        check_host(&url, lan).await?;
        let r = client().get(url.clone()).send().await.map_err(|e| NexusError::Unavailable(format!("Download failed: {e}")))?;
        if r.status().is_redirection() {
            let next = r
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|l| l.to_str().ok())
                .and_then(|l| url.join(l).ok())
                .ok_or_else(|| NexusError::Unavailable("The server sent a broken redirect".into()))?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(NexusError::invalid("The link redirects somewhere HearthDrive cannot download from"));
            }
            url = next;
            continue;
        }
        if !r.status().is_success() {
            return Err(NexusError::Unavailable(format!("The server answered {}", r.status())));
        }
        response = Some(r);
        break;
    }
    let response = response.ok_or_else(|| NexusError::Unavailable("Too many redirects".into()))?;
    let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| name_from(&url, response.headers()));
    job.update(|i| {
        i.current = Some(name.clone());
        i.bytes_total = response.content_length().unwrap_or(0);
    });
    let target = s.storage.prepare_upload(root_id, parent, &name, response.content_length()).await?;
    let written = async {
        let mut out = tokio::fs::File::create(&target.staging).await?;
        let mut hasher = blake3::Hasher::new();
        let mut stream = response.bytes_stream();
        let mut n: u64 = 0;
        while let Some(chunk) = stream.next().await {
            if job.is_cancelled() {
                return Err(NexusError::invalid("Cancelled"));
            }
            let chunk = chunk.map_err(|e| NexusError::Unavailable(format!("Download interrupted: {e}")))?;
            n += chunk.len() as u64;
            hasher.update(&chunk);
            out.write_all(&chunk).await?;
            job.set_bytes(n);
        }
        out.flush().await?;
        out.sync_all().await?;
        let rel = s.storage.commit_upload_by(&target, policy, Some(&user.username)).await?;
        Ok::<_, NexusError>((rel, hasher.finalize().to_hex().to_string()))
    }
    .await;
    let (rel, hash) = match written {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&target.staging).await;
            return Err(e);
        }
    };
    s.indexer.reindex_path(&target.root, &rel).await?;
    if let Some(entry) = s.db.get_file_by_path(&target.root.id, &rel).await? {
        s.db.set_hash(&entry.id, Some(&hash), entry.modified_at, now_ms()).await?;
    }
    Ok(paths::split_parent(&rel).1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_stay_inside() {
        assert_eq!(safe_entry_path("a/b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(safe_entry_path("./a\\b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(safe_entry_path("../evil.txt"), None);
        assert_eq!(safe_entry_path("/etc/passwd"), None);
        assert_eq!(safe_entry_path("C:\\Windows\\x"), None);
        assert_eq!(safe_entry_path("a/b:c.txt").as_deref(), Some("a/b_c.txt"));
    }

    #[test]
    fn local_addresses_are_blocked() {
        assert!(blocked("127.0.0.1".parse().unwrap(), true));
        assert!(blocked("169.254.1.1".parse().unwrap(), true));
        assert!(blocked("192.168.1.10".parse().unwrap(), false));
        assert!(!blocked("192.168.1.10".parse().unwrap(), true));
        assert!(!blocked("93.184.216.34".parse().unwrap(), false));
        assert!(blocked("::1".parse().unwrap(), true));
        assert!(blocked("::ffff:127.0.0.1".parse().unwrap(), true));
    }

    #[test]
    fn archives_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("t.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("docs/a.txt", o).unwrap();
            std::io::Write::write_all(&mut w, b"hello").unwrap();
            w.start_file("../escape.txt", o).unwrap();
            std::io::Write::write_all(&mut w, b"nope").unwrap();
            w.finish().unwrap();
        }
        let listing = list_archive(&zip_path, Format::Zip).unwrap();
        assert_eq!(listing.total_entries, 2);
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let (files, bytes) = extract_archive(&zip_path, Format::Zip, &out, &|_| {}, &|| false).unwrap();
        assert_eq!((files, bytes), (1, 5));
        assert_eq!(std::fs::read_to_string(out.join("docs").join("a.txt")).unwrap(), "hello");
        assert!(!dir.path().join("escape.txt").exists());
        assert_eq!(Format::Zip.stem("Photos.zip"), "Photos");
        assert_eq!(Format::TarGz.stem("src.tar.gz"), "src");
    }
}

// ---------------------------------------------------------------------------
// Other copies of a file

#[derive(Deserialize)]
pub struct ByHashQuery {
    size: i64,
}

/// `GET /files/by-hash/{hash}?size=`: a file with this content that the
/// caller may read, in storage that is connected now. Lets another device
/// fetch parts of a big file from several devices at once.
pub async fn by_hash(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(hash): Path<String>,
    Query(q): Query<ByHashQuery>,
) -> ApiResult<Json<FileEntry>> {
    user.require(Action::Read)?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || q.size < 0 {
        return Err(NexusError::invalid("Give a BLAKE3 hash and a size").into());
    }
    for file in s.db.files_with_hash(&hash.to_ascii_lowercase(), q.size, 20).await? {
        if !user.may(&file.root_id, &file.relative_path, Need::Read) {
            continue;
        }
        let Ok(root) = s.db.get_root(&file.root_id).await else { continue };
        if s.storage.ensure_online(&root).is_ok() {
            return Ok(Json(file));
        }
    }
    Err(NexusError::NotFound("file with this content".into()).into())
}
