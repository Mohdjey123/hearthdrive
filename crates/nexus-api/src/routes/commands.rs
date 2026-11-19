//! Remote commands: a fixed set of actions a person may run on this node
//! from another device (through the peer proxy) or from here. Nothing here
//! runs programs or arbitrary commands.

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

fn allowed(user: &AuthUser) -> Result<(), NexusError> {
    user.require_person().map_err(|e| e.0)?;
    if !matches!(user.role, Role::Owner | Role::Administrator) {
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
                if root.device_id != s.node.device_id || s.storage.ensure_online(&root).is_err() || s.indexer.is_scanning(&root.id) {
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
