//! NEXUS Cloud app shell.
//!
//! **Desktop.** The window is only a client: storage, indexing and the API
//! live in the separate `nexus-service` process, which keeps running when
//! the window closes. The shell finds (or starts) that service and hands the
//! UI its connection details.
//!
//! **Phones.** There is no separate service, so the app hosts the same node
//! in its own process, with its data in the app's private directory. The
//! node runs while the app is open.

use serde::Serialize;

#[derive(Clone, Serialize)]
struct Connection {
    base_url: String,
    token: String,
    data_dir: String,
    /// `desktop` or `mobile`: lets the UI pick its layout and setup flow.
    shell: &'static str,
}

#[cfg(desktop)]
mod host {
    use super::Connection;
    use nexus_common::runtime::{RUNTIME_FILE, RuntimeInfo, default_data_dir};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

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
            shell: "desktop",
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

    pub async fn connection(_app: tauri::AppHandle) -> Result<Connection, String> {
        tauri::async_runtime::spawn_blocking(connect).await.map_err(|e| e.to_string())?
    }

    /// Desktop web views save downloads themselves.
    pub async fn save_download(_app: tauri::AppHandle, _url: String, _filename: String) -> Result<String, String> {
        Err("Downloads are saved by the browser on this platform".into())
    }
}

#[cfg(mobile)]
mod host {
    use super::Connection;
    use nexus_core::Config;
    use std::path::PathBuf;
    use tauri::Manager;
    use tokio::sync::OnceCell;

    /// The node embedded in this app, started on first use.
    static NODE: OnceCell<Result<Connection, String>> = OnceCell::const_new();

    async fn start(data_dir: PathBuf) -> Result<Connection, String> {
        std::fs::create_dir_all(&data_dir).map_err(|e| format!("Could not create the app's data folder: {e}"))?;
        // The guard must outlive the node; the node lives as long as the app.
        if let Ok(guard) = nexus_core::init_logging(&data_dir) {
            std::mem::forget(guard);
        }
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (failed_tx, failed_rx) = tokio::sync::oneshot::channel::<String>();
        let config = Config::embedded(data_dir.clone(), "My phone");
        std::thread::Builder::new()
            .name("nexus-node".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("nexus").build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = failed_tx.send(e.to_string());
                        return;
                    }
                };
                if let Err(e) = runtime.block_on(nexus_core::run_with(config, Some(ready_tx))) {
                    tracing::error!(error = %format!("{e:#}"), "embedded node stopped");
                    let _ = failed_tx.send(format!("{e:#}"));
                }
            })
            .map_err(|e| e.to_string())?;

        match ready_rx.await {
            Ok(info) => Ok(Connection {
                base_url: info.base_url,
                token: info.local_token,
                data_dir: data_dir.to_string_lossy().into_owned(),
                shell: "mobile",
            }),
            Err(_) => Err(match failed_rx.await {
                Ok(e) => format!("NEXUS could not start: {e}"),
                Err(_) => "NEXUS could not start".into(),
            }),
        }
    }

    pub async fn connection(app: tauri::AppHandle) -> Result<Connection, String> {
        let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        NODE.get_or_init(|| start(data_dir)).await.clone()
    }

    /// The phone's shared Download folder, where other apps can see files.
    const PUBLIC_DOWNLOADS: &str = "/storage/emulated/0/Download";

    /// Keeps only a plain file name: no folders, no control characters.
    fn safe_name(name: &str) -> String {
        let name: String = name
            .chars()
            .map(|c| if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c })
            .collect();
        let name = name.trim().trim_matches('.').to_string();
        if name.is_empty() { "download".into() } else { name }
    }

    /// `name.ext`, or `name (2).ext` and so on if that already exists.
    fn free_path(dir: &std::path::Path, name: &str) -> PathBuf {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
            _ => (name.to_string(), String::new()),
        };
        let mut candidate = dir.join(name);
        let mut n = 2;
        while candidate.exists() {
            candidate = dir.join(format!("{stem} ({n}){ext}"));
            n += 1;
        }
        candidate
    }

    async fn fetch_to(url: &str, path: &std::path::Path) -> Result<(), String> {
        use futures_util::StreamExt;
        use tokio::io::AsyncWriteExt;
        let res = reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(|e| e.to_string())?
            .get(url)
            .send()
            .await
            .map_err(|e| format!("The download could not start: {e}"))?;
        if !res.status().is_success() {
            return Err(format!("The download failed (HTTP {})", res.status().as_u16()));
        }
        let mut file = tokio::fs::File::create(path).await.map_err(|e| e.to_string())?;
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("The download was interrupted: {e}"))?;
            file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        file.flush().await.map_err(|e| e.to_string())
    }

    /// Streams a file from this app's own node into the phone's Download
    /// folder (`Download/NEXUS`), or the app's own folder if that is not
    /// writable. Returns where it was saved, for display.
    pub async fn save_download(app: tauri::AppHandle, url: String, filename: String) -> Result<String, String> {
        // Only this app's node may be the source.
        let node = connection(app.clone()).await?;
        if !url.starts_with(&format!("{}/api/v1/", node.base_url)) {
            return Err("Downloads must come from this app's own node".into());
        }
        let name = safe_name(&filename);
        let public = PathBuf::from(PUBLIC_DOWNLOADS).join("NEXUS");
        let private = app.path().app_data_dir().map_err(|e| e.to_string())?.join("Downloads");
        let mut last_error = String::new();
        for (dir, label) in [(public, "Download/NEXUS"), (private, "the app's Downloads folder")] {
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                last_error = e.to_string();
                continue;
            }
            let path = free_path(&dir, &name);
            match fetch_to(&url, &path).await {
                Ok(()) => {
                    let saved = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
                    return Ok(format!("{label}/{saved}"));
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&path).await;
                    last_error = e;
                }
            }
        }
        Err(last_error)
    }
}

/// Returns the node's address and local owner token, starting the node
/// first if needed.
#[tauri::command]
async fn service_connection(app: tauri::AppHandle) -> Result<Connection, String> {
    host::connection(app).await
}

/// Saves a file from the node to the phone's storage (phones only).
#[tauri::command]
async fn save_download(app: tauri::AppHandle, url: String, filename: String) -> Result<String, String> {
    host::save_download(app, url, filename).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    builder
        .invoke_handler(tauri::generate_handler![service_connection, save_download])
        .run(tauri::generate_context!())
        .expect("error while running NEXUS Cloud");
}
