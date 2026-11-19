//! Upload links: a page anyone with the link can add files to a folder
//! from, to collect files from people who have no NEXUS (a file request).
//!
//! The page is served on the address for programs ([`crate::programs`]),
//! so it works for anyone who can reach that address: this computer, or
//! the local network when that is allowed. NEXUS has no server on the
//! internet, so it never reaches further.
//!
//! Files are added as the person who made the link: never more than they
//! may do themselves (a link stops working when they lose access, are
//! locked or removed), never replacing anything (a name already there gets
//! a number), and only up to the link's size limit and until it expires.

use crate::auth::{AuthUser, Need};
use crate::error::{ApiError, ApiResult};
use crate::routes::{audit, files::store_stream};
use crate::state::{AppState, SharedState};
use crate::tokens;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use nexus_common::NexusError;
use nexus_common::time::now_ms;
use nexus_database::{NewUploadLink, UploadLink};
use nexus_security::{Action, role_allows};
use nexus_storage::{ConflictPolicy, paths};
use serde::{Deserialize, Serialize};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const MB: i64 = 1024 * 1024;
/// Most a link may take in all: 100 GB.
const MAX_LINK_MB: i64 = 100 * 1024;

// ---------------------------------------------------------------------------
// Managing links (the app)

#[derive(Serialize)]
pub struct LinkView {
    #[serde(flatten)]
    link: UploadLink,
    root_name: String,
    /// The page's path on the address for programs.
    link_path: String,
    /// The link at every address that can open it, local network first.
    /// Empty while the address for programs is off.
    urls: Vec<String>,
    /// Someone on another device can open it (the local network is allowed).
    reachable_from_network: bool,
    /// Expired or full: it takes nothing more.
    expired: bool,
}

async fn view(s: &AppState, link: UploadLink) -> Result<LinkView, NexusError> {
    let root_name = s.db.get_root(&link.root_id).await.map(|r| r.name).unwrap_or_default();
    let secret = tokens::open(s, &link.secret)?;
    let link_path = format!("/upload/{}/{secret}", link.id);
    let status = s.programs.status();
    let mut urls: Vec<String> = status.addresses.iter().map(|a| format!("{a}{link_path}")).collect();
    urls.sort_by_key(|u| u.contains("://127.0.0.1"));
    let reachable_from_network = urls.iter().any(|u| !u.contains("://127.0.0.1"));
    let expired = link.expires_at.is_some_and(|e| e <= now_ms()) || link.received_bytes >= link.max_bytes;
    Ok(LinkView { link, root_name, link_path, urls, reachable_from_network, expired })
}

fn manages(user: &AuthUser) -> bool {
    user.require(Action::ManageUsers).is_ok()
}

/// `GET /upload-links`: the caller's links (everyone's, for owners and
/// administrators).
pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<LinkView>>> {
    user.require_person()?;
    let Some(me) = user.user_id.as_deref() else { return Ok(Json(Vec::new())) };
    let links = s.db.list_upload_links(if manages(&user) { None } else { Some(me) }).await?;
    let mut out = Vec::with_capacity(links.len());
    for l in links {
        out.push(view(&s, l).await?);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct NewLink {
    root_id: String,
    #[serde(default)]
    path: String,
    title: Option<String>,
    #[serde(default)]
    message: String,
    /// Days it works; 0 or none: until removed. Defaults to 7.
    expires_days: Option<u32>,
    /// The most it takes in all, in MB. Defaults to 2 GB.
    max_mb: Option<i64>,
}

/// `POST /upload-links`: makes a link that adds files to a folder.
pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(req): Json<NewLink>) -> ApiResult<(StatusCode, Json<LinkView>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    if user.via_token {
        return Err(NexusError::PermissionDenied("Upload links are made in the app".into()).into());
    }
    let Some(user_id) = user.user_id.clone() else {
        return Err(NexusError::PermissionDenied("Sign in to make upload links".into()).into());
    };
    let path = paths::normalize_relative(&req.path)?;
    user.require_access(&req.root_id, &path, Need::Add)?;
    let root = s.db.get_root(&req.root_id).await?;
    if root.read_only {
        return Err(NexusError::invalid("This storage location is read-only").into());
    }
    let folder_name = if path.is_empty() {
        root.name.clone()
    } else {
        match s.db.get_file_by_path(&root.id, &path).await? {
            Some(f) if f.is_dir => f.filename,
            _ => return Err(NexusError::NotFound("Folder".into()).into()),
        }
    };
    let title = req.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or(&folder_name).to_string();
    let message = req.message.trim().to_string();
    if title.chars().count() > 100 || message.chars().count() > 1000 {
        return Err(NexusError::invalid("Keep the title under 100 characters and the message under 1000").into());
    }
    let max_mb = req.max_mb.unwrap_or(2048);
    if !(1..=MAX_LINK_MB).contains(&max_mb) {
        return Err(NexusError::invalid(format!("The limit is 1 MB to {} GB", MAX_LINK_MB / 1024)).into());
    }
    let days = req.expires_days.unwrap_or(7).min(365);
    let id = tokens::random_string(16, tokens::ALPHABET);
    let secret = tokens::random_string(32, tokens::SECRET_ALPHABET);
    let link = s
        .db
        .create_upload_link(NewUploadLink {
            id: &id,
            secret: &tokens::seal(&s, &secret),
            root_id: &root.id,
            path: &path,
            title: &title,
            message: &message,
            user_id: &user_id,
            created_by: &user.username,
            expires_at: (days > 0).then(|| now_ms() + days as i64 * DAY_MS),
            max_bytes: max_mb * MB,
        })
        .await?;
    audit(&s, &user, "upload_link.created", Some(&format!("{}/{}", root.name, path))).await;
    Ok((StatusCode::CREATED, Json(view(&s, link).await?)))
}

/// `DELETE /upload-links/{id}`: the link stops working at once.
pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    let link = s.db.get_upload_link(&id).await?;
    if user.user_id.as_deref() != Some(link.user_id.as_str()) && !manages(&user) {
        return Err(NexusError::NotFound("Upload link".into()).into());
    }
    s.db.delete_upload_link(&id).await?;
    audit(&s, &user, "upload_link.removed", Some(&link.title)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// The page people upload from (address for programs, no sign-in)

/// The link, and who files are added as, if it still works.
async fn open_link(s: &AppState, id: &str, secret: &str) -> Result<(UploadLink, AuthUser), &'static str> {
    const GONE: &str = "This link does not work (any more). Ask whoever sent it for a new one.";
    let link = s.db.get_upload_link(id).await.map_err(|_| GONE)?;
    let stored = tokens::open(s, &link.secret).map_err(|_| GONE)?;
    if !tokens::same(secret, &stored) {
        return Err(GONE);
    }
    if link.expires_at.is_some_and(|e| e <= now_ms()) {
        return Err("This link has expired. Ask whoever sent it for a new one.");
    }
    let person = s.db.get_user(&link.user_id).await.map_err(|_| GONE)?;
    if s.db.user_locked(&person.id).await.unwrap_or(true) {
        return Err(GONE);
    }
    let user = AuthUser::person(person).with_shared(s).await.map_err(|_| GONE)?;
    if !role_allows(user.role, Action::Write) || !user.may(&link.root_id, &link.path, Need::Add) {
        return Err(GONE);
    }
    match s.db.get_root(&link.root_id).await {
        Ok(r) if r.enabled && std::path::Path::new(&r.absolute_path).is_dir() => Ok((link, user)),
        _ => Err("The folder is not available right now. Try again later."),
    }
}

fn esc(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn page(status: StatusCode, title: &str, body: String) -> Response {
    let mut nonce = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).expect("random");
    let nonce = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let html = PAGE.replace("{title}", &esc(title)).replace("{body}", &body).replace("{nonce}", &nonce);
    let csp = format!(
        "default-src 'none'; style-src 'nonce-{nonce}'; script-src 'nonce-{nonce}'; connect-src 'self'; img-src data:; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    let mut res = (status, html).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(&csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    res
}

/// `GET /upload/{id}/{secret}`
pub async fn upload_page(State(s): State<SharedState>, Path((id, secret)): Path<(String, String)>) -> Response {
    match open_link(&s, &id, &secret).await {
        Err(why) => page(StatusCode::GONE, "Upload link", format!("<h1>Upload link</h1><p class=\"note\">{}</p>", esc(why))),
        Ok((link, _)) => {
            let left = (link.max_bytes - link.received_bytes).max(0);
            let expires = link.expires_at.map(|e| format!(" until {}", esc(&date(e)))).unwrap_or_default();
            let message = if link.message.is_empty() { String::new() } else { format!("<p class=\"message\">{}</p>", esc(&link.message)) };
            let body = format!(
                "<h1>{title}</h1><p class=\"note\">{by} asks you to add files here. Nobody can see or change what is already there.</p>\
                 {message}\
                 <label class=\"drop\" id=\"drop\"><input type=\"file\" id=\"pick\" multiple><span>Choose files, or drop them here</span></label>\
                 <label class=\"from\">Your name (optional) <input type=\"text\" id=\"from\" maxlength=\"60\" autocomplete=\"name\"></label>\
                 <ul id=\"list\"></ul>\
                 <p class=\"small\">Up to {left} more{expires}.</p>",
                title = esc(&link.title),
                by = esc(&link.created_by),
                left = esc(&size(left)),
            );
            page(StatusCode::OK, &link.title, body)
        }
    }
}

#[derive(Deserialize)]
pub struct UploadQuery {
    name: String,
    #[serde(default)]
    from: String,
}

#[derive(Serialize)]
pub struct Received {
    name: String,
    size: i64,
}

fn refuse(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": { "message": message } }))).into_response()
}

/// `POST /upload/{id}/{secret}/files?name=`: one file, as the request body.
pub async fn upload_file(
    State(s): State<SharedState>,
    Path((id, secret)): Path<(String, String)>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let (link, user) = match open_link(&s, &id, &secret).await {
        Ok(v) => v,
        Err(why) => return refuse(StatusCode::GONE, why),
    };
    let Some(len) = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok()) else {
        return refuse(StatusCode::LENGTH_REQUIRED, "The file's size is needed");
    };
    let name = q.name.trim();
    if name.is_empty() || name.contains(['/', '\\']) {
        return refuse(StatusCode::BAD_REQUEST, "That is not a file name");
    }
    match s.db.reserve_upload_room(&link.id, len).await {
        Ok(true) => {}
        Ok(false) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "This link has no room left for that file"),
        Err(e) => return ApiError::from(e).into_response(),
    }
    match store_stream(&s, &user, &link.root_id, &link.path, name, Some(len as u64), body, ConflictPolicy::Rename).await {
        Ok(entry) => {
            let _ = s.db.upload_link_received(&link.id).await;
            let from = q.from.trim().chars().take(60).collect::<String>();
            let detail = if from.is_empty() { entry.relative_path.clone() } else { format!("{} (from {from})", entry.relative_path) };
            let _ = s.db.log_activity("upload_link.received", Some(&detail), Some(&link.user_id), None).await;
            Json(Received { name: entry.filename, size: entry.size }).into_response()
        }
        Err(e) => {
            let _ = s.db.release_upload_room(&link.id, len).await;
            e.into_response()
        }
    }
}

fn size(bytes: i64) -> String {
    let b = bytes as f64;
    if b >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024.0 * 1024.0 {
        format!("{:.0} MB", b / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB", (b / 1024.0).ceil())
    }
}

fn date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%-d %B %Y").to_string()).unwrap_or_default()
}

const PAGE: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>{title}</title>
<style nonce="{nonce}">
:root { --bg: #f6f7f8; --panel: #fff; --fg: #1b1f23; --muted: #5b6570; --line: #dde1e5; --accent: #2f8f5b; --bad: #b3261e; }
@media (prefers-color-scheme: dark) { :root { --bg: #111417; --panel: #181c20; --fg: #e8ebee; --muted: #9aa4ae; --line: #2a3036; --accent: #5cc48a; --bad: #f2a29c; } }
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg); font: 15px/1.5 system-ui, -apple-system, "Segoe UI", sans-serif; }
main { max-width: 560px; margin: 0 auto; padding: 40px 16px; }
h1 { font-size: 22px; margin: 0 0 6px; }
.note, .small { color: var(--muted); }
.small { font-size: 13px; }
.message { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 10px 12px; white-space: pre-wrap; }
.drop { display: flex; align-items: center; justify-content: center; min-height: 140px; margin: 20px 0 12px; border: 2px dashed var(--line);
  border-radius: 10px; background: var(--panel); cursor: pointer; text-align: center; padding: 16px; }
.drop.over { border-color: var(--accent); }
.drop input { display: none; }
.from { display: block; font-size: 13px; color: var(--muted); }
.from input { display: block; width: 100%; margin-top: 4px; padding: 8px 10px; border: 1px solid var(--line); border-radius: 6px; background: var(--panel); color: var(--fg); font: inherit; }
ul { list-style: none; padding: 0; margin: 16px 0; }
li { background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 8px 12px; margin-bottom: 6px; font-size: 14px; }
li .bar { height: 4px; background: var(--line); border-radius: 2px; margin-top: 6px; overflow: hidden; }
li .bar i { display: block; height: 100%; width: 0; background: var(--accent); }
li.done .state { color: var(--accent); }
li.failed .state { color: var(--bad); }
.state { float: right; font-size: 13px; color: var(--muted); }
</style>
</head>
<body>
<main>
{body}
</main>
<script nonce="{nonce}">
(function () {
  var pick = document.getElementById('pick'), drop = document.getElementById('drop'), list = document.getElementById('list');
  if (!pick) return;
  var queue = [], busy = false;
  function size(n) { return n > 1073741824 ? (n / 1073741824).toFixed(1) + ' GB' : n > 1048576 ? Math.round(n / 1048576) + ' MB' : Math.ceil(n / 1024) + ' KB'; }
  function add(files) {
    for (var i = 0; i < files.length; i++) {
      var f = files[i], li = document.createElement('li'), state = document.createElement('span'), bar = document.createElement('div'), fill = document.createElement('i');
      state.className = 'state'; state.textContent = 'Waiting';
      bar.className = 'bar'; bar.appendChild(fill);
      li.appendChild(state); li.appendChild(document.createTextNode(f.name + ' (' + size(f.size) + ')')); li.appendChild(bar);
      list.appendChild(li);
      queue.push({ file: f, li: li, state: state, fill: fill });
    }
    next();
  }
  function next() {
    if (busy || !queue.length) return;
    busy = true;
    var job = queue.shift(), x = new XMLHttpRequest();
    var from = document.getElementById('from').value;
    x.open('POST', location.pathname.replace(/\/$/, '') + '/files?name=' + encodeURIComponent(job.file.name) + '&from=' + encodeURIComponent(from));
    x.upload.onprogress = function (e) { if (e.lengthComputable) { job.fill.style.width = (100 * e.loaded / e.total) + '%'; job.state.textContent = Math.round(100 * e.loaded / e.total) + '%'; } };
    x.onload = function () {
      if (x.status >= 200 && x.status < 300) { job.li.className = 'done'; job.state.textContent = 'Added'; job.fill.style.width = '100%'; }
      else { job.li.className = 'failed'; var m = 'Failed'; try { m = JSON.parse(x.responseText).error.message || m; } catch (e) {} job.state.textContent = m; }
      busy = false; next();
    };
    x.onerror = function () { job.li.className = 'failed'; job.state.textContent = 'The connection dropped'; busy = false; next(); };
    job.state.textContent = '0%';
    x.send(job.file);
  }
  pick.addEventListener('change', function () { add(pick.files); pick.value = ''; });
  drop.addEventListener('dragover', function (e) { e.preventDefault(); drop.className = 'drop over'; });
  drop.addEventListener('dragleave', function () { drop.className = 'drop'; });
  drop.addEventListener('drop', function (e) { e.preventDefault(); drop.className = 'drop'; add(e.dataTransfer.files); });
})();
</script>
</body>
</html>
"#;
