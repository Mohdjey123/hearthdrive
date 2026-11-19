//! A feed of which storage locations changed, for paired devices that show
//! this node's files (the virtual drive): they ask with the last number they
//! saw and the request waits until something changes, so changes arrive in
//! seconds instead of on the next check.

use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{Query, State};
use nexus_common::events::Event;
use nexus_security::Action;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Changes remembered; a device further behind starts over.
const KEEP: usize = 4096;
/// Longest a request waits (below the peer proxy's time limit).
const MAX_WAIT: u64 = 15;

pub struct ChangeLog {
    seq: AtomicU64,
    recent: Mutex<VecDeque<(u64, String)>>,
    notify: tokio::sync::Notify,
    /// The newest change forgotten (too old to keep).
    dropped: AtomicU64,
    /// Where numbering started this run: a device that saw numbers from an
    /// earlier run starts over.
    epoch: u64,
}

impl Default for ChangeLog {
    fn default() -> Self {
        Self {
            seq: AtomicU64::new(0),
            recent: Mutex::new(VecDeque::new()),
            notify: tokio::sync::Notify::new(),
            dropped: AtomicU64::new(0),
            epoch: nexus_common::time::now_ms() as u64,
        }
    }
}

impl ChangeLog {
    pub fn record(&self, root_id: &str) {
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut recent = self.recent.lock().unwrap();
        if recent.back().is_some_and(|(_, r)| r == root_id) {
            // Same location again: move it up instead of repeating it.
            recent.pop_back();
        }
        recent.push_back((n, root_id.to_string()));
        while recent.len() > KEEP {
            if let Some((n, _)) = recent.pop_front() {
                self.dropped.store(n, Ordering::SeqCst);
            }
        }
        drop(recent);
        self.notify.notify_waiters();
    }

    fn since(&self, since: u64) -> (u64, Option<Vec<String>>) {
        let now = self.seq.load(Ordering::SeqCst);
        let recent = self.recent.lock().unwrap();
        if since < self.dropped.load(Ordering::SeqCst) || since > now {
            return (now, None);
        }
        let mut roots: Vec<String> = recent.iter().filter(|(n, _)| *n > since).map(|(_, r)| r.clone()).collect();
        roots.sort();
        roots.dedup();
        (now, Some(roots))
    }
}

/// Records every change to files and storage locations.
pub fn spawn(s: SharedState) {
    let mut events = s.events.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(e) => {
                    let counts = matches!(
                        e,
                        Event::FileCreated { .. }
                            | Event::FileModified { .. }
                            | Event::FileDeleted { .. }
                            | Event::FileMoved { .. }
                            | Event::StorageChanged { .. }
                            | Event::ScanCompleted { .. }
                    );
                    if counts && let Some(root) = e.root_id() {
                        s.changes.record(root);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => s.changes.record("*"),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

#[derive(Deserialize)]
pub struct ChangesQuery {
    /// `epoch.number` from the last answer; empty to start.
    since: Option<String>,
    /// Seconds to wait for a change.
    wait: Option<u64>,
}

#[derive(Serialize)]
pub struct ChangesReply {
    /// Pass back as `since`.
    cursor: String,
    /// Storage locations that changed. `*` or `reset`: look at everything.
    roots: Vec<String>,
    reset: bool,
}

/// `GET /storage/changes`: storage locations changed since `since`, waiting
/// up to `wait` seconds for one.
pub async fn changes(
    State(s): State<SharedState>,
    user: AuthUser,
    Query(q): Query<ChangesQuery>,
) -> ApiResult<Json<ChangesReply>> {
    user.require(Action::Read)?;
    let log = &s.changes;
    let since = q
        .since
        .as_deref()
        .and_then(|c| c.split_once('.'))
        .filter(|(epoch, _)| epoch.parse::<u64>().ok() == Some(log.epoch))
        .and_then(|(_, n)| n.parse::<u64>().ok());
    let cursor = |n: u64| format!("{}.{n}", log.epoch);
    let Some(since) = since else {
        let now = log.seq.load(Ordering::SeqCst);
        return Ok(Json(ChangesReply { cursor: cursor(now), roots: vec![], reset: q.since.is_some() }));
    };
    let wait = Duration::from_secs(q.wait.unwrap_or(0).min(MAX_WAIT));
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = log.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let (now, roots) = log.since(since);
        let Some(mut roots) = roots else {
            return Ok(Json(ChangesReply { cursor: cursor(now), roots: vec![], reset: true }));
        };
        roots.retain(|r| r == "*" || user.sees_any(r));
        if !roots.is_empty() || tokio::time::Instant::now() >= deadline {
            let reset = roots.iter().any(|r| r == "*");
            return Ok(Json(ChangesReply { cursor: cursor(now), roots, reset }));
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            let (now, _) = log.since(since);
            return Ok(Json(ChangesReply { cursor: cursor(now), roots: vec![], reset: false }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_changed_locations() {
        let log = ChangeLog::default();
        log.record("a");
        log.record("b");
        log.record("a");
        assert_eq!(log.since(0).1.unwrap().len(), 2);
        let (now, roots) = log.since(1);
        assert_eq!(now, 3);
        assert_eq!(roots.unwrap(), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(log.since(3).1.unwrap(), Vec::<String>::new());
    }
}
