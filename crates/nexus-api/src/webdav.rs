//! WebDAV: each storage location on this computer as a folder under
//! `/dav/`, for file managers and sync tools. Sign in with any user name
//! and a token for programs as the password.

use crate::auth::{AuthUser, Need};
use crate::programs::named_roots;
use crate::state::SharedState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use dav_server::davpath::DavPath;
use dav_server::fakels::FakeLs;
use dav_server::fs::{
    DavDirEntry, DavFile, DavFileSystem, DavMetaData, DavProp, FsError, FsFuture, FsStream, OpenOptions, ReadDirMeta,
};
use dav_server::localfs::LocalFs;
use dav_server::{DavHandler, DavMethodSet};
use futures_util::StreamExt;
use nexus_security::Action;
use nexus_storage::paths;
use percent_encoding::percent_decode_str;
use std::time::SystemTime;

/// A storage location's folder, without Hearthdrive's own `.nexus` folder.
#[derive(Clone)]
struct StorageFs(Box<LocalFs>);

fn internal(path: &DavPath) -> bool {
    path.as_rel_ospath()
        .components()
        .next()
        .is_some_and(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(paths::INTERNAL_DIR))
}

macro_rules! guard {
    ($($p:expr),+) => {
        if $(internal($p))||+ {
            return Box::pin(async { Err(FsError::NotFound) });
        }
    };
}

impl DavFileSystem for StorageFs {
    fn open<'a>(&'a self, path: &'a DavPath, options: OpenOptions) -> FsFuture<'a, Box<dyn DavFile>> {
        guard!(path);
        self.0.open(path, options)
    }

    fn read_dir<'a>(&'a self, path: &'a DavPath, meta: ReadDirMeta) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        guard!(path);
        let top = path.as_rel_ospath().as_os_str().is_empty();
        Box::pin(async move {
            let entries = self.0.read_dir(path, meta).await?;
            if !top {
                return Ok(entries);
            }
            let hidden = paths::INTERNAL_DIR.as_bytes().to_ascii_lowercase();
            let filtered = entries.filter(move |e| {
                let keep = !matches!(e, Ok(e) if e.name().to_ascii_lowercase() == hidden);
                std::future::ready(keep)
            });
            Ok(Box::pin(filtered) as FsStream<Box<dyn DavDirEntry>>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        guard!(path);
        self.0.metadata(path)
    }

    fn symlink_metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        guard!(path);
        self.0.symlink_metadata(path)
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        guard!(path);
        self.0.create_dir(path)
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        guard!(path);
        self.0.remove_dir(path)
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        guard!(path);
        self.0.remove_file(path)
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        guard!(from, to);
        self.0.rename(from, to)
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        guard!(from, to);
        self.0.copy(from, to)
    }

    fn set_accessed<'a>(&'a self, path: &'a DavPath, tm: SystemTime) -> FsFuture<'a, ()> {
        guard!(path);
        self.0.set_accessed(path, tm)
    }

    fn set_modified<'a>(&'a self, path: &'a DavPath, tm: SystemTime) -> FsFuture<'a, ()> {
        guard!(path);
        self.0.set_modified(path, tm)
    }

    fn have_props<'a>(&'a self, path: &'a DavPath) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        self.0.have_props(path)
    }

    fn patch_props<'a>(&'a self, path: &'a DavPath, patch: Vec<(bool, DavProp)>) -> FsFuture<'a, Vec<(StatusCode, DavProp)>> {
        guard!(path);
        self.0.patch_props(path, patch)
    }

    fn get_props<'a>(&'a self, path: &'a DavPath, do_content: bool) -> FsFuture<'a, Vec<DavProp>> {
        guard!(path);
        self.0.get_props(path, do_content)
    }

    fn get_prop<'a>(&'a self, path: &'a DavPath, prop: DavProp) -> FsFuture<'a, Vec<u8>> {
        guard!(path);
        self.0.get_prop(path, prop)
    }

    fn get_quota(&self) -> FsFuture<'_, (u64, Option<u64>)> {
        self.0.get_quota()
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"Hearthdrive\", charset=\"UTF-8\""))],
        "Sign in with a token for programs as the password.",
    )
        .into_response()
}

/// Basic (any user name, the token as password) or a bearer token.
async fn caller(s: &SharedState, value: Option<String>) -> Option<AuthUser> {
    let value = value?;
    let token = if let Some(b) = value.strip_prefix("Basic ") {
        let raw = base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()?;
        let pair = String::from_utf8(raw).ok()?;
        pair.split_once(':')?.1.to_string()
    } else {
        value.strip_prefix("Bearer ")?.trim().to_string()
    };
    crate::tokens::authenticate(s, &token).await.ok()
}

fn decode(s: &str) -> String {
    percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// A path under `/dav/`: the storage's name and the path inside, decoded.
fn split(path: &str) -> (String, String, String) {
    let rest = path.strip_prefix("/dav").unwrap_or(path).trim_start_matches('/');
    let (seg, inner) = rest.split_once('/').unwrap_or((rest, ""));
    (seg.to_string(), decode(seg), decode(inner))
}

pub async fn handle(State(s): State<SharedState>, req: Request) -> Response {
    let auth = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let Some(user) = caller(&s, auth).await else { return unauthorized() };
    let (raw_seg, name, inner) = split(req.uri().path());
    if name.is_empty() {
        let depth0 = req.headers().get("depth").and_then(|v| v.to_str().ok()) == Some("0");
        return top(&s, &user, req.method().clone(), depth0).await;
    }
    let Some((_, root)) = named_roots(&s, &user).await.into_iter().find(|(n, _)| n.eq_ignore_ascii_case(&name)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(rel) = paths::normalize_relative(&inner) else { return StatusCode::BAD_REQUEST.into_response() };
    if paths::is_hidden_internal(&rel) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let method = req.method().clone();
    let reads = matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS" | "PROPFIND");
    let need = match method.as_str() {
        _ if reads => Need::Read,
        "DELETE" | "MOVE" => Need::Change,
        _ => Need::Write,
    };
    let can_write = user.require(Action::Write).is_ok();
    if !reads && !can_write {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !user.may(&root.id, &rel, need) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // MOVE and COPY stay within one storage location.
    let mut dest_rel = None;
    if matches!(method.as_str(), "MOVE" | "COPY") {
        let Some(dest) = req.headers().get("destination").and_then(|v| v.to_str().ok()) else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let dest_path = dest.parse::<axum::http::Uri>().map(|u| u.path().to_string()).unwrap_or_else(|_| dest.to_string());
        let (_, dest_name, dest_inner) = split(&dest_path);
        if !dest_name.eq_ignore_ascii_case(&name) {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        let Ok(d) = paths::normalize_relative(&dest_inner) else { return StatusCode::BAD_REQUEST.into_response() };
        if paths::is_hidden_internal(&d) || !user.may(&root.id, &d, Need::Add) {
            return StatusCode::FORBIDDEN.into_response();
        }
        dest_rel = Some(d);
    }
    if s.storage.ensure_online(&root).is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "This storage is offline").into_response();
    }
    if !reads && root.read_only {
        return StatusCode::FORBIDDEN.into_response();
    }

    let handler = DavHandler::builder()
        .filesystem(Box::new(StorageFs(LocalFs::new(&root.absolute_path, false, false, false))))
        .locksystem(FakeLs::new())
        .strip_prefix(format!("/dav/{raw_seg}"))
        .methods(if can_write && !root.read_only { DavMethodSet::WEBDAV_RW } else { DavMethodSet::WEBDAV_RO })
        .build_handler();
    let response = handler.handle(req).await;
    if !reads && response.status().is_success() {
        let s = s.clone();
        tokio::spawn(async move {
            for path in std::iter::once(rel).chain(dest_rel) {
                let _ = s.indexer.reindex_path(&root, &path).await;
            }
        });
    }
    response.map(Body::new)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `/dav/` itself: the storage locations, as folders.
async fn top(s: &SharedState, user: &AuthUser, method: Method, depth0: bool) -> Response {
    let roots = named_roots(s, user).await;
    match method {
        Method::OPTIONS => (
            StatusCode::OK,
            [("dav", "1, 2"), ("allow", "OPTIONS, GET, HEAD, PROPFIND"), ("ms-author-via", "DAV")],
        )
            .into_response(),
        Method::GET | Method::HEAD => {
            let mut out = String::from("Hearthdrive storage:\n");
            for (name, root) in &roots {
                out.push_str(&format!("/dav/{name}/  ({})\n", root.name));
            }
            out.into_response()
        }
        _ if method.as_str() == "PROPFIND" => {
            let entry = |href: &str, display: &str| {
                format!(
                    "<D:response><D:href>{}</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype>\
                     <D:displayname>{}</D:displayname></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
                    xml_escape(href),
                    xml_escape(display)
                )
            };
            let mut body = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">");
            body.push_str(&entry("/dav/", "Hearthdrive"));
            if !depth0 {
                for (name, root) in &roots {
                    body.push_str(&entry(&format!("/dav/{name}/"), &root.name));
                }
            }
            body.push_str("</D:multistatus>");
            (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], body).into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}
