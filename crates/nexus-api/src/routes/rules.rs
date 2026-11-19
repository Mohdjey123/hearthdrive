//! Automation rules and storage suggestions.

use super::audit;
use crate::auth::AuthUser;
use crate::automation::{self, Conditions, PreviewItem, RuleAction, RunReport};
use crate::error::ApiResult;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use nexus_common::time::now_ms;
use nexus_common::types::{FileEntry, Role};
use nexus_common::{NexusError, new_id};
use nexus_database::Rule;
use nexus_storage::StorageManager;
use serde::{Deserialize, Serialize};

fn allowed(user: &AuthUser) -> Result<(), NexusError> {
    user.require_person().map_err(|e| e.0)?;
    if !matches!(user.role, Role::Owner | Role::Administrator) {
        return Err(NexusError::PermissionDenied("Only an owner or administrator can set up rules".into()));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct RuleView {
    #[serde(flatten)]
    rule: Rule,
    #[serde(rename = "when")]
    conditions_parsed: Option<Conditions>,
    #[serde(rename = "then")]
    action_parsed: Option<RuleAction>,
    root_name: Option<String>,
}

async fn view(s: &SharedState, rule: Rule) -> RuleView {
    let parsed = automation::parse(&rule).ok();
    let root_name = s.db.get_root(&rule.root_id).await.ok().map(|r| r.name);
    RuleView {
        conditions_parsed: parsed.as_ref().map(|p| p.0.clone()),
        action_parsed: parsed.map(|p| p.1),
        root_name,
        rule,
    }
}

/// `GET /rules`
pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<RuleView>>> {
    allowed(&user)?;
    let mut out = Vec::new();
    for rule in s.db.list_rules().await? {
        out.push(view(&s, rule).await);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct RuleBody {
    name: String,
    #[serde(default = "yes")]
    enabled: bool,
    trigger: String,
    root_id: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    when: Conditions,
    then: RuleAction,
}

fn yes() -> bool {
    true
}

async fn build(s: &SharedState, user: &AuthUser, body: RuleBody, existing: Option<Rule>) -> Result<Rule, NexusError> {
    let root = s.db.get_root(&body.root_id).await?;
    if root.device_id != s.node.device_id {
        return Err(NexusError::invalid("Rules look at storage on this computer"));
    }
    let now = now_ms();
    let rule = Rule {
        id: existing.as_ref().map(|r| r.id.clone()).unwrap_or_else(new_id),
        name: body.name.trim().to_string(),
        enabled: body.enabled,
        trigger: body.trigger,
        root_id: body.root_id,
        path: nexus_storage::paths::normalize_relative(&body.path)?,
        conditions: serde_json::to_string(&body.when).map_err(|e| NexusError::internal(e.to_string()))?,
        action: serde_json::to_string(&body.then).map_err(|e| NexusError::internal(e.to_string()))?,
        created_by: existing.as_ref().map(|r| r.created_by.clone()).unwrap_or_else(|| user.username.clone()),
        created_at: existing.as_ref().map(|r| r.created_at).unwrap_or(now),
        last_run: existing.as_ref().and_then(|r| r.last_run),
        last_result: existing.as_ref().and_then(|r| r.last_result.clone()),
        files_done: existing.as_ref().map(|r| r.files_done).unwrap_or(0),
    };
    automation::validate(&rule, &body.when, &body.then, &s.node.device_id)?;
    Ok(rule)
}

/// `POST /rules`
pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(body): Json<RuleBody>) -> ApiResult<(StatusCode, Json<RuleView>)> {
    allowed(&user)?;
    let rule = build(&s, &user, body, None).await?;
    s.db.save_rule(&rule).await?;
    audit(&s, &user, "rule.created", Some(&rule.name)).await;
    Ok((StatusCode::CREATED, Json(view(&s, rule).await)))
}

/// `PUT /rules/{id}`
pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<RuleBody>,
) -> ApiResult<Json<RuleView>> {
    allowed(&user)?;
    let old = s.db.get_rule(&id).await?;
    let rule = build(&s, &user, body, Some(old.clone())).await?;
    // Different files now: start afresh.
    if rule.conditions != old.conditions || rule.path != old.path || rule.root_id != old.root_id || rule.action != old.action {
        s.db.reset_rule(&id).await?;
    }
    s.db.save_rule(&rule).await?;
    audit(&s, &user, "rule.changed", Some(&rule.name)).await;
    Ok(Json(view(&s, rule).await))
}

/// `DELETE /rules/{id}`
pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    allowed(&user)?;
    let rule = s.db.get_rule(&id).await?;
    s.db.delete_rule(&id).await?;
    audit(&s, &user, "rule.deleted", Some(&rule.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /rules/{id}/run`: runs it now on every matching file.
pub async fn run(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<RunReport>> {
    allowed(&user)?;
    let rule = s.db.get_rule(&id).await?;
    let report = automation::run(&s, &rule, false).await?;
    audit(&s, &user, "rule.run", Some(&rule.name)).await;
    Ok(Json(report))
}

/// `GET /rules/{id}/preview`: what it would do now.
pub async fn preview(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Vec<PreviewItem>>> {
    allowed(&user)?;
    let rule = s.db.get_rule(&id).await?;
    Ok(Json(automation::preview(&s, &rule).await?))
}

/// `POST /rules/preview`: what a rule being written would do.
pub async fn preview_draft(State(s): State<SharedState>, user: AuthUser, Json(body): Json<RuleBody>) -> ApiResult<Json<Vec<PreviewItem>>> {
    allowed(&user)?;
    let mut rule = build(&s, &user, body, None).await?;
    rule.id = String::new();
    Ok(Json(automation::preview(&s, &rule).await?))
}

#[derive(Serialize)]
pub struct Suggestion {
    /// Stable, so people can dismiss one.
    id: String,
    /// `full` | `duplicates` | `sort` | `old_downloads`
    kind: &'static str,
    title: String,
    detail: String,
    root_id: String,
    /// Space it could free or tidy.
    bytes: i64,
    /// Files it is about (the biggest first), if any.
    files: Vec<FileEntry>,
    /// A rule that would do it, ready to be reviewed and saved.
    rule: Option<serde_json::Value>,
    /// Other storage, here or on a paired device, with room for the files.
    room_on: Vec<Room>,
}

#[derive(Serialize, Clone)]
pub struct Room {
    device_id: String,
    device_name: String,
    root_id: Option<String>,
    root_name: Option<String>,
    available_bytes: Option<u64>,
}

const FULL: f64 = 0.85;
const YEAR_MS: i64 = 365 * 24 * 3600 * 1000;
/// Tags with a folder of their own in `Documents`.
const SORTED_TAGS: &[(&str, &str)] = &[
    ("invoice", "Invoices"),
    ("receipt", "Receipts"),
    ("bank statement", "Bank statements"),
    ("tax", "Tax"),
    ("payslip", "Payslips"),
    ("contract", "Contracts"),
    ("insurance", "Insurance"),
    ("medical", "Medical"),
];

/// `GET /storage/suggestions`: where things could go, and what to tidy.
pub async fn suggestions(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<Suggestion>>> {
    allowed(&user)?;
    let here = s.node.device_id.clone();
    let roots: Vec<_> = s.db.list_roots().await?.into_iter().filter(|r| r.device_id == here && r.enabled).collect();
    let drives = tokio::task::spawn_blocking(nexus_storage::drives::list_drives).await.unwrap_or_default();
    let space = |path: &str| {
        let p = path.to_lowercase();
        drives
            .iter()
            .filter(|d| p.starts_with(&d.mount_point.to_lowercase()))
            .max_by_key(|d| d.mount_point.len())
            .map(|d| (d.mount_point.clone(), d.total_bytes, d.available_bytes))
    };

    // Where else there is room: other drives here, and paired devices.
    let mut rooms: Vec<(String, Room)> = Vec::new();
    for r in &roots {
        if StorageManager::is_online(r)
            && !r.read_only
            && let Some((mount, total, free)) = space(&r.absolute_path)
            && total > 0
            && (free as f64) / (total as f64) > 1.0 - FULL
        {
            rooms.push((
                mount,
                Room {
                    device_id: here.clone(),
                    device_name: "This computer".into(),
                    root_id: Some(r.id.clone()),
                    root_name: Some(r.name.clone()),
                    available_bytes: Some(free),
                },
            ));
        }
    }
    for d in s.db.list_devices().await?.into_iter().filter(|d| !d.is_local && d.status == "online" && d.blocked_at.is_none()) {
        rooms.push((String::new(), Room { device_id: d.id, device_name: d.name, root_id: None, root_name: None, available_bytes: None }));
    }

    let mut out = Vec::new();
    let now = now_ms();
    for root in &roots {
        if !StorageManager::is_online(root) {
            continue;
        }
        let mount = space(&root.absolute_path);
        // 1. A full drive: big files nobody changed for a year could live elsewhere.
        if let Some((m, total, free)) = &mount
            && *total > 0
            && (*free as f64) / (*total as f64) < 1.0 - FULL
        {
            let files = s.db.big_old_files(&root.id, now - YEAR_MS, 20).await?;
            let bytes: i64 = files.iter().map(|f| f.size).sum();
            if bytes > 0 {
                let pct = 100.0 - (*free as f64) * 100.0 / (*total as f64);
                out.push(Suggestion {
                    id: format!("full:{}", root.id),
                    kind: "full",
                    title: format!("{} is {:.0}% full", root.name, pct),
                    detail: "These big files haven't changed for over a year. Moving them to storage with more room frees space here."
                        .into(),
                    root_id: root.id.clone(),
                    bytes,
                    files,
                    rule: None,
                    room_on: rooms.iter().filter(|(rm, _)| rm != m).map(|(_, r)| r.clone()).collect(),
                });
            }
        }
        // 2. The same files more than once.
        let (waste, extra) = s.db.duplicate_waste(&root.id).await?;
        if waste >= 100 * 1024 * 1024 {
            out.push(Suggestion {
                id: format!("duplicates:{}", root.id),
                kind: "duplicates",
                title: format!("{extra} duplicate files in {}", root.name),
                detail: "The same content is stored more than once. Review the copies in Duplicates and keep one.".into(),
                root_id: root.id.clone(),
                bytes: waste,
                files: Vec::new(),
                rule: None,
                room_on: Vec::new(),
            });
        }
        // 3. Paperwork lying around: a rule could sort it.
        let rules = s.db.list_rules().await?;
        for (tag, folder) in SORTED_TAGS {
            let target = format!("Documents/{folder}");
            let n = s.db.tagged_outside(&root.id, tag, &target).await?;
            let has_rule = rules.iter().any(|r| r.root_id == root.id && r.conditions.contains(&format!("\"tag\":\"{tag}\"")));
            if n >= 3 && !has_rule {
                out.push(Suggestion {
                    id: format!("sort:{}:{tag}", root.id),
                    kind: "sort",
                    title: format!("Sort {n} {}s into {target}", tag),
                    detail: format!("Hearthdrive recognised these documents as {tag}s from what they say. A rule can file them, now and when new ones arrive."),
                    root_id: root.id.clone(),
                    bytes: 0,
                    files: Vec::new(),
                    rule: Some(serde_json::json!({
                        "name": format!("Sort {}s", tag),
                        "trigger": "added",
                        "root_id": root.id,
                        "path": "",
                        "when": { "subfolders": true, "tag": tag },
                        "then": { "type": "move", "folder": target },
                    })),
                    room_on: Vec::new(),
                });
            }
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.bytes));
    Ok(Json(out))
}
