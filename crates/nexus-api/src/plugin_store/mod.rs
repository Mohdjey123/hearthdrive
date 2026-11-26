//! The plugin store: plugins that come with HearthDrive, installed by the
//! owner of a computer and run on its storage.
//!
//! Each plugin says what it needs (read the files it is run on, add new
//! files, rename files). Installing one grants exactly that, in the storage
//! locations the owner picks; the plugin then reaches files only through a
//! [`Sandbox`] that holds it to those grants. If a later version of a plugin
//! needs more, it stays off until the owner allows it again.
//!
//! Plugins people write themselves (a folder with a `plugin.json` that
//! runs a program) are in [`crate::plugins`]; the store lists them too.
//!
//! Only owners install, remove, set up or turn plugins on and off; owners
//! and administrators run them. Paired devices can do none of it: this API
//! is not open to them (see `AREAS` in `p2p.rs` and `PROXY_AREAS` in
//! `peers.rs`), and every call checks for a person as well.

mod builtin;
mod sandbox;

use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use nexus_common::time::now_ms;
use nexus_common::types::{FileEntry, Role};
use nexus_common::{NexusError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub use sandbox::Sandbox;

const STATE_SETTING: &str = "plugins.store";
/// How long one run may take; plugins stop there and say what is left.
const RUN_BUDGET: Duration = Duration::from_secs(240);

/// Serialises changes to what is installed.
static CHANGES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A kind of access a plugin can be given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Read the files and folders it is run on.
    Read,
    /// Add new files (never replacing any).
    Add,
    /// Rename files where they are.
    Rename,
}

impl Access {
    pub fn verb(self) -> &'static str {
        match self {
            Access::Read => "read files",
            Access::Add => "add files",
            Access::Rename => "rename files",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Access::Read => "Read the files and folders you use it on",
            Access::Add => "Add new files next to them (it never replaces or deletes anything)",
            Access::Rename => "Rename the files you use it on",
        }
    }
}

/// What a plugin's command can be used on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    File,
    Folder,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandDef {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    #[serde(rename = "for")]
    pub target: Target,
    /// For files: only these extensions (none: any file).
    pub extensions: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChoiceOption {
    pub value: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingKind {
    Number { min: i64, max: i64, default: i64, unit: &'static str },
    Choice { options: Vec<ChoiceOption>, default: &'static str },
    Toggle { default: bool },
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingDef {
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    #[serde(flatten)]
    pub kind: SettingKind,
}

impl SettingDef {
    fn default_value(&self) -> Value {
        match &self.kind {
            SettingKind::Number { default, .. } => Value::from(*default),
            SettingKind::Choice { default, .. } => Value::from(*default),
            SettingKind::Toggle { default } => Value::from(*default),
        }
    }

    fn check(&self, v: &Value) -> Result<Value> {
        let bad = || NexusError::invalid(format!("{}: not a value this setting takes", self.label));
        Ok(match &self.kind {
            SettingKind::Number { min, max, .. } => {
                let n = v.as_i64().ok_or_else(bad)?;
                if n < *min || n > *max {
                    return Err(NexusError::invalid(format!("{}: between {min} and {max}", self.label)));
                }
                Value::from(n)
            }
            SettingKind::Choice { options, .. } => {
                let s = v.as_str().ok_or_else(bad)?;
                options.iter().find(|o| o.value == s).ok_or_else(bad)?;
                Value::from(s)
            }
            SettingKind::Toggle { .. } => Value::from(v.as_bool().ok_or_else(bad)?),
        })
    }
}

/// A plugin's settings, each with its default filled in.
pub struct Values(Map<String, Value>);

impl Values {
    fn new(defs: &[SettingDef], saved: &Map<String, Value>) -> Self {
        let mut map = Map::new();
        for d in defs {
            let v = saved.get(d.key).and_then(|v| d.check(v).ok()).unwrap_or_else(|| d.default_value());
            map.insert(d.key.into(), v);
        }
        Self(map)
    }

    pub fn number(&self, key: &str) -> i64 {
        self.0.get(key).and_then(Value::as_i64).unwrap_or_default()
    }

    pub fn on(&self, key: &str) -> bool {
        self.0.get(key).and_then(Value::as_bool).unwrap_or_default()
    }

    pub fn choice(&self, key: &str) -> &str {
        self.0.get(key).and_then(Value::as_str).unwrap_or_default()
    }
}

/// The file or folder a command runs on.
pub struct Subject {
    /// Relative to the storage location.
    pub rel: String,
    pub name: String,
    pub is_dir: bool,
    /// The folder results go into: the folder itself, or the file's folder.
    pub folder: String,
}

/// What a run did, for the person who started it.
#[derive(Default)]
pub struct Report {
    pub lines: Vec<String>,
    /// Something went wrong with some of the files.
    pub problems: bool,
    pub stopped_early: bool,
}

impl Report {
    pub fn say(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    /// A count and up to ten names under it.
    pub fn list(&mut self, heading: String, names: &[String]) {
        if names.is_empty() {
            return;
        }
        self.lines.push(heading);
        for n in names.iter().take(10) {
            self.lines.push(format!("  {n}"));
        }
        if names.len() > 10 {
            self.lines.push(format!("  and {} more", names.len() - 10));
        }
    }
}

type RunFn = fn(&mut Sandbox, &Subject, &Values, &str) -> Result<Report>;

/// A plugin in the store.
#[derive(Clone, Serialize)]
pub struct Plugin {
    pub id: &'static str,
    pub name: &'static str,
    /// One line, for the list.
    pub summary: &'static str,
    /// What it does, in a few sentences.
    pub description: &'static str,
    /// Photos, Files, Documents or Storage.
    pub category: &'static str,
    pub version: &'static str,
    pub needs: Vec<Access>,
    pub commands: Vec<CommandDef>,
    pub settings: Vec<SettingDef>,
    #[serde(skip)]
    pub run: RunFn,
}

pub fn catalogue() -> Vec<Plugin> {
    builtin::all()
}

fn find(id: &str) -> Result<Plugin> {
    catalogue().into_iter().find(|p| p.id == id).ok_or_else(|| NexusError::NotFound("plugin".into()))
}

/// A plugin as installed on this computer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Installed {
    pub enabled: bool,
    /// What the owner allowed.
    pub granted: Vec<Access>,
    /// The storage locations it may work in (`None`: all of them).
    pub roots: Option<Vec<String>>,
    #[serde(default)]
    pub settings: Map<String, Value>,
    pub installed_at: i64,
    #[serde(default)]
    pub installed_by: Option<String>,
}

impl Installed {
    /// The plugin now needs something it was not given.
    fn needs_more(&self, p: &Plugin) -> bool {
        p.needs.iter().any(|n| !self.granted.contains(n))
    }

    fn may_use(&self, root_id: &str) -> bool {
        self.roots.as_ref().is_none_or(|r| r.iter().any(|x| x == root_id))
    }

    /// Whether its commands run now.
    fn active(&self, p: &Plugin) -> bool {
        self.enabled && !self.needs_more(p)
    }
}

async fn installed(s: &AppState) -> BTreeMap<String, Installed> {
    s.db.get_setting(STATE_SETTING).await.ok().flatten().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

async fn save(s: &AppState, all: &BTreeMap<String, Installed>) -> Result<()> {
    s.db.set_setting(STATE_SETTING, &serde_json::to_string(all).unwrap()).await
}

/// A plugin in the store, with whether and how it is installed here.
#[derive(Serialize)]
pub struct StoreEntry {
    #[serde(flatten)]
    plugin: Plugin,
    needs_text: Vec<&'static str>,
    installed: Option<InstalledView>,
}

#[derive(Serialize)]
struct InstalledView {
    enabled: bool,
    /// It needs more than it was given, so it is off until allowed again.
    needs_more: bool,
    roots: Option<Vec<String>>,
    settings: Map<String, Value>,
    installed_at: i64,
}

fn entry(p: Plugin, inst: Option<&Installed>) -> StoreEntry {
    let installed = inst.map(|i| InstalledView {
        enabled: i.enabled,
        needs_more: i.needs_more(&p),
        roots: i.roots.clone(),
        settings: Values::new(&p.settings, &i.settings).0,
        installed_at: i.installed_at,
    });
    StoreEntry { needs_text: p.needs.iter().map(|a| a.label()).collect(), plugin: p, installed }
}

/// Every plugin in the store, as installed here.
pub async fn entries(s: &AppState) -> Vec<StoreEntry> {
    let all = installed(s).await;
    catalogue().into_iter().map(|p| { let i = all.get(p.id).cloned(); entry(p, i.as_ref()) }).collect()
}

/// Owners and administrators signed in on this computer (not paired devices,
/// not programs with a token).
pub(crate) fn may_run(user: &AuthUser) -> Result<()> {
    user.require_person().map_err(|e| e.0)?;
    if user.via_token || !matches!(user.role, Role::Owner | Role::Administrator) {
        return Err(NexusError::PermissionDenied("Only an owner or administrator can use plugins".into()));
    }
    Ok(())
}

/// Only the owner installs plugins or changes what they may do.
pub(crate) fn may_manage(user: &AuthUser) -> Result<()> {
    may_run(user)?;
    if user.role != Role::Owner {
        return Err(NexusError::PermissionDenied("Only the owner can install plugins or change them".into()));
    }
    Ok(())
}

async fn check_roots(s: &AppState, roots: &Option<Vec<String>>) -> Result<()> {
    if let Some(list) = roots {
        if list.is_empty() {
            return Err(NexusError::invalid("Pick at least one storage location"));
        }
        for id in list {
            s.db.get_root(id).await?;
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct InstallBody {
    /// `None`: every storage location.
    #[serde(default)]
    roots: Option<Vec<String>>,
}

/// `POST /plugins/store/{id}`: installs a plugin, granting what it needs in
/// the storage locations given. It is on straight away.
pub async fn install(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<InstallBody>,
) -> ApiResult<Json<StoreEntry>> {
    may_manage(&user)?;
    let plugin = find(&id)?;
    check_roots(&s, &body.roots).await?;
    let _lock = CHANGES.lock().await;
    let mut all = installed(&s).await;
    let previous = all.remove(&id);
    let inst = Installed {
        enabled: true,
        granted: plugin.needs.clone(),
        roots: body.roots,
        settings: previous.map(|p| p.settings).unwrap_or_default(),
        installed_at: now_ms(),
        installed_by: user.user_id.clone(),
    };
    all.insert(id.clone(), inst.clone());
    save(&s, &all).await?;
    crate::routes::audit(&s, &user, "plugin.installed", Some(&id)).await;
    Ok(Json(entry(plugin, Some(&inst))))
}

/// `DELETE /plugins/store/{id}`: removes a plugin and its settings. Files it
/// made stay.
pub async fn remove(State(s): State<SharedState>, user: AuthUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<StoreEntry>> {
    may_manage(&user)?;
    let plugin = find(&id)?;
    let _lock = CHANGES.lock().await;
    let mut all = installed(&s).await;
    if all.remove(&id).is_some() {
        save(&s, &all).await?;
        crate::routes::audit(&s, &user, "plugin.removed", Some(&id)).await;
    }
    Ok(Json(entry(plugin, None)))
}

#[derive(Deserialize)]
pub struct ChangeBody {
    #[serde(default)]
    enabled: Option<bool>,
    /// Present: change where it may work (`null`: everywhere).
    #[serde(default, deserialize_with = "present")]
    roots: Option<Option<Vec<String>>>,
    #[serde(default)]
    settings: Option<Map<String, Value>>,
}

/// A field that may be `null`: present (even as `null`) is `Some`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Option<Vec<String>>>, D::Error> {
    Option::<Vec<String>>::deserialize(d).map(Some)
}

/// `PATCH /plugins/store/{id}`: turns an installed plugin on or off, changes
/// where it may work, or its settings. Turning it on allows what it needs
/// now (which matters when an update made it need more).
pub async fn change(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<ChangeBody>,
) -> ApiResult<Json<StoreEntry>> {
    may_manage(&user)?;
    let plugin = find(&id)?;
    if let Some(Some(_)) = &body.roots {
        check_roots(&s, body.roots.as_ref().unwrap()).await?;
    }
    let _lock = CHANGES.lock().await;
    let mut all = installed(&s).await;
    let inst = all.get_mut(&id).ok_or_else(|| NexusError::invalid("Install this plugin first"))?;
    if let Some(on) = body.enabled {
        inst.enabled = on;
        if on {
            inst.granted = plugin.needs.clone();
        }
    }
    if let Some(roots) = body.roots {
        inst.roots = roots;
    }
    if let Some(changes) = body.settings {
        for (key, value) in changes {
            let def = plugin.settings.iter().find(|d| d.key == key).ok_or_else(|| NexusError::invalid(format!("{} has no setting {key}", plugin.name)))?;
            inst.settings.insert(key, def.check(&value)?);
        }
    }
    let inst = inst.clone();
    save(&s, &all).await?;
    crate::routes::audit(&s, &user, "plugin.changed", Some(&id)).await;
    Ok(Json(entry(plugin, Some(&inst))))
}

fn applies(c: &CommandDef, file: &FileEntry) -> bool {
    match c.target {
        Target::Folder => file.is_dir,
        Target::File => {
            !file.is_dir
                && (c.extensions.is_empty() || file.extension.as_deref().is_some_and(|e| c.extensions.iter().any(|x| x.eq_ignore_ascii_case(e))))
        }
    }
}

/// A store command that works on a file, for the puzzle button in Files.
#[derive(Serialize)]
pub struct Available {
    pub plugin: &'static str,
    pub plugin_name: &'static str,
    pub store: bool,
    #[serde(flatten)]
    pub command: CommandDef,
}

/// The commands of installed, active plugins that work on this file and may
/// work in its storage location.
pub async fn commands_for(s: &AppState, file: Option<&FileEntry>) -> Vec<Available> {
    let Some(file) = file else { return Vec::new() };
    let all = installed(s).await;
    let mut out = Vec::new();
    for p in catalogue() {
        let Some(inst) = all.get(p.id) else { continue };
        if !inst.active(&p) || !inst.may_use(&file.root_id) {
            continue;
        }
        for c in p.commands.iter().filter(|c| applies(c, file)) {
            out.push(Available { plugin: p.id, plugin_name: p.name, store: true, command: c.clone() });
        }
    }
    out
}

#[derive(Deserialize)]
pub struct RunBody {
    file_id: String,
}

/// `POST /plugins/store/{id}/commands/{command}`: runs a command on a file
/// or folder and waits for it.
pub async fn run(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath((id, command)): UrlPath<(String, String)>,
    Json(body): Json<RunBody>,
) -> ApiResult<Json<crate::plugins::RunResult>> {
    may_run(&user)?;
    let plugin = find(&id)?;
    let inst = installed(&s).await.remove(&id).ok_or_else(|| NexusError::invalid("This plugin is not installed"))?;
    if inst.needs_more(&plugin) {
        return Err(NexusError::PermissionDenied(format!("{} now needs more access. Look at it in Plugins to allow it.", plugin.name)).into());
    }
    if !inst.enabled {
        return Err(NexusError::PermissionDenied(format!("{} is turned off", plugin.name)).into());
    }
    let cmd = plugin.commands.iter().find(|c| c.id == command).cloned().ok_or_else(|| NexusError::NotFound("command".into()))?;
    let file = s.db.get_file(&body.file_id).await?;
    if !applies(&cmd, &file) {
        return Err(NexusError::invalid("This command does not work on that").into());
    }
    let root = s.db.get_root(&file.root_id).await?;
    if !inst.may_use(&root.id) {
        return Err(NexusError::PermissionDenied(format!("{} is not allowed in {}. Change where it may work in Plugins.", plugin.name, root.name)).into());
    }
    // The person needs the same access themselves.
    let writes = plugin.needs.iter().any(|a| *a != Access::Read);
    user.require_access(&root.id, &file.relative_path, if writes { Need::Write } else { Need::Read })?;
    s.storage.ensure_online(&root)?;
    crate::routes::audit(&s, &user, "plugin.run", Some(&format!("{id}/{command} {}", file.relative_path))).await;

    let granted: Vec<Access> = inst.granted.iter().copied().filter(|a| plugin.needs.contains(a)).collect();
    let subject = Subject {
        rel: file.relative_path.clone(),
        name: file.filename.clone(),
        is_dir: file.is_dir,
        folder: if file.is_dir { file.relative_path.clone() } else { file.parent_path.clone() },
    };
    let values = Values::new(&plugin.settings, &inst.settings);
    let started = Instant::now();
    let run = plugin.run;
    let root_for_run = root.clone();
    let (result, sandbox) = tokio::task::spawn_blocking(move || {
        let mut sb = Sandbox::new(root_for_run, granted, RUN_BUDGET);
        let r = run(&mut sb, &subject, &values, cmd.id);
        (r, sb)
    })
    .await
    .map_err(|e| NexusError::Internal(format!("The plugin stopped: {e}")))?;

    // Renamed files keep what HearthDrive knows about them (tags, comments).
    for (old, new) in &sandbox.renamed {
        let (parent, name) = nexus_storage::paths::split_parent(new);
        let (ext, mime) = nexus_storage::paths::describe(&name, false);
        if let Err(e) = s.db.move_entry(&root.id, old, new, &parent, &name, ext, mime, false).await {
            tracing::warn!(error = %e, "could not record a plugin's rename");
        }
    }
    if !sandbox.added.is_empty() || !sandbox.renamed.is_empty() {
        let folder = if file.is_dir { file.relative_path.clone() } else { file.parent_path.clone() };
        let _ = s.indexer.reindex_path(&root, &folder).await;
    }
    let took_ms = started.elapsed().as_millis() as u64;
    Ok(Json(match result {
        Ok(report) => crate::plugins::RunResult {
            exit_code: Some(if report.problems { 1 } else { 0 }),
            output: report.lines.join("\n"),
            took_ms,
            timed_out: report.stopped_early,
        },
        Err(e) => crate::plugins::RunResult { exit_code: Some(1), output: e.to_string(), took_ms, timed_out: false },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(role: Role) -> AuthUser {
        AuthUser {
            user_id: Some("u".into()),
            username: "u".into(),
            role,
            local: false,
            device_id: None,
            allowed_roots: None,
            shared: Vec::new(),
            via_token: false,
        }
    }

    #[test]
    fn only_people_on_this_computer_manage_plugins() {
        assert!(may_manage(&person(Role::Owner)).is_ok());
        assert!(may_manage(&person(Role::Administrator)).is_err());
        assert!(may_run(&person(Role::Administrator)).is_ok());
        assert!(may_run(&person(Role::Editor)).is_err());
        // A paired device, even acting for the owner.
        let mut device = person(Role::Owner);
        device.device_id = Some("phone".into());
        assert!(may_manage(&device).is_err());
        assert!(may_run(&device).is_err());
        // A program with a token.
        let mut program = person(Role::Owner);
        program.via_token = true;
        assert!(may_manage(&program).is_err());
    }

    #[test]
    fn the_catalogue_is_well_formed() {
        let all = catalogue();
        assert!(all.len() >= 6);
        let mut ids: Vec<_> = all.iter().map(|p| p.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "plugin ids are unique");
        for p in &all {
            assert!(!p.needs.is_empty() && p.needs.contains(&Access::Read), "{}", p.id);
            assert!(!p.commands.is_empty(), "{}", p.id);
            for s in &p.settings {
                // Every default is a value the setting accepts.
                s.check(&s.default_value()).unwrap();
            }
        }
    }

    #[test]
    fn a_plugin_that_needs_more_is_off_until_allowed() {
        let p = find("rename-by-date").unwrap();
        let mut inst = Installed { enabled: true, granted: vec![Access::Read], ..Default::default() };
        assert!(inst.needs_more(&p));
        assert!(!inst.active(&p));
        inst.granted = p.needs.clone();
        assert!(inst.active(&p));
    }

    #[test]
    fn settings_are_checked() {
        let p = find("web-copies").unwrap();
        let size = p.settings.iter().find(|s| s.key == "size").unwrap();
        assert!(size.check(&Value::from("big")).is_err());
        assert!(size.check(&Value::from("2048")).is_ok());
        let quality = p.settings.iter().find(|s| s.key == "quality").unwrap();
        assert!(quality.check(&Value::from(10)).is_err());
        assert!(quality.check(&Value::from(80)).is_ok());
        // Saved values that no longer fit fall back to the default.
        let mut saved = Map::new();
        saved.insert("quality".into(), Value::from(5000));
        assert_eq!(Values::new(&p.settings, &saved).number("quality"), 85);
    }

    #[test]
    fn storage_locations_limit_a_plugin() {
        let inst = Installed { roots: Some(vec!["a".into()]), ..Default::default() };
        assert!(inst.may_use("a"));
        assert!(!inst.may_use("b"));
        assert!(Installed::default().may_use("b"));
    }
}
