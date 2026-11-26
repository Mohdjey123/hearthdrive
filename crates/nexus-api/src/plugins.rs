//! Plugins: programs on this computer that HearthDrive can run on files and
//! folders ("Convert to PDF", "Make a web-sized copy", "Scan for viruses").
//!
//! Each plugin is a folder in `plugins` in HearthDrive's data folder with a
//! `plugin.json`:
//!
//! ```json
//! { "name": "PDF tools", "description": "…", "version": "1.0",
//!   "commands": [{ "id": "to-pdf", "label": "Convert to PDF", "for": "file",
//!                  "extensions": ["docx", "odt"], "program": "soffice",
//!                  "args": ["--headless", "--convert-to", "pdf", "{path}"] }] }
//! ```
//!
//! `program` is a file in the plugin's folder or a program on the PATH; it
//! runs directly (no shell) with `{path}`, `{name}`, `{folder}` and `{root}`
//! filled in, in the file's folder. Nothing runs until the owner turns the
//! plugin on, and a plugin whose `plugin.json` changes is off again until
//! they look at it once more. Only owners and administrators run plugins.

use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use nexus_common::types::Role;
use nexus_common::{NexusError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const ENABLED_SETTING: &str = "plugins.enabled";
const MAX_OUTPUT: usize = 64 * 1024;
const DEFAULT_TIMEOUT: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    /// `file`, `folder` or `none` (runs on nothing in particular).
    #[serde(rename = "for", default = "file")]
    pub applies_to: String,
    /// For files: only these extensions (none: any file).
    #[serde(default)]
    pub extensions: Vec<String>,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

fn file() -> String {
    "file".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginView {
    /// The plugin's folder name: how it is addressed.
    pub id: String,
    pub manifest: Option<Manifest>,
    /// Why `plugin.json` could not be read.
    pub error: Option<String>,
    pub enabled: bool,
    /// It was on, but its `plugin.json` changed since.
    pub changed: bool,
    pub folder: String,
}

pub fn dir(s: &AppState) -> PathBuf {
    s.data_dir.join("plugins")
}

fn fingerprint(raw: &[u8]) -> String {
    blake3::hash(raw).to_hex()[..32].to_string()
}

async fn approvals(s: &AppState) -> BTreeMap<String, String> {
    s.db.get_setting(ENABLED_SETTING).await.ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

/// Every plugin folder, read now.
pub async fn list(s: &AppState) -> Vec<PluginView> {
    let base = dir(s);
    let _ = tokio::fs::create_dir_all(&base).await;
    let approved = approvals(s).await;
    let mut out = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(&base).await else { return out };
    while let Ok(Some(e)) = entries.next_entry().await {
        if !e.file_type().await.is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let id = e.file_name().to_string_lossy().into_owned();
        let (manifest, error, print) = match tokio::fs::read(e.path().join("plugin.json")).await {
            Ok(raw) => match serde_json::from_slice::<Manifest>(&raw) {
                Ok(m) => (Some(m), None, Some(fingerprint(&raw))),
                Err(err) => (None, Some(format!("plugin.json: {err}")), None),
            },
            Err(_) => (None, Some("No plugin.json in this folder".into()), None),
        };
        let was = approved.get(&id);
        let enabled = print.is_some() && was == print.as_ref();
        out.push(PluginView { changed: was.is_some() && !enabled, enabled, id, manifest, error, folder: e.path().to_string_lossy().into_owned() });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn owner_or_admin(user: &AuthUser) -> Result<()> {
    user.require_person().map_err(|e| e.0)?;
    if user.via_token || !matches!(user.role, Role::Owner | Role::Administrator) {
        return Err(NexusError::PermissionDenied("Only an owner or administrator can use plugins".into()));
    }
    Ok(())
}

/// `GET /plugins`: the store's plugins and the ones in the plugins folder.
pub async fn get_all(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    owner_or_admin(&user)?;
    Ok(Json(serde_json::json!({
        "folder": dir(&s).to_string_lossy(),
        "plugins": list(&s).await,
        "store": crate::plugin_store::entries(&s).await,
    })))
}

#[derive(Deserialize)]
pub struct EnableBody {
    enabled: bool,
}

/// `PUT /plugins/{id}`: turns a plugin on (as its `plugin.json` is now) or
/// off. Owners only: a plugin runs programs on this computer.
pub async fn set_enabled(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<EnableBody>,
) -> ApiResult<Json<PluginView>> {
    owner_or_admin(&user)?;
    if user.role != Role::Owner {
        return Err(NexusError::PermissionDenied("Only an owner can turn plugins on or off".into()).into());
    }
    let mut approved = approvals(&s).await;
    if body.enabled {
        let raw = tokio::fs::read(dir(&s).join(&id).join("plugin.json")).await.map_err(|_| NexusError::NotFound("plugin".into()))?;
        serde_json::from_slice::<Manifest>(&raw).map_err(|e| NexusError::invalid(format!("plugin.json: {e}")))?;
        approved.insert(id.clone(), fingerprint(&raw));
    } else {
        approved.remove(&id);
    }
    s.db.set_setting(ENABLED_SETTING, &serde_json::to_string(&approved).unwrap()).await?;
    crate::routes::audit(&s, &user, if body.enabled { "plugin.enabled" } else { "plugin.disabled" }, Some(&id)).await;
    let view = list(&s).await.into_iter().find(|p| p.id == id).ok_or_else(|| NexusError::NotFound("plugin".into()))?;
    Ok(Json(view))
}

#[derive(Serialize)]
pub struct Available {
    plugin: String,
    plugin_name: String,
    #[serde(flatten)]
    command: Command,
}

#[derive(Deserialize)]
pub struct ForQuery {
    file_id: Option<String>,
}

fn applies(c: &Command, file: Option<&nexus_common::types::FileEntry>) -> bool {
    match (c.applies_to.as_str(), file) {
        ("none", _) => true,
        ("folder", Some(f)) => f.is_dir,
        ("file", Some(f)) if !f.is_dir => {
            c.extensions.is_empty()
                || f.extension.as_deref().is_some_and(|e| c.extensions.iter().any(|x| x.trim_start_matches('.').eq_ignore_ascii_case(e)))
        }
        _ => false,
    }
}

/// `GET /plugins/commands?file_id=`: what can run on this file or folder,
/// from the store's plugins first.
pub async fn commands(State(s): State<SharedState>, user: AuthUser, Query(q): Query<ForQuery>) -> ApiResult<Json<Vec<serde_json::Value>>> {
    if owner_or_admin(&user).is_err() {
        return Ok(Json(Vec::new()));
    }
    let file = match &q.file_id {
        Some(id) => Some(s.db.get_file(id).await?),
        None => None,
    };
    let mut out: Vec<serde_json::Value> =
        crate::plugin_store::commands_for(&s, file.as_ref()).await.into_iter().filter_map(|a| serde_json::to_value(a).ok()).collect();
    for p in list(&s).await.into_iter().filter(|p| p.enabled) {
        let Some(m) = p.manifest else { continue };
        for c in m.commands {
            if applies(&c, file.as_ref()) {
                out.extend(serde_json::to_value(Available { plugin: p.id.clone(), plugin_name: m.name.clone(), command: c }).ok());
            }
        }
    }
    Ok(Json(out))
}

#[derive(Serialize)]
pub struct RunResult {
    pub(crate) exit_code: Option<i32>,
    pub(crate) output: String,
    pub(crate) took_ms: u64,
    pub(crate) timed_out: bool,
}

#[derive(Deserialize)]
pub struct RunBody {
    file_id: Option<String>,
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut cut = MAX_OUTPUT;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push_str("\n…");
    }
    s
}

/// The program to start: one in the plugin's folder, or one on the PATH.
fn program(plugin_dir: &Path, name: &str) -> Result<(PathBuf, Vec<String>)> {
    let local = plugin_dir.join(name);
    let local = dunce_canonical(&local);
    let inside = local.as_ref().is_some_and(|l| l.starts_with(dunce_canonical(plugin_dir).unwrap_or_default()));
    let path = match local {
        Some(l) if inside && l.is_file() => l,
        _ if name.contains(['/', '\\']) => return Err(NexusError::invalid(format!("The plugin's program {name} is missing"))),
        _ => PathBuf::from(name),
    };
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    Ok(match ext.as_str() {
        "ps1" => (
            PathBuf::from("powershell.exe"),
            vec!["-NoProfile".into(), "-NonInteractive".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), path.to_string_lossy().into_owned()],
        ),
        "py" => (PathBuf::from("python"), vec![path.to_string_lossy().into_owned()]),
        _ => (path, Vec::new()),
    })
}

fn dunce_canonical(p: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(p).ok().map(|c| {
        let s = c.to_string_lossy().into_owned();
        PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
    })
}

/// `POST /plugins/{id}/commands/{command}`: runs a command and waits for it.
pub async fn run(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath((id, command)): UrlPath<(String, String)>,
    Json(body): Json<RunBody>,
) -> ApiResult<Json<RunResult>> {
    owner_or_admin(&user)?;
    let plugin = list(&s).await.into_iter().find(|p| p.id == id).ok_or_else(|| NexusError::NotFound("plugin".into()))?;
    if !plugin.enabled {
        return Err(NexusError::PermissionDenied("Turn this plugin on in Plugins first".into()).into());
    }
    let manifest = plugin.manifest.ok_or_else(|| NexusError::invalid("The plugin cannot be read"))?;
    let cmd = manifest.commands.into_iter().find(|c| c.id == command).ok_or_else(|| NexusError::NotFound("command".into()))?;
    let file = match &body.file_id {
        Some(fid) => Some(s.db.get_file(fid).await?),
        None => None,
    };
    if !applies(&cmd, file.as_ref()) {
        return Err(NexusError::invalid("This command does not work on that").into());
    }
    let plugin_dir = dir(&s).join(&id);
    let (mut vars, mut cwd, mut reindex) = (BTreeMap::new(), plugin_dir.clone(), None);
    if let Some(f) = &file {
        user.require_access(&f.root_id, &f.relative_path, Need::Write)?;
        let root = s.db.get_root(&f.root_id).await?;
        s.storage.ensure_online(&root)?;
        let path = s.storage.resolve(&root, &f.relative_path)?;
        let folder = if f.is_dir { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or_default() };
        vars.insert("{path}", path.to_string_lossy().into_owned());
        vars.insert("{name}", f.filename.clone());
        vars.insert("{folder}", folder.to_string_lossy().into_owned());
        vars.insert("{root}", root.absolute_path.clone());
        cwd = folder;
        reindex = Some((root, if f.is_dir { f.relative_path.clone() } else { f.parent_path.clone() }));
    }
    let (exe, mut args) = program(&plugin_dir, &cmd.program)?;
    for a in &cmd.args {
        let mut a = a.clone();
        for (k, v) in &vars {
            a = a.replace(k, v);
        }
        args.push(a);
    }
    let timeout = Duration::from_secs(cmd.timeout_secs.unwrap_or(DEFAULT_TIMEOUT).clamp(1, 3600));
    crate::routes::audit(&s, &user, "plugin.run", Some(&format!("{id}/{command} {}", vars.get("{path}").map(String::as_str).unwrap_or("")))).await;

    let mut c = tokio::process::Command::new(&exe);
    c.args(&args).current_dir(&cwd).stdin(std::process::Stdio::null()).kill_on_drop(true);
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    c.env("NEXUS_PLUGIN_DIR", &plugin_dir);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    let started = Instant::now();
    let child = c.spawn().map_err(|e| NexusError::invalid(format!("{} could not be started: {e}", exe.display())))?;
    let (exit_code, output, timed_out) = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.trim().is_empty() {
                text.push_str(&err);
            }
            (out.status.code(), clip(text), false)
        }
        Ok(Err(e)) => (None, e.to_string(), false),
        Err(_) => (None, format!("Stopped after {} seconds", timeout.as_secs()), true),
    };
    if let Some((root, rel)) = reindex {
        let _ = s.indexer.reindex_path(&root, &rel).await;
    }
    Ok(Json(RunResult { exit_code, output, took_ms: started.elapsed().as_millis() as u64, timed_out }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_manifest_and_matches_files() {
        let m: Manifest = serde_json::from_str(
            r#"{"name":"PDF","commands":[{"id":"a","label":"To PDF","extensions":[".docx"],"program":"x","args":["{path}"]},
                {"id":"b","label":"Folder","for":"folder","program":"y"}]}"#,
        )
        .unwrap();
        let mut f = nexus_common::types::FileEntry {
            id: "1".into(), root_id: "r".into(), relative_path: "a.DOCX".into(), parent_path: "".into(), filename: "a.DOCX".into(),
            is_dir: false, extension: Some("docx".into()), mime_type: None, size: 1, created_at: None, modified_at: None,
            content_hash: None, indexed_at: 0, availability: nexus_common::types::Availability::Online,
        };
        assert!(applies(&m.commands[0], Some(&f)));
        assert!(!applies(&m.commands[1], Some(&f)));
        f.extension = Some("txt".into());
        assert!(!applies(&m.commands[0], Some(&f)));
        f.is_dir = true;
        assert!(applies(&m.commands[1], Some(&f)));
    }

    #[test]
    fn programs_outside_the_plugin_folder_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("run.ps1"), "").unwrap();
        let (exe, args) = program(dir.path(), "run.ps1").unwrap();
        assert_eq!(exe, PathBuf::from("powershell.exe"));
        assert!(args.last().unwrap().ends_with("run.ps1"));
        assert!(program(dir.path(), "../../evil.exe").is_err());
        assert_eq!(program(dir.path(), "soffice").unwrap().0, PathBuf::from("soffice"));
    }
}
