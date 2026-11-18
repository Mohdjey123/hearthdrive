//! `nexus-service`: the background storage node.
//!
//! Runs independently of the desktop window: closing the UI never stops the
//! storage server.
//!
//! Release builds on Windows have no console window: the service is started
//! by the app, at sign-in and by the installer, and logs to files.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use nexus_core::{Config, init_logging, run};

fn main() -> anyhow::Result<()> {
    let config = Config::from_env_and_args()?;
    let _guard = init_logging(&config.data_dir)?;

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
