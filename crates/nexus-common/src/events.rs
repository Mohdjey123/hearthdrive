use serde::Serialize;
use tokio::sync::broadcast;

/// Live events pushed to connected clients over the `/api/v1/events` WebSocket.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "storage.changed")]
    StorageChanged { root_id: String },
    #[serde(rename = "storage.scan.started")]
    ScanStarted { root_id: String },
    #[serde(rename = "storage.scan.progress")]
    ScanProgress { root_id: String, files_seen: u64, bytes_seen: u64 },
    #[serde(rename = "storage.scan.completed")]
    ScanCompleted { root_id: String, files: u64, bytes: u64, removed: u64, duration_ms: u64 },
    #[serde(rename = "storage.scan.failed")]
    ScanFailed { root_id: String, error: String },
    #[serde(rename = "file.created")]
    FileCreated { root_id: String, path: String },
    #[serde(rename = "file.modified")]
    FileModified { root_id: String, path: String },
    #[serde(rename = "file.deleted")]
    FileDeleted { root_id: String, path: String },
    #[serde(rename = "file.moved")]
    FileMoved { root_id: String, from: String, to: String },
    /// A paired device was added, renamed or re-addressed.
    #[serde(rename = "device.changed")]
    DeviceChanged { device_id: String },
    #[serde(rename = "device.online")]
    DeviceOnline { device_id: String, latency_ms: i64 },
    #[serde(rename = "device.offline")]
    DeviceOffline { device_id: String },
    #[serde(rename = "device.removed")]
    DeviceRemoved { device_id: String },
    /// Someone used an invitation; waiting for approval on this node.
    #[serde(rename = "pairing.requested")]
    PairingRequested { request_id: String, name: String },
    /// Progress of a request this node sent to join another node.
    #[serde(rename = "pairing.updated")]
    PairingUpdated { id: String, status: String },
    #[serde(rename = "network.changed")]
    NetworkChanged,
    /// A copy or sync job started, finished or was cancelled.
    #[serde(rename = "transfer.updated")]
    TransferUpdated { id: String, status: String },
    /// A sync pair started or finished a run, or was changed.
    #[serde(rename = "sync.updated")]
    SyncUpdated { id: String },
    /// A backup (snapshot set) started or finished a run or check, or was changed.
    #[serde(rename = "snapshots.updated")]
    SnapshotsUpdated { id: String },
    /// An update arrived, or checking for one started or finished.
    #[serde(rename = "updates.changed")]
    UpdatesChanged,
    /// Drive health changed: disks read, a storage location went missing or
    /// came back, or a verification started or finished.
    #[serde(rename = "health.updated")]
    HealthUpdated,
}

impl Event {
    /// The storage location an event is about, if any: people limited to
    /// some locations only hear about those.
    pub fn root_id(&self) -> Option<&str> {
        match self {
            Event::StorageChanged { root_id }
            | Event::ScanStarted { root_id }
            | Event::ScanProgress { root_id, .. }
            | Event::ScanCompleted { root_id, .. }
            | Event::ScanFailed { root_id, .. }
            | Event::FileCreated { root_id, .. }
            | Event::FileModified { root_id, .. }
            | Event::FileDeleted { root_id, .. }
            | Event::FileMoved { root_id, .. } => Some(root_id),
            _ => None,
        }
    }
}

impl Event {
    /// The files an event is about, if any: people who see only some
    /// folders of a storage location hear about those.
    pub fn paths(&self) -> Vec<&str> {
        match self {
            Event::FileCreated { path, .. } | Event::FileModified { path, .. } | Event::FileDeleted { path, .. } => vec![path],
            Event::FileMoved { from, to, .. } => vec![from, to],
            _ => vec![],
        }
    }
}

/// In-process fan-out for [`Event`]s. Cheap to clone.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publishes an event. Having no subscribers is not an error.
    pub fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}
