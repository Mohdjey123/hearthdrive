// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! NEXUS Cloud desktop shell.
//!
//! The window is only a client: storage, indexing and the API live in the
//! separate `nexus-service` process, which keeps running when the window
//! closes. This shell finds (or starts) that service and hands the UI its
//! connection details.

use nexus_common::runtime::{RUNTIME_FILE, RuntimeInfo, default_data_dir};
use serde::Serialize;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Serialize)]
struct Connection {
    base_url: String,
    token: String,
    data_dir: String,
}

fn read_live_runtime(data_dir: &Path) -> Option<RuntimeInfo> {
    let raw = std::fs::read(data_dir.join(RUNTIME_FILE)).ok()?;
    let info: RuntimeInfo = serde_json::from_slice(&raw).ok()?;
    let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok()?;
    Some(info)
}

fn service_executable() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) { "nexus-service.exe" } else { "nexus-service" };
    let candidate = exe.with_file_name(name);
    if candidate.is_file() {
        Ok(candidate)
    } else {
        Err(format!(
            "The NEXUS background service was not found at {}. Build it with `cargo build -p nexus-core`.",
            candidate.display()
        ))
    }
}

fn spawn_service() -> Result<(), String> {
    let mut command = Command::new(service_executable()?);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    command.spawn().map(|_| ()).map_err(|e| format!("Could not start the NEXUS service: {e}"))
}

fn connect() -> Result<Connection, String> {
    let data_dir = default_data_dir();
    let to_connection = |info: RuntimeInfo| Connection {
        base_url: info.base_url,
        token: info.local_token,
        data_dir: data_dir.to_string_lossy().into_owned(),
    };
    if let Some(info) = read_live_runtime(&data_dir) {
        return Ok(to_connection(info));
    }
    spawn_service()?;
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(250));
        if let Some(info) = read_live_runtime(&data_dir) {
            return Ok(to_connection(info));
        }
    }
    Err(format!(
        "The NEXUS service did not start in time. Its logs are in {}",
        data_dir.join("logs").display()
    ))
}

/// Returns the running service's address and local owner token, starting
/// the service first if needed.
#[tauri::command]
async fn service_connection() -> Result<Connection, String> {
    tauri::async_runtime::spawn_blocking(connect)
        .await
        .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![service_connection])
        .run(tauri::generate_context!())
        .expect("error while running NEXUS Cloud");
}
