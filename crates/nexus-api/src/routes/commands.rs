//! Remote commands: a fixed set of actions a person may run on this node
//! from another device (through the peer proxy) or from here. Nothing here
//! runs programs or arbitrary commands. Computers and phones both answer
//! them; putting the device to sleep is for Windows only.

use super::audit;
use crate::auth::AuthUser;
use crate::error::ApiResult;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{Path, State};
use nexus_common::NexusError;
use nexus_common::types::Role;
use serde::Serialize;

#[derive(Serialize)]
pub struct CommandInfo {
    name: &'static str,
    label: &'static str,
    description: &'static str,
}

const COMMANDS: &[CommandInfo] = &[
    CommandInfo { name: "rescan", label: "Look for changes", description: "Scans every connected storage location again." },
    CommandInfo { name: "check-updates", label: "Check for updates", description: "Asks for a newer HearthDrive version now." },
    CommandInfo { name: "pause-transfers", label: "Pause transfers", description: "Pauses every copy, sync and backup." },
    CommandInfo { name: "resume-transfers", label: "Resume transfers", description: "Continues paused copies, syncs and backups." },
    CommandInfo { name: "sleep", label: "Put to sleep", description: "Puts the computer to sleep. Wake it again from Devices." },
];

/// `GET /commands`: what can be run here.
pub async fn list(user: AuthUser) -> ApiResult<Json<Vec<&'static CommandInfo>>> {
    allowed(&user)?;
    let sleep = cfg!(windows);
    Ok(Json(COMMANDS.iter().filter(|c| c.name != "sleep" || sleep).collect()))
}

/// People here need to be an owner or administrator. A paired device needs a
/// role here that lets it change files (Editor, the role devices are paired
/// with, or higher); its own node already checked that the person using it is
/// an owner or administrator there before sending the command.
fn allowed(user: &AuthUser) -> Result<(), NexusError> {
    if user.device_id.is_some() {
        if !matches!(user.role, Role::Owner | Role::Administrator | Role::Editor) {
            return Err(NexusError::PermissionDenied(
                "This device's role here does not allow commands. On the other device, open Devices and give this one the Editor role or higher".into(),
            ));
        }
    } else if !matches!(user.role, Role::Owner | Role::Administrator) {
        return Err(NexusError::PermissionDenied("Only an owner or administrator can run commands on this device".into()));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct CommandResult {
    message: String,
}

/// `POST /commands/{name}`: runs one of the commands.
pub async fn run(State(s): State<SharedState>, user: AuthUser, Path(name): Path<String>) -> ApiResult<Json<CommandResult>> {
    allowed(&user)?;
    let message = match name.as_str() {
        "rescan" => {
            let mut started = 0;
            for root in s.db.list_roots().await? {
                if root.device_id != s.node.device_id
                    || !user.sees_root(&root.id)
                    || s.storage.ensure_online(&root).is_err()
                    || s.indexer.is_scanning(&root.id)
                {
                    continue;
                }
                s.indexer.spawn_scan(root);
                started += 1;
            }
            format!("Looking for changes in {started} storage location{}", if started == 1 { "" } else { "s" })
        }
        "check-updates" => {
            let state = s.clone();
            tokio::spawn(async move { crate::routes::updates::check(&state).await });
            "Checking for updates".into()
        }
        "pause-transfers" => {
            let n = s.jobs.pause_all(&s, true);
            format!("Paused {n} transfer{}", if n == 1 { "" } else { "s" })
        }
        "resume-transfers" => {
            let n = s.jobs.pause_all(&s, false);
            format!("Resumed {n} transfer{}", if n == 1 { "" } else { "s" })
        }
        "sleep" => {
            if !cfg!(windows) {
                return Err(NexusError::invalid("This device cannot be put to sleep from here").into());
            }
            // Answer first, then sleep.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                crate::power::sleep_now();
            });
            "Going to sleep".into()
        }
        _ => return Err(NexusError::NotFound("command".into()).into()),
    };
    audit(&s, &user, "command.run", Some(&name)).await;
    Ok(Json(CommandResult { message }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(role: Role, device: bool) -> AuthUser {
        AuthUser {
            user_id: None,
            username: "someone".into(),
            role,
            local: false,
            device_id: device.then(|| "phone".into()),
            allowed_roots: None,
            shared: Vec::new(),
            via_token: false,
        }
    }

    #[test]
    fn people_need_to_manage_and_devices_need_to_edit() {
        assert!(allowed(&caller(Role::Owner, false)).is_ok());
        assert!(allowed(&caller(Role::Administrator, false)).is_ok());
        assert!(allowed(&caller(Role::Editor, false)).is_err());
        // Paired devices, with the role they are paired with or higher.
        assert!(allowed(&caller(Role::Editor, true)).is_ok());
        assert!(allowed(&caller(Role::Administrator, true)).is_ok());
        assert!(allowed(&caller(Role::Contributor, true)).is_err());
        assert!(allowed(&caller(Role::Viewer, true)).is_err());
    }
}
