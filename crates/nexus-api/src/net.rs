//! Network state shared by the API, the peer-to-peer server and the monitor.

use crate::state::SharedState;
use nexus_common::NexusError;
use nexus_common::events::Event;
use nexus_common::time::now_ms;
use nexus_network::{NetworkStatus, P2pNode};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::{Notify, watch};

pub const REMOTE_ENABLED_SETTING: &str = "network.remote_enabled";

#[derive(Debug, Clone, Serialize)]
pub struct NetSnapshot {
    pub status: NetworkStatus,
    /// The owner's switch for remote access (on by default).
    pub remote_enabled: bool,
    /// How other devices can reach this node (`relay:<url>` or `ip:port`).
    pub advertised: Vec<String>,
    pub updated_at: i64,
    /// Increments on every refresh; lets callers wait for a fresh status.
    pub generation: u64,
}

pub struct NetRuntime {
    pub p2p: Arc<P2pNode>,
    snapshot: RwLock<NetSnapshot>,
    enabled: watch::Sender<bool>,
    /// Wakes the network monitor to re-check now.
    pub wake: Notify,
    /// Wakes the peer monitor to contact paired devices now.
    pub peer_wake: Notify,
    /// Set once the network monitor is running and publishing updates.
    pub managed: AtomicBool,
}

impl NetRuntime {
    pub fn new(p2p: Arc<P2pNode>, remote_enabled: bool) -> Self {
        let snapshot = NetSnapshot {
            status: p2p.status(remote_enabled),
            remote_enabled,
            advertised: p2p.hints(),
            updated_at: now_ms(),
            generation: 0,
        };
        Self {
            p2p,
            snapshot: RwLock::new(snapshot),
            enabled: watch::Sender::new(remote_enabled),
            wake: Notify::new(),
            peer_wake: Notify::new(),
            managed: AtomicBool::new(false),
        }
    }

    pub fn snapshot(&self) -> NetSnapshot {
        self.snapshot.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn advertised(&self) -> Vec<String> {
        self.p2p.hints()
    }

    pub fn remote_enabled(&self) -> bool {
        *self.enabled.borrow()
    }

    /// Watches the remote-access switch (the peer-to-peer server closes
    /// incoming connections when it turns off).
    pub fn watch_enabled(&self) -> watch::Receiver<bool> {
        self.enabled.subscribe()
    }

    /// The HTTP API listens on loopback only, so only loopback names are
    /// valid hosts. Peer-to-peer requests are marked and checked separately.
    pub fn host_allowed(&self, host: &str) -> bool {
        let host = match host.rsplit_once(':') {
            Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
            _ => host,
        };
        matches!(host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1")
    }

    /// Re-reads the endpoint's state. Returns true if anything visible changed.
    pub fn refresh(&self, remote_enabled: bool) -> bool {
        self.enabled.send_if_modified(|v| std::mem::replace(v, remote_enabled) != remote_enabled);
        let status = self.p2p.status(remote_enabled);
        let advertised = self.p2p.hints();
        let mut snap = self.snapshot.write().unwrap_or_else(|e| e.into_inner());
        let changed = snap.status != status || snap.remote_enabled != remote_enabled || snap.advertised != advertised;
        snap.status = status;
        snap.remote_enabled = remote_enabled;
        snap.advertised = advertised;
        snap.updated_at = now_ms();
        snap.generation += 1;
        changed
    }

    /// Waits (briefly) for the monitor to publish a newer state.
    pub async fn wait_for_update(&self, after_generation: u64, timeout: Duration) {
        if !self.managed.load(Ordering::Relaxed) {
            return;
        }
        let deadline = tokio::time::Instant::now() + timeout;
        while self.snapshot().generation <= after_generation && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

pub async fn remote_enabled_setting(state: &SharedState) -> bool {
    state.db.get_setting(REMOTE_ENABLED_SETTING).await.ok().flatten().map(|v| v != "0").unwrap_or(true)
}

/// Keeps the published network state current: every 10 seconds, when the
/// endpoint's addresses or relay connection change, and on request.
pub fn spawn_network_monitor(state: SharedState) {
    state.net.managed.store(true, Ordering::Relaxed);
    tokio::spawn(async move {
        let mut last_state = None;
        loop {
            let enabled = remote_enabled_setting(&state).await;
            if state.net.refresh(enabled) {
                let snap = state.net.snapshot();
                if last_state != Some(snap.status.state) {
                    tracing::info!(state = ?snap.status.state, relay = ?snap.status.relay_url, "network state");
                    last_state = Some(snap.status.state);
                }
                state.events.emit(Event::NetworkChanged);
                state.net.peer_wake.notify_one();
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(10)) => {}
                _ = state.net.wake.notified() => {}
                _ = state.net.p2p.changed() => {
                    // Address changes come in bursts; let them settle.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    });
}

/// Counts failed password and pairing-code attempts per address.
#[derive(Default)]
pub struct Throttle {
    failures: Mutex<HashMap<IpAddr, (u32, i64)>>,
}

const MAX_FAILURES: u32 = 10;
const WINDOW_MS: i64 = 10 * 60 * 1000;

impl Throttle {
    pub fn check(&self, ip: IpAddr) -> Result<(), NexusError> {
        let map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(&ip) {
            Some((count, since)) if *count >= MAX_FAILURES && now_ms() - since < WINDOW_MS => Err(NexusError::RateLimited),
            _ => Ok(()),
        }
    }

    pub fn fail(&self, ip: IpAddr) {
        let now = now_ms();
        let mut map = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 10_000 {
            map.retain(|_, (_, since)| now - *since < WINDOW_MS);
        }
        let entry = map.entry(ip).or_insert((0, now));
        if now - entry.1 >= WINDOW_MS {
            *entry = (0, now);
        }
        entry.0 += 1;
    }

    pub fn clear(&self, ip: IpAddr) {
        self.failures.lock().unwrap_or_else(|e| e.into_inner()).remove(&ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttle_blocks_after_repeated_failures() {
        let t = Throttle::default();
        let ip: IpAddr = "fd00::9".parse().unwrap();
        for _ in 0..MAX_FAILURES {
            assert!(t.check(ip).is_ok());
            t.fail(ip);
        }
        assert!(t.check(ip).is_err());
        t.clear(ip);
        assert!(t.check(ip).is_ok());
    }
}
