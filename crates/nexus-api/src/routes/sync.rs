//! Folder sync with paired devices.
//!
//! A sync pair keeps a folder here in step with a folder on a paired
//! device: both ways, or one way (upload: this computer wins; download: the
//! device wins). Only this node knows about the pair; the device just
//! answers ordinary file requests.
//!
//! Each run lists both folders and compares every path with how both sides
//! looked after the previous run (`sync_state`), so a change on one side is
//! told apart from a change on the other:
//!
//! * changed on one side only: copied to the other, or, if it was deleted,
//!   moved to the trash there;
//! * changed on both sides: the newer copy keeps the name and the other is
//!   kept beside it as "name (Device's copy date).ext";
//! * replacing a file always keeps the old content as a version.
//!
//! Nothing is ever deleted outright: removals go to the trash on that device.

use super::audit;
use super::files::{TreeItem, tree_of};
use super::transfers::{Meta, Mover, Side, finish_job, peer_target};
use super::uploads::NewUpload;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::{NexusError, Result};
use nexus_database::{Fingerprints, NewSyncPair, StateChange, SyncPair, SyncState};
use nexus_security::Action;
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::Notify;

/// Every pair is checked at least this often (changes on the device are
/// only noticed by looking).
const INTERVAL_MS: i64 = 5 * 60 * 1000;
/// After a change here, wait this long for more before syncing.
const SETTLE_MS: i64 = 10 * 1000;
const TICK: Duration = Duration::from_secs(10);

pub const MODES: &[&str] = &["two_way", "upload", "download"];

/// Wakes the sync loop and tracks what needs a run.
#[derive(Default)]
pub struct SyncControl {
    wake: Notify,
    /// Pairs someone asked to sync now.
    forced: Mutex<HashSet<String>>,
    /// Storage roots with changes here, and when the last one happened.
    dirty: Mutex<BTreeMap<String, i64>>,
    /// Pairs running right now.
    running: Mutex<HashSet<String>>,
}

impl SyncControl {
    pub fn run_now(&self, pair_id: &str) {
        self.forced.lock().unwrap().insert(pair_id.to_string());
        self.wake.notify_one();
    }

    fn is_running(&self, pair_id: &str) -> bool {
        self.running.lock().unwrap().contains(pair_id)
    }
}

// ---------------------------------------------------------------------------
// Planning (pure)

/// What one side has at a path.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The path as this side spells it (keys are lower-cased, because
    /// Windows names differing only in case are the same file).
    pub path: String,
    pub id: String,
    pub is_dir: bool,
    pub size: i64,
    pub modified_at: Option<i64>,
    pub hash: Option<String>,
}

impl Entry {
    fn of(item: &TreeItem) -> Self {
        Self {
            path: item.path.clone(),
            id: item.id.clone(),
            is_dir: item.is_dir,
            size: item.size,
            modified_at: item.modified_at,
            hash: item.content_hash.clone(),
        }
    }

    /// Changes whenever the content does (on the same side).
    pub fn fingerprint(&self) -> String {
        if self.is_dir {
            "dir".into()
        } else {
            format!("{}:{}", self.size, self.modified_at.unwrap_or(0))
        }
    }

    /// Whether both sides hold the same thing. Copies keep the modification
    /// time, so size and time are enough when a hash is missing.
    fn same_as(&self, other: &Entry) -> bool {
        match (self.is_dir, other.is_dir) {
            (true, true) => true,
            (false, false) => match (&self.hash, &other.hash) {
                (Some(a), Some(b)) => a == b,
                _ => self.size == other.size && self.modified_at == other.modified_at,
            },
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Copy here → device (replacing what is there).
    Push(String),
    /// Copy device → here.
    Pull(String),
    MkdirRemote(String),
    MkdirLocal(String),
    TrashRemote(String),
    TrashLocal(String),
    /// Both changed: keep the device's copy here under a new name.
    KeepRemoteCopy(String),
    /// Both changed: keep this computer's copy on the device under a new name.
    KeepLocalCopy(String),
    /// Both sides already agree; remember how they look.
    Record(String),
    /// Gone from both sides.
    Forget(String),
    /// Cannot be synced (a file on one side, a folder on the other).
    Clash(String),
}

impl Op {
    fn is_change(&self) -> bool {
        !matches!(self, Op::Record(_) | Op::Forget(_))
    }
}

/// Files that are never synced: temporary and system files.
fn ignored(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    name.starts_with("~$")
        || name.ends_with(".tmp")
        || name.ends_with(".part")
        || name.ends_with(".crdownload")
        || name == "desktop.ini"
        || name == "thumbs.db"
        || name == ".ds_store"
}

fn under(rel: &str, dirs: &[String]) -> bool {
    dirs.iter().any(|d| rel.len() > d.len() && rel.starts_with(d.as_str()) && rel.as_bytes()[d.len()] == b'/')
}

/// Decides what to do. `mode` is `two_way`, `upload` or `download`.
pub fn plan(mode: &str, local: &BTreeMap<String, Entry>, remote: &BTreeMap<String, Entry>, state: &SyncState) -> Vec<Op> {
    let paths: BTreeSet<&String> = local.keys().chain(remote.keys()).chain(state.keys()).collect();
    let mut ops = Vec::new();
    // Folders going to the trash whole: skip what is inside them.
    let mut trashed: Vec<String> = Vec::new();

    // A folder is trashed whole only if nothing in it changed since the last run.
    let untouched = |side: &BTreeMap<String, Entry>, rel: &str, pick: fn(&Fingerprints) -> &Option<String>| {
        side.range::<String, _>((std::ops::Bound::Excluded(rel.to_string()), std::ops::Bound::Unbounded))
            .take_while(|(p, _)| p.starts_with(rel))
            .filter(|(p, _)| p.as_bytes().get(rel.len()) == Some(&b'/'))
            .all(|(p, e)| state.get(p.as_str()).is_some_and(|s| pick(s).as_deref() == Some(e.fingerprint().as_str())))
    };

    for rel in paths {
        if ignored(rel) || under(rel, &trashed) {
            continue;
        }
        let l = local.get(rel);
        let r = remote.get(rel);
        let st = state.get(rel);
        let lfp = l.map(Entry::fingerprint);
        let rfp = r.map(Entry::fingerprint);
        let (lch, rch) = match st {
            Some((sl, sr)) => (lfp != *sl, rfp != *sr),
            None => (l.is_some(), r.is_some()),
        };
        if st.is_some() && !lch && !rch {
            continue;
        }
        if l.is_none() && r.is_none() {
            ops.push(Op::Forget(rel.clone()));
            continue;
        }
        if let (Some(a), Some(b)) = (l, r) {
            if a.same_as(b) {
                ops.push(Op::Record(rel.clone()));
                continue;
            }
            if a.is_dir != b.is_dir {
                ops.push(Op::Clash(rel.clone()));
                trashed.push(rel.clone()); // nothing below it can be synced either
                continue;
            }
        }
        let push = |ops: &mut Vec<Op>, e: &Entry| ops.push(if e.is_dir { Op::MkdirRemote(rel.clone()) } else { Op::Push(rel.clone()) });
        let pull = |ops: &mut Vec<Op>, e: &Entry| ops.push(if e.is_dir { Op::MkdirLocal(rel.clone()) } else { Op::Pull(rel.clone()) });

        match mode {
            "upload" => match (l, r) {
                (Some(a), _) => push(&mut ops, a),
                (None, Some(_)) if st.is_some() => {
                    ops.push(Op::TrashRemote(rel.clone()));
                    trashed.push(rel.clone());
                }
                _ => {} // only on the device and never synced: leave it alone
            },
            "download" => match (l, r) {
                (_, Some(b)) => pull(&mut ops, b),
                (Some(_), None) if st.is_some() => {
                    ops.push(Op::TrashLocal(rel.clone()));
                    trashed.push(rel.clone());
                }
                _ => {}
            },
            _ => match (l, r, lch, rch) {
                // New or changed on one side only.
                (Some(a), _, true, false) => push(&mut ops, a),
                (_, Some(b), false, true) => pull(&mut ops, b),
                // Deleted on one side, untouched on the other.
                (None, Some(b), true, false) => {
                    if !b.is_dir || untouched(remote, rel, |s| &s.1) {
                        ops.push(Op::TrashRemote(rel.clone()));
                        trashed.push(rel.clone());
                    }
                }
                (Some(a), None, false, true) => {
                    if !a.is_dir || untouched(local, rel, |s| &s.0) {
                        ops.push(Op::TrashLocal(rel.clone()));
                        trashed.push(rel.clone());
                    }
                }
                // Changed on both sides.
                (Some(a), None, _, _) => push(&mut ops, a),
                (None, Some(b), _, _) => pull(&mut ops, b),
                (Some(a), Some(_), _, _) if a.is_dir => ops.push(Op::Record(rel.clone())),
                (Some(a), Some(b), _, _) => {
                    if a.modified_at.unwrap_or(0) >= b.modified_at.unwrap_or(0) {
                        ops.push(Op::KeepRemoteCopy(rel.clone()));
                        ops.push(Op::Push(rel.clone()));
                    } else {
                        ops.push(Op::KeepLocalCopy(rel.clone()));
                        ops.push(Op::Pull(rel.clone()));
                    }
                }
                (None, None, _, _) => {}
            },
        }
    }
    ops
}

/// "report.docx" → "report (Laptop's copy 2026-11-17).docx"
pub fn conflict_name(name: &str, device: &str, at_ms: i64) -> String {
    let (y, m, d) = civil_date(at_ms.div_euclid(86_400_000));
    let tag = format!(" ({device}'s copy {y:04}-{m:02}-{d:02})");
    match name.rfind('.') {
        Some(i) if i > 0 => format!("{}{tag}{}", &name[..i], &name[i..]),
        _ => format!("{name}{tag}"),
    }
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

// ---------------------------------------------------------------------------
// Running a pair

/// The person a pair acts for, as they are now.
async fn acting_user(s: &AppState, pair: &SyncPair) -> Result<AuthUser> {
    let (user, _) = s
        .db
        .find_credentials(&pair.created_by)
        .await?
        .ok_or_else(|| NexusError::PermissionDenied(format!("{} no longer has an account here", pair.created_by)))?;
    Ok(AuthUser::person(user))
}

fn to_map(items: Vec<TreeItem>) -> BTreeMap<String, Entry> {
    items.into_iter().map(|i| (i.path.to_lowercase(), Entry::of(&i))).collect()
}

/// More removals than this in one run (and over half of what was synced)
/// look like a folder that vanished, not like a person deleting files.
const MASS_REMOVAL: usize = 20;

/// Removals the plan would make on each side, if they look like an accident.
fn mass_removal(ops: &[Op], state: &SyncState) -> Option<(usize, usize)> {
    let here = ops.iter().filter(|o| matches!(o, Op::TrashLocal(_))).count();
    let there = ops.iter().filter(|o| matches!(o, Op::TrashRemote(_))).count();
    let limit = MASS_REMOVAL.max(state.len() / 2);
    (here > limit || there > limit).then_some((here, there))
}

/// Runs one pair now. Returns a summary for the pair's status. `confirmed`
/// (someone pressed Sync now) lets a run remove many files at once.
async fn run_pair(s: &SharedState, pair: &SyncPair, confirmed: bool) -> Result<String> {
    let user = acting_user(s, pair).await?;
    user.require(Action::Write).map_err(|e| e.0)?;
    // Still allowed to use the folder here?
    user.require_root(&pair.local_root_id).map_err(|e| e.0)?;
    let device = peer_target(s, &pair.device_id).await?;
    let remote_side = Side::Peer(device.clone());
    let mover = Mover { s, user: &user };

    let root = s.db.get_root(&pair.local_root_id).await?;
    s.storage.ensure_online(&root)?;
    if s.indexer.is_scanning(&root.id) {
        return Err(NexusError::Unavailable(format!("Waiting for {} to finish indexing", root.name)));
    }
    let local = to_map(tree_of(s, &root.id, &pair.local_path).await?);
    let remote = to_map(mover.tree(&remote_side, &pair.remote_root_id, &pair.remote_path).await?);
    let state = s.db.load_sync_state(&pair.id).await?;
    let mut ops = plan(&pair.mode, &local, &remote, &state);
    let mut held_back = None;
    if let Some((here, there)) = mass_removal(&ops, &state).filter(|_| !confirmed) {
        ops.retain(|o| !matches!(o, Op::TrashLocal(_) | Op::TrashRemote(_)));
        held_back = Some(if here > there {
            format!("{here} files disappeared on {} at once", device.name)
        } else {
            format!("{there} files disappeared here at once")
        });
    }

    let mut changes: Vec<StateChange> = Vec::new();
    let fp = |e: Option<&Entry>| e.map(Entry::fingerprint);
    let work: Vec<&Op> = ops.iter().filter(|o| o.is_change()).collect();
    for op in &ops {
        match op {
            Op::Record(rel) => changes.push((rel.clone(), Some((fp(local.get(rel)), fp(remote.get(rel)))))),
            Op::Forget(rel) => changes.push((rel.clone(), None)),
            _ => {}
        }
    }
    let held = |result: Result<String>| match &held_back {
        Some(what) => Err(NexusError::Conflict(format!(
            "{what}, so they were not removed on the other side. If that was meant, press Sync now to remove them there too (they go to the trash)."
        ))),
        None => result,
    };
    if work.is_empty() {
        s.db.save_sync_state(&pair.id, &changes).await?;
        return held(Ok("Up to date".into()));
    }

    let folder = paths::split_parent(&pair.local_path).1;
    let title = format!("Sync {}", if folder.is_empty() { &root.name } else { &folder });
    let arrow = match pair.mode.as_str() {
        "upload" => "→",
        "download" => "←",
        _ => "⇄",
    };
    let job = s.jobs.add("sync", title, format!("This computer {arrow} {}", device.name), user.username.clone());
    let Some(_slot) = s.jobs.start(s, &job).await else {
        return Ok("Cancelled".into());
    };
    let (files, bytes) = work.iter().fold((0u64, 0u64), |(f, b), op| match op {
        Op::Push(rel) | Op::KeepLocalCopy(rel) => (f + 1, b + local.get(rel).map_or(0, |e| e.size as u64)),
        Op::Pull(rel) | Op::KeepRemoteCopy(rel) => (f + 1, b + remote.get(rel).map_or(0, |e| e.size as u64)),
        _ => (f + 1, b),
    });
    job.update(|i| {
        i.files_total = files;
        i.bytes_total = bytes;
    });

    let local_name = s.node.hostname.clone();
    // Ops name paths by key; each side's entry knows how that side spells it.
    let real = |key: &str| local.get(key).or_else(|| remote.get(key)).map(|e| e.path.clone()).unwrap_or_else(|| key.to_string());
    let send = |from: &Side, to: &Side, src: &Entry, rel: &str, name: Option<String>, replace: bool| {
        let (parent, file_name) = paths::split_parent(rel);
        let (src_root, src_base, dst_root, dst_base) = match from {
            Side::Local => (&pair.local_root_id, &pair.local_path, &pair.remote_root_id, &pair.remote_path),
            Side::Peer(_) => (&pair.remote_root_id, &pair.remote_path, &pair.local_root_id, &pair.local_path),
        };
        let meta = Meta {
            id: src.id.clone(),
            root_id: src_root.clone(),
            relative_path: paths::join(src_base, rel),
            filename: file_name.clone(),
            is_dir: false,
            size: src.size,
            modified_at: src.modified_at,
            content_hash: src.hash.clone(),
        };
        let req = NewUpload {
            root_id: dst_root.clone(),
            path: paths::join(dst_base, &parent),
            name: name.unwrap_or(file_name),
            size: src.size as u64,
            on_conflict: Some(if replace { "replace" } else { "rename" }.into()),
            modified_at: src.modified_at,
            make_parents: true,
        };
        let job = job.clone();
        let mover = &mover;
        let (from, to) = (from.clone(), to.clone());
        async move {
            job.update(|i| i.current = Some(meta.filename.clone()));
            mover.send_file(&job, &from, &meta, &to, req).await
        }
    };

    for op in work {
        if job.is_cancelled() {
            break;
        }
        let done = |job: &super::transfers::Job| job.update(|i| i.files_done += 1);
        let outcome: Result<()> = async {
            match op {
                Op::Push(rel) => {
                    let src = &local[rel];
                    let placed = send(&Side::Local, &remote_side, src, &src.path, None, true).await?;
                    let after = Entry { path: src.path.clone(), id: placed.id, is_dir: false, size: placed.size, modified_at: placed.modified_at, hash: placed.content_hash };
                    changes.push((rel.clone(), Some((Some(src.fingerprint()), Some(after.fingerprint())))));
                }
                Op::Pull(rel) => {
                    let src = &remote[rel];
                    let placed = send(&remote_side, &Side::Local, src, &src.path, None, true).await?;
                    let after = Entry { path: src.path.clone(), id: placed.id, is_dir: false, size: placed.size, modified_at: placed.modified_at, hash: placed.content_hash };
                    changes.push((rel.clone(), Some((Some(after.fingerprint()), Some(src.fingerprint())))));
                }
                Op::KeepRemoteCopy(rel) => {
                    let src = &remote[rel];
                    let name = conflict_name(&paths::split_parent(&src.path).1, &device.name, now_ms());
                    send(&remote_side, &Side::Local, src, &src.path, Some(name), false).await?;
                }
                Op::KeepLocalCopy(rel) => {
                    let src = &local[rel];
                    let name = conflict_name(&paths::split_parent(&src.path).1, &local_name, now_ms());
                    send(&Side::Local, &remote_side, src, &src.path, Some(name), false).await?;
                }
                Op::MkdirRemote(rel) | Op::MkdirLocal(rel) => {
                    let (side, root_id, base) = match op {
                        Op::MkdirRemote(_) => (&remote_side, &pair.remote_root_id, &pair.remote_path),
                        _ => (&Side::Local, &pair.local_root_id, &pair.local_path),
                    };
                    let (parent, name) = paths::split_parent(&real(rel));
                    let parent = paths::join(base, &parent);
                    // `false` means it is already there, which is just as good.
                    mover.mkdir(side, root_id, &parent, &name).await?;
                    changes.push((rel.clone(), Some((Some("dir".into()), Some("dir".into())))));
                }
                Op::TrashRemote(rel) => {
                    mover.trash(&remote_side, &remote[rel].id).await?;
                    changes.push((rel.clone(), None));
                }
                Op::TrashLocal(rel) => {
                    mover.trash(&Side::Local, &local[rel].id).await?;
                    changes.push((rel.clone(), None));
                }
                Op::Clash(rel) => {
                    return Err(NexusError::Conflict(format!("\"{rel}\" is a file on one side and a folder on the other")));
                }
                Op::Record(_) | Op::Forget(_) => {}
            }
            Ok(())
        }
        .await;
        match outcome {
            Ok(()) => done(&job),
            Err(_) if job.is_cancelled() => break,
            Err(e) => job.fail_file(&real(op_path(op)), &e),
        }
    }
    s.db.save_sync_state(&pair.id, &changes).await?;
    finish_job(s, &job);
    let info = job_summary(&job);
    if info.1 > 0 {
        return Err(NexusError::Unavailable(format!("{} changes synced, {} could not be: {}", info.0, info.1, info.2)));
    }
    held(Ok(format!("{} change{} synced", info.0, if info.0 == 1 { "" } else { "s" })))
}

fn op_path(op: &Op) -> &str {
    match op {
        Op::Push(p) | Op::Pull(p) | Op::MkdirRemote(p) | Op::MkdirLocal(p) | Op::TrashRemote(p) | Op::TrashLocal(p) => p,
        Op::KeepRemoteCopy(p) | Op::KeepLocalCopy(p) | Op::Record(p) | Op::Forget(p) | Op::Clash(p) => p,
    }
}

/// (done, failed, first error)
fn job_summary(job: &super::transfers::Job) -> (u64, usize, String) {
    let mut out = (0, 0, String::new());
    job.update(|i| out = (i.files_done, i.errors.len(), i.errors.first().cloned().unwrap_or_default()));
    out
}

async fn run_and_record(s: &SharedState, pair: &SyncPair, confirmed: bool) {
    s.sync.running.lock().unwrap().insert(pair.id.clone());
    s.events.emit(Event::SyncUpdated { id: pair.id.clone() });
    let result = run_pair(s, pair, confirmed).await;
    let (ok, text, error) = match &result {
        Ok(text) => (true, text.clone(), None),
        Err(NexusError::PeerUnreachable(_)) => (false, "Waiting for the device".to_string(), Some(result.as_ref().unwrap_err().to_string())),
        Err(e) => (false, "Needs attention".to_string(), Some(e.to_string())),
    };
    if let Err(e) = s.db.record_sync_run(&pair.id, ok, &text, error.as_deref()).await {
        tracing::warn!(error = %e, "could not record a sync run");
    }
    s.sync.running.lock().unwrap().remove(&pair.id);
    s.events.emit(Event::SyncUpdated { id: pair.id.clone() });
}

/// Runs pairs when they are due, asked for, or their folder here changed.
pub fn spawn(s: SharedState) {
    // Note changes in storage locations that have sync pairs.
    let watcher = s.clone();
    tokio::spawn(async move {
        let mut events = watcher.events.subscribe();
        loop {
            match events.recv().await {
                Ok(Event::FileCreated { root_id, .. })
                | Ok(Event::FileModified { root_id, .. })
                | Ok(Event::FileDeleted { root_id, .. })
                | Ok(Event::FileMoved { root_id, .. }) => {
                    watcher.sync.dirty.lock().unwrap().insert(root_id, now_ms());
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });

    tokio::spawn(async move {
        let mut last_cleanup = 0;
        loop {
            tokio::select! {
                _ = s.sync.wake.notified() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
            let now = now_ms();
            if now - last_cleanup > 60 * 60 * 1000 {
                last_cleanup = now;
                super::uploads::clean_abandoned(&s).await;
            }
            let Ok(pairs) = s.db.list_sync_pairs().await else { continue };
            for pair in pairs {
                let forced = s.sync.forced.lock().unwrap().remove(&pair.id);
                if !pair.enabled && !forced {
                    continue;
                }
                let last = pair.last_run_at.unwrap_or(0);
                let changed_here = s
                    .sync
                    .dirty
                    .lock()
                    .unwrap()
                    .get(&pair.local_root_id)
                    .is_some_and(|at| *at > last && now - *at >= SETTLE_MS);
                if forced || changed_here || now - last >= INTERVAL_MS {
                    run_and_record(&s, &pair, forced).await;
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Routes

#[derive(Serialize)]
pub struct PairView {
    #[serde(flatten)]
    pair: SyncPair,
    device_name: String,
    local_label: String,
    running: bool,
}

async fn view(s: &AppState, pair: SyncPair) -> PairView {
    let device_name = s.db.get_device(&pair.device_id).await.map(|d| d.name).unwrap_or_else(|_| "Removed device".into());
    let root = s.db.get_root(&pair.local_root_id).await.map(|r| r.name).unwrap_or_else(|_| "Removed storage".into());
    let local_label = if pair.local_path.is_empty() { root } else { format!("{root} / {}", pair.local_path) };
    let running = s.sync.is_running(&pair.id);
    PairView { pair, device_name, local_label, running }
}

/// Whether the caller may see a pair: its folder here must be visible to them.
fn sees(user: &AuthUser, pair: &SyncPair) -> bool {
    user.sees_root(&pair.local_root_id)
}

/// A pair the caller may change: their own, or any if they manage people.
async fn manageable(s: &AppState, user: &AuthUser, id: &str) -> Result<SyncPair> {
    let pair = s.db.get_sync_pair(id).await?;
    if !sees(user, &pair) {
        return Err(NexusError::NotFound("Sync".into()));
    }
    if pair.created_by != user.username && !nexus_security::role_allows(user.role, Action::ManageUsers) {
        return Err(NexusError::PermissionDenied(format!("Only {} or an administrator can change this sync", pair.created_by)));
    }
    Ok(pair)
}

pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<PairView>>> {
    user.require_person()?;
    user.require(Action::Read)?;
    let mut out = Vec::new();
    for pair in s.db.list_sync_pairs().await?.into_iter().filter(|p| sees(&user, p)) {
        out.push(view(&s, pair).await);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct CreatePair {
    local_root_id: String,
    #[serde(default)]
    local_path: String,
    device_id: String,
    remote_root_id: String,
    #[serde(default)]
    remote_path: String,
    mode: String,
}

pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(req): Json<CreatePair>) -> ApiResult<(StatusCode, Json<PairView>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    if user.user_id.is_none() {
        return Err(NexusError::PermissionDenied("Sign in with an account to set up sync".into()).into());
    }
    user.require_root(&req.local_root_id)?;
    if !MODES.contains(&req.mode.as_str()) {
        return Err(NexusError::invalid("Choose how to sync").into());
    }
    let root = s.db.get_root(&req.local_root_id).await?;
    if root.read_only && req.mode != "upload" {
        return Err(NexusError::invalid(format!("{} is read-only here, so it can only be uploaded from", root.name)).into());
    }
    let local_path = paths::normalize_relative(&req.local_path)?;
    let remote_path = paths::normalize_relative(&req.remote_path)?;
    tree_of(&s, &root.id, &local_path).await.map_err(|_| NexusError::NotFound("The folder here".into()))?;

    // The folder on the device must exist and be reachable.
    let device = peer_target(&s, &req.device_id).await?;
    #[derive(Deserialize)]
    struct Listing {
        root: RootName,
    }
    #[derive(Deserialize)]
    struct RootName {
        name: String,
        read_only: bool,
    }
    let url = format!(
        "/api/v1/files?root_id={}&path={}&limit=1",
        super::files::percent_encode(&req.remote_root_id),
        super::files::percent_encode(&remote_path)
    );
    let (listing, _): (Listing, _) = s
        .peers
        .call(&device, axum::http::Method::GET, &url, None::<&()>, Some(crate::peers::Acting::of(&user)), crate::peers::CALL_TIMEOUT)
        .await?;
    if listing.root.read_only && req.mode != "download" {
        return Err(NexusError::invalid(format!("{} is read-only on {}, so it can only be downloaded from", listing.root.name, device.name)).into());
    }
    let remote_label = if remote_path.is_empty() { listing.root.name } else { format!("{} / {remote_path}", listing.root.name) };

    let existing = s.db.list_sync_pairs().await?;
    if existing.iter().any(|p| {
        p.local_root_id == root.id && p.local_path.eq_ignore_ascii_case(&local_path) && p.device_id == req.device_id
            && p.remote_root_id == req.remote_root_id && p.remote_path.eq_ignore_ascii_case(&remote_path)
    }) {
        return Err(NexusError::Conflict("These folders are already synced".into()).into());
    }

    let pair = s
        .db
        .create_sync_pair(&NewSyncPair {
            local_root_id: &root.id,
            local_path: &local_path,
            device_id: &req.device_id,
            remote_root_id: &req.remote_root_id,
            remote_path: &remote_path,
            remote_label: &remote_label,
            mode: &req.mode,
            created_by: &user.username,
            created_role: user.role.as_str(),
        })
        .await?;
    audit(&s, &user, "sync.created", Some(&format!("{} ⇄ {}: {remote_label}", view(&s, pair.clone()).await.local_label, device.name))).await;
    s.sync.run_now(&pair.id);
    Ok((StatusCode::CREATED, Json(view(&s, pair).await)))
}

#[derive(Deserialize)]
pub struct UpdatePair {
    enabled: Option<bool>,
    mode: Option<String>,
}

pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdatePair>,
) -> ApiResult<Json<PairView>> {
    user.require_person()?;
    user.require(Action::Write)?;
    if let Some(mode) = &req.mode
        && !MODES.contains(&mode.as_str()) {
            return Err(NexusError::invalid("Choose how to sync").into());
        }
    manageable(&s, &user, &id).await?;
    let pair = s.db.update_sync_pair(&id, req.enabled, req.mode.as_deref()).await?;
    audit(&s, &user, "sync.changed", Some(&pair.remote_label)).await;
    if pair.enabled {
        s.sync.run_now(&pair.id);
    }
    Ok(Json(view(&s, pair).await))
}

pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::Write)?;
    let pair = manageable(&s, &user, &id).await?;
    s.db.delete_sync_pair(&id).await?;
    audit(&s, &user, "sync.removed", Some(&pair.remote_label)).await;
    s.events.emit(Event::SyncUpdated { id });
    Ok(StatusCode::NO_CONTENT)
}

pub async fn run(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::Write)?;
    manageable(&s, &user, &id).await?;
    s.sync.run_now(&id);
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: i64, mtime: i64) -> Entry {
        Entry { path: String::new(), id: format!("id{size}{mtime}"), is_dir: false, size, modified_at: Some(mtime), hash: None }
    }
    fn dir() -> Entry {
        Entry { path: String::new(), id: "d".into(), is_dir: true, size: 0, modified_at: Some(1), hash: None }
    }
    fn map(items: &[(&str, Entry)]) -> BTreeMap<String, Entry> {
        items.iter().map(|(p, e)| (p.to_string(), e.clone())).collect()
    }
    fn synced(items: &[(&str, &Entry)]) -> SyncState {
        items.iter().map(|(p, e)| (p.to_string(), (Some(e.fingerprint()), Some(e.fingerprint())))).collect()
    }

    #[test]
    fn new_files_go_both_ways() {
        let local = map(&[("a.txt", file(1, 10)), ("docs", dir()), ("docs/b.txt", file(2, 20))]);
        let remote = map(&[("c.txt", file(3, 30))]);
        let ops = plan("two_way", &local, &remote, &SyncState::new());
        assert_eq!(
            ops,
            vec![Op::Push("a.txt".into()), Op::Pull("c.txt".into()), Op::MkdirRemote("docs".into()), Op::Push("docs/b.txt".into())]
        );
    }

    #[test]
    fn identical_files_are_only_recorded() {
        let local = map(&[("a.txt", file(1, 10))]);
        let remote = map(&[("a.txt", file(1, 10))]);
        assert_eq!(plan("two_way", &local, &remote, &SyncState::new()), vec![Op::Record("a.txt".into())]);
    }

    #[test]
    fn unchanged_is_quiet() {
        let a = file(1, 10);
        let local = map(&[("a.txt", a.clone())]);
        let remote = map(&[("a.txt", a.clone())]);
        assert!(plan("two_way", &local, &remote, &synced(&[("a.txt", &a)])).is_empty());
    }

    #[test]
    fn one_sided_change_and_delete() {
        let a = file(1, 10);
        let b = file(2, 20);
        let state = synced(&[("a.txt", &a), ("b.txt", &b)]);
        let local = map(&[("a.txt", file(5, 50)), ("b.txt", b.clone())]);
        let remote = map(&[("a.txt", a.clone())]); // b deleted on the device
        assert_eq!(plan("two_way", &local, &remote, &state), vec![Op::Push("a.txt".into()), Op::TrashLocal("b.txt".into())]);
    }

    #[test]
    fn both_changed_keeps_both() {
        let a = file(1, 10);
        let state = synced(&[("a.txt", &a)]);
        let local = map(&[("a.txt", file(2, 20))]);
        let remote = map(&[("a.txt", file(3, 30))]);
        assert_eq!(
            plan("two_way", &local, &remote, &state),
            vec![Op::KeepLocalCopy("a.txt".into()), Op::Pull("a.txt".into())]
        );
    }

    #[test]
    fn deleted_folder_is_trashed_whole_unless_something_new_is_inside() {
        let d = dir();
        let x = file(1, 10);
        let state = synced(&[("d", &d), ("d/x", &x)]);
        let remote = map(&[("d", d.clone()), ("d/x", x.clone())]);
        assert_eq!(plan("two_way", &BTreeMap::new(), &remote, &state), vec![Op::TrashRemote("d".into())]);

        let remote = map(&[("d", d.clone()), ("d/x", x.clone()), ("d/new", file(2, 20))]);
        let ops = plan("two_way", &BTreeMap::new(), &remote, &state);
        assert_eq!(ops, vec![Op::Pull("d/new".into()), Op::TrashRemote("d/x".into())]);
    }

    #[test]
    fn one_way_modes() {
        let a = file(1, 10);
        let state = synced(&[("a.txt", &a)]);
        let local = map(&[("new.txt", file(2, 20))]);
        let remote = map(&[("a.txt", a.clone()), ("theirs.txt", file(3, 30))]);
        // Upload: this computer wins; files only on the device are left alone.
        assert_eq!(
            plan("upload", &local, &remote, &state),
            vec![Op::TrashRemote("a.txt".into()), Op::Push("new.txt".into())]
        );
        // Download: the device wins.
        assert_eq!(
            plan("download", &local, &remote, &state),
            vec![Op::Pull("a.txt".into()), Op::Pull("theirs.txt".into())]
        );
    }

    #[test]
    fn a_vanished_folder_is_not_mirrored_without_confirmation() {
        let files: Vec<(String, Entry)> = (0..30).map(|i| (format!("f{i}"), file(i, i))).collect();
        let state: SyncState = files.iter().map(|(p, e)| (p.clone(), (Some(e.fingerprint()), Some(e.fingerprint())))).collect();
        let remote: BTreeMap<String, Entry> = files.into_iter().collect();
        let ops = plan("two_way", &BTreeMap::new(), &remote, &state);
        assert_eq!(mass_removal(&ops, &state), Some((0, 30)));
        // Deleting a few files is normal.
        let few: BTreeMap<String, Entry> = remote.iter().skip(3).map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(mass_removal(&plan("two_way", &few, &remote, &state), &state), None);
    }

    #[test]
    fn names_differing_only_in_case_are_one_file() {
        let items = |name: &str| vec![TreeItem { id: "x".into(), path: name.into(), is_dir: false, size: 1, modified_at: Some(1), content_hash: None }];
        let local = to_map(items("Notes.TXT"));
        let remote = to_map(items("notes.txt"));
        assert_eq!(plan("two_way", &local, &remote, &SyncState::new()), vec![Op::Record("notes.txt".into())]);
    }

    #[test]
    fn temporary_files_are_skipped() {
        let local = map(&[("~$report.docx", file(1, 1)), ("x.tmp", file(1, 1)), ("Thumbs.db", file(1, 1))]);
        assert!(plan("two_way", &local, &BTreeMap::new(), &SyncState::new()).is_empty());
    }

    #[test]
    fn conflict_names() {
        // 2026-11-17 is day 20774.
        assert_eq!(conflict_name("report.docx", "Laptop", 20_774 * 86_400_000), "report (Laptop's copy 2026-11-17).docx");
        assert_eq!(conflict_name("Makefile", "PC", 0), "Makefile (PC's copy 1970-01-01)");
        assert_eq!(conflict_name(".env", "PC", 0), ".env (PC's copy 1970-01-01)");
    }
}
