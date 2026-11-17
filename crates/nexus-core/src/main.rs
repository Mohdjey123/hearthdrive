//! `nexus-service`: the background storage node.
//!
//! Runs independently of the desktop window: closing the UI never stops the
//! storage server.

use nexus_core::{Config, run};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> anyhow::Result<()> {
    let config = Config::from_env_and_args()?;
    let log_dir = config.data_dir.join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let (file_writer, _guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&log_dir, "nexus-service.log"));

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn,notify=warn")))
        .with(fmt::layer().with_target(false))
        .with(fmt::layer().with_ansi(false).with_writer(file_writer))
        .init();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(panic = %info, "nexus-service panicked");
        default_hook(info);
    }));

    tracing::info!(version = nexus_common::VERSION, data_dir = %config.data_dir.display(), "starting nexus-service");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("nexus")
        .build()?
        .block_on(run(config))
        .inspect_err(|e| tracing::error!(error = %format!("{e:#}"), "nexus-service failed"))
}
