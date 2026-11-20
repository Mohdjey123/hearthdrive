//! The transfers notification on Android: uploads, backups and files sent
//! from the share menu, with a progress bar while they run and a short note
//! when they end.
//!
//! The node runs in `NodeService`'s process. This watches its transfers
//! through its own API (`GET /api/v1/transfers`, with the local owner
//! token) and hands what to show to `NodeService.showTransfers` (Kotlin)
//! through JNI, only when that changes. It looks every second while
//! something runs and every few seconds otherwise.

use jni::JavaVM;
use jni::objects::{GlobalRef, JClass, JValue};
use nexus_common::runtime::RuntimeInfo;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

/// The VM and the `NodeService` class, kept from the JNI call that started
/// the node: a thread the node starts cannot look up the app's classes.
static TARGET: OnceLock<(JavaVM, GlobalRef)> = OnceLock::new();

/// Keeps what [`show`] needs to reach `NodeService.showTransfers`.
pub fn remember(vm: JavaVM, node_service: GlobalRef) {
    let _ = TARGET.set((vm, node_service));
}

const NONE: i32 = 0;
const RUNNING: i32 = 1;
const DONE: i32 = 2;
const FAILED: i32 = 3;

const BUSY: Duration = Duration::from_secs(1);
const IDLE: Duration = Duration::from_secs(4);

/// The kinds of job the notification shows: files leaving the phone.
const SHOWN: &[&str] = &["copy", "backup", "share"];

#[derive(Deserialize)]
struct Job {
    id: String,
    kind: String,
    title: String,
    status: String,
    files_total: u64,
    files_done: u64,
    bytes_total: u64,
    bytes_done: u64,
    #[serde(default)]
    errors: Vec<String>,
}

impl Job {
    fn active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running" | "paused")
    }

    fn failed(&self) -> bool {
        self.status == "failed" || !self.errors.is_empty()
    }
}

#[derive(PartialEq, Clone)]
struct Shown {
    title: String,
    text: String,
    done: u64,
    total: u64,
    state: i32,
}

/// Watches the node's transfers for as long as the process runs.
pub async fn watch(ready: tokio::sync::oneshot::Receiver<RuntimeInfo>) {
    let Ok(info) = ready.await else { return };
    let Ok(client) = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build() else { return };
    let url = format!("{}/api/v1/transfers", info.base_url);
    // Jobs seen running since the notification last showed nothing.
    let mut batch: HashSet<String> = HashSet::new();
    let mut last: Option<Shown> = None;
    loop {
        let jobs: Vec<Job> = match client.get(&url).bearer_auth(&info.local_token).send().await {
            Ok(res) if res.status().is_success() => res.json().await.unwrap_or_default(),
            _ => {
                tokio::time::sleep(IDLE).await;
                continue;
            }
        };
        let jobs: Vec<Job> = jobs.into_iter().filter(|j| SHOWN.contains(&j.kind.as_str())).collect();
        let active: Vec<&Job> = jobs.iter().filter(|j| j.active()).collect();
        let next = if !active.is_empty() {
            batch.extend(active.iter().map(|j| j.id.clone()));
            Some(running(&active))
        } else if !batch.is_empty() {
            let ended: Vec<&Job> = jobs.iter().filter(|j| batch.contains(&j.id)).collect();
            batch.clear();
            finished(&ended)
        } else {
            // Nothing new: a finished note stays until it times out or is swiped.
            last.clone().filter(|s| s.state != RUNNING)
        };
        if next != last {
            match &next {
                Some(s) => show(&s.title, &s.text, s.done, s.total, s.state),
                None => show("", "", 0, 0, NONE),
            }
            last = next;
        }
        tokio::time::sleep(if active.is_empty() { IDLE } else { BUSY }).await;
    }
}

fn running(active: &[&Job]) -> Shown {
    let files_done: u64 = active.iter().map(|j| j.files_done).sum();
    let files_total: u64 = active.iter().map(|j| j.files_total).sum();
    let done: u64 = active.iter().map(|j| j.bytes_done).sum();
    let total: u64 = active.iter().map(|j| j.bytes_total).sum();
    let title = if active.len() == 1 { active[0].title.clone() } else { format!("{} transfers", active.len()) };
    let mut text = if files_total > 0 {
        format!("{files_done} of {files_total} file{}", if files_total == 1 { "" } else { "s" })
    } else {
        "Starting…".to_string()
    };
    if total > 0 {
        text.push_str(&format!(" · {} of {}", bytes(done), bytes(total)));
    }
    if active.iter().all(|j| j.status == "queued" || j.status == "paused") {
        text = "Waiting to start".into();
    }
    Shown { title, text, done, total, state: RUNNING }
}

/// What to say when the jobs in a batch ended; nothing when they all
/// disappeared (cleared) or were cancelled.
fn finished(ended: &[&Job]) -> Option<Shown> {
    let ended: Vec<&&Job> = ended.iter().filter(|j| j.status != "cancelled").collect();
    if ended.is_empty() {
        return None;
    }
    let files: u64 = ended.iter().map(|j| j.files_done).sum();
    let failed: Vec<&&&Job> = ended.iter().filter(|j| j.failed()).collect();
    let files_text = format!("{files} file{} sent", if files == 1 { "" } else { "s" });
    Some(if failed.is_empty() {
        let title = if ended.len() == 1 { ended[0].title.clone() } else { format!("{} transfers finished", ended.len()) };
        Shown { title, text: files_text, done: 0, total: 0, state: DONE }
    } else {
        let errors: usize = failed.iter().map(|j| j.errors.len().max(1)).sum();
        let title = if ended.len() == 1 { format!("{} did not finish", ended[0].title) } else { "Some transfers did not finish".into() };
        let text = format!("{files_text}, {errors} failed. Open Transfers to retry.");
        Shown { title, text, done: 0, total: 0, state: FAILED }
    })
}

fn bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

/// Calls `NodeService.showTransfers(title, text, done, total, state)`.
fn show(title: &str, text: &str, done: u64, total: u64, state: i32) {
    let Some((vm, class)) = TARGET.get() else { return };
    let Ok(mut env) = vm.attach_current_thread_permanently() else { return };
    let (Ok(title), Ok(text)) = (env.new_string(title), env.new_string(text)) else { return };
    let class: &JClass = class.as_obj().into();
    let _ = env.call_static_method(
        class,
        "showTransfers",
        "(Ljava/lang/String;Ljava/lang/String;JJI)V",
        &[
            JValue::Object(&title),
            JValue::Object(&text),
            JValue::Long(done.min(i64::MAX as u64) as i64),
            JValue::Long(total.min(i64::MAX as u64) as i64),
            JValue::Int(state),
        ],
    );
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    let _ = env.delete_local_ref(title);
    let _ = env.delete_local_ref(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(status: &str, done: u64, total: u64, errors: usize) -> Job {
        Job {
            id: "1".into(),
            kind: "copy".into(),
            title: "Send 3 files".into(),
            status: status.into(),
            files_total: 3,
            files_done: done,
            bytes_total: total,
            bytes_done: total / 2,
            errors: vec!["x".into(); errors],
        }
    }

    #[test]
    fn running_and_finished_text() {
        let j = job("running", 1, 2_000_000, 0);
        let s = running(&[&j]);
        assert_eq!(s.text, "1 of 3 files · 1.0 MB of 2.0 MB");
        let done = job("done", 3, 10, 0);
        assert_eq!(finished(&[&done]).unwrap().state, DONE);
        let failed = job("done", 2, 10, 1);
        let f = finished(&[&failed]).unwrap();
        assert_eq!(f.state, FAILED);
        assert_eq!(f.text, "2 files sent, 1 failed. Open Transfers to retry.");
        assert!(finished(&[&job("cancelled", 0, 0, 0)]).is_none());
    }
}
