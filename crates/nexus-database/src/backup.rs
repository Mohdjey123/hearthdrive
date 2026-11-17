//! Phone backup settings and what has been backed up.

use sqlx::AssertSqlSafe;
use crate::Database;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::Serialize;
use std::collections::HashMap;

/// Photos and videos on this phone copied to a folder on a paired computer.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Backup {
    pub id: String,
    pub device_id: String,
    pub root_id: String,
    pub path: String,
    pub label: String,
    /// JSON array of phone folders.
    #[serde(skip)]
    pub sources: String,
    pub enabled: bool,
    pub created_by: String,
    pub last_run_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
}

impl Backup {
    pub fn source_list(&self) -> Vec<String> {
        serde_json::from_str(&self.sources).unwrap_or_default()
    }
}

pub struct NewBackup<'a> {
    pub device_id: &'a str,
    pub root_id: &'a str,
    pub path: &'a str,
    pub label: &'a str,
    pub sources: &'a [String],
    pub enabled: bool,
    pub created_by: &'a str,
}

const COLUMNS: &str = "id, device_id, root_id, path, label, sources, enabled, created_by, last_run_at, last_ok_at, \
                       last_result, last_error, created_at";

impl Database {
    /// The phone's backup, if one is set up.
    pub async fn get_backup(&self) -> Result<Option<Backup>> {
        Ok(sqlx::query_as::<_, Backup>(AssertSqlSafe(format!("SELECT {COLUMNS} FROM backups ORDER BY created_at LIMIT 1")))
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Sets up the backup, replacing an earlier one. What was already backed
    /// up is kept when the destination stays the same.
    pub async fn set_backup(&self, b: &NewBackup<'_>) -> Result<Backup> {
        let sources = serde_json::to_string(b.sources).map_err(NexusError::internal)?;
        let mut tx = self.pool.begin().await?;
        let existing: Option<(String, String, String, String)> =
            sqlx::query_as("SELECT id, device_id, root_id, path FROM backups ORDER BY created_at LIMIT 1")
                .fetch_optional(&mut *tx)
                .await?;
        let id = match existing {
            Some((id, device, root, path)) if device == b.device_id && root == b.root_id && path == b.path => {
                sqlx::query("UPDATE backups SET label = ?, sources = ?, enabled = ?, created_by = ? WHERE id = ?")
                    .bind(b.label)
                    .bind(&sources)
                    .bind(b.enabled)
                    .bind(b.created_by)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                id
            }
            _ => {
                // A new destination starts from nothing backed up.
                sqlx::query("DELETE FROM backups").execute(&mut *tx).await?;
                let id = new_id();
                sqlx::query(
                    "INSERT INTO backups (id, device_id, root_id, path, label, sources, enabled, created_by, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(b.device_id)
                .bind(b.root_id)
                .bind(b.path)
                .bind(b.label)
                .bind(&sources)
                .bind(b.enabled)
                .bind(b.created_by)
                .bind(now_ms())
                .execute(&mut *tx)
                .await?;
                id
            }
        };
        tx.commit().await?;
        sqlx::query_as::<_, Backup>(AssertSqlSafe(format!("SELECT {COLUMNS} FROM backups WHERE id = ?")))
            .bind(&id)
            .fetch_one(&self.pool)
            .await
            .map_err(Into::into)
    }

    pub async fn set_backup_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        sqlx::query("UPDATE backups SET enabled = ? WHERE id = ?").bind(enabled).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_backup(&self) -> Result<()> {
        sqlx::query("DELETE FROM backups").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn record_backup_run(&self, id: &str, ok: bool, result: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "UPDATE backups SET last_run_at = ?, last_ok_at = CASE WHEN ? THEN ? ELSE last_ok_at END, \
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

    /// Backed-up files and their fingerprints then.
    pub async fn backed_up(&self, backup_id: &str) -> Result<HashMap<String, String>> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT rel_path, fingerprint FROM backup_files WHERE backup_id = ?")
            .bind(backup_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn mark_backed_up(&self, backup_id: &str, rel_path: &str, fingerprint: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO backup_files (backup_id, rel_path, fingerprint) VALUES (?, ?, ?) \
             ON CONFLICT (backup_id, rel_path) DO UPDATE SET fingerprint = excluded.fingerprint",
        )
        .bind(backup_id)
        .bind(rel_path)
        .bind(fingerprint)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
