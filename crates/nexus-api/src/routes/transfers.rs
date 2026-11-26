//! Copies between devices, run as background jobs.
//!
//! The source and the destination can each be this computer or a paired
//! device. Content streams straight through this node; nothing is buffered
//! in memory or kept here. Every file goes through a resumable upload, so a
//! dropped connection costs only a retry from where it stopped.
//!
//! Jobs take turns ([`TransferSettings::parallel`] at a time, first in line
//! first), share one speed limit, can be paused, and wait outside transfer
//! hours. Pausing lets go of the connection and the job's turn; resuming
//! continues each file from where it stopped. A copied file whose original
//! has a known BLAKE3 hash is checked against it.
//!
//! [`Mover`] holds the operations on either side; sync uses them too.

use super::audit;
use super::sealed;
use super::files::{TreeItem, TreeResponse, percent_encode, tree_of};
use super::uploads::{self, NewUpload, UploadProgress};
use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::peers::{Acting, CALL_TIMEOUT, TRANSFER_TIMEOUT, Target, peer_error, read_body};
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use futures_util::StreamExt;
use http_body_util::{BodyExt, Limited};
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_common::types::FileEntry;
use nexus_common::{NexusError, Result, new_id};
use nexus_security::Action;
use nexus_storage::paths;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// How often one file is tried again after its connection drops.
const RETRIES: u32 = 8;
/// Files at least this big are downloaded from several devices when they can.
const MULTI_SOURCE_MIN: u64 = 16 * 1024 * 1024;
/// The pieces of such downloads, and how long one may take.
const PIECE_SIZE: u64 = 8 * 1024 * 1024;
const PIECE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_SOURCES: usize = 4;
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Folder listings can be large.
const MAX_TREE: usize = 256 * 1024 * 1024;
const TREE_TIMEOUT: Duration = Duration::from_secs(120);
/// Finished jobs kept for the Transfers page.
const KEEP_FINISHED: usize = 100;

// ---------------------------------------------------------------------------
// Jobs

#[derive(Debug, Clone, Default, Serialize)]
pub struct JobInfo {
    /// "Copy Photos", "Sync Documents"
    pub title: String,
    /// `queued` | `running` | `paused` | `done` | `failed` | `cancelled`
    pub status: String,
    /// Why a job is waiting ("Waits for transfer hours, 01:00–07:00").
    pub note: Option<String>,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    /// The file being sent right now.
    pub current: Option<String>,
    /// Files that could not be sent, with the reason.
    pub errors: Vec<String>,
    /// Times a connection dropped and the transfer continued.
    pub retries: u32,
    /// Files checked against the original's BLAKE3 hash after copying.
    pub verified: u64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    /// What the job works on (a sync pair, a backup set, a storage root or a
    /// file), so a page can show the progress next to it.
    pub subject: Option<String>,
}

pub struct Job {
    pub id: String,
    /// `copy` | `sync` | `backup` | `share`
    pub kind: &'static str,
    /// "This computer → Laptop"
    pub route: String,
    pub by: String,
    pub created_at: i64,
    bytes_done: AtomicU64,
    cancelled: AtomicBool,
    paused: AtomicBool,
    /// Lower goes first.
    order: AtomicI64,
    /// Waiting for a slot / holding one.
    wants_slot: AtomicBool,
    holds_slot: AtomicBool,
    /// What "Retry" sends again (copies only).
    retry: Mutex<Option<RetryPlan>>,
    shared: Arc<Shared>,
    info: Mutex<JobInfo>,
}

#[derive(Serialize)]
pub struct JobView {
    pub(crate) id: String,
    kind: &'static str,
    route: String,
    by: String,
    created_at: i64,
    bytes_done: u64,
    /// Failed files that "Retry" would send again.
    can_retry: bool,
    #[serde(flatten)]
    info: JobInfo,
}

impl Job {
    pub(crate) fn view(&self) -> JobView {
        let can_retry = self.finished() && self.retry.lock().unwrap().as_ref().is_some_and(|r| !r.files.is_empty());
        JobView {
            id: self.id.clone(),
            kind: self.kind,
            route: self.route.clone(),
            by: self.by.clone(),
            created_at: self.created_at,
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            can_retry,
            info: self.info.lock().unwrap().clone(),
        }
    }

    /// Ties the job to what it works on (see [`JobInfo::subject`]).
    pub fn about(self: Arc<Self>, subject: &str) -> Arc<Self> {
        self.update(|i| i.subject = Some(subject.to_string()));
        self
    }

    pub fn update(&self, f: impl FnOnce(&mut JobInfo)) {
        f(&mut self.info.lock().unwrap());
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Whether the job should let go of its connection now: it was paused,
    /// or transfer hours are over.
    fn must_stop(&self) -> bool {
        self.is_paused() || !self.shared.in_hours.load(Ordering::Relaxed)
    }

    fn bytes(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }

    pub(crate) fn set_bytes(&self, n: u64) {
        self.bytes_done.store(n, Ordering::Relaxed);
    }

    /// Bytes sent so far and in total.
    pub(crate) fn progress(&self) -> (u64, u64) {
        (self.bytes(), self.info.lock().unwrap().bytes_total)
    }

    /// Asks the job to stop; it notices at its next step.
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn finished(&self) -> bool {
        matches!(self.info.lock().unwrap().status.as_str(), "done" | "failed" | "cancelled")
    }

    pub fn error_count(&self) -> usize {
        self.info.lock().unwrap().errors.len()
    }

    /// Records one file that could not be sent.
    pub fn fail_file(&self, name: &str, e: &NexusError) {
        self.update(|i| i.errors.push(format!("{name}: {e}")));
    }
}

/// How transfers run: how many at once, how fast, and when.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransferSettings {
    /// Jobs running at the same time (1–4).
    pub parallel: usize,
    /// Upper speed for all copies, syncs and backups together, in KB/s; 0
    /// for no limit.
    pub limit_kbps: u64,
    /// Transfers run only between these times ("HH:MM", local time); both
    /// empty for any time. The end may be before the start (overnight).
    #[serde(default)]
    pub hours_start: Option<String>,
    #[serde(default)]
    pub hours_end: Option<String>,
    /// Transfers wait while this computer runs on battery.
    #[serde(default = "yes")]
    pub pause_on_battery: bool,
    /// This computer does not go to sleep while transfers run, or while
    /// another device is copying to or from it.
    #[serde(default = "yes")]
    pub keep_awake: bool,
}

fn yes() -> bool {
    true
}

impl Default for TransferSettings {
    fn default() -> Self {
        Self { parallel: 2, limit_kbps: 0, hours_start: None, hours_end: None, pause_on_battery: true, keep_awake: true }
    }
}

pub const SETTINGS_KEY: &str = "transfers.settings";

fn minutes(hhmm: &str) -> Option<u32> {
    let (h, m) = hhmm.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl TransferSettings {
    /// The allowed window in minutes after midnight, if there is one.
    fn window(&self) -> Option<(u32, u32)> {
        let start = minutes(self.hours_start.as_deref()?)?;
        let end = minutes(self.hours_end.as_deref()?)?;
        (start != end).then_some((start, end))
    }

    /// Whether transfers may run at `now` (minutes after midnight).
    pub fn allows(&self, now: u32) -> bool {
        match self.window() {
            None => true,
            Some((start, end)) if start < end => now >= start && now < end,
            // Overnight, like 22:00–06:00.
            Some((start, end)) => now >= start || now < end,
        }
    }

    fn validate(mut self) -> Result<Self> {
        if !(1..=4).contains(&self.parallel) {
            return Err(NexusError::invalid("Run between 1 and 4 transfers at once"));
        }
        self.hours_start = self.hours_start.filter(|h| !h.trim().is_empty());
        self.hours_end = self.hours_end.filter(|h| !h.trim().is_empty());
        match (&self.hours_start, &self.hours_end) {
            (None, None) => {}
            (Some(a), Some(b)) if minutes(a).is_some() && minutes(b).is_some() && minutes(a) != minutes(b) => {}
            _ => return Err(NexusError::invalid("Give transfer hours as two different times, like 22:00 and 07:00")),
        }
        Ok(self)
    }

    fn hours_note(&self, on_battery: bool) -> Option<String> {
        if on_battery && self.pause_on_battery {
            return Some("Waits for this computer to be plugged in".into());
        }
        Some(format!("Waits for transfer hours, {}–{}", self.hours_start.as_deref()?, self.hours_end.as_deref()?))
    }
}

fn local_minutes() -> u32 {
    use chrono::Timelike;
    let now = chrono::Local::now();
    now.hour() * 60 + now.minute()
}

/// One speed limit shared by every job: each chunk waits for its turn.
#[derive(Default)]
pub struct RateLimit {
    bytes_per_sec: AtomicU64,
    next_free: Mutex<Option<tokio::time::Instant>>,
}

impl RateLimit {
    fn set(&self, bytes_per_sec: u64) {
        self.bytes_per_sec.store(bytes_per_sec, Ordering::Relaxed);
        *self.next_free.lock().unwrap() = None;
    }

    async fn take(&self, n: usize) {
        let rate = self.bytes_per_sec.load(Ordering::Relaxed);
        if rate == 0 || n == 0 {
            return;
        }
        let at = {
            let mut next = self.next_free.lock().unwrap();
            let now = tokio::time::Instant::now();
            let at = next.filter(|t| *t > now).unwrap_or(now);
            *next = Some(at + Duration::from_secs_f64(n as f64 / rate as f64));
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

/// What every job reads while it runs.
pub struct Shared {
    settings: Mutex<TransferSettings>,
    limit: RateLimit,
    /// Whether it is within transfer hours (looked at every few seconds),
    /// and not on battery when that pauses transfers.
    in_hours: AtomicBool,
    on_battery: AtomicBool,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            settings: Mutex::new(TransferSettings::default()),
            limit: RateLimit::default(),
            in_hours: AtomicBool::new(true),
            on_battery: AtomicBool::new(false),
        }
    }
}

/// Copy and sync jobs on this node, newest first.
pub struct Jobs {
    list: Mutex<Vec<Arc<Job>>>,
    /// Slots in use; jobs beyond `parallel` wait their turn.
    running: Mutex<usize>,
    wake: tokio::sync::Notify,
    shared: Arc<Shared>,
    next_order: AtomicI64,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            list: Mutex::new(Vec::new()),
            running: Mutex::new(0),
            wake: tokio::sync::Notify::new(),
            shared: Arc::new(Shared::default()),
            next_order: AtomicI64::new(0),
        }
    }
}

/// A job's turn to run; ends when dropped.
pub struct Slot<'a> {
    jobs: &'a Jobs,
    job: &'a Job,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.jobs.release(self.job);
    }
}

impl Jobs {
    pub fn add(&self, kind: &'static str, title: String, route: String, by: String) -> Arc<Job> {
        let job = Arc::new(Job {
            id: new_id(),
            kind,
            route,
            by,
            created_at: now_ms(),
            bytes_done: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            order: AtomicI64::new(self.next_order.fetch_add(1, Ordering::Relaxed)),
            wants_slot: AtomicBool::new(false),
            holds_slot: AtomicBool::new(false),
            retry: Mutex::new(None),
            shared: self.shared.clone(),
            info: Mutex::new(JobInfo { status: "queued".into(), title, ..Default::default() }),
        });
        let mut list = self.list.lock().unwrap();
        list.insert(0, job.clone());
        let mut finished = 0;
        list.retain(|j| !j.finished() || {
            finished += 1;
            finished <= KEEP_FINISHED
        });
        job
    }

    /// Jobs the caller may see: all of them, or their own if they are
    /// limited to some storage.
    fn views(&self, user: &AuthUser) -> Vec<JobView> {
        self.list.lock().unwrap().iter().filter(|j| !user.is_limited() || j.by == user.username).map(|j| j.view()).collect()
    }

    /// Whether any job is queued or running.
    pub(crate) fn busy(&self) -> bool {
        self.list.lock().unwrap().iter().any(|j| !j.finished())
    }

    pub(crate) fn find(&self, id: &str) -> Option<Arc<Job>> {
        self.list.lock().unwrap().iter().find(|j| j.id == id).cloned()
    }

    /// Forgets finished jobs: the caller's own if they are limited to some
    /// storage (they cannot see anyone else's).
    fn clear_finished(&self, user: &AuthUser) {
        self.list.lock().unwrap().retain(|j| !j.finished() || (user.is_limited() && j.by != user.username));
    }

    pub fn settings(&self) -> TransferSettings {
        self.shared.settings.lock().unwrap().clone()
    }

    /// Applies new settings and lets waiting jobs look again.
    pub fn apply(&self, settings: TransferSettings) {
        self.shared.limit.set(settings.limit_kbps.saturating_mul(1024));
        let battery = self.shared.on_battery.load(Ordering::Relaxed) && settings.pause_on_battery;
        self.shared.in_hours.store(settings.allows(local_minutes()) && !battery, Ordering::Relaxed);
        *self.shared.settings.lock().unwrap() = settings;
        self.wake.notify_waiters();
    }

    /// Looks at the clock for transfer hours, and at the power supply.
    /// `peers_busy`: files are moving to or from another device right now
    /// (it may be that device's own transfer, which has no job here).
    fn tick(&self, on_battery: bool, peers_busy: bool) {
        self.shared.on_battery.store(on_battery, Ordering::Relaxed);
        let settings = self.shared.settings.lock().unwrap().clone();
        let allowed = settings.allows(local_minutes()) && !(on_battery && settings.pause_on_battery);
        if self.shared.in_hours.swap(allowed, Ordering::Relaxed) != allowed {
            self.wake.notify_waiters();
        }
        // Stay awake, and at full speed, while something runs.
        let busy = *self.running.lock().unwrap() > 0 || peers_busy;
        crate::power::keep_awake(busy && settings.keep_awake);
        crate::power::full_speed(busy);
    }

    /// Whether `job` is first in line among the jobs waiting to run.
    fn next_in_line(&self, job: &Job) -> bool {
        self.list
            .lock()
            .unwrap()
            .iter()
            .filter(|j| j.wants_slot.load(Ordering::Relaxed) && !j.is_paused() && !j.is_cancelled())
            .min_by_key(|j| j.order.load(Ordering::Relaxed))
            .is_some_and(|j| j.id == job.id)
    }

    /// Waits until `job` may run: not paused, within transfer hours, first
    /// in line and a slot free. `false` if it was cancelled meanwhile.
    async fn acquire(&self, s: &AppState, job: &Job) -> bool {
        job.wants_slot.store(true, Ordering::Relaxed);
        let mut shown = String::new();
        loop {
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if job.is_cancelled() {
                job.wants_slot.store(false, Ordering::Relaxed);
                return false;
            }
            let settings = self.settings();
            let in_hours = self.shared.in_hours.load(Ordering::Relaxed);
            {
                let mut running = self.running.lock().unwrap();
                if !job.is_paused() && in_hours && *running < settings.parallel && self.next_in_line(job) {
                    *running += 1;
                    job.holds_slot.store(true, Ordering::Relaxed);
                    job.wants_slot.store(false, Ordering::Relaxed);
                    return true;
                }
            }
            // Say why it waits, once per change.
            let (status, note) = if job.is_paused() {
                ("paused", None)
            } else if !in_hours {
                ("queued", settings.hours_note(self.shared.on_battery.load(Ordering::Relaxed)))
            } else {
                ("queued", None)
            };
            let now = format!("{status}{note:?}");
            if now != shown {
                job.update(|i| i.note = note);
                set_status(s, job, status);
                shown = now;
            }
            notified.await;
        }
    }

    fn release(&self, job: &Job) {
        if job.holds_slot.swap(false, Ordering::Relaxed) {
            let mut running = self.running.lock().unwrap();
            *running = running.saturating_sub(1);
            drop(running);
            self.wake.notify_waiters();
        }
    }

    /// Waits for a turn to run. Returns `None` if the job was cancelled
    /// meanwhile.
    pub async fn start<'a>(&'a self, s: &AppState, job: &'a Job) -> Option<Slot<'a>> {
        if !self.acquire(s, job).await {
            set_status(s, job, "cancelled");
            return None;
        }
        job.update(|i| i.note = None);
        set_status(s, job, "running");
        Some(Slot { jobs: self, job })
    }

    /// Between files and after an interrupted file: if the job was paused
    /// or transfer hours ended, gives its slot to others and waits for its
    /// turn again. `false` if it was cancelled meanwhile.
    pub async fn checkpoint(&self, s: &AppState, job: &Job) -> bool {
        if job.is_cancelled() {
            return false;
        }
        if !job.must_stop() {
            return true;
        }
        let current = job.info.lock().unwrap().current.clone();
        self.release(job);
        if !self.acquire(s, job).await {
            return false;
        }
        job.update(|i| {
            i.note = None;
            i.current = current;
        });
        set_status(s, job, "running");
        true
    }

    /// Pauses or resumes every unfinished job. Returns how many changed.
    pub fn pause_all(&self, s: &AppState, on: bool) -> usize {
        let jobs: Vec<Arc<Job>> = self.list.lock().unwrap().iter().filter(|j| !j.finished() && j.is_paused() != on).cloned().collect();
        for job in &jobs {
            self.pause(job, on);
            let status = job.info.lock().unwrap().status.clone();
            match (on, status.as_str()) {
                (true, "queued") => set_status(s, job, "paused"),
                (false, "paused") => set_status(s, job, "queued"),
                _ => {}
            }
        }
        jobs.len()
    }

    fn pause(&self, job: &Job, on: bool) {
        job.paused.store(on, Ordering::Relaxed);
        self.wake.notify_waiters();
    }

    /// Puts a waiting job first in line.
    fn move_first(&self, job: &Job) {
        let first = self.list.lock().unwrap().iter().map(|j| j.order.load(Ordering::Relaxed)).min().unwrap_or(0);
        job.order.store(first - 1, Ordering::Relaxed);
        self.wake.notify_waiters();
    }
}

const TICK_SECS: u64 = 10;
/// Traffic with other devices over one tick that counts as files moving
/// (about 100 KB/s): more than keep-alives and the odd thumbnail.
const PEERS_BUSY_BYTES: u64 = 100 * 1024 * TICK_SECS;

/// Loads the transfer settings and keeps transfer hours current.
pub fn spawn(s: SharedState) {
    tokio::spawn(async move {
        if let Ok(Some(json)) = s.db.get_setting(SETTINGS_KEY).await
            && let Ok(settings) = serde_json::from_str::<TransferSettings>(&json)
        {
            s.jobs.apply(settings);
        }
        let mut traffic = s.net.p2p.total_traffic();
        loop {
            let on_battery = tokio::task::spawn_blocking(crate::power::on_battery).await.unwrap_or(false);
            let now = s.net.p2p.total_traffic();
            let peers_busy = now.saturating_sub(traffic) >= PEERS_BUSY_BYTES;
            traffic = now;
            s.jobs.tick(on_battery, peers_busy);
            tokio::time::sleep(Duration::from_secs(TICK_SECS)).await;
        }
    });
}

pub fn set_status(s: &AppState, job: &Job, status: &str) {
    job.update(|i| {
        i.status = status.to_string();
        match status {
            "running" => {
                i.started_at.get_or_insert(now_ms());
            }
            "done" | "failed" | "cancelled" => {
                i.finished_at = Some(now_ms());
                i.current = None;
                i.note = None;
            }
            _ => {}
        }
    });
    s.events.emit(Event::TransferUpdated { id: job.id.clone(), status: status.to_string() });
}

/// Ends a job: cancelled, failed (nothing went through) or done.
pub fn finish_job(s: &AppState, job: &Job) {
    let (done, errors) = {
        let i = job.info.lock().unwrap();
        (i.files_done, i.errors.len())
    };
    let status = if job.is_cancelled() {
        "cancelled"
    } else if errors > 0 && done == 0 {
        "failed"
    } else {
        "done"
    };
    set_status(s, job, status);
}

// ---------------------------------------------------------------------------
// Moving files between this node and paired devices

/// Where a file lives or goes.
#[derive(Clone)]
pub enum Side {
    Local,
    Peer(Target),
}

impl Side {
    pub fn name(&self) -> String {
        match self {
            Side::Local => "this computer".into(),
            Side::Peer(t) => t.name.clone(),
        }
    }
}

/// What a copy or sync needs to know about a file.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub id: String,
    pub root_id: String,
    pub relative_path: String,
    pub filename: String,
    pub is_dir: bool,
    pub size: i64,
    pub modified_at: Option<i64>,
    #[serde(default)]
    pub content_hash: Option<String>,
}

impl From<FileEntry> for Meta {
    fn from(f: FileEntry) -> Self {
        Self {
            id: f.id,
            root_id: f.root_id,
            relative_path: f.relative_path,
            filename: f.filename,
            is_dir: f.is_dir,
            size: f.size,
            modified_at: f.modified_at,
            content_hash: f.content_hash,
        }
    }
}

impl Meta {
    /// A folder tree entry, under a folder at `base` in `root_id`.
    pub fn of_tree(item: &TreeItem, root_id: &str, base: &str) -> Self {
        let (_, name) = paths::split_parent(&item.path);
        Self {
            id: item.id.clone(),
            root_id: root_id.to_string(),
            relative_path: paths::join(base, &item.path),
            filename: name,
            is_dir: item.is_dir,
            size: item.size,
            modified_at: item.modified_at,
            content_hash: item.content_hash.clone(),
        }
    }
}

pub async fn peer_target(s: &AppState, device_id: &str) -> Result<Target> {
    let device = s.db.get_device(device_id).await?;
    if device.is_local || device.public_key.is_none() {
        return Err(NexusError::NotFound("paired device".into()));
    }
    Target::of(&device)
}

/// Errors worth another try: the connection dropped, the device is busy,
/// or the other side has not noticed the last attempt ended yet.
pub fn retryable(e: &NexusError) -> bool {
    matches!(e, NexusError::PeerUnreachable(_) | NexusError::Io(_) | NexusError::Conflict(_) | NexusError::Unavailable(_))
}

fn cancelled() -> NexusError {
    NexusError::Unavailable("Cancelled".into())
}

/// Reads a local file from `offset` as a stream of chunks.
fn file_body<R: tokio::io::AsyncRead + Unpin + Send + 'static>(file: R) -> Body {
    Body::from_stream(futures_util::stream::unfold(file, |mut file| async move {
        let mut buf = vec![0u8; 256 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(Bytes::from(buf)), file))
            }
            Err(e) => Some((Err(e), file)),
        }
    }))
}

/// Counts bytes into the job as they pass, keeps to the speed limit, and
/// stops when the job is cancelled or paused.
fn counted(body: Body, job: Arc<Job>) -> Body {
    Body::from_stream(body.into_data_stream().then(move |chunk| {
        let job = job.clone();
        async move {
            if job.is_cancelled() {
                return Err(std::io::Error::other("cancelled"));
            }
            if job.must_stop() {
                return Err(std::io::Error::other("paused"));
            }
            let chunk = chunk.map_err(std::io::Error::other)?;
            job.shared.limit.take(chunk.len()).await;
            job.bytes_done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            Ok(chunk)
        }
    }))
}

/// What a file is sent from.
pub(crate) enum Source<'a> {
    /// A file in a storage location on either side, by id.
    Item(&'a Side, &'a str),
    /// A file on this device's disk outside any storage location.
    Disk(&'a std::path::Path),
    /// A file on this device's disk of `plain` bytes, encrypted as it is
    /// read (backups); what is sent is its sealed form.
    Sealed { path: &'a std::path::Path, key: [u8; 32], plain: u64, hash: sealed::PlainHash },
    /// An encrypted content of `plain` bytes on either side, decrypted as
    /// it is read.
    Unsealed { side: &'a Side, id: &'a str, key: [u8; 32], plain: u64 },
}

/// Runs file operations on either side for one person.
pub struct Mover<'a> {
    pub s: &'a AppState,
    pub user: &'a AuthUser,
}

impl Mover<'_> {
    fn acting(&self) -> Option<Acting<'_>> {
        Some(Acting::of(self.user))
    }

    async fn json<T: DeserializeOwned>(&self, t: &Target, m: Method, path: &str, body: Option<&impl Serialize>, timeout: Duration) -> Result<T> {
        Ok(self.s.peers.call(t, m, path, body, self.acting(), timeout).await?.0)
    }

    pub async fn info(&self, side: &Side, file_id: &str) -> Result<Meta> {
        match side {
            Side::Local => {
                self.user.require(Action::Read).map_err(|e| e.0)?;
                let file = self.s.db.get_file(file_id).await?;
                self.user.require_access(&file.root_id, &file.relative_path, Need::Read).map_err(|e| e.0)?;
                Ok(file.into())
            }
            Side::Peer(t) => self.json(t, Method::GET, &format!("/api/v1/files/{file_id}"), None::<&()>, CALL_TIMEOUT).await,
        }
    }

    /// Everything under a folder, paths relative to it, parents first.
    pub async fn tree(&self, side: &Side, root_id: &str, path: &str) -> Result<Vec<TreeItem>> {
        match side {
            Side::Local => {
                self.user.require(Action::Read).map_err(|e| e.0)?;
                self.user.require_access(root_id, &paths::normalize_relative(path)?, Need::Read).map_err(|e| e.0)?;
                tree_of(self.s, root_id, path).await
            }
            Side::Peer(t) => {
                let url = format!("/api/v1/files/tree?root_id={}&path={}", percent_encode(root_id), percent_encode(path));
                let (reply, _): (TreeResponse, _) =
                    self.s.peers.call_limited(t, Method::GET, &url, None::<&()>, self.acting(), TREE_TIMEOUT, MAX_TREE).await?;
                Ok(reply.items)
            }
        }
    }

    /// Creates a folder. `Ok(false)` if something with that name exists.
    pub async fn mkdir(&self, side: &Side, root_id: &str, parent: &str, name: &str) -> Result<bool> {
        let made = match side {
            Side::Local => {
                self.user.require(Action::Write).map_err(|e| e.0)?;
                self.user.require_access(root_id, &paths::normalize_relative(parent)?, Need::Add).map_err(|e| e.0)?;
                match self.s.storage.create_folder(root_id, parent, name).await {
                    Ok((root, rel)) => {
                        self.s.indexer.reindex_path(&root, &rel).await?;
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            Side::Peer(t) => self
                .json::<serde_json::Value>(
                    t,
                    Method::POST,
                    "/api/v1/files/folder",
                    Some(&json!({ "root_id": root_id, "path": parent, "name": name })),
                    CALL_TIMEOUT,
                )
                .await
                .map(|_| ()),
        };
        match made {
            Ok(()) => Ok(true),
            Err(NexusError::Conflict(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Creates a folder under a free name ("Photos", "Photos (1)", ...).
    pub async fn mkdir_free(&self, side: &Side, root_id: &str, parent: &str, name: &str) -> Result<String> {
        for n in 0..1000 {
            let candidate = paths::numbered_name(name, n, true);
            if self.mkdir(side, root_id, parent, &candidate).await? {
                return Ok(candidate);
            }
        }
        Err(NexusError::Conflict("Too many folders with this name".into()))
    }

    /// Moves a file or folder to the trash on its device.
    pub async fn trash(&self, side: &Side, file_id: &str) -> Result<()> {
        match side {
            Side::Local => {
                self.user.require(Action::Write).map_err(|e| e.0)?;
                let file = self.s.db.get_file(file_id).await?;
                self.user.require_access(&file.root_id, &file.relative_path, Need::Change).map_err(|e| e.0)?;
                self.s.storage.trash(&file, self.user.user_id.as_deref()).await?;
                self.s.events.emit(Event::FileDeleted { root_id: file.root_id.clone(), path: file.relative_path.clone() });
                audit(self.s, self.user, "file.trashed", Some(&file.relative_path)).await;
                Ok(())
            }
            Side::Peer(t) => self
                .json::<serde_json::Value>(t, Method::DELETE, &format!("/api/v1/files/{file_id}"), None::<&()>, CALL_TIMEOUT)
                .await
                .map(|_| ()),
        }
    }

    /// Opens a file's content from `offset`.
    pub(crate) async fn open(&self, side: &Side, file_id: &str, offset: u64) -> Result<Body> {
        self.open_range(side, file_id, offset, None).await
    }

    /// Reads bytes `start..end` of a file whole (one piece of a download
    /// from several devices).
    async fn read_range(&self, side: &Side, file_id: &str, start: u64, end: u64) -> Result<Bytes> {
        let read = async {
            let body = self.open_range(side, file_id, start, Some(end)).await?;
            let limit = (end - start) as usize;
            let bytes = Limited::new(body, limit + 1)
                .collect()
                .await
                .map_err(|e| NexusError::PeerUnreachable(format!("{}: {e}", side.name())))?
                .to_bytes();
            if bytes.len() != limit {
                return Err(NexusError::PeerUnreachable(format!("{} sent part of the file only", side.name())));
            }
            Ok(bytes)
        };
        tokio::time::timeout(PIECE_TIMEOUT, read)
            .await
            .unwrap_or_else(|_| Err(NexusError::PeerUnreachable(format!("{} was too slow", side.name()))))
    }

    /// Opens a file's content from `offset`, up to `end` (exclusive) if given.
    async fn open_range(&self, side: &Side, file_id: &str, offset: u64, end: Option<u64>) -> Result<Body> {
        match side {
            Side::Local => {
                self.user.require(Action::Read).map_err(|e| e.0)?;
                let file = self.s.db.get_file(file_id).await?;
                self.user.require_access(&file.root_id, &file.relative_path, Need::Read).map_err(|e| e.0)?;
                if file.is_dir {
                    return Err(NexusError::invalid("This is a folder"));
                }
                let root = self.s.db.get_root(&file.root_id).await?;
                self.s.storage.ensure_online(&root)?;
                let path = self.s.storage.resolve(&root, &file.relative_path)?;
                let mut handle = tokio::fs::File::open(&path).await?;
                if offset > 0 {
                    handle.seek(std::io::SeekFrom::Start(offset)).await?;
                }
                match end {
                    Some(end) => Ok(file_body(handle.take(end.saturating_sub(offset)))),
                    None => Ok(file_body(handle)),
                }
            }
            Side::Peer(t) => {
                let link: serde_json::Value =
                    self.json(t, Method::POST, &format!("/api/v1/files/{file_id}/link"), None::<&()>, CALL_TIMEOUT).await?;
                let url = link["url"]
                    .as_str()
                    .ok_or_else(|| NexusError::PeerUnreachable(format!("{} sent an unexpected reply", t.name)))?;
                let mut headers = HeaderMap::new();
                let range = match end {
                    Some(end) => Some(format!("bytes={offset}-{}", end.saturating_sub(1))),
                    None if offset > 0 => Some(format!("bytes={offset}-")),
                    None => None,
                };
                if let Some(range) = range {
                    headers.insert(header::RANGE, HeaderValue::from_str(&range).map_err(NexusError::internal)?);
                }
                let (res, _) = self.s.peers.send(t, Method::GET, url, headers, Body::empty(), self.acting(), CALL_TIMEOUT).await?;
                let status = res.status();
                if !status.is_success() {
                    let bytes = read_body(res).await.unwrap_or_default();
                    return Err(peer_error(&t.name, status, &bytes));
                }
                if (offset > 0 || end.is_some()) && status != StatusCode::PARTIAL_CONTENT {
                    return Err(NexusError::PeerUnreachable(format!("{} could not continue from the middle of the file", t.name)));
                }
                Ok(Body::new(res.into_body()))
            }
        }
    }

    async fn begin(&self, side: &Side, req: &NewUpload) -> Result<UploadProgress> {
        match side {
            Side::Local => uploads::begin(self.s, self.user, req).await,
            Side::Peer(t) => self.json(t, Method::POST, "/api/v1/files/uploads", Some(req), CALL_TIMEOUT).await,
        }
    }

    async fn offset(&self, side: &Side, id: &str) -> Result<u64> {
        match side {
            Side::Local => Ok(uploads::progress(self.s, self.user, id).await?.offset),
            Side::Peer(t) => {
                let p: UploadProgress =
                    self.json(t, Method::GET, &format!("/api/v1/files/uploads/{id}"), None::<&()>, CALL_TIMEOUT).await?;
                Ok(p.offset)
            }
        }
    }

    async fn append(&self, side: &Side, id: &str, offset: u64, len: u64, body: Body) -> Result<()> {
        match side {
            Side::Local => uploads::append(self.s, self.user, id, offset, body).await.map(|_| ()),
            Side::Peer(t) => {
                let mut headers = HeaderMap::new();
                headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
                headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
                let url = format!("/api/v1/files/uploads/{id}?offset={offset}");
                let (res, _) = self.s.peers.send(t, Method::PUT, &url, headers, body, self.acting(), TRANSFER_TIMEOUT).await?;
                let status = res.status();
                let bytes = read_body(res).await.map_err(|e| NexusError::PeerUnreachable(format!("{}: {e}", t.name)))?;
                if !status.is_success() {
                    return Err(peer_error(&t.name, status, &bytes));
                }
                Ok(())
            }
        }
    }

    async fn finish(&self, side: &Side, id: &str) -> Result<Meta> {
        match side {
            Side::Local => Ok(uploads::finish(self.s, self.user, id).await?.into()),
            Side::Peer(t) => {
                self.json(t, Method::POST, &format!("/api/v1/files/uploads/{id}/complete"), None::<&()>, TRANSFER_TIMEOUT).await
            }
        }
    }

    async fn abort(&self, side: &Side, id: &str) {
        let _ = match side {
            Side::Local => uploads::cancel(self.s, self.user, id).await,
            Side::Peer(t) => self
                .json::<serde_json::Value>(t, Method::DELETE, &format!("/api/v1/files/uploads/{id}"), None::<&()>, CALL_TIMEOUT)
                .await
                .map(|_| ()),
        };
    }

    /// Sends one file, continuing from where it stopped when the connection
    /// drops. Returns the file as it now exists at the destination.
    pub async fn send_file(&self, job: &Arc<Job>, from: &Side, src: &Meta, to: &Side, req: NewUpload) -> Result<Meta> {
        let source = Source::Item(from, &src.id);
        let placed = match self.other_copies(from, src, to).await {
            Some(sources) => match self.send_pieces(job, &src.filename, &sources, to, req.clone()).await {
                Ok(placed) => placed,
                Err(e) if job.is_cancelled() => return Err(e),
                Err(e) => {
                    tracing::info!(file = %src.filename, error = %e, "download from several devices stopped; continuing from one");
                    self.send_from(job, &src.filename, &source, to, req).await?
                }
            },
            None => self.send_from(job, &src.filename, &source, to, req).await?,
        };
        let Some(want) = src.content_hash.as_deref() else { return Ok(placed) };
        if placed.content_hash.as_deref() == Some(want) {
            job.update(|i| i.verified += 1);
            return Ok(placed);
        }
        // Different content arrived: send it once more over the copy.
        tracing::warn!(file = %src.filename, "copy differs from the original; sending again");
        let (parent, name) = paths::split_parent(&placed.relative_path);
        let again = NewUpload {
            root_id: placed.root_id.clone(),
            path: parent,
            name,
            size: src.size as u64,
            on_conflict: Some("replace".into()),
            modified_at: src.modified_at,
            make_parents: true,
        };
        let size = src.size as u64;
        job.set_bytes(job.bytes().saturating_sub(size));
        let placed = self.send_from(job, &src.filename, &source, to, again).await?;
        if placed.content_hash.as_deref() == Some(want) {
            job.update(|i| i.verified += 1);
            Ok(placed)
        } else {
            Err(NexusError::Conflict(
                "The copy differs from the original (BLAKE3). The original may have changed while it was copied".into(),
            ))
        }
    }

    /// For a big file coming from a paired device to this computer: the same
    /// content on other online devices, to fetch parts of it from each.
    /// `None` when there is only the one source.
    async fn other_copies(&self, from: &Side, src: &Meta, to: &Side) -> Option<Vec<(Side, String)>> {
        let (Side::Peer(origin), Side::Local) = (from, to) else { return None };
        let hash = src.content_hash.as_deref()?;
        if (src.size as u64) < MULTI_SOURCE_MIN {
            return None;
        }
        let devices = self.s.db.list_peer_devices().await.ok()?;
        let candidates: Vec<Target> = devices
            .iter()
            .filter(|d| d.id != origin.device_id && d.status == "online" && d.blocked_at.is_none())
            .filter_map(|d| Target::of(d).ok())
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let url = format!("/api/v1/files/by-hash/{hash}?size={}", src.size);
        let found = futures_util::future::join_all(candidates.into_iter().map(|t| {
            let url = url.clone();
            async move {
                let meta: Meta = self.json(&t, Method::GET, &url, None::<&()>, LOOKUP_TIMEOUT).await.ok()?;
                (meta.size == src.size && meta.content_hash.as_deref() == Some(hash)).then(|| (Side::Peer(t), meta.id))
            }
        }))
        .await;
        let mut sources = vec![(from.clone(), src.id.clone())];
        sources.extend(found.into_iter().flatten().take(MAX_SOURCES - 1));
        (sources.len() > 1).then_some(sources)
    }

    /// Downloads a file in pieces from several devices at once, writing the
    /// pieces in order. A piece one device fails to send is fetched from the
    /// first device instead.
    async fn send_pieces(&self, job: &Arc<Job>, label: &str, sources: &[(Side, String)], to: &Side, req: NewUpload) -> Result<Meta> {
        let base = job.bytes();
        let size = req.size;
        let up = self.begin(to, &req).await?;
        tracing::info!(file = %label, sources = sources.len(), "downloading from several devices");
        let result = async {
            let mut offset = 0u64;
            while offset < size {
                if job.is_cancelled() {
                    return Err(cancelled());
                }
                if job.must_stop() {
                    return Err(NexusError::Unavailable("Paused".into()));
                }
                let pieces: Vec<(u64, u64)> = (0..sources.len() as u64 * 2)
                    .map(|i| offset + i * PIECE_SIZE)
                    .take_while(|start| *start < size)
                    .map(|start| (start, (start + PIECE_SIZE).min(size)))
                    .collect();
                let fetched = futures_util::future::join_all(pieces.iter().enumerate().map(|(i, &(start, end))| {
                    let (side, id) = &sources[i % sources.len()];
                    self.read_range(side, id, start, end)
                }))
                .await;
                for (&(start, end), piece) in pieces.iter().zip(fetched) {
                    let bytes = match piece {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            tracing::debug!(file = %label, error = %e, "piece failed; asking the first device");
                            self.read_range(&sources[0].0, &sources[0].1, start, end).await?
                        }
                    };
                    self.append(to, &up.id, start, end - start, counted(Body::from(bytes), job.clone())).await?;
                    offset = end;
                }
            }
            self.finish(to, &up.id).await
        }
        .await;
        match result {
            Ok(placed) => {
                job.set_bytes(base + size);
                Ok(placed)
            }
            Err(e) => {
                self.abort(to, &up.id).await;
                job.set_bytes(base);
                Err(e)
            }
        }
    }

    /// Sends a file that is not in any storage location (a phone's photos),
    /// read straight from disk.
    pub async fn send_disk_file(&self, job: &Arc<Job>, path: &std::path::Path, to: &Side, req: NewUpload) -> Result<Meta> {
        let name = req.name.clone();
        self.send_from(job, &name, &Source::Disk(path), to, req).await
    }

    async fn open_source(&self, src: &Source<'_>, offset: u64) -> Result<Body> {
        match src {
            Source::Item(side, id) => self.open(side, id, offset).await,
            Source::Disk(path) => {
                let mut handle = tokio::fs::File::open(path).await?;
                if offset > 0 {
                    handle.seek(std::io::SeekFrom::Start(offset)).await?;
                }
                Ok(file_body(handle))
            }
            Source::Sealed { path, key, plain, hash } => {
                Ok(Body::from_stream(sealed::seal_file(path.to_path_buf(), *key, *plain, offset, hash.clone()).await?))
            }
            Source::Unsealed { side, id, key, plain } => {
                let (at, first, skip) = sealed::plain_start(offset);
                let body = self.open(side, id, at).await?;
                Ok(Body::from_stream(sealed::open_stream(body.into_data_stream(), *key, *plain, first, skip)))
            }
        }
    }

    /// The resumable sending loop.
    pub(crate) async fn send_from(&self, job: &Arc<Job>, label: &str, src: &Source<'_>, to: &Side, req: NewUpload) -> Result<Meta> {
        let base = job.bytes();
        let size = req.size;
        let mut up = self.begin(to, &req).await?;
        let mut tries = 0;
        let mut resumed = false;
        loop {
            if job.is_cancelled() {
                self.abort(to, &up.id).await;
                return Err(cancelled());
            }
            let attempt = async {
                let offset = if resumed { self.offset(to, &up.id).await? } else { 0 };
                job.set_bytes(base + offset);
                if offset < size {
                    let body = self.open_source(src, offset).await?;
                    self.append(to, &up.id, offset, size - offset, counted(body, job.clone())).await?;
                }
                self.finish(to, &up.id).await
            }
            .await;
            match attempt {
                Ok(placed) => {
                    job.set_bytes(base + size);
                    return Ok(placed);
                }
                Err(_) if job.is_cancelled() => {
                    self.abort(to, &up.id).await;
                    return Err(cancelled());
                }
                Err(_) if job.must_stop() => {
                    // Paused, or outside transfer hours: wait for the turn
                    // to come back, then continue where it stopped.
                    if !self.s.jobs.checkpoint(self.s, job).await {
                        self.abort(to, &up.id).await;
                        return Err(cancelled());
                    }
                    resumed = true;
                    // After a long pause the half-sent file may have been
                    // cleared away at the destination: start it again.
                    if matches!(self.offset(to, &up.id).await, Err(NexusError::NotFound(_))) {
                        up = self.begin(to, &req).await?;
                        resumed = false;
                    }
                }
                Err(e) if retryable(&e) && tries < RETRIES => {
                    tries += 1;
                    resumed = true;
                    job.update(|i| i.retries += 1);
                    tracing::info!(file = %label, error = %e, attempt = tries, "transfer interrupted; continuing");
                    tokio::time::sleep(Duration::from_secs((1u64 << tries.min(5)).min(30))).await;
                }
                Err(e) => {
                    self.abort(to, &up.id).await;
                    job.set_bytes(base);
                    return Err(e);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Copy jobs

#[derive(Deserialize)]
pub struct CopyRequest {
    /// Device holding the items; omitted or empty for this computer.
    from_device: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
    #[serde(default)]
    file_ids: Vec<String>,
    /// Device to copy to; omitted or empty for this computer.
    to_device: Option<String>,
    root_id: String,
    #[serde(default)]
    path: String,
    /// Move: send the originals to the trash once they are copied.
    #[serde(default)]
    remove_originals: bool,
}

enum Piece {
    File(Meta),
    Folder(Meta, Vec<TreeItem>),
}

/// The files a copy could not send, to send again on "Retry".
pub struct RetryPlan {
    from: Side,
    to: Side,
    root_id: String,
    /// Each file and the folder it goes to.
    files: Vec<(Meta, String)>,
}

/// `POST /api/v1/transfers/copy`: starts copying files and folders to a
/// folder on another device (or this one). Answers at once with the job.
pub async fn copy(State(s): State<SharedState>, user: AuthUser, Json(req): Json<CopyRequest>) -> ApiResult<(StatusCode, Json<JobView>)> {
    user.require_person()?;
    user.require(Action::Write)?;
    let mut ids = req.file_ids.clone();
    ids.extend(req.file_id.clone());
    let view = start_copy(&s, user, req.from_device.as_deref(), req.to_device.as_deref(), ids, req.root_id, &req.path, req.remove_originals)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(view)))
}

/// Starts a job copying files and folders to a folder on a device (or this
/// one; `None` or this device's id). Answers at once with the job.
pub(crate) async fn start_copy(
    s: &SharedState,
    user: AuthUser,
    from_device: Option<&str>,
    to_device: Option<&str>,
    mut ids: Vec<String>,
    root_id: String,
    path: &str,
    remove_originals: bool,
) -> Result<JobView> {
    ids.dedup();
    if ids.is_empty() {
        return Err(NexusError::invalid("Choose something to copy"));
    }
    let side = |id: Option<&str>| {
        let s = s.clone();
        let id = id.filter(|d| !d.is_empty() && *d != s.node.device_id).map(str::to_string);
        async move {
            match id {
                Some(id) => Ok::<_, NexusError>(Side::Peer(peer_target(&s, &id).await?)),
                None => Ok(Side::Local),
            }
        }
    };
    let from = side(from_device).await?;
    let to = side(to_device).await?;
    let path = paths::normalize_relative(path)?;

    let verb = if remove_originals { "Move" } else { "Copy" };
    let title = if ids.len() == 1 { verb.to_string() } else { format!("{verb} {} items", ids.len()) };
    // The title gets the item's name once it has been looked up.
    let route = format!("{} → {}", capitalise(&from.name()), capitalise(&to.name()));
    let job = s.jobs.add("copy", title, route, user.username.clone());
    let view = job.view();
    let state = s.clone();
    tokio::spawn(async move {
        run_copy(&state, &user, &job, from, to, ids, root_id, path, remove_originals).await;
    });
    Ok(view)
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_copy(
    s: &AppState,
    user: &AuthUser,
    job: &Arc<Job>,
    from: Side,
    to: Side,
    ids: Vec<String>,
    root_id: String,
    path: String,
    remove_originals: bool,
) {
    let Some(_slot) = s.jobs.start(s, job).await else { return };
    let mover = Mover { s, user };

    // Look at everything first, so progress has totals.
    let mut pieces = Vec::new();
    for id in &ids {
        let found = async {
            let meta = mover.info(&from, id).await?;
            if meta.is_dir {
                let items = mover.tree(&from, &meta.root_id, &meta.relative_path).await?;
                Ok::<_, NexusError>(Piece::Folder(meta, items))
            } else {
                Ok(Piece::File(meta))
            }
        }
        .await;
        match found {
            Ok(piece) => pieces.push(piece),
            Err(e) => job.fail_file(id, &e),
        }
    }
    let (files, bytes) = pieces.iter().fold((0u64, 0u64), |(f, b), p| match p {
        Piece::File(m) => (f + 1, b + m.size as u64),
        Piece::Folder(_, items) => items
            .iter()
            .filter(|i| !i.is_dir)
            .fold((f, b), |(f, b), i| (f + 1, b + i.size as u64)),
    });
    let first = pieces.first().map(|p| match p {
        Piece::File(m) | Piece::Folder(m, _) => m.filename.clone(),
    });
    job.update(|i| {
        i.files_total = files;
        i.bytes_total = bytes;
        if let (Some(name), 1) = (&first, ids.len()) {
            i.title = format!("{} {name}", if remove_originals { "Move" } else { "Copy" });
        }
    });

    let failed = Mutex::new(Vec::new());
    let copy_one = |src: Meta, parent: String| {
        let mover = &mover;
        let from = &from;
        let to = &to;
        let root_id = &root_id;
        let failed = &failed;
        async move {
            copy_file(mover, job, from, to, root_id, src, parent, failed).await;
        }
    };

    // Originals copied without a problem, for a move.
    let mut copied = Vec::new();
    for piece in pieces {
        if job.is_cancelled() {
            break;
        }
        let errors_before = job.error_count();
        let failed_before = failed.lock().unwrap().len();
        let original = match &piece {
            Piece::File(m) | Piece::Folder(m, _) => m.id.clone(),
        };
        match piece {
            Piece::File(meta) => copy_one(meta, path.clone()).await,
            Piece::Folder(meta, items) => {
                let top = match mover.mkdir_free(&to, &root_id, &path, &meta.filename).await {
                    Ok(name) => paths::join(&path, &name),
                    Err(e) => {
                        job.fail_file(&meta.filename, &e);
                        continue;
                    }
                };
                for item in items {
                    if job.is_cancelled() {
                        break;
                    }
                    let (parent, name) = paths::split_parent(&item.path);
                    let dest_parent = paths::join(&top, &parent);
                    if item.is_dir {
                        if let Err(e) = mover.mkdir(&to, &root_id, &dest_parent, &name).await {
                            job.fail_file(&item.path, &e);
                        }
                    } else {
                        copy_one(Meta::of_tree(&item, &meta.root_id, &meta.relative_path), dest_parent).await;
                    }
                }
            }
        }
        if !job.is_cancelled() && job.error_count() == errors_before && failed.lock().unwrap().len() == failed_before {
            copied.push(original);
        }
    }
    if remove_originals {
        for id in copied {
            if let Err(e) = mover.trash(&from, &id).await {
                job.fail_file(&id, &NexusError::internal(format!("copied, but the original stays: {e}")));
            }
        }
    }

    *job.retry.lock().unwrap() = Some(RetryPlan { from, to, root_id, files: failed.into_inner().unwrap() });
    finish_job(s, job);
    let what = match (&first, ids.len()) {
        (Some(name), 1) => name.clone(),
        _ => format!("{} items", ids.len()),
    };
    audit(s, user, "file.copied", Some(&format!("{what}: {}", job.route))).await;
}

/// Sends one file of a copy; a failure is recorded, with the file kept for
/// "Retry".
#[allow(clippy::too_many_arguments)]
async fn copy_file(
    mover: &Mover<'_>,
    job: &Arc<Job>,
    from: &Side,
    to: &Side,
    root_id: &str,
    src: Meta,
    parent: String,
    failed: &Mutex<Vec<(Meta, String)>>,
) {
    if !mover.s.jobs.checkpoint(mover.s, job).await {
        return;
    }
    job.update(|i| i.current = Some(src.filename.clone()));
    let req = NewUpload {
        root_id: root_id.to_string(),
        path: parent.clone(),
        name: src.filename.clone(),
        size: src.size as u64,
        on_conflict: None,
        modified_at: src.modified_at,
        make_parents: true,
    };
    match mover.send_file(job, from, &src, to, req).await {
        Ok(_) => job.update(|i| i.files_done += 1),
        Err(e) if job.is_cancelled() => drop(e),
        Err(e) => {
            job.set_bytes(job.bytes() + src.size as u64);
            job.fail_file(&src.filename, &e);
            failed.lock().unwrap().push((src, parent));
        }
    }
}

/// Sends the files an earlier copy could not, as a new job.
async fn run_retry(s: &AppState, user: &AuthUser, job: &Arc<Job>, plan: RetryPlan) {
    let Some(_slot) = s.jobs.start(s, job).await else { return };
    let mover = Mover { s, user };
    job.update(|i| {
        i.files_total = plan.files.len() as u64;
        i.bytes_total = plan.files.iter().map(|(m, _)| m.size as u64).sum();
    });
    let failed = Mutex::new(Vec::new());
    for (src, parent) in plan.files {
        if job.is_cancelled() {
            break;
        }
        copy_file(&mover, job, &plan.from, &plan.to, &plan.root_id, src, parent, &failed).await;
    }
    *job.retry.lock().unwrap() =
        Some(RetryPlan { from: plan.from, to: plan.to, root_id: plan.root_id, files: failed.into_inner().unwrap() });
    finish_job(s, job);
    audit(s, user, "file.copied", Some(&format!("retry: {}", job.route))).await;
}

// ---------------------------------------------------------------------------
// Job routes

pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<JobView>>> {
    user.require_person()?;
    Ok(Json(s.jobs.views(&user)))
}

pub async fn cancel(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::Write)?;
    let job = s
        .jobs
        .find(&id)
        .filter(|j| !user.is_limited() || j.by == user.username)
        .ok_or_else(|| NexusError::NotFound("Transfer".into()))?;
    job.cancel();
    // A waiting or paused job notices at once.
    s.jobs.wake.notify_waiters();
    s.events.emit(Event::TransferUpdated { id: job.id.clone(), status: "cancelling".into() });
    Ok(StatusCode::ACCEPTED)
}

fn own_job(s: &AppState, user: &AuthUser, id: &str) -> ApiResult<Arc<Job>> {
    user.require_person()?;
    user.require(Action::Write)?;
    Ok(s.jobs.find(id).filter(|j| !user.is_limited() || j.by == user.username).ok_or_else(|| NexusError::NotFound("Transfer".into()))?)
}

/// `POST /api/v1/transfers/{id}/pause`: lets go of the connection after the
/// current chunk; the job keeps its place and its progress.
pub async fn pause(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let job = own_job(&s, &user, &id)?;
    if !job.finished() {
        s.jobs.pause(&job, true);
        if job.info.lock().unwrap().status == "queued" {
            set_status(&s, &job, "paused");
        }
    }
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/v1/transfers/{id}/resume`
pub async fn resume(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let job = own_job(&s, &user, &id)?;
    s.jobs.pause(&job, false);
    if job.info.lock().unwrap().status == "paused" {
        set_status(&s, &job, "queued");
    }
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/v1/transfers/{id}/first`: runs this job next.
pub async fn first(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let job = own_job(&s, &user, &id)?;
    s.jobs.move_first(&job);
    s.events.emit(Event::TransferUpdated { id: job.id.clone(), status: job.info.lock().unwrap().status.clone() });
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/v1/transfers/{id}/retry`: sends the files a finished copy
/// could not, as a new job.
pub async fn retry(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<(StatusCode, Json<JobView>)> {
    let old = own_job(&s, &user, &id)?;
    if !old.finished() {
        return Err(NexusError::Conflict("This transfer is still running".into()).into());
    }
    let plan = old
        .retry
        .lock()
        .unwrap()
        .take()
        .filter(|p| !p.files.is_empty())
        .ok_or_else(|| NexusError::invalid("Nothing to retry"))?;
    let title = old.info.lock().unwrap().title.clone();
    let job = s.jobs.add("copy", format!("Retry: {}", title.trim_start_matches("Retry: ")), old.route.clone(), user.username.clone());
    let view = job.view();
    let state = s.clone();
    tokio::spawn(async move { run_retry(&state, &user, &job, plan).await });
    Ok((StatusCode::ACCEPTED, Json(view)))
}

#[derive(Serialize)]
pub struct SettingsView {
    #[serde(flatten)]
    settings: TransferSettings,
    can_configure: bool,
}

/// `GET /api/v1/transfers/settings`
pub async fn settings(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<SettingsView>> {
    user.require_person()?;
    Ok(Json(SettingsView { settings: s.jobs.settings(), can_configure: user.require(Action::ManageStorage).is_ok() }))
}

/// `PUT /api/v1/transfers/settings`
pub async fn set_settings(
    State(s): State<SharedState>,
    user: AuthUser,
    Json(body): Json<TransferSettings>,
) -> ApiResult<Json<SettingsView>> {
    user.require_person()?;
    user.require(Action::ManageStorage)?;
    let settings = body.validate()?;
    s.db.set_setting(SETTINGS_KEY, &serde_json::to_string(&settings).map_err(NexusError::internal)?).await?;
    s.jobs.apply(settings.clone());
    audit(&s, &user, "transfers.settings", None).await;
    Ok(Json(SettingsView { settings, can_configure: true }))
}

pub async fn clear(State(s): State<SharedState>, user: AuthUser) -> ApiResult<StatusCode> {
    user.require_person()?;
    s.jobs.clear_finished(&user);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hours(a: &str, b: &str) -> TransferSettings {
        TransferSettings { hours_start: Some(a.into()), hours_end: Some(b.into()), ..Default::default() }
    }

    #[test]
    fn transfer_hours() {
        assert!(TransferSettings::default().allows(0));
        let day = hours("09:00", "17:30");
        assert!(day.allows(9 * 60) && day.allows(17 * 60 + 29));
        assert!(!day.allows(17 * 60 + 30) && !day.allows(8 * 60 + 59));
        let night = hours("22:00", "06:00");
        assert!(night.allows(23 * 60) && night.allows(0) && night.allows(5 * 60 + 59));
        assert!(!night.allows(6 * 60) && !night.allows(12 * 60));
    }

    #[test]
    fn settings_are_checked() {
        assert!(hours("22:00", "06:00").validate().is_ok());
        assert!(hours("22:00", "22:00").validate().is_err());
        assert!(hours("25:00", "06:00").validate().is_err());
        assert!(TransferSettings { hours_start: Some("01:00".into()), ..Default::default() }.validate().is_err());
        assert!(TransferSettings { parallel: 0, ..Default::default() }.validate().is_err());
        assert!(TransferSettings { parallel: 5, ..Default::default() }.validate().is_err());
        let empty = TransferSettings { hours_start: Some(" ".into()), hours_end: Some(String::new()), ..Default::default() };
        assert_eq!(empty.validate().unwrap().hours_start, None);
    }

    #[tokio::test(start_paused = true)]
    async fn speed_limit_spaces_chunks() {
        let limit = RateLimit::default();
        limit.set(100 * 1024);
        let start = tokio::time::Instant::now();
        for _ in 0..5 {
            limit.take(50 * 1024).await;
        }
        // Five half-second chunks: the fifth starts after two seconds.
        assert_eq!(start.elapsed().as_millis(), 2000);
    }
}
