//! HearthDrive app shell.
//!
//! **Desktop.** The window is only a client: storage, indexing and the API
//! live in the separate `nexus-service` process, which keeps running when
//! the window closes. The shell finds (or starts) that service and hands the
//! UI its connection details. Closing the window hides it to the system
//! tray, so the app (and its notifications) keep going; Quit in the tray's
//! menu really exits. Opening the app again shows the window already
//! running instead of starting another.
//!
//! **Phones.** There is no separate service, so the app hosts the same node
//! in its own process, with its data in the app's private directory. On
//! Android a foreground service (`NodeService`, with a notification) keeps
//! that process, and so the node, running after the app is left or swiped
//! away, until the person taps Quit or Android force-stops the app.

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
    use nexus_common::runtime::{RUNTIME_FILE, RuntimeInfo, default_data_dir, program_stamp};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    pub fn read_live_runtime(data_dir: &Path) -> Option<RuntimeInfo> {
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
                "The HearthDrive background service was not found at {}. Build it with `cargo build -p nexus-core`.",
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
        command.spawn().map(|_| ()).map_err(|e| format!("Could not start the HearthDrive service: {e}"))
    }

    /// Whether the running service is the program next to this app, as it
    /// is on disk now. Services from before this check say neither.
    fn is_current(info: &RuntimeInfo) -> bool {
        let Ok(ours) = service_executable() else { return true };
        let same_file = info.exe.as_deref().is_some_and(|exe| {
            match (std::fs::canonicalize(exe), std::fs::canonicalize(&ours)) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            }
        });
        same_file && info.exe_modified.is_some() && info.exe_modified == program_stamp(&ours)
    }

    fn stop_service(pid: u32) {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // The image-name filter makes sure a reused process id is left alone.
            let _ = Command::new("taskkill")
                .args(["/F", "/PID", &pid.to_string(), "/FI", "IMAGENAME eq nexus-service.exe"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();
        }
        #[cfg(not(windows))]
        {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }

    fn wait_until_stopped(data_dir: &Path) -> bool {
        for _ in 0..40 {
            if read_live_runtime(data_dir).is_none() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(125));
        }
        false
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
            if is_current(&info) {
                return Ok(to_connection(info));
            }
            // An older service is still running (the app was updated, or
            // rebuilt during development): replace it with the one that
            // matches this app. If it cannot be stopped, use it anyway.
            stop_service(info.pid);
            if !wait_until_stopped(&data_dir)
                && let Some(info) = read_live_runtime(&data_dir)
            {
                return Ok(to_connection(info));
            }
        }
        spawn_service()?;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(250));
            if let Some(info) = read_live_runtime(&data_dir) {
                return Ok(to_connection(info));
            }
        }
        Err(format!(
            "The HearthDrive service did not start in time. Its logs are in {}",
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

    /// Phones only: computers open files from their own folders.
    pub async fn fetch_for_app(_app: tauri::AppHandle, _url: String, _filename: String) -> Result<String, String> {
        Err("Only the phone app hands files to other apps".into())
    }
}

#[cfg(mobile)]
mod host {
    use super::Connection;
    use nexus_common::runtime::{RUNTIME_FILE, RuntimeInfo};
    use nexus_core::Config;
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tauri::Manager;

    /// The phone's shared storage, where the camera and other apps keep
    /// photos and videos.
    const SHARED_STORAGE: &str = "/storage/emulated/0";

    /// Written to the data folder when the node fails to start, so the
    /// app (possibly in another process) can say why.
    const START_ERROR_FILE: &str = "start-error.txt";

    static STARTED: AtomicBool = AtomicBool::new(false);

    /// Starts the node in this process, on its own thread, once. On Android
    /// this runs in `NodeService`'s process (see [`super::android`]); the
    /// app's window then finds it like any other process would.
    pub fn start_node(data_dir: PathBuf, share_inbox: PathBuf) {
        if STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = std::thread::Builder::new().name("nexus-node".into()).spawn(move || {
            let error_file = data_dir.join(START_ERROR_FILE);
            let _ = std::fs::remove_file(&error_file);
            let fail = |e: String| {
                let _ = std::fs::write(&error_file, &e);
            };
            if let Err(e) = std::fs::create_dir_all(&data_dir) {
                return fail(format!("Could not create the app's data folder: {e}"));
            }
            // The guard must outlive the node; the node lives as long as the process.
            if let Ok(guard) = nexus_core::init_logging(&data_dir) {
                std::mem::forget(guard);
            }
            let mut config = Config::embedded(data_dir.clone(), "My phone");
            config.media_dir = Some(PathBuf::from(SHARED_STORAGE));
            // MainActivity copies files shared to the app into `<cache>/shared`.
            config.share_inbox = Some(share_inbox);
            let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("nexus").build() {
                Ok(rt) => rt,
                Err(e) => return fail(e.to_string()),
            };
            // On Android the node's transfers show in a notification.
            let (ready, ready_rx) = tokio::sync::oneshot::channel();
            #[cfg(target_os = "android")]
            runtime.spawn(super::transfers::watch(ready_rx));
            #[cfg(not(target_os = "android"))]
            drop(ready_rx);
            if let Err(e) = runtime.block_on(nexus_core::run_with(config, Some(ready))) {
                tracing::error!(error = %format!("{e:#}"), "embedded node stopped");
                fail(format!("{e:#}"));
            }
        });
    }

    /// The running node's address, from the file it writes when it starts,
    /// if something is listening there.
    ///
    /// No check of `/proc/<pid>`: Android hides other processes' entries
    /// from release builds of an app, even the app's own `:node` process.
    fn read_live_runtime(data_dir: &Path) -> Option<RuntimeInfo> {
        let raw = std::fs::read(data_dir.join(RUNTIME_FILE)).ok()?;
        let info: RuntimeInfo = serde_json::from_slice(&raw).ok()?;
        let addr = SocketAddr::from(([127, 0, 0, 1], info.port));
        is_hearthdrive(&addr).then_some(info)
    }

    /// Whether `addr` answers HearthDrive's health check. A file left from an
    /// earlier run can name a port that another app has taken since.
    fn is_hearthdrive(addr: &SocketAddr) -> bool {
        use std::io::{Read, Write};
        let Ok(mut stream) = TcpStream::connect_timeout(addr, Duration::from_millis(400)) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_millis(800)));
        if stream.write_all(b"GET /api/v1/health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").is_err() {
            return false;
        }
        let mut reply = Vec::new();
        let _ = stream.take(4096).read_to_end(&mut reply);
        let reply = String::from_utf8_lossy(&reply);
        reply.starts_with("HTTP/1.") && reply.contains("\"status\":\"ok\"")
    }

    fn wait_for_node(data_dir: PathBuf) -> Result<Connection, String> {
        let error_file = data_dir.join(START_ERROR_FILE);
        let since = std::time::SystemTime::now();
        // A first start sets up the database, which can take a while on a slow phone.
        for _ in 0..240 {
            if let Some(info) = read_live_runtime(&data_dir) {
                return Ok(Connection {
                    base_url: info.base_url,
                    token: info.local_token,
                    data_dir: data_dir.to_string_lossy().into_owned(),
                    shell: "mobile",
                });
            }
            let fresh_error = std::fs::metadata(&error_file).and_then(|m| m.modified()).is_ok_and(|t| t >= since);
            if fresh_error && let Ok(e) = std::fs::read_to_string(&error_file) {
                return Err(format!("HearthDrive could not start: {}", e.trim()));
            }
            std::thread::sleep(Duration::from_millis(125));
        }
        Err("HearthDrive did not start in time".into())
    }

    pub async fn connection(app: tauri::AppHandle) -> Result<Connection, String> {
        let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        // On Android, MainActivity has already started the node (in
        // NodeService's process). Elsewhere it runs inside the app.
        #[cfg(not(target_os = "android"))]
        start_node(data_dir.clone(), app.path().app_cache_dir().map_err(|e| e.to_string())?.join("shared"));
        tauri::async_runtime::spawn_blocking(move || wait_for_node(data_dir)).await.map_err(|e| e.to_string())?
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
    /// folder (`Download/HearthDrive`), or the app's own folder if that is not
    /// writable. Returns where it was saved, for display.
    pub async fn save_download(app: tauri::AppHandle, url: String, filename: String) -> Result<String, String> {
        // Only this app's node may be the source.
        let node = connection(app.clone()).await?;
        if !url.starts_with(&format!("{}/api/v1/", node.base_url)) {
            return Err("Downloads must come from this app's own node".into());
        }
        let name = safe_name(&filename);
        let public = PathBuf::from(PUBLIC_DOWNLOADS).join("HearthDrive");
        let private = app.path().app_data_dir().map_err(|e| e.to_string())?.join("Downloads");
        let mut last_error = String::new();
        for (dir, label) in [(public, "Download/HearthDrive"), (private, "the app's Downloads folder")] {
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

    /// Files fetched for another app to open or for the share menu are kept
    /// this long, then cleared.
    const HANDOFF_KEEP: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

    /// Streams a file from this app's own node into the app's cache
    /// (`<cache>/open/<n>/<name>`, keeping its name, which other apps show),
    /// for Android to hand to another app. Returns its full path. Earlier
    /// ones older than a day are cleared first.
    pub async fn fetch_for_app(app: tauri::AppHandle, url: String, filename: String) -> Result<String, String> {
        let node = connection(app.clone()).await?;
        if !url.starts_with(&format!("{}/api/v1/", node.base_url)) {
            return Err("Files must come from this app's own node".into());
        }
        let root = app.path().app_cache_dir().map_err(|e| e.to_string())?.join("open");
        if let Ok(mut entries) = tokio::fs::read_dir(&root).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let old = entry
                    .metadata()
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > HANDOFF_KEEP);
                if old {
                    let _ = tokio::fs::remove_dir_all(entry.path()).await;
                }
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = root.join(stamp.to_string());
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        let path = dir.join(safe_name(&filename));
        if let Err(e) = fetch_to(&url, &path).await {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            return Err(e);
        }
        Ok(path.to_string_lossy().into_owned())
    }
}

/// The JNI entry `NodeService` (Kotlin) starts the node through.
#[cfg(target_os = "android")]
mod transfers;

#[cfg(target_os = "android")]
mod android {
    use jni::JNIEnv;
    use jni::objects::{JClass, JObject, JString};
    use jni::sys::jboolean;
    use std::path::PathBuf;

    /// `NodeService.startNode(context, dataDir, shareInbox, ownProcess)`.
    ///
    /// `ownProcess` is true in the service's own process, where nothing
    /// else has set up the Android context the node's networking reads
    /// (DNS servers, network interfaces); the app's window process already
    /// has it. Returns at once; the node starts on its own thread.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_app_nexuscloud_desktop_NodeService_startNode<'l>(
        mut env: JNIEnv<'l>,
        class: JClass<'l>,
        context: JObject<'l>,
        data_dir: JString<'l>,
        share_inbox: JString<'l>,
        own_process: jboolean,
    ) {
        let (Ok(data_dir), Ok(share_inbox)) = (env.get_string(&data_dir), env.get_string(&share_inbox)) else {
            return;
        };
        let (data_dir, share_inbox) = (PathBuf::from(String::from(data_dir)), PathBuf::from(String::from(share_inbox)));
        // For the transfers notification, which calls back into NodeService.
        if let (Ok(vm), Ok(class)) = (env.get_java_vm(), env.new_global_ref(&class)) {
            super::transfers::remember(vm, class);
        }
        if own_process != 0 {
            static CONTEXT_SET: std::sync::Once = std::sync::Once::new();
            CONTEXT_SET.call_once(|| {
                let (Ok(vm), Ok(context)) = (env.get_java_vm(), env.new_global_ref(&context)) else { return };
                let raw = context.as_obj().as_raw();
                // The context lives as long as the process.
                std::mem::forget(context);
                // SAFETY: the VM and a global reference to the application
                // context, both valid for the rest of the process.
                unsafe { ndk_context::initialize_android_context(vm.get_java_vm_pointer().cast(), raw.cast()) };
            });
        }
        super::host::start_node(data_dir, share_inbox);
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

/// Fetches a file from the node into the app's cache, for Android to hand to
/// another app ("Open with", the share menu). Returns its path (phones only).
#[tauri::command]
async fn fetch_for_app(app: tauri::AppHandle, url: String, filename: String) -> Result<String, String> {
    host::fetch_for_app(app, url, filename).await
}

/// The system tray icon, closing to the tray, and one running copy.
///
/// Started with `--hidden` (the "Start with Windows" entry does that), the
/// app opens straight into the tray: the window loads but stays hidden, so
/// notifications work from sign-in on.
#[cfg(desktop)]
mod tray {
    use serde_json::Value;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use tauri::{AppHandle, Manager, Runtime, WindowEvent};

    /// The argument that starts the app in the tray, without its window.
    pub const HIDDEN_ARG: &str = "--hidden";

    /// How often the tray's status line is brought up to date.
    const STATUS_EVERY: Duration = Duration::from_secs(4);

    /// The virtual drive's folder while it runs, for "Open HearthDrive folder".
    static DRIVE_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);

    /// Shows the main window, restored and in front.
    pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        }
    }

    /// Shows the main window at one of its pages (`transfers`, `settings`…).
    fn show_page<R: Runtime>(app: &AppHandle<R>, page: &str) {
        show_main(app);
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.eval(format!("location.hash = '#/{page}'"));
        }
    }

    fn open_drive_folder() {
        let folder = DRIVE_FOLDER.lock().ok().and_then(|f| f.clone());
        if let Some(folder) = folder {
            let _ = std::process::Command::new("explorer.exe").arg(folder).spawn();
        }
    }

    pub fn single_instance<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
        tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app))
    }

    pub fn setup<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
        // The window is made hidden (see tauri.conf.json), so it never
        // flashes when the app starts in the tray.
        if !std::env::args().any(|a| a == HIDDEN_ARG) {
            show_main(app.handle());
        }
        let status = MenuItem::with_id(app, "status", "Starting…", false, None::<&str>)?;
        let open = MenuItem::with_id(app, "open", "Open HearthDrive", true, None::<&str>)?;
        let folder = MenuItem::with_id(app, "folder", "Open HearthDrive folder", false, None::<&str>)?;
        let transfers = MenuItem::with_id(app, "transfers", "Transfers", true, None::<&str>)?;
        let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
        let (sep1, sep2) = (PredefinedMenuItem::separator(app)?, PredefinedMenuItem::separator(app)?);
        let menu = Menu::with_items(app, &[&status, &sep1, &open, &folder, &transfers, &settings, &sep2, &quit])?;
        let mut tray = TrayIconBuilder::with_id("main")
            .tooltip("HearthDrive")
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id().as_ref() {
                "open" => show_main(app),
                "folder" => open_drive_folder(),
                "transfers" => show_page(app, "transfers"),
                "settings" => show_page(app, "settings"),
                // The storage service is a separate process and keeps running.
                "quit" => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                    show_main(tray.app_handle());
                }
            });
        if let Some(icon) = app.default_window_icon() {
            tray = tray.icon(icon.clone());
        }
        tray.build(app)?;
        tauri::async_runtime::spawn(keep_status(app.handle().clone(), status, folder));
        Ok(())
    }

    /// What the tray shows: one status line, and the drive's folder if it runs.
    struct Status {
        line: String,
        drive: Option<PathBuf>,
    }

    async fn get(client: &reqwest::Client, base: &str, token: &str, path: &str) -> Option<Value> {
        let res = client.get(format!("{base}/api/v1{path}")).bearer_auth(token).send().await.ok()?;
        res.error_for_status().ok()?.json().await.ok()
    }

    async fn read_status(client: &reqwest::Client) -> Status {
        let data_dir = nexus_common::runtime::default_data_dir();
        let info = tauri::async_runtime::spawn_blocking(move || super::host::read_live_runtime(&data_dir))
            .await
            .ok()
            .flatten();
        let Some(info) = info else {
            return Status { line: "The storage service is not running".into(), drive: None };
        };
        let (base, token) = (info.base_url.as_str(), info.local_token.as_str());
        let (devices, jobs, drive) = tokio::join!(
            get(client, base, token, "/devices"),
            get(client, base, token, "/transfers"),
            get(client, base, token, "/vdrive"),
        );
        let drive = drive
            .filter(|d| d["running"].as_bool() == Some(true))
            .and_then(|d| d["path"].as_str().map(PathBuf::from));
        Status { line: status_line(devices.as_ref(), jobs.as_ref()), drive }
    }

    /// "Transferring 2 items · 40%", or how many devices can be reached.
    fn status_line(devices: Option<&Value>, jobs: Option<&Value>) -> String {
        let running: Vec<&Value> = jobs
            .and_then(Value::as_array)
            .map(|all| all.iter().filter(|j| j["status"] == "running").collect())
            .unwrap_or_default();
        if !running.is_empty() {
            let sum = |key: &str| running.iter().filter_map(|j| j[key].as_u64()).sum::<u64>();
            let (done, total) = (sum("bytes_done"), sum("bytes_total"));
            let items = if running.len() == 1 { "1 item".to_string() } else { format!("{} items", running.len()) };
            return match (done.min(total) * 100).checked_div(total) {
                Some(percent) => format!("Transferring {items} · {percent}%"),
                None => format!("Transferring {items}"),
            };
        }
        let others: Vec<&Value> = devices
            .and_then(Value::as_array)
            .map(|all| all.iter().filter(|d| d["is_local"] != true && d["blocked_at"].is_null()).collect())
            .unwrap_or_default();
        let online = others.iter().filter(|d| d["online"] == true).count();
        match (others.len(), online) {
            (0, _) => "Up to date · no other devices yet".into(),
            (1, 1) => "Up to date · your other device is online".into(),
            (total, online) if online == total => format!("Up to date · all {total} devices online"),
            (total, online) => format!("Up to date · {online} of {total} devices online"),
        }
    }

    async fn keep_status<R: Runtime>(app: AppHandle<R>, status: MenuItem<R>, folder: MenuItem<R>) {
        let Ok(client) = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build() else {
            return;
        };
        let mut shown = String::new();
        loop {
            let now = read_status(&client).await;
            if now.line != shown {
                let _ = status.set_text(&now.line);
                if let Some(tray) = app.tray_by_id("main") {
                    let _ = tray.set_tooltip(Some(format!("HearthDrive\n{}", now.line)));
                }
                shown = now.line;
            }
            let _ = folder.set_enabled(now.drive.is_some());
            if let Ok(mut f) = DRIVE_FOLDER.lock() {
                *f = now.drive;
            }
            tokio::time::sleep(STATUS_EVERY).await;
        }
    }

    /// Closing the window hides it; the app stays in the tray.
    pub fn on_window_event<R: Runtime>(window: &tauri::Window<R>, event: &WindowEvent) {
        if let WindowEvent::CloseRequested { api, .. } = event
            && window.label() == "main"
        {
            api.prevent_close();
            let _ = window.hide();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::status_line;
        use serde_json::json;

        #[test]
        fn says_what_is_transferring() {
            let jobs = json!([
                { "status": "running", "bytes_done": 30, "bytes_total": 100 },
                { "status": "running", "bytes_done": 10, "bytes_total": 0 },
                { "status": "done", "bytes_done": 5, "bytes_total": 5 },
            ]);
            assert_eq!(status_line(None, Some(&jobs)), "Transferring 2 items · 40%");
        }

        #[test]
        fn counts_other_devices() {
            let devices = json!([
                { "is_local": true, "online": true, "blocked_at": null },
                { "is_local": false, "online": true, "blocked_at": null },
                { "is_local": false, "online": false, "blocked_at": null },
                { "is_local": false, "online": false, "blocked_at": 12 },
            ]);
            assert_eq!(status_line(Some(&devices), Some(&json!([]))), "Up to date · 1 of 2 devices online");
            assert_eq!(status_line(Some(&json!([{ "is_local": true }])), None), "Up to date · no other devices yet");
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    // Registered first, so a second copy hands over before doing anything.
    #[cfg(desktop)]
    let builder = builder
        .plugin(tray::single_instance())
        .setup(|app| Ok(tray::setup(app)?))
        .on_window_event(tray::on_window_event);
    let builder = builder
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init());
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());
    builder
        .invoke_handler(tauri::generate_handler![service_connection, save_download, fetch_for_app])
        .run(tauri::generate_context!())
        .expect("error while running HearthDrive");
}
