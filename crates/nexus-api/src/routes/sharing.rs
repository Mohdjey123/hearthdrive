//! Shared folders and comments.
//!
//! A shared folder gives people on this node, or paired devices, a folder in
//! a storage location they otherwise cannot see: to view, to edit, or only
//! to add files (a drop folder, whose contents stay hidden). Sharing adds to
//! the storage locations someone may see and never goes beyond their role:
//! a viewer given "edit" can still only view. Owners and administrators
//! decide what is shared.
//!
//! Groups of people ("Family") can be members too: everyone in the group
//! gets the folder, and people added to the group later get it as well.
//!
//! Anyone who may open a file may comment on it. Authors can delete their
//! comments; owners and administrators can delete any.

use super::audit;
use super::files::RootSummary;
use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::state::{AppState, SharedState};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use nexus_common::NexusError;
use nexus_common::types::StorageRoot;
use nexus_database::{Comment, FolderAccess, Group, SharedFolder};
use nexus_security::{Action, role_allows};
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The longest comment.
const MAX_COMMENT: usize = 4000;

/// Someone a folder is shared with, as the people managing it see them.
#[derive(Serialize)]
pub struct MemberView {
    member: String,
    access: FolderAccess,
    added_at: i64,
    /// "person", "device" or "group".
    kind: &'static str,
    name: String,
    /// Why they can do less (or more) than `access` says, if they can.
    note: Option<String>,
}

#[derive(Serialize)]
pub struct FolderView {
    id: String,
    root: RootSummary,
    path: String,
    name: String,
    created_by: String,
    created_at: i64,
    /// The folder's file id; `None` when it was moved or deleted outside Hearthdrive.
    file_id: Option<String>,
    members: Vec<MemberView>,
}

/// A person's or device's name, role and storage limits.
type Entry = (String, nexus_common::types::Role, Option<Vec<String>>);

/// Names, roles and limits of everyone a folder can be shared with.
struct Directory {
    people: HashMap<String, Entry>,
    devices: HashMap<String, Entry>,
    groups: HashMap<String, Group>,
}

impl Directory {
    async fn load(s: &AppState) -> Result<Self, NexusError> {
        let people = s.db.list_users().await?.into_iter().map(|u| {
            let limits = u.limited_roots();
            (u.id, (u.username, u.role, limits))
        });
        let devices = s.db.list_peer_devices().await?.into_iter().map(|d| {
            let limits = d.allowed_root_list();
            (d.id, (d.name, d.role, limits))
        });
        let groups = s.db.list_groups().await?.into_iter().map(|g| (g.id.clone(), g));
        Ok(Self { people: people.collect(), devices: devices.collect(), groups: groups.collect() })
    }

    /// Whether `member` is someone (or a group) here.
    fn knows(&self, member: &str) -> bool {
        match member.strip_prefix("group:") {
            Some(id) => self.groups.contains_key(id),
            None => self.find(member).is_some(),
        }
    }

    fn find(&self, member: &str) -> Option<(&'static str, &Entry)> {
        if let Some(id) = member.strip_prefix("user:") {
            return self.people.get(id).map(|p| ("person", p));
        }
        if let Some(id) = member.strip_prefix("device:") {
            return self.devices.get(id).map(|d| ("device", d));
        }
        None
    }

    fn view(&self, root: &StorageRoot, member: &nexus_database::FolderMember) -> MemberView {
        if let Some(id) = member.member.strip_prefix("group:") {
            let (name, note) = match self.groups.get(id) {
                Some(g) => {
                    let names: Vec<&str> = g.members.iter().filter_map(|u| self.people.get(u)).map(|p| p.0.as_str()).collect();
                    let note = if names.is_empty() { "Nobody is in it yet".to_string() } else { names.join(", ") };
                    (g.name.clone(), Some(note))
                }
                None => ("Removed group".to_string(), None),
            };
            return MemberView { member: member.member.clone(), access: member.access, added_at: member.added_at, kind: "group", name, note };
        }
        let (kind, name, note) = match self.find(&member.member) {
            Some((kind, (name, role, limits))) => {
                let writes = role_allows(*role, Action::Write);
                let note = if limits.as_ref().is_none_or(|l| l.contains(&root.id)) {
                    Some(format!("Already sees everything in {}", root.name))
                } else {
                    match member.access {
                        FolderAccess::Edit if !writes => Some("Their role only lets them view".to_string()),
                        FolderAccess::Add if !writes => Some("Their role does not let them add files".to_string()),
                        _ => None,
                    }
                };
                (kind, name.clone(), note)
            }
            None => ("person", "Removed".to_string(), None),
        };
        MemberView { member: member.member.clone(), access: member.access, added_at: member.added_at, kind, name, note }
    }
}

async fn view_of(s: &AppState, dir: &Directory, roots: &HashMap<String, StorageRoot>, folder: SharedFolder) -> ApiResult<FolderView> {
    let root = roots.get(&folder.root_id).ok_or_else(|| NexusError::NotFound("Storage location".into()))?;
    let file_id = if folder.path.is_empty() {
        None
    } else {
        s.db.get_file_by_path(&folder.root_id, &folder.path).await?.filter(|f| f.is_dir).map(|f| f.id)
    };
    Ok(FolderView {
        id: folder.id,
        root: RootSummary::from(root),
        members: folder.members.iter().map(|m| dir.view(root, m)).collect(),
        path: folder.path,
        name: folder.name,
        created_by: folder.created_by,
        created_at: folder.created_at,
        file_id,
    })
}

async fn roots_by_id(s: &AppState) -> Result<HashMap<String, StorageRoot>, NexusError> {
    Ok(s.db.list_roots().await?.into_iter().map(|r| (r.id.clone(), r)).collect())
}

/// Only owners and administrators, signed in here, share folders.
fn require_manager(user: &AuthUser) -> ApiResult<()> {
    user.require(Action::ManageUsers)?;
    user.require_person()?;
    Ok(())
}

/// `GET /shared-folders`: every shared folder on this node.
pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<FolderView>>> {
    require_manager(&user)?;
    let dir = Directory::load(&s).await?;
    let roots = roots_by_id(&s).await?;
    let mut out = Vec::new();
    for folder in s.db.list_shared_folders().await? {
        out.push(view_of(&s, &dir, &roots, folder).await?);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct MemberBody {
    member: String,
    access: FolderAccess,
}

/// Checks who a folder is to be shared with.
fn members_of(dir: &Directory, body: Vec<MemberBody>) -> Result<Vec<(String, FolderAccess)>, NexusError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in body {
        if !dir.knows(&m.member) {
            return Err(NexusError::invalid("Share with people or groups on this computer, or paired devices"));
        }
        if seen.insert(m.member.clone()) {
            out.push((m.member, m.access));
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
pub struct NewFolder {
    root_id: String,
    #[serde(default)]
    path: String,
    name: Option<String>,
    #[serde(default)]
    members: Vec<MemberBody>,
}

/// `POST /shared-folders`: shares a folder.
pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(req): Json<NewFolder>) -> ApiResult<(StatusCode, Json<FolderView>)> {
    require_manager(&user)?;
    user.require_root(&req.root_id)?;
    let root = s.db.get_root(&req.root_id).await?;
    let path = paths::normalize_relative(&req.path)?;
    let default_name = if path.is_empty() {
        root.name.clone()
    } else {
        match s.db.get_file_by_path(&root.id, &path).await? {
            Some(f) if f.is_dir => f.filename,
            _ => return Err(NexusError::NotFound("Folder".into()).into()),
        }
    };
    let name = req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&default_name).to_string();
    let dir = Directory::load(&s).await?;
    let members = members_of(&dir, req.members)?;
    let folder = s.db.create_shared_folder(&root.id, &path, &name, &user.username).await?;
    s.db.set_folder_members(&folder.id, &members).await?;
    audit(&s, &user, "folder.shared", Some(&format!("{}/{}", root.name, path))).await;
    let roots = roots_by_id(&s).await?;
    Ok((StatusCode::CREATED, Json(view_of(&s, &dir, &roots, s.db.get_shared_folder(&folder.id).await?).await?)))
}

#[derive(Deserialize)]
pub struct FolderChange {
    name: Option<String>,
    members: Option<Vec<MemberBody>>,
}

/// `PATCH /shared-folders/{id}`: renames it or changes who it is shared with.
pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<FolderChange>,
) -> ApiResult<Json<FolderView>> {
    require_manager(&user)?;
    let folder = s.db.get_shared_folder(&id).await?;
    user.require_root(&folder.root_id)?;
    let dir = Directory::load(&s).await?;
    if let Some(name) = req.name.as_deref().map(str::trim) {
        if name.is_empty() {
            return Err(NexusError::invalid("Give the shared folder a name").into());
        }
        s.db.rename_shared_folder(&id, name).await?;
    }
    if let Some(members) = req.members {
        s.db.set_folder_members(&id, &members_of(&dir, members)?).await?;
    }
    audit(&s, &user, "folder.share.changed", Some(&folder.name)).await;
    let roots = roots_by_id(&s).await?;
    Ok(Json(view_of(&s, &dir, &roots, s.db.get_shared_folder(&id).await?).await?))
}

/// `DELETE /shared-folders/{id}`: stops sharing. The files stay.
pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    require_manager(&user)?;
    let folder = s.db.get_shared_folder(&id).await?;
    user.require_root(&folder.root_id)?;
    s.db.delete_shared_folder(&id).await?;
    audit(&s, &user, "folder.unshared", Some(&folder.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

/// A folder shared with the caller.
#[derive(Serialize)]
pub struct MineView {
    id: String,
    name: String,
    root: RootSummary,
    path: String,
    /// What the caller may do there, after their role.
    access: FolderAccess,
    shared_by: String,
    /// They see all of its storage location anyway.
    sees_all: bool,
}

/// `GET /shared-folders/mine`: folders shared with the caller.
pub async fn mine(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<MineView>>> {
    user.require(Action::Read)?;
    let roots = roots_by_id(&s).await?;
    let writes = role_allows(user.role, Action::Write);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for g in &user.shared {
        let Some(root) = roots.get(&g.root_id) else { continue };
        let access = match g.access {
            FolderAccess::Edit if !writes => FolderAccess::View,
            FolderAccess::Add if !writes => continue,
            a => a,
        };
        if seen.insert(g.folder_id.clone()) {
            out.push(MineView {
                id: g.folder_id.clone(),
                name: g.name.clone(),
                root: RootSummary::from(root),
                path: g.path.clone(),
                access,
                shared_by: g.created_by.clone(),
                sees_all: user.sees_root(&g.root_id),
            });
        }
    }
    Ok(Json(out))
}

// -- groups -----------------------------------------------------------------

#[derive(Serialize)]
pub struct GroupView {
    id: String,
    name: String,
    created_at: i64,
    members: Vec<GroupMemberView>,
    /// Folders shared with the group.
    folders: Vec<String>,
}

#[derive(Serialize)]
pub struct GroupMemberView {
    user_id: String,
    username: String,
}

async fn group_views(s: &AppState, only: Option<&str>) -> Result<Vec<GroupView>, NexusError> {
    let people: HashMap<String, String> = s.db.list_users().await?.into_iter().map(|u| (u.id, u.username)).collect();
    let folders = s.db.list_shared_folders().await?;
    Ok(s.db
        .list_groups()
        .await?
        .into_iter()
        .filter(|g| only.is_none_or(|id| id == g.id))
        .map(|g| {
            let key = format!("group:{}", g.id);
            GroupView {
                folders: folders.iter().filter(|f| f.members.iter().any(|m| m.member == key)).map(|f| f.name.clone()).collect(),
                members: g
                    .members
                    .iter()
                    .filter_map(|u| people.get(u).map(|name| GroupMemberView { user_id: u.clone(), username: name.clone() }))
                    .collect(),
                id: g.id,
                name: g.name,
                created_at: g.created_at,
            }
        })
        .collect())
}

/// `GET /groups`
pub async fn groups(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<GroupView>>> {
    require_manager(&user)?;
    Ok(Json(group_views(&s, None).await?))
}

#[derive(Deserialize)]
pub struct GroupBody {
    name: Option<String>,
    /// The ids of the people in it.
    members: Option<Vec<String>>,
}

fn group_name(name: Option<&str>) -> Result<Option<String>, NexusError> {
    match name.map(str::trim) {
        None => Ok(None),
        Some("") => Err(NexusError::invalid("Give the group a name")),
        Some(n) if n.chars().count() > 60 => Err(NexusError::invalid("Group names are at most 60 characters")),
        Some(n) => Ok(Some(n.to_string())),
    }
}

async fn check_people(s: &AppState, ids: &[String]) -> Result<Vec<String>, NexusError> {
    let people: HashSet<String> = s.db.list_users().await?.into_iter().map(|u| u.id).collect();
    let mut out = Vec::new();
    for id in ids {
        if !people.contains(id) {
            return Err(NexusError::invalid("Only people on this computer can be in a group"));
        }
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// `POST /groups`
pub async fn create_group(State(s): State<SharedState>, user: AuthUser, Json(body): Json<GroupBody>) -> ApiResult<(StatusCode, Json<GroupView>)> {
    require_manager(&user)?;
    let name = group_name(body.name.as_deref())?.ok_or_else(|| NexusError::invalid("Give the group a name"))?;
    let members = check_people(&s, body.members.as_deref().unwrap_or_default()).await?;
    let group = s.db.create_group(&name, &user.username).await?;
    s.db.set_group_members(&group.id, &members).await?;
    audit(&s, &user, "group.created", Some(&name)).await;
    let view = group_views(&s, Some(&group.id)).await?.pop().ok_or_else(|| NexusError::NotFound("Group".into()))?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// `PATCH /groups/{id}`: renames it or changes who is in it.
pub async fn update_group(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(body): Json<GroupBody>,
) -> ApiResult<Json<GroupView>> {
    require_manager(&user)?;
    let group = s.db.get_group(&id).await?;
    if let Some(name) = group_name(body.name.as_deref())? {
        s.db.rename_group(&id, &name).await?;
    }
    if let Some(members) = body.members {
        let members = check_people(&s, &members).await?;
        s.db.set_group_members(&id, &members).await?;
    }
    audit(&s, &user, "group.changed", Some(&group.name)).await;
    let view = group_views(&s, Some(&id)).await?.pop().ok_or_else(|| NexusError::NotFound("Group".into()))?;
    Ok(Json(view))
}

/// `DELETE /groups/{id}`: its folders stay, shared with whoever else they are.
pub async fn delete_group(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    require_manager(&user)?;
    let group = s.db.get_group(&id).await?;
    s.db.delete_group(&id).await?;
    audit(&s, &user, "group.removed", Some(&group.name)).await;
    Ok(StatusCode::NO_CONTENT)
}

// -- comments ---------------------------------------------------------------

#[derive(Serialize)]
pub struct CommentView {
    #[serde(flatten)]
    comment: Comment,
    /// The caller may delete it.
    can_delete: bool,
}

fn can_delete(user: &AuthUser, c: &Comment) -> bool {
    c.author_key == user.owner_key() || (user.require(Action::ManageUsers).is_ok() && user.require_person().is_ok())
}

/// Someone who may open the file.
async fn readable_file(s: &AppState, user: &AuthUser, id: &str) -> ApiResult<nexus_common::types::FileEntry> {
    user.require(Action::Read)?;
    let file = s.db.get_file(id).await?;
    user.require_access(&file.root_id, &file.relative_path, Need::Read)?;
    Ok(file)
}

/// `GET /files/{id}/comments`
pub async fn comments(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<Json<Vec<CommentView>>> {
    let file = readable_file(&s, &user, &id).await?;
    Ok(Json(
        s.db.comments_of(&file.id)
            .await?
            .into_iter()
            .map(|comment| CommentView { can_delete: can_delete(&user, &comment), comment })
            .collect(),
    ))
}

#[derive(Deserialize)]
pub struct NewComment {
    body: String,
}

/// `POST /files/{id}/comments`
pub async fn add_comment(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<NewComment>,
) -> ApiResult<(StatusCode, Json<CommentView>)> {
    let file = readable_file(&s, &user, &id).await?;
    let body = req.body.trim();
    if body.is_empty() {
        return Err(NexusError::invalid("Write a comment first").into());
    }
    if body.chars().count() > MAX_COMMENT {
        return Err(NexusError::invalid(format!("Comments are at most {MAX_COMMENT} characters")).into());
    }
    let comment = s.db.add_comment(&file.id, &file.root_id, &user.username, &user.owner_key(), body).await?;
    audit(&s, &user, "comment.added", Some(&file.relative_path)).await;
    Ok((StatusCode::CREATED, Json(CommentView { can_delete: true, comment })))
}

/// `DELETE /files/{id}/comments/{comment}`
pub async fn remove_comment(
    State(s): State<SharedState>,
    user: AuthUser,
    Path((id, comment_id)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let file = readable_file(&s, &user, &id).await?;
    let comment = s.db.get_comment(&comment_id).await?;
    if comment.file_id != file.id {
        return Err(NexusError::NotFound("Comment".into()).into());
    }
    if !can_delete(&user, &comment) {
        return Err(NexusError::PermissionDenied("Only its author or an administrator can delete a comment".into()).into());
    }
    s.db.delete_comment(&comment.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
