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
    /// Initial device name. Defaults to the computer's host name.
    pub device_name: Option<String>,
    /// Phones: the shared storage photos are backed up from
    /// (`NEXUS_MEDIA_DIR` elsewhere, for testing).
    pub media_dir: Option<PathBuf>,
    /// Phones: where files shared to the app wait (`NEXUS_SHARE_INBOX`).
    pub share_inbox: Option<PathBuf>,
    /// Only the host app (holding the local token) may complete first-run
    /// setup. On for phones, where every app can reach the loopback port.
    pub setup_needs_token: bool,
}

impl Config {
    /// A node hosted inside an app (the Android shell): any free loopback
    /// port, no web UI, data in the app's private directory.
    pub fn embedded(data_dir: PathBuf, device_name: &str) -> Self {
        Self {
            data_dir,
            port: 0,
            ui_dir: None,
            device_name: Some(device_name.to_string()),
            media_dir: None,
            share_inbox: None,
            setup_needs_token: true,
        }
    }
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
        Ok(Self {
            data_dir,
            port,
            ui_dir: ui_dir.or_else(locate_ui_dir),
            device_name: None,
            media_dir: std::env::var_os("NEXUS_MEDIA_DIR").map(PathBuf::from),
            share_inbox: std::env::var_os("NEXUS_SHARE_INBOX").map(PathBuf::from),
            setup_needs_token: false,
        })
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

/// Sets up logging to the console and to daily files in `<data_dir>/logs`.
/// Keep the returned guard alive for as long as the process logs.
pub fn init_logging(data_dir: &Path) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let log_dir = data_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let (file_writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&log_dir, "nexus-service.log"));
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,notify=warn")))
        .with(fmt::layer().with_target(false))
        .with(fmt::layer().with_ansi(false).with_writer(file_writer))
        .try_init()
        .map_err(|e| anyhow::anyhow!("logging is already set up: {e}"))?;
    Ok(guard)
}

/// Runs the node until the process is asked to stop.
pub async fn run(config: Config) -> anyhow::Result<()> {
    run_with(config, None).await
}

/// Like [`run`], and reports the API address and local owner token through
/// `ready` once the node is listening (for a host app that embeds the node).
pub async fn run_with(
    config: Config,
    ready: Option<tokio::sync::oneshot::Sender<RuntimeInfo>>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data directory {}", config.data_dir.display()))?;
    let db = Database::open(&config.data_dir.join("nexus.db")).await.context("opening database")?;

    let host = config.device_name.clone().unwrap_or_else(hostname);
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
        format!("port {} is in use — is HearthDrive already running?", config.port)
    })?;
    let address = listener.local_addr()?;

    // `NEXUS_NETWORK=local` keeps the endpoint on loopback without relays:
    // only other nodes on this computer can connect (development).
    let p2p_config = match std::env::var("NEXUS_NETWORK").as_deref() {
        Ok("local") => P2pConfig::loopback(),
        _ => P2pConfig::from_env(address.port()),
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
    )
    .with_phone_dirs(config.media_dir.clone(), config.share_inbox.clone())
    .with_setup_token_required(config.setup_needs_token));
    if let Some(ui) = &config.ui_dir {
        tracing::info!(path = %ui.display(), "serving web interface");
    }
    let app = nexus_api::router(state.clone(), config.ui_dir.clone());
    nexus_api::p2p::spawn_server(state.clone(), app.clone());
    nexus_api::net::spawn_network_monitor(state.clone());
    nexus_api::peers::spawn_peer_monitor(state.clone());
    nexus_api::nearby::spawn(state.clone());
    nexus_api::media::spawn(state.clone());
    nexus_api::documents::spawn(state.clone());
    nexus_api::automation::spawn(state.clone());
    nexus_api::programs::spawn(state.clone());
    nexus_api::spawn_sync(state.clone());
    nexus_api::spawn_backup(state.clone());
    nexus_api::spawn_shares(state.clone());
    nexus_api::spawn_transfers(state.clone());
    nexus_api::spawn_changes(state.clone());
    nexus_api::spawn_snapshots(state.clone());
    nexus_api::spawn_health(state.clone());
    nexus_api::spawn_updates(state.clone());
    nexus_api::spawn_introductions(state.clone());
    start_virtual_drive(&state, &app, &local_token);

    // Keep the database's write-ahead log from growing without bound.
    let checkpoint_db = db.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            if let Err(e) = checkpoint_db.checkpoint().await {
                tracing::debug!(error = %e, "database checkpoint skipped");
            }
        }
    });

    let runtime_path = config.data_dir.join(RUNTIME_FILE);
    let exe = std::env::current_exe().ok();
    let runtime = RuntimeInfo {
        pid: std::process::id(),
        port: address.port(),
        base_url: format!("http://{address}"),
        local_token,
        started_at: now_ms(),
        exe_modified: exe.as_deref().and_then(nexus_common::runtime::program_stamp),
        exe: exe.map(|p| p.to_string_lossy().into_owned()),
    };
    write_runtime(&runtime_path, &runtime)?;
    tracing::info!(%address, "HearthDrive service listening");
    if let Some(ready) = ready {
        let _ = ready.send(runtime);
    }

    let served = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await;

    let _ = std::fs::remove_file(&runtime_path);
    p2p.close().await;
    db.close().await;
    served.context("API server stopped unexpectedly")
}

/// Connects the virtual drive to this node (it reads devices and files
/// through the node's own API, as its owner, like the app does) and turns it
/// on if it was on.
fn start_virtual_drive(state: &SharedState, app: &axum::Router, local_token: &str) {
    use axum::extract::ConnectInfo;
    use tower::ServiceExt;
    let app = app.clone();
    let send: nexus_vdrive::client::Responder = Arc::new(move |mut req| {
        let app = app.clone();
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)));
        Box::pin(async move {
            match app.oneshot(req).await {
                Ok(res) => res,
                Err(never) => match never {},
            }
        })
    });
    state.vdrive.attach(nexus_vdrive::Context {
        client: nexus_vdrive::client::NodeClient::new(send, local_token),
        events: state.events.clone(),
        data_dir: state.data_dir.clone(),
        account: state.node.device_id.clone(),
        runtime: tokio::runtime::Handle::current(),
    });
    if !nexus_vdrive::VirtualDrive::supported() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        state.vdrive.set_cache(nexus_api::saved_vdrive_cache(&state).await);
        let (enabled, path) = match nexus_api::saved_vdrive(&state).await {
            Ok(saved) => saved,
            Err(e) => {
                tracing::warn!(error = %e, "could not read the virtual drive setting");
                return;
            }
        };
        if let Err(e) = state.vdrive.apply(enabled, path.clone()).await {
            state.vdrive.remember(enabled, path, Some(e));
        }
    });
}

/// Writes `runtime.json`, which holds the owner-level local token. The data
/// folder is private to this account on Windows; elsewhere the file is made
/// readable by its owner only.
fn write_runtime(path: &Path, info: &RuntimeInfo) -> anyhow::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(&tmp)?, &serde_json::to_vec_pretty(info)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

async fn shutdown_signal() {
    // Without a console (the Windows service) there may be no Ctrl+C to
    // wait for: then run until stopped.
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
    tracing::info!("shutting down");
}
