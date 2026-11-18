//! People who can sign in to this node (from a browser on this computer).
//!
//! The first account is the owner, created during setup. Owners and
//! administrators can add accounts with a lower role, change them and remove
//! them. The last owner can never be removed or demoted.
//!
//! People other than owners and administrators can be limited to some
//! storage locations, here or on paired devices. The limit applies from
//! their next request, wherever it is sent.
//!
//! An account can be locked (it cannot sign in and is signed out
//! everywhere), and someone who lost their authenticator app can have
//! two-step sign-in turned off for them.

use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use nexus_common::NexusError;
use nexus_common::types::{Role, User};
use nexus_security::{Action, hash_password};
use serde::{Deserialize, Serialize};

fn validate_username(username: &str) -> Result<(), NexusError> {
    let ok = (3..=32).contains(&username.len())
        && username.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok { Ok(()) } else { Err(NexusError::invalid("Usernames are 3–32 letters, digits, '.', '_' or '-'")) }
}

fn validate_password(password: &str) -> Result<(), NexusError> {
    if password.chars().count() >= 8 {
        Ok(())
    } else {
        Err(NexusError::invalid("Use a password of at least 8 characters"))
    }
}

async fn hash(password: String) -> Result<String, NexusError> {
    tokio::task::spawn_blocking(move || hash_password(&password)).await.map_err(NexusError::internal)?
}

/// An account with how it signs in.
#[derive(Serialize)]
pub struct UserView {
    #[serde(flatten)]
    user: User,
    two_factor: bool,
    passkeys: i64,
    locked_at: Option<i64>,
}

async fn views(s: &SharedState, users: Vec<User>) -> Result<Vec<UserView>, NexusError> {
    let security = s.db.user_security().await?;
    Ok(users
        .into_iter()
        .map(|user| {
            let found = security.iter().find(|x| x.user_id == user.id);
            UserView {
                two_factor: found.is_some_and(|x| x.two_factor),
                passkeys: found.map_or(0, |x| x.passkeys),
                locked_at: found.and_then(|x| x.locked_at),
                user,
            }
        })
        .collect())
}

async fn view(s: &SharedState, id: &str) -> Result<UserView, NexusError> {
    let user = s.db.get_user(id).await?;
    Ok(views(s, vec![user]).await?.remove(0))
}

pub async fn list(State(s): State<SharedState>, user: AuthUser) -> ApiResult<Json<Vec<UserView>>> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    Ok(Json(views(&s, s.db.list_users().await?).await?))
}

#[derive(Deserialize)]
pub struct NewUser {
    username: String,
    password: String,
    role: Role,
}

pub async fn create(State(s): State<SharedState>, user: AuthUser, Json(req): Json<NewUser>) -> ApiResult<(StatusCode, Json<User>)> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let username = req.username.trim().to_string();
    validate_username(&username)?;
    validate_password(&req.password)?;
    if matches!(req.role, Role::Owner) && !matches!(user.role, Role::Owner) {
        return Err(NexusError::PermissionDenied("Only an owner can add another owner".into()).into());
    }
    let created = s.db.create_user(&username, &hash(req.password).await?, req.role).await?;
    audit(&s, &user, "user.created", Some(&format!("{username} ({})", req.role.as_str()))).await;
    Ok((StatusCode::CREATED, Json(created)))
}

#[derive(Deserialize)]
pub struct UpdateUser {
    role: Option<Role>,
    password: Option<String>,
    /// Limit the person to these storage locations (here or on paired devices)…
    allowed_roots: Option<Vec<String>>,
    /// …or let them see everything again.
    all_roots: Option<bool>,
    /// Lock the account (signing it out everywhere) or unlock it.
    locked: Option<bool>,
    /// `false` turns two-step sign-in off for someone who lost their app.
    two_factor: Option<bool>,
}

/// Root ids are generated ids; anything else is a mistake or an attack.
fn valid_root_ids(list: &[String]) -> bool {
    list.len() <= 500
        && list.iter().all(|id| (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

pub async fn update(
    State(s): State<SharedState>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateUser>,
) -> ApiResult<Json<UserView>> {
    user.require_person()?;
    let target = s.db.get_user(&id).await?;
    let own_account = user.user_id.as_deref() == Some(id.as_str());
    // Anyone may change their own password; everything else needs the right.
    let changes_access =
        req.role.is_some() || req.allowed_roots.is_some() || req.all_roots.is_some() || req.locked.is_some() || req.two_factor.is_some();
    if !(own_account && !changes_access) {
        user.require(Action::ManageUsers)?;
    }
    let caller_is_owner = matches!(user.role, Role::Owner);
    if matches!(target.role, Role::Owner) && !caller_is_owner {
        return Err(NexusError::PermissionDenied("Only an owner can change an owner".into()).into());
    }
    // Check everything first, so a refused request changes nothing.
    if let Some(role) = req.role {
        if matches!(role, Role::Owner) && !caller_is_owner {
            return Err(NexusError::PermissionDenied("Only an owner can make someone an owner".into()).into());
        }
        if matches!(target.role, Role::Owner) && !matches!(role, Role::Owner) && s.db.count_owners().await? <= 1 {
            return Err(NexusError::Conflict("This is the only owner. Make someone else an owner first.".into()).into());
        }
    }
    let access = if req.all_roots == Some(true) || req.allowed_roots.is_some() {
        let role = req.role.unwrap_or(target.role);
        let mut chosen = if req.all_roots == Some(true) { None } else { req.allowed_roots };
        if chosen.is_some() && matches!(role, Role::Owner | Role::Administrator) {
            return Err(NexusError::invalid("Owners and administrators always see all storage").into());
        }
        if let Some(list) = &mut chosen {
            if !valid_root_ids(list) {
                return Err(NexusError::invalid("Unknown storage location").into());
            }
            list.sort();
            list.dedup();
        }
        Some(chosen)
    } else {
        None
    };
    if let Some(password) = &req.password {
        validate_password(password)?;
    }
    if req.locked == Some(true) {
        if own_account {
            return Err(NexusError::invalid("You cannot lock your own account").into());
        }
        if matches!(target.role, Role::Owner) && !s.db.user_locked(&id).await? && s.db.count_unlocked_owners().await? <= 1 {
            return Err(NexusError::Conflict("This is the only owner who can sign in. It cannot be locked.".into()).into());
        }
    }
    if req.two_factor == Some(true) {
        return Err(NexusError::invalid("People turn on two-step sign-in themselves, in Settings → Security").into());
    }
    if req.two_factor == Some(false) && own_account {
        return Err(NexusError::invalid("Turn off your own two-step sign-in in Settings → Security").into());
    }

    if let Some(role) = req.role {
        s.db.set_user_role(&id, role).await?;
        audit(&s, &user, "user.role.changed", Some(&format!("{} → {}", target.username, role.as_str()))).await;
    }
    if let Some(chosen) = access {
        s.db.set_user_roots(&id, chosen.as_deref()).await?;
        let summary = match &chosen {
            None => "all storage".to_string(),
            Some(list) => format!("{} storage location(s)", list.len()),
        };
        audit(&s, &user, "user.access.changed", Some(&format!("{} → {summary}", target.username))).await;
    }
    if let Some(password) = req.password {
        s.db.set_user_password(&id, &hash(password).await?).await?;
        audit(&s, &user, "user.password.changed", Some(&target.username)).await;
    }
    if let Some(locked) = req.locked
        && locked != s.db.user_locked(&id).await?
    {
        s.db.set_user_locked(&id, locked).await?;
        audit(&s, &user, if locked { "user.locked" } else { "user.unlocked" }, Some(&target.username)).await;
    }
    if req.two_factor == Some(false) && super::security::two_factor_on(&s, &id).await? {
        s.db.disable_totp(&id).await?;
        audit(&s, &user, "user.2fa.reset", Some(&target.username)).await;
    }
    Ok(Json(view(&s, &id).await?))
}

pub async fn remove(State(s): State<SharedState>, user: AuthUser, Path(id): Path<String>) -> ApiResult<StatusCode> {
    user.require_person()?;
    user.require(Action::ManageUsers)?;
    let target = s.db.get_user(&id).await?;
    if user.user_id.as_deref() == Some(id.as_str()) {
        return Err(NexusError::invalid("You cannot remove your own account").into());
    }
    if matches!(target.role, Role::Owner) {
        if !matches!(user.role, Role::Owner) {
            return Err(NexusError::PermissionDenied("Only an owner can remove an owner".into()).into());
        }
        if s.db.count_owners().await? <= 1 {
            return Err(NexusError::Conflict("The only owner cannot be removed".into()).into());
        }
    }
    s.db.delete_user(&id).await?;
    audit(&s, &user, "user.removed", Some(&target.username)).await;
    Ok(StatusCode::NO_CONTENT)
}
