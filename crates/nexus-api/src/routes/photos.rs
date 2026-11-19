//! Photos: a timeline, months, a map, memories, photos that look alike,
//! previews (also of camera raw files), and albums that can be shared.

use super::audit;
use crate::auth::{AuthUser, Need, authenticate, bearer};
use crate::error::ApiResult;
use crate::media;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use nexus_common::time::now_ms;
use nexus_common::{NexusError, new_id};
use nexus_database::{Album, AlbumMember, MediaItem, MonthCount, PhotoFilter, Place};
use nexus_security::Action;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tower::ServiceExt;
use tower_http::services::ServeFile;

const TICKET_TTL_SECS: i64 = 6 * 60 * 60;
const MAX_PAGE: i64 = 500;
/// Photos whose hashes differ in at most this many bits look alike.
const SIMILAR_BITS: u32 = 6;
const MAX_SIMILAR_GROUPS: usize = 200;

/// Who someone is, in album ownership and membership.
fn who(user: &AuthUser) -> String {
    match (&user.user_id, &user.device_id) {
        (Some(id), _) => format!("user:{id}"),
        (None, Some(dev)) => format!("device:{dev}"),
        _ => "local".into(),
    }
}

/// The storage locations a person may see photos in (`None`: all).
fn visible_roots(s: &AppState, user: &AuthUser, roots: &[nexus_common::types::StorageRoot]) -> Option<Vec<String>> {
    let _ = s;
    user.is_limited().then(|| roots.iter().filter(|r| user.sees_any(&r.id)).map(|r| r.id.clone()).collect())
}

#[derive(Serialize)]
pub struct PhotoView {
    #[serde(flatten)]
    item: MediaItem,
    /// A small preview, without signing in (`<img src>`).
    thumb_url: Option<String>,
    /// A bigger preview (for camera raw and HEIC photos, which browsers
    /// cannot show).
    preview_url: Option<String>,
    /// The photo itself without signing in (albums shared with people who
    /// cannot see the storage it is in).
    #[serde(skip_serializing_if = "Option::is_none")]
    content_url: Option<String>,
}

fn view(s: &AppState, item: MediaItem) -> PhotoView {
    let has_preview = item.media.kind == "photo" && item.media.phash.is_some();
    let ticket = has_preview.then(|| s.tickets.issue("thumb", &item.file.id, TICKET_TTL_SECS));
    let url = |size: u32| ticket.as_ref().map(|t| format!("/api/v1/files/{}/thumbnail?size={size}&ticket={t}", item.file.id));
    PhotoView { thumb_url: url(256), preview_url: url(1024), content_url: None, item }
}

#[derive(Deserialize, Default)]
pub struct PhotoQuery {
    /// `photo` (default) or `video`.
    kind: Option<String>,
    from: Option<i64>,
    to: Option<i64>,
    camera: Option<String>,
    root_id: Option<String>,
    /// `1`: only photos with a place.
    place: Option<u8>,
    /// `1`: only photos with people in them.
    people: Option<u8>,
    offset: Option<i64>,
    limit: Option<i64>,
}

impl PhotoQuery {
    fn filter(&self, roots: Option<Vec<String>>) -> Result<PhotoFilter, NexusError> {
        let kind = self.kind.clone().unwrap_or_else(|| "photo".into());
        if !matches!(kind.as_str(), "photo" | "video") {
            return Err(NexusError::invalid("Ask for photos or videos"));
        }
        let roots = match (&self.root_id, roots) {
            (Some(r), Some(list)) => Some(list.into_iter().filter(|x| x == r).collect()),
            (Some(r), None) => Some(vec![r.clone()]),
            (None, list) => list,
        };
        Ok(PhotoFilter {
            roots,
            kind: Some(kind),
            from: self.from,
            to: self.to,
            with_place: self.place == Some(1),
            with_people: self.people == Some(1),
            camera: self.camera.clone().filter(|c| !c.is_empty()),
            ..Default::default()
        })
    }
}

async fn filter_for(s: &AppState, user: &AuthUser, q: &PhotoQuery) -> ApiResult<PhotoFilter> {
    user.require(Action::Read)?;
    let roots = s.db.list_roots().await?;
    Ok(q.filter(visible_roots(s, user, &roots))?)
}

/// Drops what a person with shared folders only may not see.
fn readable(user: &AuthUser, items: Vec<MediaItem>) -> Vec<MediaItem> {
    if !user.is_limited() {
        return items;
    }
    items.into_iter().filter(|i| user.may(&i.file.root_id, &i.file.relative_path, Need::Read)).collect()
}

#[derive(Serialize)]
pub struct PhotoPage {
    items: Vec<PhotoView>,
    /// More may follow from `offset + limit`.
    more: bool,
}

/// `GET /photos`: photos (or videos), newest first.
pub async fn list(State(s): State<SharedState>, user: AuthUser, Query(q): Query<PhotoQuery>) -> ApiResult<Json<PhotoPage>> {
    let filter = filter_for(&s, &user, &q).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_PAGE);
    let items = s.db.photos(&filter, q.offset.unwrap_or(0).max(0), limit).await?;
    let more = items.len() as i64 == limit;
    Ok(Json(PhotoPage { items: readable(&user, items).into_iter().map(|i| view(&s, i)).collect(), more }))
}

#[derive(Serialize)]
pub struct Overview {
    months: Vec<MonthCount>,
    cameras: Vec<(String, i64)>,
    /// Photos not read yet (previews and details come shortly).
    reading: bool,
}

/// `GET /photos/months`: how many photos each month has, and the cameras.
pub async fn months(State(s): State<SharedState>, user: AuthUser, Query(q): Query<PhotoQuery>) -> ApiResult<Json<Overview>> {
    let filter = filter_for(&s, &user, &q).await?;
    let months = s.db.photo_months(&filter).await?;
    let cameras = s.db.photo_cameras(&filter).await?;
    let reading = !s.db.media_todo(&["jpg", "jpeg", "png", "heic", "dng", "cr2", "nef", "arw"], 1).await?.is_empty();
    Ok(Json(Overview { months, cameras, reading }))
}

/// `GET /photos/places`: where photos were taken, for the map.
pub async fn places(State(s): State<SharedState>, user: AuthUser, Query(q): Query<PhotoQuery>) -> ApiResult<Json<Vec<PlaceView>>> {
    let filter = filter_for(&s, &user, &q).await?;
    let places = s.db.photo_places(&filter, 20_000).await?;
    let places: Vec<Place> = if user.is_limited() {
        // Paths are needed to check shared folders.
        let ids: Vec<String> = places.iter().map(|p| p.file_id.clone()).collect();
        let ok: std::collections::HashSet<String> =
            readable(&user, s.db.media_items(&ids).await?).into_iter().map(|i| i.file.id).collect();
        places.into_iter().filter(|p| ok.contains(&p.file_id)).collect()
    } else {
        places
    };
    Ok(Json(
        places
            .into_iter()
            .map(|p| PlaceView { thumb_url: format!("/api/v1/files/{}/thumbnail?size=256&ticket={}", p.file_id, s.tickets.issue("thumb", &p.file_id, TICKET_TTL_SECS)), place: p })
            .collect(),
    ))
}

#[derive(Serialize)]
pub struct PlaceView {
    #[serde(flatten)]
    place: Place,
    thumb_url: String,
}

#[derive(Serialize)]
pub struct Memory {
    year: i32,
    years_ago: i32,
    items: Vec<PhotoView>,
}

#[derive(Deserialize)]
pub struct MemoryQuery {
    /// `MM-DD`; today if left out.
    day: Option<String>,
}

/// `GET /photos/memories`: photos taken on this day in earlier years.
pub async fn memories(State(s): State<SharedState>, user: AuthUser, Query(q): Query<MemoryQuery>) -> ApiResult<Json<Vec<Memory>>> {
    use chrono::Datelike;
    let today = chrono::Local::now().naive_local();
    let day = q.day.unwrap_or_else(|| today.format("%m-%d").to_string());
    if day.len() != 5 || !day.chars().enumerate().all(|(i, c)| if i == 2 { c == '-' } else { c.is_ascii_digit() }) {
        return Err(NexusError::invalid("Give the day as MM-DD").into());
    }
    let start_of_year = chrono::NaiveDate::from_ymd_opt(today.year(), 1, 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc().timestamp_millis())
        .unwrap_or(0);
    let base = filter_for(&s, &user, &PhotoQuery::default()).await?;
    let filter = PhotoFilter { on_day: Some((day, start_of_year)), ..base };
    let items = readable(&user, s.db.photos(&filter, 0, 300).await?);
    let mut by_year: Vec<Memory> = Vec::new();
    for item in items {
        let Some(taken) = item.media.taken_at else { continue };
        let Some(dt) = chrono::DateTime::from_timestamp_millis(taken) else { continue };
        let year = dt.year();
        match by_year.iter_mut().find(|m| m.year == year) {
            Some(m) if m.items.len() < 24 => m.items.push(view(&s, item)),
            Some(_) => {}
            None => by_year.push(Memory { year, years_ago: today.year() - year, items: vec![view(&s, item)] }),
        }
    }
    by_year.sort_by_key(|m| -m.year);
    Ok(Json(by_year))
}

/// Groups photo hashes that differ in at most `bits` bits. Candidates come
/// from eight 8-bit bands: two hashes within 6 bits share at least two bands.
pub fn similar_groups(hashes: &[(String, u64)], bits: u32) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..hashes.len()).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut i = i;
        while p[i] != r {
            let next = p[i];
            p[i] = r;
            i = next;
        }
        r
    }
    for band in 0..8 {
        let mut buckets: HashMap<u8, Vec<usize>> = HashMap::new();
        for (i, (_, h)) in hashes.iter().enumerate() {
            buckets.entry((h >> (band * 8)) as u8).or_default().push(i);
        }
        for members in buckets.values() {
            if members.len() > 2000 {
                continue; // A flat colour: everything looks alike.
            }
            for (a_pos, &a) in members.iter().enumerate() {
                for &b in &members[a_pos + 1..] {
                    if (hashes[a].1 ^ hashes[b].1).count_ones() <= bits {
                        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
                        if ra != rb {
                            parent[ra] = rb;
                        }
                    }
                }
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..hashes.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut out: Vec<Vec<usize>> = groups.into_values().filter(|g| g.len() > 1).collect();
    out.sort_by_key(|g| std::cmp::Reverse(g.len()));
    out
}

/// `GET /photos/similar`: photos that look alike (bursts, edits, copies in
/// other sizes), biggest groups first.
pub async fn similar(State(s): State<SharedState>, user: AuthUser, Query(q): Query<PhotoQuery>) -> ApiResult<Json<Vec<Vec<PhotoView>>>> {
    let filter = filter_for(&s, &user, &q).await?;
    let hashes = s.db.photo_hashes(filter.roots.as_deref()).await?;
    let parsed: Vec<(String, u64)> =
        hashes.into_iter().filter_map(|h| u64::from_str_radix(&h.phash, 16).ok().map(|v| (h.file_id, v))).collect();
    let groups = tokio::task::spawn_blocking(move || {
        let groups = similar_groups(&parsed, SIMILAR_BITS);
        groups
            .into_iter()
            .take(MAX_SIMILAR_GROUPS)
            .map(|g| g.into_iter().map(|i| parsed[i].0.clone()).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| NexusError::internal(e.to_string()))?;
    let mut out = Vec::new();
    for ids in groups {
        let mut items = readable(&user, s.db.media_items(&ids).await?);
        if items.len() < 2 {
            continue;
        }
        // Biggest (most detailed) first.
        items.sort_by_key(|i| std::cmp::Reverse((i.media.width.unwrap_or(0) * i.media.height.unwrap_or(0), i.file.size)));
        out.push(items.into_iter().map(|i| view(&s, i)).collect());
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct ThumbQuery {
    size: Option<u32>,
    ticket: Option<String>,
}

/// `GET /files/{id}/thumbnail?size=`: a JPEG preview of a photo (256 or
/// 1024 pixels), also for camera raw and HEIC photos.
pub async fn thumbnail(
    State(s): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<ThumbQuery>,
    req: Request,
) -> ApiResult<Response> {
    let caller = match q.ticket.as_deref() {
        Some(t) if s.tickets.verify(t, "thumb", &id) || s.tickets.verify(t, "content", &id) => None,
        Some(_) => return Err(NexusError::Unauthorized.into()),
        None => match bearer(req.headers()) {
            Some(token) => Some(authenticate(&s, token).await?),
            None => return Err(NexusError::Unauthorized.into()),
        },
    };
    let file = s.db.get_file(&id).await?;
    if let Some(user) = &caller {
        user.require(Action::Read)?;
        if user.require_access(&file.root_id, &file.relative_path, Need::Read).is_err() && !s.db.in_shared_album(&file.id, &who(user)).await? {
            return Err(NexusError::PermissionDenied("You cannot see this photo".into()).into());
        }
    }
    let ext = file.extension.as_deref().unwrap_or_default().to_ascii_lowercase();
    if file.is_dir || !media::is_photo(&ext) {
        return Err(NexusError::invalid("Only photos have previews").into());
    }
    let root = s.db.get_root(&file.root_id).await?;
    let data_dir = s.data_dir.clone();
    let size = q.size.unwrap_or(256);
    let cached = media::preview_path(&data_dir, &file, media::SIZES.iter().copied().find(|x| *x >= size).unwrap_or(1024));
    let path = if cached.exists() {
        cached
    } else {
        s.storage.ensure_online(&root)?;
        let source = s.storage.resolve(&root, &file.relative_path)?;
        let f = file.clone();
        tokio::task::spawn_blocking(move || media::preview(&data_dir, &f, &source, size))
            .await
            .map_err(|e| NexusError::internal(e.to_string()))?
            .map_err(|e| NexusError::Unavailable(format!("No preview: {e}")))?
    };
    let response = match ServeFile::new(&path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let mut response = response.map(Body::new);
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    Ok(response)
}

// ---------------------------------------------------------------------------
// Albums

#[derive(Serialize)]
pub struct AlbumView {
    #[serde(flatten)]
    album: Album,
    mine: bool,
    can_add: bool,
    members: Vec<AlbumMember>,
    cover_url: Option<String>,
}

async fn album_view(s: &AppState, user: &AuthUser, album: Album) -> ApiResult<AlbumView> {
    let me = who(user);
    let members = s.db.album_members(&album.id).await?;
    let mine = album.created_by == me || (user.role == nexus_common::types::Role::Owner && user.device_id.is_none());
    let can_add = mine || members.iter().any(|m| m.member == me && m.can_add);
    let cover_url = album
        .cover_id
        .as_ref()
        .map(|id| format!("/api/v1/files/{id}/thumbnail?size=256&ticket={}", s.tickets.issue("thumb", id, TICKET_TTL_SECS)));
    Ok(AlbumView { album, mine, can_add, members, cover_url })
}

async fn album_for(s: &AppState, user: &AuthUser, id: &str) -> ApiResult<AlbumView> {
    user.require(Action::Read)?;
    let album = s.db.get_album(id).await?;
    let view = album_view(s, user, album).await?;
    let me = who(user);
    if !view.mine && !view.members.iter().any(|m| m.member == me) {
        return Err(NexusError::NotFound("album".into()).into());
    }
    Ok(view)
}

/// `GET /albums`: albums this person made or that are shared with them.
pub async fn albums(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<AlbumView>>> {
    user.require(Action::Read)?;
    let owner = user.role == nexus_common::types::Role::Owner && user.device_id.is_none();
    let me = who(&user);
    let list = s.db.list_albums(if owner { None } else { Some(&me) }).await?;
    let mut out = Vec::with_capacity(list.len());
    for a in list {
        out.push(album_view(&s, &user, a).await?);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct NewAlbum {
    name: String,
    #[serde(default)]
    file_ids: Vec<String>,
}

fn album_name(name: &str) -> Result<String, NexusError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err(NexusError::invalid("Give the album a name of up to 120 characters"));
    }
    Ok(name.to_string())
}

/// `POST /albums`
pub async fn create_album(State(s): State<SharedState>, user: AuthUser, Json(body): Json<NewAlbum>) -> ApiResult<Json<AlbumView>> {
    user.require_person()?;
    user.require(Action::Write)?;
    let id = new_id();
    s.db.create_album(&id, &album_name(&body.name)?, &who(&user), now_ms()).await?;
    add_items(&s, &user, &id, &body.file_ids).await?;
    audit(&s, &user, "album.created", Some(&body.name)).await;
    Ok(Json(album_for(&s, &user, &id).await?))
}

async fn add_items(s: &AppState, user: &AuthUser, album: &str, ids: &[String]) -> ApiResult<usize> {
    let mut added = 0;
    for id in ids.iter().take(5000) {
        let file = s.db.get_file(id).await?;
        // Only photos and videos the person can see go in.
        user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
        if s.db.add_to_album(album, &file.id, &file.root_id, &who(user), now_ms()).await? {
            added += 1;
        }
    }
    Ok(added)
}

#[derive(Serialize)]
pub struct AlbumDetail {
    #[serde(flatten)]
    album: AlbumView,
    items: Vec<PhotoView>,
}

/// `GET /albums/{id}`: an album and its photos. Members see them even
/// outside storage they can see.
pub async fn album(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<AlbumDetail>> {
    let album = album_for(&s, &user, &id).await?;
    let filter = PhotoFilter { album: Some(id.clone()), roots: None, ..Default::default() };
    let mut items = s.db.photos(&filter, 0, 5000).await?;
    items.extend(s.db.photos(&PhotoFilter { kind: Some("video".into()), ..filter }, 0, 2000).await?);
    items.sort_by_key(|i| std::cmp::Reverse(i.media.taken_at.or(i.file.modified_at)));
    let items = items
        .into_iter()
        .map(|i| {
            let content = s.tickets.issue("content", &i.file.id, TICKET_TTL_SECS);
            let mut v = view(&s, i);
            v.content_url = Some(format!("/api/v1/files/{}/content?ticket={content}", v.item.file.id));
            if v.thumb_url.is_none() && v.item.media.kind == "photo" {
                // Not read yet: the content ticket opens the preview too.
                v.thumb_url = Some(format!("/api/v1/files/{}/thumbnail?size=256&ticket={content}", v.item.file.id));
            }
            v
        })
        .collect();
    Ok(Json(AlbumDetail { album, items }))
}

#[derive(Deserialize)]
pub struct AlbumChange {
    name: Option<String>,
    cover_id: Option<String>,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
    /// Who the album is shared with: `user:<id>` or `device:<id>`.
    members: Option<Vec<MemberBody>>,
}

#[derive(Deserialize)]
pub struct MemberBody {
    member: String,
    #[serde(default)]
    can_add: bool,
}

/// `PATCH /albums/{id}`: rename, add or remove photos, share.
pub async fn change_album(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<AlbumChange>,
) -> ApiResult<Json<AlbumView>> {
    user.require_person()?;
    let view = album_for(&s, &user, &id).await?;
    if !body.add.is_empty() {
        if !view.can_add {
            return Err(NexusError::PermissionDenied("You cannot add to this album".into()).into());
        }
        add_items(&s, &user, &id, &body.add).await?;
    }
    let owner_change = body.name.is_some() || body.cover_id.is_some() || !body.remove.is_empty() || body.members.is_some();
    if owner_change && !view.mine {
        return Err(NexusError::PermissionDenied("Only the person who made the album can change it".into()).into());
    }
    if body.name.is_some() || body.cover_id.is_some() {
        let name = match &body.name {
            Some(n) => album_name(n)?,
            None => view.album.name.clone(),
        };
        s.db.rename_album(&id, &name, body.cover_id.as_deref()).await?;
    }
    for file in &body.remove {
        s.db.remove_from_album(&id, file).await?;
    }
    if let Some(members) = &body.members {
        let mut list = Vec::new();
        for m in members.iter().take(100) {
            let ok = match m.member.split_once(':') {
                Some(("user", uid)) => s.db.get_user(uid).await.is_ok(),
                Some(("device", did)) => s.db.get_device(did).await.is_ok_and(|d| !d.is_local),
                _ => false,
            };
            if !ok {
                return Err(NexusError::invalid(format!("Unknown person or device: {}", m.member)).into());
            }
            list.push((m.member.clone(), m.can_add));
        }
        s.db.set_album_members(&id, &list).await?;
        audit(&s, &user, "album.shared", Some(&view.album.name)).await;
    }
    Ok(Json(album_for(&s, &user, &id).await?))
}

/// `DELETE /albums/{id}`: the album goes; its photos stay where they are.
pub async fn delete_album(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    let view = album_for(&s, &user, &id).await?;
    if !view.mine {
        return Err(NexusError::PermissionDenied("Only the person who made the album can delete it".into()).into());
    }
    s.db.delete_album(&id).await?;
    audit(&s, &user, "album.deleted", Some(&view.album.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_photos_that_look_alike() {
        let hashes = vec![
            ("a".to_string(), 0xF0F0_F0F0_F0F0_F0F0u64),
            ("b".to_string(), 0xF0F0_F0F0_F0F0_F0F3u64), // 2 bits from a
            ("c".to_string(), 0x0F0F_0F0F_0F0F_0F0Fu64), // far
            ("d".to_string(), 0xF0F0_F0F0_F0F0_F0F7u64), // 1 bit from b
        ];
        let groups = similar_groups(&hashes, 6);
        assert_eq!(groups.len(), 1);
        let mut g: Vec<&str> = groups[0].iter().map(|i| hashes[*i].0.as_str()).collect();
        g.sort();
        assert_eq!(g, ["a", "b", "d"]);
    }
}
