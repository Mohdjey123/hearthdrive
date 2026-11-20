//! Documents: PDFs, Office files and pictures of paperwork, with the date,
//! sender and amount read from their text, and details people can correct.

use super::audit;
use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::media;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, Query, State};
use nexus_common::NexusError;
use nexus_common::time::now_ms;
use nexus_database::{Count, DocEdit, DocFilter, DocMeta, DocRow, DocState, FileTag};
use nexus_security::Action;
use serde::{Deserialize, Serialize};

const MAX_PAGE: i64 = 200;
const TICKET_TTL_SECS: i64 = 6 * 60 * 60;

#[derive(Deserialize, Default)]
pub struct DocQuery {
    q: Option<String>,
    tag: Option<String>,
    year: Option<String>,
    sender: Option<String>,
    root_id: Option<String>,
    offset: Option<i64>,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub struct DocView {
    #[serde(flatten)]
    row: DocRow,
    /// A preview of the first page, without signing in (`<img src>`).
    thumb_url: Option<String>,
}

#[derive(Serialize)]
pub struct DocPage {
    items: Vec<DocView>,
    /// More may follow from `offset + limit`.
    more: bool,
    years: Vec<Count>,
    senders: Vec<Count>,
    tags: Vec<Count>,
}

/// The storage locations someone may see documents in (`None`: all).
async fn roots_for(s: &AppState, user: &AuthUser, only: Option<String>) -> ApiResult<Option<Vec<String>>> {
    user.require(Action::Read)?;
    let roots = s.db.list_roots().await?;
    let mut visible: Option<Vec<String>> =
        user.is_limited().then(|| roots.iter().filter(|r| user.sees_any(&r.id)).map(|r| r.id.clone()).collect());
    if let Some(only) = only {
        visible = Some(match visible {
            Some(v) => v.into_iter().filter(|r| *r == only).collect(),
            None => vec![only],
        });
    }
    Ok(visible)
}

fn view(s: &AppState, row: DocRow) -> DocView {
    let ext = row.file.extension.as_deref().unwrap_or_default().to_ascii_lowercase();
    let thumb_url = media::has_preview(&ext).then(|| {
        let t = s.tickets.issue("thumb", &row.file.id, TICKET_TTL_SECS);
        format!("/api/v1/files/{}/thumbnail?size=256&ticket={t}", row.file.id)
    });
    DocView { row, thumb_url }
}

/// `GET /documents`: documents newest first, with counts for the filters.
pub async fn list(State(s): State<SharedState>, user: AuthUser, Query(q): Query<DocQuery>) -> ApiResult<Json<DocPage>> {
    let filter = DocFilter {
        roots: roots_for(&s, &user, q.root_id).await?,
        query: q.q.filter(|t| !t.trim().is_empty()),
        tag: q.tag.filter(|t| !t.is_empty()),
        year: q.year.filter(|y| y.len() == 4 && y.chars().all(|c| c.is_ascii_digit())),
        sender: q.sender.filter(|t| !t.is_empty()),
    };
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_PAGE);
    let rows = s.db.documents(&filter, q.offset.unwrap_or(0).max(0), limit).await?;
    let more = rows.len() as i64 == limit;
    let rows: Vec<DocRow> = if user.is_limited() {
        rows.into_iter().filter(|r| user.may(&r.file.root_id, &r.file.relative_path, Need::Read)).collect()
    } else {
        rows
    };
    let (years, senders, tags) = s.db.document_counts(&filter).await?;
    Ok(Json(DocPage { items: rows.into_iter().map(|r| view(&s, r)).collect(), more, years, senders, tags }))
}

#[derive(Serialize)]
pub struct DocDetails {
    meta: DocMeta,
    tags: Vec<FileTag>,
    text: Option<DocState>,
    /// Whether this person may change the details.
    can_edit: bool,
}

async fn details(s: &AppState, user: &AuthUser, id: &str) -> ApiResult<DocDetails> {
    let file = s.db.get_file(id).await?;
    Ok(DocDetails {
        meta: s.db.doc_meta(id).await?.unwrap_or_default(),
        tags: s.db.file_tags(id).await?,
        text: s.db.doc_state(id).await?,
        can_edit: user.require(Action::Write).is_ok() && user.may(&file.root_id, &file.relative_path, Need::Change),
    })
}

/// `GET /documents/{id}`
pub async fn get(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<DocDetails>> {
    user.require(Action::Read)?;
    let file = s.db.get_file(&id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    Ok(Json(details(&s, &user, &id).await?))
}

#[derive(Deserialize)]
pub struct DocBody {
    title: Option<String>,
    /// `YYYY-MM-DD`.
    date: Option<String>,
    sender: Option<String>,
    amount: Option<String>,
    note: Option<String>,
}

fn field(v: Option<String>, max: usize, what: &str) -> ApiResult<Option<String>> {
    let v = v.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if v.as_ref().is_some_and(|v| v.chars().count() > max || v.chars().any(|c| c.is_control() && c != '\n')) {
        return Err(NexusError::invalid(format!("The {what} is too long or has characters it cannot have")).into());
    }
    Ok(v)
}

/// `PATCH /documents/{id}`: sets the title, date, sender, amount and note
/// (an empty value shows what was found in the text again).
pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(b): Json<DocBody>,
) -> ApiResult<Json<DocDetails>> {
    user.require(Action::Write)?;
    let file = s.db.get_file(&id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Change)?;
    let date = field(b.date, 10, "date")?;
    if let Some(d) = &date
        && chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_err()
    {
        return Err(NexusError::invalid("The date must look like 2026-03-31").into());
    }
    let edit = DocEdit {
        title: field(b.title, 200, "title")?,
        doc_date: date,
        sender: field(b.sender, 120, "sender")?,
        amount: field(b.amount, 40, "amount")?,
        note: field(b.note, 4000, "note")?,
    };
    s.db.set_doc_meta(&file, &edit, &user.username, now_ms()).await?;
    audit(&s, &user, "document.edited", Some(&file.relative_path)).await;
    Ok(Json(details(&s, &user, &id).await?))
}
