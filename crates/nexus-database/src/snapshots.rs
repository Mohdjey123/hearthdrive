//! Backups as snapshots: which folder goes where, how often, how long
//! snapshots are kept, and what each snapshot held.

use crate::Database;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::{Deserialize, Serialize};
use sqlx::AssertSqlSafe;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct SnapshotSet {
    pub id: String,
    pub name: String,
    pub local_root_id: String,
    pub local_path: String,
    pub device_id: String,
    pub remote_root_id: String,
    pub remote_path: String,
    pub remote_label: String,
    pub every_hours: i64,
    pub keep_last: i64,
    pub keep_daily: i64,
    pub keep_weekly: i64,
    pub keep_monthly: i64,
    pub enabled: bool,
    pub created_by: String,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub last_check_at: Option<i64>,
    pub last_check_result: Option<String>,
    /// Contents and snapshot lists are encrypted on the device.
    pub encrypted: bool,
}

pub struct NewSnapshotSet<'a> {
    pub name: &'a str,
    pub local_root_id: &'a str,
    pub local_path: &'a str,
    pub device_id: &'a str,
    pub remote_root_id: &'a str,
    pub remote_path: &'a str,
    pub remote_label: &'a str,
    pub created_by: &'a str,
}

/// How often a set runs and which snapshots it keeps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Schedule {
    pub every_hours: i64,
    pub keep_last: i64,
    pub keep_daily: i64,
    pub keep_weekly: i64,
    pub keep_monthly: i64,
}

/// A snapshot without its file list.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Snapshot {
    pub id: String,
    pub set_id: String,
    pub taken_at: i64,
    pub files: i64,
    pub bytes: i64,
    pub new_files: i64,
    pub new_bytes: i64,
}

/// One file in a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotFile {
    /// Path inside the backed-up folder.
    pub p: String,
    pub s: i64,
    pub m: Option<i64>,
    /// BLAKE3 of the content: the object's name on the device.
    pub h: String,
}

const SET_COLUMNS: &str = "id, name, local_root_id, local_path, device_id, remote_root_id, remote_path, remote_label, every_hours, \
    keep_last, keep_daily, keep_weekly, keep_monthly, enabled, created_by, created_at, last_run_at, last_ok_at, last_result, \
    last_error, last_check_at, last_check_result, key_sealed IS NOT NULL AS encrypted";
const SNAPSHOT_COLUMNS: &str = "id, set_id, taken_at, files, bytes, new_files, new_bytes";

impl Database {
    pub async fn create_snapshot_set(&self, n: &NewSnapshotSet<'_>) -> Result<SnapshotSet> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO snapshot_sets (id, name, local_root_id, local_path, device_id, remote_root_id, remote_path, remote_label, \
             created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(n.name)
        .bind(n.local_root_id)
        .bind(n.local_path)
        .bind(n.device_id)
        .bind(n.remote_root_id)
        .bind(n.remote_path)
        .bind(n.remote_label)
        .bind(n.created_by)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        self.get_snapshot_set(&id).await
    }

    pub async fn get_snapshot_set(&self, id: &str) -> Result<SnapshotSet> {
        sqlx::query_as::<_, SnapshotSet>(AssertSqlSafe(format!("SELECT {SET_COLUMNS} FROM snapshot_sets WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("Backup".into()))
    }

    pub async fn list_snapshot_sets(&self) -> Result<Vec<SnapshotSet>> {
        Ok(sqlx::query_as::<_, SnapshotSet>(AssertSqlSafe(format!("SELECT {SET_COLUMNS} FROM snapshot_sets ORDER BY created_at")))
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn update_snapshot_set(&self, id: &str, enabled: Option<bool>, schedule: Option<Schedule>) -> Result<SnapshotSet> {
        if let Some(on) = enabled {
            sqlx::query("UPDATE snapshot_sets SET enabled = ? WHERE id = ?").bind(on).bind(id).execute(&self.pool).await?;
        }
        if let Some(k) = schedule {
            sqlx::query(
                "UPDATE snapshot_sets SET every_hours = ?, keep_last = ?, keep_daily = ?, keep_weekly = ?, keep_monthly = ? WHERE id = ?",
            )
            .bind(k.every_hours)
            .bind(k.keep_last)
            .bind(k.keep_daily)
            .bind(k.keep_weekly)
            .bind(k.keep_monthly)
            .bind(id)
            .execute(&self.pool)
            .await?;
        }
        self.get_snapshot_set(id).await
    }

    /// Stores a backup's key (already protected for this computer).
    pub async fn set_snapshot_key(&self, id: &str, key_sealed: &[u8]) -> Result<()> {
        sqlx::query("UPDATE snapshot_sets SET key_sealed = ? WHERE id = ?").bind(key_sealed).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// A backup's protected key; `None` when it is not encrypted.
    pub async fn snapshot_key(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let row: Option<(Option<Vec<u8>>,)> =
            sqlx::query_as("SELECT key_sealed FROM snapshot_sets WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.ok_or_else(|| NexusError::NotFound("Backup".into()))?.0)
    }

    pub async fn delete_snapshot_set(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM snapshot_sets WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn record_snapshot_run(&self, id: &str, ok: bool, result: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "UPDATE snapshot_sets SET last_run_at = ?, last_ok_at = CASE WHEN ? THEN ? ELSE last_ok_at END, \
             last_result = ?, last_error = ? WHERE id = ?",
        )
        .bind(now)
        .bind(ok)
        .bind(now)
        .bind(result)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_snapshot_check(&self, id: &str, result: &str) -> Result<()> {
        sqlx::query("UPDATE snapshot_sets SET last_check_at = ?, last_check_result = ? WHERE id = ?")
            .bind(now_ms())
            .bind(result)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn insert_snapshot(&self, snap: &Snapshot, files: &[SnapshotFile]) -> Result<()> {
        let manifest = serde_json::to_string(files).map_err(NexusError::internal)?;
        sqlx::query(
            "INSERT INTO snapshots (id, set_id, taken_at, files, bytes, new_files, new_bytes, manifest) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&snap.id)
        .bind(&snap.set_id)
        .bind(snap.taken_at)
        .bind(snap.files)
        .bind(snap.bytes)
        .bind(snap.new_files)
        .bind(snap.new_bytes)
        .bind(manifest)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Snapshots of a set, newest first.
    pub async fn list_snapshots(&self, set_id: &str) -> Result<Vec<Snapshot>> {
        Ok(sqlx::query_as::<_, Snapshot>(AssertSqlSafe(format!(
            "SELECT {SNAPSHOT_COLUMNS} FROM snapshots WHERE set_id = ? ORDER BY taken_at DESC"
        )))
        .bind(set_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_snapshot(&self, set_id: &str, id: &str) -> Result<(Snapshot, Vec<SnapshotFile>)> {
        let row: Option<(String, i64, i64, i64, i64, i64, String)> = sqlx::query_as(
            "SELECT id, taken_at, files, bytes, new_files, new_bytes, manifest FROM snapshots WHERE set_id = ? AND id = ?",
        )
        .bind(set_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let (id, taken_at, files, bytes, new_files, new_bytes, manifest) = row.ok_or_else(|| NexusError::NotFound("Snapshot".into()))?;
        let list = serde_json::from_str(&manifest).map_err(NexusError::internal)?;
        Ok((Snapshot { id, set_id: set_id.to_string(), taken_at, files, bytes, new_files, new_bytes }, list))
    }

    /// Every content hash the given snapshots refer to.
    pub async fn snapshot_hashes(&self, set_id: &str) -> Result<std::collections::HashSet<String>> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT manifest FROM snapshots WHERE set_id = ?").bind(set_id).fetch_all(&self.pool).await?;
        let mut out = std::collections::HashSet::new();
        for (manifest,) in rows {
            let files: Vec<SnapshotFile> = serde_json::from_str(&manifest).map_err(NexusError::internal)?;
            out.extend(files.into_iter().map(|f| f.h));
        }
        Ok(out)
    }

    pub async fn delete_snapshot(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM snapshots WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
}
