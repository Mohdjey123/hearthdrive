//! Automation rules: "when a file arrives in Downloads and it's an
//! invoice, move it to Documents/Invoices", "every day, put photos older
//! than a year in Photos/{year}", "copy new videos to the media PC".
//!
//! A rule looks in one folder of a storage location on this computer,
//! picks files by what they are and what they say, and tags, moves, copies
//! or trashes them. It runs when files arrive or every hour or day, and
//! never deals with the same file twice.

use crate::auth::AuthUser;
use crate::routes::transfers::{Job, start_copy};
use crate::state::SharedState;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::{FileEntry, Role};
use nexus_common::{NexusError, Result, kinds};
use nexus_database::{Rule, RuleScope};
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The most files one run deals with (the rest wait for the next run).
const RUN_LIMIT: i64 = 500;
const PREVIEW_LIMIT: i64 = 100;
const DAY_MS: i64 = 24 * 3600 * 1000;

/// Which files a rule picks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Conditions {
    /// Look in subfolders too.
    pub subfolders: bool,
    /// `image`, `video`, `audio`, `document`, `archive`, `code`.
    pub kinds: Vec<String>,
    pub extensions: Vec<String>,
    pub name_contains: Option<String>,
    /// Words in the document's text.
    pub text_contains: Option<String>,
    pub tag: Option<String>,
    /// Not changed for this many days.
    pub older_than_days: Option<u32>,
    pub larger_than_mb: Option<f64>,
    pub smaller_than_mb: Option<f64>,
}

impl Conditions {
    fn narrows(&self) -> bool {
        !self.kinds.is_empty()
            || !self.extensions.is_empty()
            || self.name_contains.is_some()
            || self.text_contains.is_some()
            || self.tag.is_some()
            || self.older_than_days.is_some()
            || self.larger_than_mb.is_some()
            || self.smaller_than_mb.is_some()
    }
}

/// What happens to the files.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleAction {
    Tag {
        tags: Vec<String>,
    },
    /// Into a folder of the same storage location. The folder may use
    /// `{year}`, `{month}`, `{kind}` and `{ext}` (from when a photo was
    /// taken, or else when the file last changed).
    Move {
        folder: String,
    },
    /// To a folder on another device (or another storage location here).
    Copy {
        #[serde(default)]
        device_id: Option<String>,
        root_id: String,
        #[serde(default)]
        folder: String,
    },
    Trash,
}

pub fn parse(rule: &Rule) -> Result<(Conditions, RuleAction)> {
    let cond: Conditions =
        serde_json::from_str(&rule.conditions).map_err(|e| NexusError::invalid(format!("The rule's conditions: {e}")))?;
    let action: RuleAction =
        serde_json::from_str(&rule.action).map_err(|e| NexusError::invalid(format!("The rule's action: {e}")))?;
    Ok((cond, action))
}

/// Checks a rule before it is saved.
pub fn validate(rule: &Rule, cond: &Conditions, action: &RuleAction, here: &str) -> Result<()> {
    if rule.name.trim().is_empty() {
        return Err(NexusError::invalid("Give the rule a name"));
    }
    if !matches!(rule.trigger.as_str(), "added" | "hourly" | "daily") {
        return Err(NexusError::invalid("A rule runs when files are added, hourly or daily"));
    }
    for k in &cond.kinds {
        if kinds::extensions(k).is_none() {
            return Err(NexusError::invalid(format!("Unknown kind of file: {k}")));
        }
    }
    let scope = paths::normalize_relative(&rule.path)?;
    let inside = |folder: &str| {
        let f = folder.to_lowercase();
        let p = scope.to_lowercase();
        f == p || (cond.subfolders && (p.is_empty() || f.starts_with(&format!("{p}/"))))
    };
    match action {
        RuleAction::Tag { tags } => {
            if tags.iter().all(|t| t.trim().is_empty()) {
                return Err(NexusError::invalid("Choose the tags to add"));
            }
        }
        RuleAction::Move { folder } => {
            let fixed = folder.split('{').next().unwrap_or_default();
            paths::normalize_relative(fixed)?;
            if folder.trim_matches('/').is_empty() && scope.is_empty() && !cond.subfolders {
                return Err(NexusError::invalid("Choose the folder to move files to"));
            }
        }
        RuleAction::Copy { device_id, root_id, folder } => {
            let folder = paths::normalize_relative(folder)?;
            let local = device_id.as_deref().is_none_or(|d| d.is_empty() || d == here);
            // Copies made where the rule looks would be copied again.
            if local && *root_id == rule.root_id && inside(&folder) {
                return Err(NexusError::invalid("The copies would land in the folder this rule looks in"));
            }
        }
        RuleAction::Trash => {
            if !cond.narrows() {
                return Err(NexusError::invalid("A rule that moves files to the trash needs at least one condition"));
            }
        }
    }
    Ok(())
}

fn scope(rule: &Rule, cond: &Conditions, now: i64) -> RuleScope {
    let mut extensions: Vec<String> = cond.extensions.iter().map(|e| e.trim().trim_start_matches('.').to_lowercase()).collect();
    for k in &cond.kinds {
        extensions.extend(kinds::extensions(k).unwrap_or_default().into_iter().map(str::to_string));
    }
    extensions.retain(|e| !e.is_empty());
    let mb = |v: Option<f64>| v.filter(|v| *v > 0.0).map(|v| (v * 1024.0 * 1024.0) as i64);
    RuleScope {
        root_id: rule.root_id.clone(),
        path: rule.path.clone(),
        subfolders: cond.subfolders,
        extensions,
        name_contains: cond.name_contains.clone().filter(|s| !s.trim().is_empty()),
        text_contains: cond.text_contains.clone().filter(|s| !s.trim().is_empty()),
        tag: cond.tag.clone().filter(|s| !s.trim().is_empty()),
        modified_before: cond.older_than_days.map(|d| now - d as i64 * DAY_MS),
        min_size: mb(cond.larger_than_mb),
        max_size: mb(cond.smaller_than_mb),
        indexed_after: None,
        skip_done_for: Some(rule.id.clone()),
    }
}

/// Fills in `{year}`, `{month}`, `{kind}` and `{ext}`.
async fn folder_for(s: &SharedState, template: &str, file: &FileEntry) -> Result<String> {
    let mut out = template.to_string();
    if out.contains("{year}") || out.contains("{month}") {
        let taken = match s.db.get_media(&file.id).await {
            Ok(Some(m)) => m.taken_at,
            _ => None,
        };
        let at = taken.or(file.modified_at).or(file.created_at).unwrap_or(file.indexed_at);
        let date = chrono::DateTime::from_timestamp_millis(at).unwrap_or_default();
        out = out.replace("{year}", &date.format("%Y").to_string()).replace("{month}", &date.format("%m").to_string());
    }
    let ext = file.extension.clone().unwrap_or_default().to_lowercase();
    out = out.replace("{kind}", kinds::kind_of(file.extension.as_deref())).replace("{ext}", if ext.is_empty() { "other" } else { &ext });
    paths::normalize_relative(&out)
}

/// Where a file would go (for moves), shown before a rule runs.
async fn target(s: &SharedState, action: &RuleAction, file: &FileEntry) -> Option<String> {
    match action {
        RuleAction::Move { folder } => folder_for(s, folder, file).await.ok(),
        RuleAction::Copy { folder, .. } => Some(folder.clone()),
        _ => None,
    }
}

/// Makes a folder (and the ones above it) and adds them to the index.
async fn ensure_folder(s: &SharedState, root_id: &str, folder: &str) -> Result<()> {
    if folder.is_empty() {
        return Ok(());
    }
    let root = s.db.get_root(root_id).await?;
    let mut rel = String::new();
    for part in folder.split('/') {
        rel = paths::join(&rel, part);
        let path = s.storage.resolve(&root, &rel)?;
        if tokio::fs::metadata(&path).await.map(|m| m.is_dir()).unwrap_or(false) {
            if s.db.get_file_by_path(root_id, &rel).await?.is_none() {
                s.indexer.reindex_path(&root, &rel).await?;
            }
            continue;
        }
        tokio::fs::create_dir(&path).await?;
        s.indexer.reindex_path(&root, &rel).await?;
    }
    Ok(())
}

async fn move_file(s: &SharedState, file: &FileEntry, folder: &str) -> Result<String> {
    if file.parent_path.eq_ignore_ascii_case(folder) {
        return Ok(file.relative_path.clone());
    }
    ensure_folder(s, &file.root_id, folder).await?;
    let root = s.db.get_root(&file.root_id).await?;
    let mut name = file.filename.clone();
    for n in 2..1000 {
        let taken = tokio::fs::symlink_metadata(s.storage.resolve(&root, &paths::join(folder, &name))?).await.is_ok();
        if !taken {
            break;
        }
        name = paths::numbered_name(&file.filename, n, false);
    }
    let new_rel = s.storage.rename_or_move(file, Some(folder), Some(&name)).await?;
    s.events.emit(Event::FileMoved { root_id: file.root_id.clone(), from: file.relative_path.clone(), to: new_rel.clone() });
    Ok(new_rel)
}

/// Rules act as the computer's owner.
async fn acting(s: &SharedState) -> AuthUser {
    let owner = s.db.first_owner().await.ok().flatten();
    AuthUser {
        user_id: owner.as_ref().map(|u| u.id.clone()),
        username: "Automation".into(),
        role: Role::Owner,
        local: true,
        device_id: None,
        allowed_roots: None,
        shared: Vec::new(),
        via_token: false,
    }
}

#[derive(Debug, Default, Serialize)]
pub struct RunReport {
    pub matched: usize,
    pub done: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    /// The copy job started, if the rule copies.
    pub job_id: Option<String>,
}

impl RunReport {
    fn summary(&self) -> String {
        if self.matched == 0 {
            return "Nothing to do".into();
        }
        let mut out = format!("{} of {} files done", self.done, self.matched);
        if let Some(e) = self.errors.first() {
            out.push_str(&format!("; {} failed ({e})", self.failed));
        }
        out
    }
}

static RUNNING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs a rule now. `new_only`: only files that arrived since the rule was
/// made (the "when added" trigger); otherwise every matching file. With a
/// `job`, it counts the files there as it goes, for a progress bar.
pub async fn run(s: &SharedState, rule: &Rule, new_only: bool, job: Option<&Job>) -> Result<RunReport> {
    let _one_at_a_time = RUNNING.lock().await;
    let (cond, action) = parse(rule)?;
    let now = now_ms();
    let mut sc = scope(rule, &cond, now);
    if new_only {
        sc.indexed_after = Some(rule.created_at);
    }
    let root = s.db.get_root(&rule.root_id).await?;
    s.storage.ensure_online(&root)?;
    let files = s.db.rule_files(&sc, RUN_LIMIT).await?;
    let mut report = RunReport { matched: files.len(), ..Default::default() };
    if let Some(job) = job {
        let bytes: u64 = files.iter().map(|f| f.size.max(0) as u64).sum();
        job.update(|i| {
            i.files_total = files.len() as u64;
            i.bytes_total = bytes;
        });
    }
    let by = format!("rule:{}", rule.name);
    let mut copies = Vec::new();
    let mut bytes_done = 0u64;
    for file in files {
        if let Some(job) = job {
            if job.is_cancelled() {
                break;
            }
            job.update(|i| i.current = Some(file.relative_path.clone()));
        }
        let result = match &action {
            RuleAction::Tag { tags } => {
                let tags: Vec<String> = tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
                s.db.add_file_tags(&file, &tags, &by, now).await
            }
            RuleAction::Move { folder } => match folder_for(s, folder, &file).await {
                Ok(folder) => move_file(s, &file, &folder).await.map(|_| ()),
                Err(e) => Err(e),
            },
            RuleAction::Trash => match s.storage.trash(&file, None).await {
                Ok(_) => {
                    s.events.emit(Event::FileDeleted { root_id: file.root_id.clone(), path: file.relative_path.clone() });
                    Ok(())
                }
                Err(e) => Err(e),
            },
            RuleAction::Copy { .. } => {
                copies.push(file.id.clone());
                Ok(())
            }
        };
        match result {
            Ok(()) => {
                report.done += 1;
                let _ = s.db.rule_done(&rule.id, &file.id, now).await;
            }
            Err(e) => {
                report.failed += 1;
                if let Some(job) = job {
                    job.fail_file(&file.filename, &e);
                }
                if report.errors.len() < 5 {
                    report.errors.push(format!("{}: {}", file.filename, e));
                }
            }
        }
        if let Some(job) = job {
            bytes_done += file.size.max(0) as u64;
            job.set_bytes(bytes_done);
            job.update(|i| i.files_done += 1);
        }
    }
    if let (RuleAction::Copy { device_id, root_id, folder }, false) = (&action, copies.is_empty()) {
        let user = acting(s).await;
        match start_copy(s, user, None, device_id.as_deref(), copies.clone(), root_id.clone(), folder, false).await {
            Ok(job) => report.job_id = Some(job.id.clone()),
            Err(e) => {
                // Nothing was copied: try these files again next time.
                for id in &copies {
                    let _ = s.db.rule_undone(&rule.id, id).await;
                }
                report.failed = copies.len();
                report.done -= copies.len();
                report.errors.push(e.to_string());
            }
        }
    }
    s.db.rule_ran(&rule.id, now, &report.summary(), report.done as i64).await?;
    if report.done > 0 {
        tracing::info!(rule = %rule.name, files = report.done, "rule ran");
    }
    Ok(report)
}

#[derive(Debug, Serialize)]
pub struct PreviewItem {
    pub file: FileEntry,
    /// Where it would go (moves and copies).
    pub target: Option<String>,
}

/// The files a rule would deal with if it ran now, without touching them.
pub async fn preview(s: &SharedState, rule: &Rule) -> Result<Vec<PreviewItem>> {
    let (cond, action) = parse(rule)?;
    let sc = scope(rule, &cond, now_ms());
    let files = s.db.rule_files(&sc, PREVIEW_LIMIT).await?;
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let target = target(s, &action, &file).await;
        out.push(PreviewItem { file, target });
    }
    Ok(out)
}

fn due(rule: &Rule, now: i64) -> bool {
    let every = match rule.trigger.as_str() {
        "hourly" => 3600 * 1000,
        "daily" => DAY_MS,
        _ => return false,
    };
    rule.last_run.is_none_or(|t| now - t >= every)
}

async fn run_all(s: &SharedState, added: bool) {
    let Ok(rules) = s.db.list_rules().await else { return };
    let now = now_ms();
    for rule in rules.into_iter().filter(|r| r.enabled) {
        let go = if added { rule.trigger == "added" } else { due(&rule, now) };
        if !go {
            continue;
        }
        if let Err(e) = run(s, &rule, added, None).await {
            tracing::debug!(rule = %rule.name, error = %e, "rule did not run");
            let _ = s.db.rule_ran(&rule.id, now, &e.to_string(), 0).await;
        }
    }
}

/// Runs rules when files arrive and on their schedule.
pub fn spawn(state: SharedState) {
    if std::env::var_os("NEXUS_NO_RULES").is_some() {
        return;
    }
    tokio::spawn(async move {
        let mut events = state.events.subscribe();
        let mut pending = false;
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        let mut ticks = 0u32;
        loop {
            tokio::select! {
                ev = events.recv() => match ev {
                    Ok(Event::FileCreated { .. }) | Ok(Event::FileModified { .. }) | Ok(Event::ScanCompleted { .. }) => pending = true,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    _ => {}
                },
                _ = tick.tick() => {
                    ticks += 1;
                    // New files are dealt with within about a minute; every
                    // 15 minutes also catches documents whose text was read
                    // after they arrived.
                    if pending || ticks % 15 == 0 {
                        pending = false;
                        run_all(&state, true).await;
                    }
                    run_all(&state, false).await;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(trigger: &str, path: &str) -> Rule {
        Rule {
            id: "r".into(),
            name: "Test".into(),
            enabled: true,
            trigger: trigger.into(),
            root_id: "root".into(),
            path: path.into(),
            conditions: "{}".into(),
            action: "{}".into(),
            created_by: "x".into(),
            created_at: 0,
            last_run: None,
            last_result: None,
            files_done: 0,
        }
    }

    #[test]
    fn checks_rules_before_saving() {
        let r = rule("added", "Downloads");
        let none = Conditions::default();
        assert!(validate(&r, &none, &RuleAction::Trash, "me").is_err(), "trash needs a condition");
        let old = Conditions { older_than_days: Some(30), ..Default::default() };
        assert!(validate(&r, &old, &RuleAction::Trash, "me").is_ok());
        let copy_here = RuleAction::Copy { device_id: None, root_id: "root".into(), folder: "Downloads".into() };
        assert!(validate(&r, &none, &copy_here, "me").is_err(), "copies would be copied again");
        let copy_away = RuleAction::Copy { device_id: Some("pc".into()), root_id: "root".into(), folder: "Downloads".into() };
        assert!(validate(&r, &none, &copy_away, "me").is_ok());
        assert!(validate(&rule("weekly", ""), &none, &RuleAction::Tag { tags: vec!["a".into()] }, "me").is_err());
        let action: RuleAction = serde_json::from_str(r#"{"type":"move","folder":"Photos/{year}"}"#).unwrap();
        assert!(validate(&r, &none, &action, "me").is_ok());
    }

    #[test]
    fn schedules() {
        let mut r = rule("hourly", "");
        assert!(due(&r, 10));
        r.last_run = Some(0);
        assert!(!due(&r, 1000));
        assert!(due(&r, 3600 * 1000));
        assert!(!due(&rule("added", ""), 10));
    }
}
