//! Node lifecycle: open the database, start indexing, serve the API.

use anyhow::Context;
use nexus_api::net::{NetRuntime, REMOTE_ENABLED_SETTING};
use nexus_api::{AppState, NodeInfo, SharedState};
use nexus_common::events::EventBus;
use nexus_common::runtime::{DEFAULT_PORT, RUNTIME_FILE, RuntimeInfo, default_data_dir};
use nexus_common::time::now_ms;
use nexus_database::Database;
use nexus_indexer::Indexer;
use nexus_devices::NodeIdentity;
use nexus_network::{P2pConfig, P2pNode};
use nexus_security::generate_token;
use nexus_storage::StorageManager;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub port: u16,
    /// Built web UI to serve for browser access (optional).
    pub ui_dir: Option<PathBuf>,
}

impl Config {
    /// Reads `--data-dir`, `--port`, `--ui-dir` (or `NEXUS_DATA_DIR`,
    /// `NEXUS_PORT`, `NEXUS_UI_DIR`), falling back to sensible defaults.
    pub fn from_env_and_args() -> anyhow::Result<Self> {
        let mut data_dir = default_data_dir();
        let mut port = std::env::var("NEXUS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
        let mut ui_dir = std::env::var_os("NEXUS_UI_DIR").map(PathBuf::from);

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-dir" => data_dir = PathBuf::from(args.next().context("--data-dir needs a value")?),
                "--port" => port = args.next().context("--port needs a value")?.parse().context("invalid port")?,
                "--ui-dir" => ui_dir = Some(PathBuf::from(args.next().context("--ui-dir needs a value")?)),
                "--version" | "-V" => {
                    println!("nexus-service {}", nexus_common::VERSION);
                    std::process::exit(0);
                }
                "--help" | "-h" => {
                    println!("nexus-service [--data-dir DIR] [--port PORT] [--ui-dir DIR]");
                    std::process::exit(0);
                }
                other => anyhow::bail!("unknown argument: {other}"),
            }
        }
        Ok(Self { data_dir, port, ui_dir: ui_dir.or_else(locate_ui_dir) })
    }
}

/// `<exe dir>/ui` for installed builds; `apps/desktop/dist` when running
/// from a development checkout.
fn locate_ui_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let installed = exe_dir.join("ui");
    if installed.join("index.html").is_file() {
        return Some(installed);
    }
    exe_dir
        .ancestors()
        .map(|a| a.join("apps").join("desktop").join("dist"))
        .find(|p| p.join("index.html").is_file())
}

fn hostname() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "This computer".into())
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data directory {}", config.data_dir.display()))?;
    let db = Database::open(&config.data_dir.join("nexus.db")).await.context("opening database")?;

    let host = hostname();
    let device = db.ensure_local_device(&host, std::env::consts::OS).await?;
    db.touch_device(&device.id).await?;
    let purged = db.purge_expired_sessions().await?;
    tracing::info!(device = %device.name, purged_sessions = purged, "node ready");

    let identity = Arc::new(NodeIdentity::load_or_create(&config.data_dir).context("loading device identity")?);
    db.set_local_identity(&device.id, identity.public_key(), nexus_common::VERSION).await?;
    tracing::info!(fingerprint = identity.fingerprint(), "device identity");

    // The HTTP API listens on loopback only. Other devices reach it through
    // the peer-to-peer endpoint, which serves a restricted part of it.
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config.port);
    let listener = tokio::net::TcpListener::bind(address).await.with_context(|| {
        format!("port {} is in use — is NEXUS already running?", config.port)
    })?;

    // `NEXUS_NETWORK=local` keeps the endpoint on loopback without relays:
    // only other nodes on this computer can connect (development).
    let p2p_config = match std::env::var("NEXUS_NETWORK").as_deref() {
        Ok("local") => P2pConfig::loopback(),
        _ => P2pConfig::from_env(config.port),
    };
    let p2p = Arc::new(
        P2pNode::start(identity.secret_bytes(), p2p_config)
            .await
            .map_err(|e| anyhow::anyhow!("starting the peer-to-peer endpoint: {e}"))?,
    );
    let remote_enabled = db.get_setting(REMOTE_ENABLED_SETTING).await?.map(|v| v != "0").unwrap_or(true);
    let net = NetRuntime::new(p2p.clone(), remote_enabled);

    let events = EventBus::default();
    let storage = StorageManager::new(db.clone(), device.id.clone(), vec![config.data_dir.clone()]);
    let indexer = Indexer::new(db.clone(), events.clone(), vec![config.data_dir.clone()]);
    indexer.start();
    for root in db.list_roots().await?.into_iter().filter(|r| r.device_id == device.id && r.enabled) {
        storage.clean_staging(&root).await;
        indexer.register_root(&root);
    }

    let local_token = generate_token();
    let state: SharedState = Arc::new(AppState::new(
        db.clone(),
        storage,
        indexer,
        events,
        blake3::hash(local_token.as_bytes()),
        NodeInfo { device_id: device.id.clone(), version: nexus_common::VERSION, hostname: host },
        config.data_dir.clone(),
        identity,
        net,
    ));
    if let Some(ui) = &config.ui_dir {
        tracing::info!(path = %ui.display(), "serving web interface");
    }
    let app = nexus_api::router(state.clone(), config.ui_dir.clone());
    nexus_api::p2p::spawn_server(state.clone(), app.clone());
    nexus_api::net::spawn_network_monitor(state.clone());
    nexus_api::peers::spawn_peer_monitor(state.clone());

    let runtime_path = config.data_dir.join(RUNTIME_FILE);
    write_runtime(&runtime_path, &RuntimeInfo {
        pid: std::process::id(),
        port: address.port(),
        base_url: format!("http://{address}"),
        local_token,
        started_at: now_ms(),
    })?;
    tracing::info!(%address, "NEXUS service listening");

    let served = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await;

    let _ = std::fs::remove_file(&runtime_path);
    p2p.close().await;
    db.close().await;
    served.context("API server stopped unexpectedly")
}

fn write_runtime(path: &Path, info: &RuntimeInfo) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(info)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}
