//! Terminal: a shell on this device (PowerShell on Windows, `sh` on phones)
//! that its owner can use from here or from a paired device. Each session is
//! one shell process; lines typed in the app go to its input, and what it
//! prints is kept here for the app to read, so it travels over the same
//! request/response peer proxy as everything else.
//!
//! Who may open one: the owner on this device, or a paired device whose role
//! here is Editor or higher (the role devices are paired with), and is not
//! limited to some storage locations. The device sending the request checks
//! that the person using it is its owner. Other devices can be kept out in
//! Settings › Terminal, which only someone on this device can change.

use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use nexus_common::NexusError;
use nexus_common::types::Role;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::Notify;

/// Whether paired devices may open a terminal here (on unless turned off).
const ENABLED_SETTING: &str = "terminal_enabled";
/// Open sessions per device at most.
const MAX_SESSIONS: usize = 6;
/// A session nobody has read from for this long is closed.
const IDLE_CLOSE: Duration = Duration::from_secs(30 * 60);
/// A closed session's last output is kept this long for the app to read.
const KEEP_CLOSED: Duration = Duration::from_secs(5 * 60);
/// Output kept per session; older output is dropped first.
const BUFFER_LIMIT: usize = 512 * 1024;
/// How long a read waits for new output before answering with none. Well
/// under the peer proxy's call timeout.
const WAIT: Duration = Duration::from_secs(8);
/// Longest input accepted at once (a pasted script).
const INPUT_LIMIT: usize = 64 * 1024;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static SESSIONS: LazyLock<Mutex<HashMap<String, Arc<Session>>>> = LazyLock::new(Default::default);

#[derive(Serialize, Clone)]
pub struct Chunk {
    seq: u64,
    /// `out`, `err` (the shell's error stream), `in` (what was typed) or
    /// `info` (from HearthDrive: started, ended).
    kind: &'static str,
    text: String,
}

#[derive(Default)]
struct Screen {
    chunks: VecDeque<Chunk>,
    next: u64,
    bytes: usize,
    /// Inputs sent whose end the shell has not reported yet.
    pending: u32,
    cwd: String,
    closed: Option<String>,
    closed_at: Option<Instant>,
}

/// Where typed lines go: the shell's input pipe or the SSH channel.
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// How a session's shell is ended.
enum Ender {
    Process(u32),
    Ssh(tokio::sync::Mutex<Option<russh::client::Handle<ssh::HostCheck>>>),
}

pub struct Session {
    id: String,
    owner: String,
    ender: Ender,
    marker: String,
    shell: &'static str,
    /// A POSIX shell (`sh`, bash in Termux), not PowerShell.
    posix: bool,
    stdin: tokio::sync::Mutex<Option<Writer>>,
    screen: Mutex<Screen>,
    changed: Notify,
    last_seen: Mutex<Instant>,
    /// Ended from here (Stop, closed, idle, turned off), not by itself.
    stopped: AtomicBool,
}

impl Session {
    fn push(&self, kind: &'static str, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut s = self.screen.lock().unwrap();
        let seq = s.next;
        s.next += 1;
        s.bytes += text.len();
        s.chunks.push_back(Chunk { seq, kind, text: text.replace("\r\n", "\n") });
        while s.bytes > BUFFER_LIMIT && s.chunks.len() > 1 {
            if let Some(old) = s.chunks.pop_front() {
                s.bytes -= old.text.len();
            }
        }
        drop(s);
        self.changed.notify_waiters();
    }

    fn finished_input(&self, cwd: String) {
        let mut s = self.screen.lock().unwrap();
        s.pending = s.pending.saturating_sub(1);
        s.cwd = cwd;
        drop(s);
        self.changed.notify_waiters();
    }

    fn close(&self, why: &str) {
        let mut s = self.screen.lock().unwrap();
        if s.closed.is_some() {
            return;
        }
        s.closed = Some(why.to_string());
        s.closed_at = Some(Instant::now());
        s.pending = 0;
        drop(s);
        self.push("info", &format!("{why}\n"));
    }

    async fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        match &self.ender {
            Ender::Process(pid) => kill_tree(*pid).await,
            Ender::Ssh(handle) => {
                if let Some(h) = handle.lock().await.take() {
                    let _ = h.disconnect(russh::Disconnect::ByApplication, "", "en").await;
                }
            }
        }
    }

    fn ended_why(&self, code: Option<i64>) -> String {
        match code {
            _ if self.stopped.load(Ordering::SeqCst) => "The terminal was stopped.".to_string(),
            Some(0) => "The terminal ended.".to_string(),
            Some(code) => format!("The terminal ended (exit code {code})."),
            None => "The terminal ended.".to_string(),
        }
    }

    fn touch(&self) {
        *self.last_seen.lock().unwrap() = Instant::now();
    }

    fn idle_for(&self) -> Duration {
        self.last_seen.lock().unwrap().elapsed()
    }

    fn view(&self, after: u64) -> SessionView {
        let s = self.screen.lock().unwrap();
        SessionView {
            id: self.id.clone(),
            shell: self.shell,
            cwd: s.cwd.clone(),
            busy: s.pending > 0 && s.closed.is_none(),
            closed: s.closed.clone(),
            next: s.next,
            chunks: s.chunks.iter().filter(|c| c.seq >= after).cloned().collect(),
        }
    }

    /// The line the shell runs after each input, which prints the marker and
    /// the working directory so the app knows the input finished and where
    /// the shell is now.
    fn marker_command(&self) -> String {
        if self.posix {
            format!("echo \"{}$PWD\"", self.marker)
        } else {
            format!("Write-Output ('{}' + (Get-Location).Path)", self.marker)
        }
    }

    fn prompt(&self, cwd: &str) -> String {
        if self.posix { format!("{cwd} $ ") } else { format!("PS {cwd}> ") }
    }
}

#[derive(Serialize)]
pub struct SessionView {
    id: String,
    shell: &'static str,
    cwd: String,
    busy: bool,
    closed: Option<String>,
    /// Pass as `after` to read only what comes next.
    next: u64,
    chunks: Vec<Chunk>,
}

/// Splits what the shell prints into what to show and the working
/// directories its markers report.
struct MarkerFilter {
    marker: String,
    held: String,
}

impl MarkerFilter {
    fn feed(&mut self, text: &str) -> (String, Vec<String>) {
        self.held.push_str(text);
        let mut show = String::new();
        let mut dirs = Vec::new();
        while let Some(end) = self.held.find('\n') {
            let line: String = self.held.drain(..=end).collect();
            match line.find(&self.marker) {
                Some(at) => {
                    // Output without a final newline can share the marker's line.
                    if at > 0 {
                        show.push_str(&line[..at]);
                        show.push('\n');
                    }
                    dirs.push(line[at + self.marker.len()..].trim_end_matches(['\r', '\n']).to_string());
                }
                None => show.push_str(&line),
            }
        }
        // An unfinished line (a question, progress) shows now, except an end
        // that may be the start of a marker.
        let keep = possible_marker_start(&self.held, &self.marker);
        show.push_str(&self.held[..keep]);
        self.held.drain(..keep);
        (show, dirs)
    }
}

fn possible_marker_start(s: &str, marker: &str) -> usize {
    if let Some(at) = s.find(marker) {
        return at;
    }
    s.char_indices().map(|(i, _)| i).find(|&i| marker.starts_with(&s[i..])).unwrap_or(s.len())
}

/// Turns bytes into text, keeping a character split between two reads for
/// the next one.
fn take_text(pending: &mut Vec<u8>) -> String {
    let valid = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => {
            let text = String::from_utf8_lossy(pending).into_owned();
            pending.clear();
            return text;
        }
    };
    let rest = pending.split_off(valid);
    let text = String::from_utf8(std::mem::replace(pending, rest)).unwrap_or_default();
    text
}

/// One of the shell's output streams, decoded and, for its standard output,
/// with the markers taken out.
struct Stream {
    err: bool,
    bytes: Vec<u8>,
    filter: MarkerFilter,
}

impl Stream {
    fn new(session: &Session, err: bool) -> Self {
        Stream { err, bytes: Vec::new(), filter: MarkerFilter { marker: session.marker.clone(), held: String::new() } }
    }

    fn feed(&mut self, session: &Session, data: &[u8]) {
        self.bytes.extend_from_slice(data);
        let text = take_text(&mut self.bytes);
        if self.err {
            session.push("err", &text);
            return;
        }
        let (show, dirs) = self.filter.feed(&text);
        session.push("out", &show);
        for dir in dirs {
            session.finished_input(dir);
        }
    }

    fn finish(self, session: &Session) {
        let rest = self.filter.held + &String::from_utf8_lossy(&self.bytes);
        session.push(if self.err { "err" } else { "out" }, &rest);
    }
}

async fn read_stream(session: Arc<Session>, mut from: impl AsyncRead + Unpin, err: bool) {
    let mut buf = vec![0u8; 16 * 1024];
    let mut stream = Stream::new(&session, err);
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => stream.feed(&session, &buf[..n]),
        }
    }
    stream.finish(&session);
}

fn shell_command() -> (Command, &'static str) {
    #[cfg(windows)]
    {
        let mut c = Command::new("powershell.exe");
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "-"]);
        c.creation_flags(CREATE_NO_WINDOW);
        (c, "PowerShell")
    }
    #[cfg(not(windows))]
    {
        let sh = if std::path::Path::new("/system/bin/sh").exists() { "/system/bin/sh" } else { "/bin/sh" };
        let mut c = Command::new(sh);
        // Its own process group, so stopping it stops what it started.
        c.process_group(0);
        (c, "sh")
    }
}

/// Ends the shell and everything it started.
async fn kill_tree(pid: u32) {
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("taskkill");
        c.args(["/F", "/T", "/PID", &pid.to_string()]).creation_flags(CREATE_NO_WINDOW);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = Command::new("kill");
        c.args(["-9", "--", &format!("-{pid}")]);
        c
    };
    let _ = c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await;
}

/// A path as one word for `sh`.
fn quote_posix(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// Where a new shell starts: the folder asked for if it exists, otherwise
/// the person's home folder (on a phone, its shared storage).
fn start_dir(s: &crate::state::AppState, asked: Option<&str>) -> PathBuf {
    if let Some(dir) = asked.map(PathBuf::from).filter(|p| p.is_dir()) {
        return dir;
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    s.media_dir.clone().filter(|p| p.is_dir()).or(home.filter(|p| p.is_dir())).unwrap_or_else(|| s.data_dir.clone())
}

fn new_session(owner: String, shell: &'static str, posix: bool, ender: Ender, stdin: Writer, cwd: String) -> Arc<Session> {
    Arc::new(Session {
        id: nexus_devices::random_code(24),
        owner,
        ender,
        // Random, so nothing a program prints can pass for the end of an input.
        marker: format!("__hearthdrive_{}__", nexus_devices::random_code(16)),
        shell,
        posix,
        stdin: tokio::sync::Mutex::new(Some(stdin)),
        screen: Mutex::new(Screen { cwd, pending: 1, ..Default::default() }),
        changed: Notify::new(),
        last_seen: Mutex::new(Instant::now()),
        stopped: AtomicBool::new(false),
    })
}

/// A shell started here: PowerShell on Windows, `sh` elsewhere.
async fn start_process(s: &crate::state::AppState, owner: String, asked_dir: Option<&str>) -> Result<Arc<Session>, NexusError> {
    let dir = start_dir(s, asked_dir);
    let (mut cmd, shell) = shell_command();
    cmd.current_dir(&dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| NexusError::Internal(format!("Could not start {shell}: {e}")))?;
    let pid = child.id().unwrap_or(0);
    let stdin: Writer = Box::new(child.stdin.take().ok_or_else(|| NexusError::Internal(format!("{shell} has no input")))?);
    let session = new_session(owner, shell, !cfg!(windows), Ender::Process(pid), stdin, dir.display().to_string());
    let out = child.stdout.take().map(|o| tokio::spawn(read_stream(session.clone(), o, false)));
    let err = child.stderr.take().map(|e| tokio::spawn(read_stream(session.clone(), e, true)));

    // Watches the shell: closes it when nobody has looked for a while, and
    // reports when it ends.
    let watched = session.clone();
    tokio::spawn(async move {
        let status = loop {
            tokio::select! {
                st = child.wait() => break st.ok(),
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    if watched.idle_for() > IDLE_CLOSE {
                        watched.stop().await;
                    }
                }
            }
        };
        for task in [out, err].into_iter().flatten() {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
        let why = watched.ended_why(status.and_then(|st| st.code()).map(i64::from));
        watched.close(&why);
    });

    // Text in UTF-8, no progress bars (they cannot be drawn here), then the
    // first marker says the shell is ready.
    let init = if cfg!(windows) {
        "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $OutputEncoding = [System.Text.Encoding]::UTF8".to_string()
    } else {
        format!("cd {} 2>/dev/null", quote_posix(&dir.display().to_string()))
    };
    write_lines(&session, &format!("{init}\n{}\n", session.marker_command())).await?;
    Ok(session)
}

async fn write_lines(session: &Session, text: &str) -> Result<(), NexusError> {
    let mut stdin = session.stdin.lock().await;
    let pipe = stdin.as_mut().ok_or_else(|| NexusError::Conflict("This terminal has ended".into()))?;
    pipe.write_all(text.as_bytes()).await.map_err(|_| NexusError::Conflict("This terminal has ended".into()))?;
    pipe.flush().await.map_err(|_| NexusError::Conflict("This terminal has ended".into()))?;
    Ok(())
}

/// Closes sessions nobody uses any more and forgets closed ones.
fn tidy() {
    let mut all = SESSIONS.lock().unwrap();
    all.retain(|_, sess| {
        let closed_long_ago = sess.screen.lock().unwrap().closed_at.is_some_and(|t| t.elapsed() > KEEP_CLOSED);
        !closed_long_ago && sess.idle_for() < IDLE_CLOSE + KEEP_CLOSED
    });
}

fn find(user: &AuthUser, id: &str) -> Result<Arc<Session>, NexusError> {
    let sess = SESSIONS.lock().unwrap().get(id).cloned().ok_or_else(|| NexusError::NotFound("terminal".into()))?;
    if sess.owner != user.owner_key() {
        return Err(NexusError::NotFound("terminal".into()));
    }
    sess.touch();
    Ok(sess)
}

async fn enabled(s: &crate::state::AppState) -> bool {
    s.db.get_setting(ENABLED_SETTING).await.ok().flatten().is_none_or(|v| v != "0")
}

/// Who may use a terminal here, leaving out whether other devices are let in.
fn may_use(user: &AuthUser) -> Result<(), NexusError> {
    if user.via_token {
        return Err(NexusError::PermissionDenied("Programs cannot open a terminal".into()));
    }
    if user.is_limited() {
        return Err(NexusError::PermissionDenied("A terminal reaches everything on this device, so it is only for devices that may see all storage here".into()));
    }
    let ok = if user.device_id.is_some() {
        matches!(user.role, Role::Owner | Role::Administrator | Role::Editor)
    } else {
        user.role == Role::Owner
    };
    if !ok {
        return Err(NexusError::PermissionDenied(if user.device_id.is_some() {
            "This device's role here does not allow a terminal. On the other device, open Devices and give this one the Editor role or higher".into()
        } else {
            "Only the owner can open a terminal".into()
        }));
    }
    Ok(())
}

async fn allowed(s: &crate::state::AppState, user: &AuthUser) -> Result<(), NexusError> {
    may_use(user)?;
    if user.device_id.is_some() && !enabled(s).await {
        return Err(NexusError::PermissionDenied(
            "The terminal is turned off on this device. Turn it on there in Settings › Terminal".into(),
        ));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct TerminalInfo {
    /// Whether paired devices may open a terminal here.
    enabled: bool,
    shell: &'static str,
    /// Whether the caller can change `enabled` (only from this device).
    can_configure: bool,
    /// The Termux (SSH) shell's settings, for whoever can change them.
    ssh: Option<ssh::SshView>,
}

/// `GET /terminal`: what kind of terminal this device has, and whether
/// other devices may open it.
pub async fn info(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<TerminalInfo>> {
    may_use(&user)?;
    let cfg = ssh::load(&s).await;
    let can_configure = user.device_id.is_none();
    Ok(Json(TerminalInfo {
        enabled: enabled(&s).await,
        shell: if cfg.on { cfg.label() } else if cfg!(windows) { "PowerShell" } else { "sh" },
        can_configure,
        ssh: if can_configure { Some(ssh::view(&s, &cfg)?) } else { None },
    }))
}

#[derive(Deserialize)]
pub struct TerminalSettings {
    enabled: Option<bool>,
    ssh: Option<ssh::SshChange>,
}

/// `PUT /terminal/settings`: lets paired devices in or keeps them out. Only
/// from this device; turning it off ends the sessions other devices have open.
pub async fn set_settings(State(s): State<SharedState>, user: AuthUser, Json(body): Json<TerminalSettings>) -> ApiResult<StatusCode> {
    user.require_person()?;
    if user.role != Role::Owner {
        return Err(NexusError::PermissionDenied("Only the owner can change this".into()).into());
    }
    if let Some(change) = body.ssh {
        ssh::change(&s, change).await?;
        audit(&s, &user, "terminal.shell.changed", None).await;
    }
    let Some(on) = body.enabled else { return Ok(StatusCode::NO_CONTENT) };
    s.db.set_setting(ENABLED_SETTING, if on { "1" } else { "0" }).await?;
    audit(&s, &user, if on { "terminal.enabled" } else { "terminal.disabled" }, None).await;
    if !on {
        let remote: Vec<Arc<Session>> = SESSIONS.lock().unwrap().values().filter(|x| x.owner.starts_with("device:")).cloned().collect();
        for sess in remote {
            sess.stop().await;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, Default)]
pub struct OpenBody {
    cwd: Option<String>,
}

/// `POST /terminal/sessions`: starts a shell.
pub async fn open(State(s): State<SharedState>, user: AuthUser, body: Option<Json<OpenBody>>) -> ApiResult<Json<SessionView>> {
    allowed(&s, &user).await?;
    tidy();
    let open_now = SESSIONS.lock().unwrap().values().filter(|x| x.screen.lock().unwrap().closed.is_none()).count();
    if open_now >= MAX_SESSIONS {
        return Err(NexusError::Conflict(format!("{open_now} terminals are already open on this device. Close one first")).into());
    }
    let body = body.map(|b| b.0).unwrap_or_default();
    let session = if ssh::load(&s).await.on {
        ssh::start(&s, user.owner_key(), body.cwd.as_deref()).await?
    } else {
        start_process(&s, user.owner_key(), body.cwd.as_deref()).await?
    };
    SESSIONS.lock().unwrap().insert(session.id.clone(), session.clone());
    audit(&s, &user, "terminal.opened", Some(session.shell)).await;
    Ok(Json(session.view(0)))
}

#[derive(Deserialize)]
pub struct ReadQuery {
    #[serde(default)]
    after: u64,
}

/// `GET /terminal/sessions/{id}?after=N`: output from `after` on, waiting
/// a few seconds for some if there is none yet.
pub async fn read(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>, Query(q): Query<ReadQuery>) -> ApiResult<Json<SessionView>> {
    allowed(&s, &user).await?;
    let sess = find(&user, &id)?;
    let notified = sess.changed.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();
    let quiet = {
        let screen = sess.screen.lock().unwrap();
        screen.next <= q.after && screen.closed.is_none()
    };
    if quiet {
        let _ = tokio::time::timeout(WAIT, notified).await;
    }
    Ok(Json(sess.view(q.after)))
}

#[derive(Deserialize)]
pub struct InputBody {
    text: String,
}

/// `POST /terminal/sessions/{id}/input`: runs what was typed.
pub async fn input(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>, Json(body): Json<InputBody>) -> ApiResult<StatusCode> {
    allowed(&s, &user).await?;
    let sess = find(&user, &id)?;
    if body.text.len() > INPUT_LIMIT {
        return Err(NexusError::invalid("That is too much to type at once").into());
    }
    let text = body.text.replace("\r\n", "\n");
    let text = text.trim_end_matches('\n');
    let (cwd, was_busy) = {
        let mut screen = sess.screen.lock().unwrap();
        if screen.closed.is_some() {
            return Err(NexusError::Conflict("This terminal has ended".into()).into());
        }
        screen.pending += 1;
        (screen.cwd.clone(), screen.pending > 1)
    };
    // Typed while something runs, it is an answer to that program: no prompt.
    let prompt = if was_busy { String::new() } else { sess.prompt(&cwd) };
    sess.push("in", &format!("{prompt}{text}\n"));
    if let Err(e) = write_lines(&sess, &format!("{text}\n{}\n", sess.marker_command())).await {
        sess.finished_input(cwd);
        return Err(e.into());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /terminal/sessions/{id}/stop`: ends the shell and whatever it is
/// running. Its output stays readable for a while.
pub async fn stop(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    allowed(&s, &user).await?;
    let sess = find(&user, &id)?;
    sess.stop().await;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /terminal/sessions/{id}`: ends the shell and forgets it.
pub async fn close(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    allowed(&s, &user).await?;
    let sess = find(&user, &id)?;
    sess.stop().await;
    SESSIONS.lock().unwrap().remove(&id);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /terminal/settings/test`: connects to the Termux (SSH) shell with
/// the settings saved, to check the setup. Only from this device.
pub async fn test_ssh(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<ssh::TestResult>> {
    user.require_person()?;
    if user.role != Role::Owner {
        return Err(NexusError::PermissionDenied("Only the owner can change this".into()).into());
    }
    Ok(Json(ssh::test(&s).await?))
}

/// The Termux shell: an SSH server on this device itself (Termux's `sshd`,
/// port 8022, or Android's Linux terminal with its port forwarded), signed in
/// to with a key HearthDrive makes. Android keeps each app's files to itself,
/// so this is how HearthDrive reaches Termux's Linux tools.
mod ssh {
    use super::*;
    use russh::ChannelMsg;
    use russh::client;
    use russh::keys::ssh_key::LineEnding;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use russh::keys::{PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate};

    const SETTING: &str = "terminal_ssh";
    const KEY_FILE: &str = "terminal-ssh-key";
    pub const DEFAULT_PORT: u16 = 8022;
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

    #[derive(Serialize, Deserialize, Clone)]
    pub struct SshSettings {
        pub on: bool,
        pub port: u16,
        pub user: String,
        /// The server's key, trusted the first time HearthDrive connected.
        pub host_key: Option<String>,
    }

    impl Default for SshSettings {
        fn default() -> Self {
            SshSettings { on: false, port: DEFAULT_PORT, user: "termux".into(), host_key: None }
        }
    }

    impl SshSettings {
        pub fn label(&self) -> &'static str {
            if self.port == DEFAULT_PORT { "Termux" } else { "Linux (SSH)" }
        }
    }

    #[derive(Serialize)]
    pub struct SshView {
        on: bool,
        port: u16,
        user: String,
        /// The line to add to `~/.ssh/authorized_keys` in Termux.
        public_key: String,
        /// Whether HearthDrive has connected and remembers the server's key.
        trusted: bool,
    }

    #[derive(Deserialize)]
    pub struct SshChange {
        on: Option<bool>,
        port: Option<u16>,
        user: Option<String>,
        /// Forget the server's key, after Termux was reinstalled.
        #[serde(default)]
        forget_host: bool,
    }

    #[derive(Serialize)]
    pub struct TestResult {
        message: String,
    }

    pub async fn load(s: &crate::state::AppState) -> SshSettings {
        s.db.get_setting(SETTING).await.ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
    }

    async fn save(s: &crate::state::AppState, cfg: &SshSettings) -> Result<(), NexusError> {
        s.db.set_setting(SETTING, &serde_json::to_string(cfg).unwrap_or_default()).await.map_err(|e| NexusError::Internal(e.to_string()))
    }

    pub fn view(s: &crate::state::AppState, cfg: &SshSettings) -> Result<SshView, NexusError> {
        Ok(SshView { on: cfg.on, port: cfg.port, user: cfg.user.clone(), public_key: public_line(&client_key(s)?)?, trusted: cfg.host_key.is_some() })
    }

    pub async fn change(s: &crate::state::AppState, change: SshChange) -> Result<(), NexusError> {
        let mut cfg = load(s).await;
        if let Some(on) = change.on {
            cfg.on = on;
        }
        if let Some(port) = change.port {
            if port == 0 {
                return Err(NexusError::invalid("Choose a port between 1 and 65535"));
            }
            if port != cfg.port {
                cfg.host_key = None;
            }
            cfg.port = port;
        }
        if let Some(user) = change.user {
            let user = user.trim();
            if user.is_empty() || user.len() > 64 || !user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
                return Err(NexusError::invalid("The user name can have letters, digits, dots, dashes and underscores"));
            }
            cfg.user = user.to_string();
        }
        if change.forget_host {
            cfg.host_key = None;
        }
        save(s, &cfg).await
    }

    /// HearthDrive's own key for signing in to the SSH server, made the first
    /// time it is needed and kept in the data folder.
    fn client_key(s: &crate::state::AppState) -> Result<PrivateKey, NexusError> {
        let path = s.data_dir.join(KEY_FILE);
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(key) = PrivateKey::from_openssh(&text)
        {
            return Ok(key);
        }
        let seed: [u8; 32] = *blake3::hash(nexus_devices::random_code(32).as_bytes()).as_bytes();
        let mut key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        key.set_comment("hearthdrive");
        let text = key.to_openssh(LineEnding::LF).map_err(|e| NexusError::Internal(e.to_string()))?;
        std::fs::write(&path, text.as_bytes()).map_err(|e| NexusError::Internal(format!("Could not save the terminal key: {e}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(key)
    }

    fn public_line(key: &PrivateKey) -> Result<String, NexusError> {
        key.public_key().to_openssh().map_err(|e| NexusError::Internal(e.to_string()))
    }

    /// Trusts the server's key the first time, then only that key.
    pub struct HostCheck {
        expected: Option<String>,
        seen: Arc<Mutex<Option<String>>>,
        changed: Arc<AtomicBool>,
    }

    impl client::Handler for HostCheck {
        type Error = russh::Error;

        async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
            let PublicKeyOrCertificate::PublicKey { key, .. } = key else { return Ok(false) };
            // Compare the key alone, without its comment.
            let text = key.to_openssh().unwrap_or_default().split_whitespace().take(2).collect::<Vec<_>>().join(" ");
            *self.seen.lock().unwrap() = Some(text.clone());
            match &self.expected {
                Some(expected) if *expected != text => {
                    self.changed.store(true, Ordering::SeqCst);
                    Ok(false)
                }
                _ => Ok(true),
            }
        }
    }

    async fn connect(s: &crate::state::AppState) -> Result<(client::Handle<HostCheck>, SshSettings), NexusError> {
        let mut cfg = load(s).await;
        let key = client_key(s)?;
        let seen = Arc::new(Mutex::new(None));
        let changed = Arc::new(AtomicBool::new(false));
        let handler = HostCheck { expected: cfg.host_key.clone(), seen: seen.clone(), changed: changed.clone() };
        let config = Arc::new(client::Config { keepalive_interval: Some(Duration::from_secs(30)), ..Default::default() });
        let unreachable = |cfg: &SshSettings| {
            NexusError::Conflict(format!("Could not reach {} on port {}. Open Termux and run sshd, then try again", cfg.label(), cfg.port))
        };
        let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, client::connect(config, ("127.0.0.1", cfg.port), handler)).await {
            Ok(Ok(h)) => h,
            _ if changed.load(Ordering::SeqCst) => {
                return Err(NexusError::Conflict(
                    "The SSH server's key has changed since HearthDrive last connected. If you reinstalled Termux, choose Forget the server in Settings › Terminal".into(),
                ));
            }
            _ => return Err(unreachable(&cfg)),
        };
        let auth = handle
            .authenticate_publickey(cfg.user.clone(), PrivateKeyWithHashAlg::new(Arc::new(key), None))
            .await
            .map_err(|_| unreachable(&cfg))?;
        if !auth.success() {
            return Err(NexusError::Conflict(
                "Termux did not accept HearthDrive's key. Run the setup command from Settings › Terminal in Termux, then try again".into(),
            ));
        }
        if cfg.host_key.is_none() {
            let seen = seen.lock().unwrap().clone();
            if let Some(key) = seen {
                cfg.host_key = Some(key);
                save(s, &cfg).await?;
            }
        }
        Ok((handle, cfg))
    }

    pub async fn test(s: &crate::state::AppState) -> Result<TestResult, NexusError> {
        let (handle, cfg) = connect(s).await?;
        let failed = |e: russh::Error| NexusError::Conflict(format!("Connected, but could not run a command: {e}"));
        let mut channel = handle.channel_open_session().await.map_err(failed)?;
        channel.exec(true, "uname -sm").await.map_err(failed)?;
        let mut out = Vec::new();
        let _ = tokio::time::timeout(CONNECT_TIMEOUT, async {
            while let Some(msg) = channel.wait().await {
                match msg {
                    ChannelMsg::Data { data } => out.extend_from_slice(&data),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
        })
        .await;
        let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        let system = String::from_utf8_lossy(&out).trim().to_string();
        Ok(TestResult {
            message: if system.is_empty() { format!("Connected to {}", cfg.label()) } else { format!("Connected to {} ({system})", cfg.label()) },
        })
    }

    /// Opens a shell over SSH. It has no terminal attached, like the local
    /// one: lines go in, output comes back.
    pub async fn start(s: &crate::state::AppState, owner: String, asked_dir: Option<&str>) -> Result<Arc<Session>, NexusError> {
        let (handle, cfg) = connect(s).await?;
        let label = cfg.label();
        let failed = |e: russh::Error| NexusError::Conflict(format!("Could not open a shell in {label}: {e}"));
        let channel = handle.channel_open_session().await.map_err(failed)?;
        channel.request_shell(true).await.map_err(failed)?;
        let (mut read, write) = channel.split();
        let stdin: Writer = Box::new(write.make_writer());
        let session = new_session(owner, label, true, Ender::Ssh(tokio::sync::Mutex::new(Some(handle))), stdin, "~".into());

        let watched = session.clone();
        tokio::spawn(async move {
            let mut out = Stream::new(&watched, false);
            let mut err = Stream::new(&watched, true);
            let mut code = None;
            loop {
                tokio::select! {
                    msg = read.wait() => match msg {
                        Some(ChannelMsg::Data { data }) => out.feed(&watched, &data),
                        Some(ChannelMsg::ExtendedData { data, .. }) => err.feed(&watched, &data),
                        Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(i64::from(exit_status)),
                        Some(ChannelMsg::Close) | None => break,
                        _ => {}
                    },
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {
                        if watched.idle_for() > IDLE_CLOSE {
                            watched.stop().await;
                        }
                    }
                }
            }
            out.finish(&watched);
            err.finish(&watched);
            let why = watched.ended_why(code);
            watched.close(&why);
            // Hangs up, if the shell ended by itself.
            if let Ender::Ssh(handle) = &watched.ender
                && let Some(h) = handle.lock().await.take()
            {
                let _ = h.disconnect(russh::Disconnect::ByApplication, "", "en").await;
            }
        });

        let init = match asked_dir {
            Some(dir) => format!("cd {} 2>/dev/null", quote_posix(dir)),
            None => "cd ~ 2>/dev/null".to_string(),
        };
        write_lines(&session, &format!("{init}\n{}\n", session.marker_command())).await?;
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> MarkerFilter {
        MarkerFilter { marker: "__m__".into(), held: String::new() }
    }

    #[test]
    fn markers_are_taken_out_of_the_output() {
        let mut f = filter();
        assert_eq!(f.feed("hello\n__m__C:\\Users\r\n"), ("hello\n".into(), vec!["C:\\Users".into()]));
        // Split across reads, and output without a newline before it.
        assert_eq!(f.feed("abc__"), ("abc".into(), vec![]));
        assert_eq!(f.feed("m__/home\nnext"), ("next".into(), vec!["/home".into()]));
        assert_eq!(f.feed(" line\nend__m__/tmp\n"), (" line\nend\n".into(), vec!["/tmp".into()]));
    }

    #[test]
    fn a_split_character_waits_for_the_rest() {
        let mut bytes = "é".as_bytes()[..1].to_vec();
        assert_eq!(take_text(&mut bytes), "");
        bytes.extend_from_slice(&"é!".as_bytes()[1..]);
        assert_eq!(take_text(&mut bytes), "é!");
        assert!(bytes.is_empty());
    }

    fn caller(role: Role, device: bool, limited: bool) -> AuthUser {
        AuthUser {
            user_id: None,
            username: "someone".into(),
            role,
            local: false,
            device_id: device.then(|| "phone".into()),
            allowed_roots: limited.then(Vec::new),
            shared: Vec::new(),
            via_token: false,
        }
    }

    #[test]
    fn only_the_owner_and_trusted_devices() {
        assert!(may_use(&caller(Role::Owner, false, false)).is_ok());
        assert!(may_use(&caller(Role::Administrator, false, false)).is_err());
        assert!(may_use(&caller(Role::Editor, true, false)).is_ok());
        assert!(may_use(&caller(Role::Viewer, true, false)).is_err());
        assert!(may_use(&caller(Role::Editor, true, true)).is_err());
        let mut program = caller(Role::Owner, false, false);
        program.via_token = true;
        assert!(may_use(&program).is_err());
    }
}
