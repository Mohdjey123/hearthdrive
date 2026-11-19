//! The media library (music by album, films, series and home videos, with
//! where each person stopped), tags on files, and document reading settings.

use super::audit;
use crate::auth::{AuthUser, Need, authenticate, bearer};
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use nexus_common::NexusError;
use nexus_common::time::now_ms;
use nexus_database::{FileTag, LibraryFilter, MediaItem, MusicAlbum, Playback, TagCount};
use nexus_security::Action;
use serde::{Deserialize, Serialize};

const TICKET_TTL_SECS: i64 = 6 * 60 * 60;

/// Whose place in a video this is: the same name on every device, so a
/// film started on the laptop continues on the phone.
fn viewer(user: &AuthUser) -> String {
    format!("name:{}", user.username.to_lowercase())
}

fn visible_roots(user: &AuthUser, roots: &[nexus_common::types::StorageRoot]) -> Option<Vec<String>> {
    user.is_limited().then(|| roots.iter().filter(|r| user.sees_any(&r.id)).map(|r| r.id.clone()).collect())
}

fn readable(user: &AuthUser, items: Vec<MediaItem>) -> Vec<MediaItem> {
    if !user.is_limited() {
        return items;
    }
    items.into_iter().filter(|i| user.may(&i.file.root_id, &i.file.relative_path, Need::Read)).collect()
}

async fn roots_for(s: &AppState, user: &AuthUser) -> ApiResult<Option<Vec<String>>> {
    user.require(Action::Read)?;
    Ok(visible_roots(user, &s.db.list_roots().await?))
}

/// `GET /library/music`: albums, by artist.
pub async fn music(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<MusicAlbum>>> {
    let roots = roots_for(&s, &user).await?;
    Ok(Json(s.db.music_albums(roots.as_deref()).await?))
}

#[derive(Deserialize)]
pub struct ItemsQuery {
    /// `music` (an album's songs), `film`, `episode`, `home_video`.
    #[serde(rename = "type")]
    kind: String,
    album: Option<String>,
    artist: Option<String>,
    show: Option<String>,
}

#[derive(Serialize)]
pub struct LibraryItem {
    #[serde(flatten)]
    item: MediaItem,
    /// Where this person stopped.
    playback: Option<Playback>,
}

/// `GET /library/items?type=`: an album's songs, or films, episodes or
/// home videos, with where this person stopped in each.
pub async fn items(State(s): State<SharedState>, user: AuthUser, Query(q): Query<ItemsQuery>) -> ApiResult<Json<Vec<LibraryItem>>> {
    if !matches!(q.kind.as_str(), "music" | "film" | "episode" | "home_video") {
        return Err(NexusError::invalid("Ask for music, film, episode or home_video").into());
    }
    let roots = roots_for(&s, &user).await?;
    let filter = LibraryFilter {
        roots,
        media_type: (q.kind != "music").then(|| q.kind.clone()).or(Some("music".into())),
        album: q.album.clone(),
        artist: q.artist.clone(),
        show: q.show.clone(),
    };
    let items = readable(&user, s.db.library_items(&filter, 5000).await?);
    let who = viewer(&user);
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let playback = if item.media.kind == "video" { s.db.get_playback(&who, &item.file.id).await? } else { None };
        out.push(LibraryItem { item, playback });
    }
    Ok(Json(out))
}

#[derive(Serialize)]
pub struct ContinueItem {
    #[serde(flatten)]
    item: MediaItem,
    playback: Playback,
}

/// `GET /playback`: what this person started watching and did not finish.
pub async fn continue_watching(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<ContinueItem>>> {
    user.require(Action::Read)?;
    let list = s.db.continue_watching(&viewer(&user), 20).await?;
    let ids: Vec<String> = list.iter().map(|p| p.file_id.clone()).collect();
    let items = readable(&user, s.db.media_items(&ids).await?);
    Ok(Json(
        list.into_iter()
            .filter_map(|p| items.iter().find(|i| i.file.id == p.file_id).cloned().map(|item| ContinueItem { item, playback: p }))
            .collect(),
    ))
}

/// `GET /playback/{file_id}`
pub async fn get_playback(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Option<Playback>>> {
    user.require(Action::Read)?;
    Ok(Json(s.db.get_playback(&viewer(&user), &id).await?))
}

#[derive(Deserialize)]
pub struct PlaybackBody {
    position_ms: i64,
    duration_ms: Option<i64>,
    /// Which device it was watched on (shown as "on Laptop").
    device: Option<String>,
}

/// `PUT /playback/{file_id}`: where this person is in a video.
pub async fn set_playback(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(b): Json<PlaybackBody>,
) -> ApiResult<StatusCode> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    if b.position_ms < 0 || b.duration_ms.is_some_and(|d| d < 0) {
        return Err(NexusError::invalid("Give a position in milliseconds").into());
    }
    let device = b.device.unwrap_or_default().chars().filter(|c| !c.is_control()).take(64).collect::<String>();
    s.db.save_playback(&viewer(&user), &id, b.position_ms, b.duration_ms, &device, now_ms()).await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Converting videos for playback (only with ffmpeg installed)

fn ffmpeg() -> Option<std::path::PathBuf> {
    static FOUND: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
            std::env::var_os("PATH")
                .map(|paths| std::env::split_paths(&paths).map(|p| p.join(exe)).find(|p| p.is_file()))
                .flatten()
        })
        .clone()
}

#[derive(Serialize)]
pub struct Capabilities {
    /// Videos browsers cannot play can be converted while they play.
    transcode: bool,
    /// Text in scanned documents and pictures can be read (Windows).
    ocr: bool,
    /// Faces can be found in photos (Windows).
    faces: bool,
    /// Text in pictures is read too (a setting).
    ocr_images: bool,
}

/// `GET /library/capabilities`
pub async fn capabilities(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Capabilities>> {
    user.require(Action::Read)?;
    let ocr = tokio::task::spawn_blocking(crate::winrt::available).await.unwrap_or(false);
    let ocr_images = s.db.get_setting(crate::documents::OCR_IMAGES_SETTING).await?.as_deref() == Some("true");
    Ok(Json(Capabilities { transcode: ffmpeg().is_some(), ocr, faces: cfg!(windows), ocr_images }))
}

#[derive(Deserialize)]
pub struct StreamQuery {
    ticket: Option<String>,
    /// Where to start, in seconds.
    start: Option<f64>,
}

/// `GET /files/{id}/stream?start=`: a video converted to MP4 (H.264 and
/// AAC) while it plays, for formats browsers cannot play. Needs ffmpeg on
/// this computer; nothing is downloaded for it.
pub async fn stream(State(s): State<SharedState>, Path(id): Path<String>, Query(q): Query<StreamQuery>, req: Request) -> ApiResult<Response> {
    let caller = match q.ticket.as_deref() {
        Some(t) if s.tickets.verify(t, "content", &id) => None,
        Some(_) => return Err(NexusError::Unauthorized.into()),
        None => match bearer(req.headers()) {
            Some(token) => Some(authenticate(&s, token).await?),
            None => return Err(NexusError::Unauthorized.into()),
        },
    };
    let file = s.db.get_file(&id).await?;
    if let Some(user) = &caller {
        user.require(Action::Read)?;
        user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    }
    let Some(exe) = ffmpeg() else {
        return Err(NexusError::Unavailable("Converting videos needs ffmpeg installed on this computer".into()).into());
    };
    let root = s.db.get_root(&file.root_id).await?;
    s.storage.ensure_online(&root)?;
    let path = s.storage.resolve(&root, &file.relative_path)?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.kill_on_drop(true).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // No console window.
    if let Some(start) = q.start.filter(|t| t.is_finite() && *t > 0.0) {
        cmd.arg("-ss").arg(format!("{start:.3}"));
    }
    cmd.arg("-i").arg(&path).args([
        "-map", "0:v:0?", "-map", "0:a:0?", "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", "160k", "-ac", "2", "-movflags", "frag_keyframe+empty_moov+default_base_moof", "-f", "mp4", "pipe:1",
    ]);
    let mut child = cmd.spawn().map_err(|e| NexusError::Unavailable(format!("ffmpeg did not start: {e}")))?;
    let stdout = child.stdout.take().ok_or_else(|| NexusError::internal("no output"))?;
    // The child ends when the stream is dropped (the viewer stopped).
    let body = Body::from_stream(futures_util::stream::unfold((stdout, child), |(mut out, child)| async move {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 256 * 1024];
        match out.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), (out, child)))
            }
            Err(e) => Some((Err(e), (out, child))),
        }
    }));
    let mut res = Response::new(body);
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// `POST /files/{id}/stream-link`: a link to the converted video.
pub async fn stream_link(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    let ticket = s.tickets.issue("content", &file.id, TICKET_TTL_SECS);
    Ok(Json(serde_json::json!({ "url": format!("/api/v1/files/{}/stream?ticket={ticket}", file.id) })))
}

// ---------------------------------------------------------------------------
// Tags

fn clean_tags(tags: &[String]) -> Result<Vec<String>, NexusError> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.iter().take(50) {
        let t = t.trim();
        if t.is_empty() {
            continue;
        }
        if t.len() > 60 || t.chars().any(char::is_control) {
            return Err(NexusError::invalid("Tags are up to 60 characters"));
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(t)) {
            out.push(t.to_string());
        }
    }
    Ok(out)
}

#[derive(Serialize)]
pub struct TagsView {
    tags: Vec<FileTag>,
    /// How the document's text was read (`pdf`, `ocr`…) and how much.
    text: Option<nexus_database::DocState>,
}

/// `GET /files/{id}/tags`
pub async fn file_tags(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<TagsView>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    Ok(Json(TagsView { tags: s.db.file_tags(&id).await?, text: s.db.doc_state(&id).await? }))
}

#[derive(Deserialize)]
pub struct TagsBody {
    /// The tags people give the file (replacing theirs).
    tags: Vec<String>,
    /// Tags NEXUS gave it that should stay (the others are removed).
    #[serde(default)]
    keep_auto: Option<Vec<String>>,
}

/// `PUT /files/{id}/tags`
pub async fn set_file_tags(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(b): Json<TagsBody>,
) -> ApiResult<Json<TagsView>> {
    user.require(Action::Write)?;
    let file = s.db.get_file(&id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Change)?;
    let tags = clean_tags(&b.tags)?;
    let now = now_ms();
    s.db.set_file_tags(&file, &tags, false, &user.username, now).await?;
    if let Some(keep) = b.keep_auto {
        let current: Vec<String> = s.db.file_tags(&id).await?.into_iter().filter(|t| t.auto).map(|t| t.tag).collect();
        let kept: Vec<String> = current.into_iter().filter(|t| keep.iter().any(|k| k.eq_ignore_ascii_case(t))).collect();
        s.db.set_file_tags(&file, &kept, true, "nexus", now).await?;
    }
    audit(&s, &user, "file.tagged", Some(&file.relative_path)).await;
    Ok(Json(TagsView { tags: s.db.file_tags(&id).await?, text: s.db.doc_state(&id).await? }))
}

/// `GET /tags`: tags in use, most used first.
pub async fn tags(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<TagCount>>> {
    let roots = roots_for(&s, &user).await?;
    Ok(Json(s.db.all_tags(roots.as_deref()).await?))
}

#[derive(Deserialize)]
pub struct DocumentSettings {
    ocr_images: bool,
}

/// `PUT /documents/settings`: whether text in pictures is read too.
pub async fn set_document_settings(State(s): State<SharedState>, user: AuthUser, Json(b): Json<DocumentSettings>) -> ApiResult<Json<Capabilities>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    s.db.set_setting(crate::documents::OCR_IMAGES_SETTING, if b.ocr_images { "true" } else { "false" }).await?;
    audit(&s, &user, "documents.settings", None).await;
    capabilities(State(s), user).await
}
