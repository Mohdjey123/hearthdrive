pub mod backup;
pub mod changes;
pub mod commands;
pub mod library;
pub mod photos;
pub mod devices;
pub mod documents;
pub mod events;
pub mod files;
pub mod health;
pub mod introductions;
pub mod network;
pub mod sealed;
pub mod security;
pub mod shares;
pub mod sharing;
pub mod snapshots;
pub mod rules;
pub mod storage;
pub mod sync;
pub mod system;
pub mod tools;
pub mod transfers;
pub mod updates;
pub mod uploads;
pub mod users;
pub mod vdrive;

use crate::auth::AuthUser;
use crate::state::AppState;

/// Best-effort audit log entry; never fails the request.
pub(crate) async fn audit(state: &AppState, user: &AuthUser, action: &str, resource: Option<&str>) {
    if let Err(e) = state
        .db
        .log_activity(action, resource, user.user_id.as_deref(), Some(user.device_id.as_deref().unwrap_or(&state.node.device_id)))
        .await
    {
        tracing::warn!(error = %e, "could not write activity log");
    }
}

pub(crate) fn clamp_limit(limit: Option<i64>, default: i64, max: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, max)
}
