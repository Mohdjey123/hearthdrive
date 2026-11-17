use crate::net::{NetRuntime, Throttle};
use crate::peers::{JoinState, PeerClient};
use nexus_common::events::EventBus;
use nexus_database::Database;
use nexus_devices::{NodeIdentity, ReplayGuard};
use nexus_indexer::Indexer;
use nexus_security::TicketSigner;
use nexus_storage::StorageManager;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize)]
pub struct NodeInfo {
    pub device_id: String,
    pub version: &'static str,
    pub hostname: String,
}

pub struct AppState {
    pub db: Database,
    pub storage: StorageManager,
    pub indexer: Arc<Indexer>,
    pub events: EventBus,
    pub tickets: TicketSigner,
    /// Hash of the per-run token handed to the desktop shell.
    pub local_token: blake3::Hash,
    pub node: NodeInfo,
    pub data_dir: PathBuf,
    pub started_at: i64,
    /// This node's device key.
    pub identity: Arc<NodeIdentity>,
    /// The peer-to-peer endpoint and its published state.
    pub net: NetRuntime,
    /// Signs and sends requests to paired devices.
    pub peers: PeerClient,
    pub replay: ReplayGuard,
    /// Slows down password and pairing-code guessing.
    pub throttle: Throttle,
    /// Requests this node has sent to join another node, by request id.
    pub joins: Mutex<HashMap<String, JoinState>>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Database,
        storage: StorageManager,
        indexer: Arc<Indexer>,
        events: EventBus,
        local_token: blake3::Hash,
        node: NodeInfo,
        data_dir: PathBuf,
        identity: Arc<NodeIdentity>,
        net: NetRuntime,
    ) -> Self {
        let peers = PeerClient::new(identity.clone(), node.device_id.clone(), net.p2p.clone());
        Self {
            db,
            storage,
            indexer,
            events,
            tickets: TicketSigner::new(),
            local_token,
            node,
            data_dir,
            started_at: nexus_common::time::now_ms(),
            identity,
            net,
            peers,
            replay: ReplayGuard::default(),
            throttle: Throttle::default(),
            joins: Mutex::new(HashMap::new()),
        }
    }
}

pub type SharedState = Arc<AppState>;
