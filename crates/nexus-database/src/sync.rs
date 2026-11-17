//! Resumable uploads, earlier versions of files, and folder sync pairs.

use sqlx::AssertSqlSafe;
use crate::files::FILE_COLUMNS;
use crate::{Database, like_escape};
use nexus_common::types::FileEntry;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::Serialize;
use std::collections::HashMap;

/// An upload that can be continued after the connection drops.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UploadSession {
    pub id: String,
    pub root_id: String,
    pub parent_path: String,
    pub filename: String,
    pub size: i64,
    pub on_conflict: String,
    pub modified_at: Option<i64>,
    pub make_parents: bool,
    #[serde(skip)]
    pub owner: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// An earlier content of a file.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FileVersion {
    pub id: String,
    pub file_id: String,
    pub root_id: String,
    pub relative_path: String,
    pub size: i64,
    pub modified_at: Option<i64>,
    pub content_hash: Option<String>,
    pub created_at: i64,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct SyncPair {
    pub id: String,
    pub local_root_id: String,
    pub local_path: String,
    pub device_id: String,
    pub remote_root_id: String,
    pub remote_path: String,
    pub remote_label: String,
    pub mode: String,
    pub enabled: bool,
    pub created_by: String,
    #[serde(skip)]
    pub created_role: String,
    pub last_run_at: Option<i64>,
    pub last_ok_at: Option<i64>,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
}

pub struct NewSyncPair<'a> {
    pub local_root_id: &'a str,
    pub local_path: &'a str,
    pub device_id: &'a str,
    pub remote_root_id: &'a str,
    pub remote_path: &'a str,
    pub remote_label: &'a str,
    pub mode: &'a str,
    pub created_by: &'a str,
    pub created_role: &'a str,
}

/// Fingerprints of one path on each side (here, there) after it was last in step.
pub type Fingerprints = (Option<String>, Option<String>);
pub type SyncState = HashMap<String, Fingerprints>;
/// A path's new fingerprints, or `None` to forget it (and everything under it).
pub type StateChange = (String, Option<Fingerprints>);

const UPLOAD_COLUMNS: &str =
    "id, root_id, parent_path, filename, size, on_conflict, modified_at, make_parents, owner, created_at, updated_at";
const VERSION_COLUMNS: &str =
    "id, file_id, root_id, relative_path, size, modified_at, content_hash, created_at, created_by";
const PAIR_COLUMNS: &str = "id, local_root_id, local_path, device_id, remote_root_id, remote_path, remote_label, mode, \
     enabled, created_by, created_role, last_run_at, last_ok_at, last_result, last_error, created_at";

impl Database {
    // -- uploads ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create_upload(
        &self,
        root_id: &str,
        parent: &str,
        name: &str,
        size: u64,
        on_conflict: &str,
        modified_at: Option<i64>,
        make_parents: bool,
        owner: &str,
    ) -> Result<UploadSession> {
        let id = new_id();
        let now = now_ms();
        sqlx::query(AssertSqlSafe(format!("INSERT INTO uploads ({UPLOAD_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")))
            .bind(&id)
            .bind(root_id)
            .bind(parent)
            .bind(name)
            .bind(size as i64)
            .bind(on_conflict)
            .bind(modified_at)
            .bind(make_parents)
            .bind(owner)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;
        self.get_upload(&id).await
    }

    pub async fn get_upload(&self, id: &str) -> Result<UploadSession> {
        sqlx::query_as::<_, UploadSession>(AssertSqlSafe(format!("SELECT {UPLOAD_COLUMNS} FROM uploads WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("Upload".into()))
    }

    pub async fn touch_upload(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE uploads SET updated_at = ? WHERE id = ?").bind(now_ms()).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete_upload(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM uploads WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Uploads not continued for `max_age_ms`; their staging files can go.
    pub async fn stale_uploads(&self, max_age_ms: i64) -> Result<Vec<UploadSession>> {
        Ok(sqlx::query_as::<_, UploadSession>(AssertSqlSafe(format!("SELECT {UPLOAD_COLUMNS} FROM uploads WHERE updated_at < ?")))
            .bind(now_ms() - max_age_ms)
            .fetch_all(&self.pool)
            .await?)
    }

    // -- versions ---------------------------------------------------------

    pub async fn insert_version(&self, v: &FileVersion) -> Result<()> {
        sqlx::query(AssertSqlSafe(format!("INSERT INTO file_versions ({VERSION_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")))
            .bind(&v.id)
            .bind(&v.file_id)
            .bind(&v.root_id)
            .bind(&v.relative_path)
            .bind(v.size)
            .bind(v.modified_at)
            .bind(&v.content_hash)
            .bind(v.created_at)
            .bind(&v.created_by)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Newest first.
    pub async fn list_versions(&self, file_id: &str) -> Result<Vec<FileVersion>> {
        Ok(sqlx::query_as::<_, FileVersion>(AssertSqlSafe(format!(
            "SELECT {VERSION_COLUMNS} FROM file_versions WHERE file_id = ? ORDER BY created_at DESC, id"
        )))
        .bind(file_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_version(&self, id: &str) -> Result<FileVersion> {
        sqlx::query_as::<_, FileVersion>(AssertSqlSafe(format!("SELECT {VERSION_COLUMNS} FROM file_versions WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("Version".into()))
    }

    pub async fn delete_version(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM file_versions WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    // -- trees ------------------------------------------------------------

    /// Every entry under `rel` ('' = the whole root), parents before children.
    pub async fn subtree_entries(&self, root_id: &str, rel: &str) -> Result<Vec<FileEntry>> {
        let (clause, pattern) = if rel.is_empty() {
            ("", String::new())
        } else {
            ("AND relative_path LIKE ? ESCAPE '\\'", format!("{}/%", like_escape(rel)))
        };
        let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE root_id = ? {clause} ORDER BY relative_path LIMIT 500001");
        let mut q = sqlx::query_as::<_, FileEntry>(AssertSqlSafe(sql)).bind(root_id);
        if !rel.is_empty() {
            q = q.bind(pattern);
        }
        let items = q.fetch_all(&self.pool).await?;
        if items.len() > 500_000 {
            return Err(NexusError::invalid("This folder holds more than 500,000 items; choose a smaller folder"));
        }
        Ok(items)
    }

    // -- sync pairs -------------------------------------------------------

    pub async fn create_sync_pair(&self, p: &NewSyncPair<'_>) -> Result<SyncPair> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO sync_pairs (id, local_root_id, local_path, device_id, remote_root_id, remote_path, remote_label, \
             mode, enabled, created_by, created_role, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
        )
        .bind(&id)
        .bind(p.local_root_id)
        .bind(p.local_path)
        .bind(p.device_id)
        .bind(p.remote_root_id)
        .bind(p.remote_path)
        .bind(p.remote_label)
        .bind(p.mode)
        .bind(p.created_by)
        .bind(p.created_role)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        self.get_sync_pair(&id).await
    }

    pub async fn get_sync_pair(&self, id: &str) -> Result<SyncPair> {
        sqlx::query_as::<_, SyncPair>(AssertSqlSafe(format!("SELECT {PAIR_COLUMNS} FROM sync_pairs WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("Sync".into()))
    }

    pub async fn list_sync_pairs(&self) -> Result<Vec<SyncPair>> {
        Ok(sqlx::query_as::<_, SyncPair>(AssertSqlSafe(format!("SELECT {PAIR_COLUMNS} FROM sync_pairs ORDER BY created_at")))
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn update_sync_pair(&self, id: &str, enabled: Option<bool>, mode: Option<&str>) -> Result<SyncPair> {
        if let Some(enabled) = enabled {
            sqlx::query("UPDATE sync_pairs SET enabled = ? WHERE id = ?").bind(enabled).bind(id).execute(&self.pool).await?;
        }
        if let Some(mode) = mode {
            sqlx::query("UPDATE sync_pairs SET mode = ? WHERE id = ?").bind(mode).bind(id).execute(&self.pool).await?;
        }
        self.get_sync_pair(id).await
    }

    pub async fn delete_sync_pair(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sync_pairs WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn record_sync_run(&self, id: &str, ok: bool, result: &str, error: Option<&str>) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "UPDATE sync_pairs SET last_run_at = ?, last_ok_at = CASE WHEN ? THEN ? ELSE last_ok_at END, \
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

    pub async fn load_sync_state(&self, pair_id: &str) -> Result<SyncState> {
        let rows: Vec<(String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT rel_path, local_fp, remote_fp FROM sync_state WHERE pair_id = ?")
                .bind(pair_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(|(p, l, r)| (p, (l, r))).collect())
    }

    /// Writes the changed paths: `Some` sets a path's fingerprints, `None` forgets it.
    pub async fn save_sync_state(
        &self,
        pair_id: &str,
        changes: &[StateChange],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for (path, change) in changes {
            match change {
                Some((l, r)) => {
                    sqlx::query(
                        "INSERT INTO sync_state (pair_id, rel_path, local_fp, remote_fp) VALUES (?, ?, ?, ?) \
                         ON CONFLICT (pair_id, rel_path) DO UPDATE SET local_fp = excluded.local_fp, remote_fp = excluded.remote_fp",
                    )
                    .bind(pair_id)
                    .bind(path)
                    .bind(l)
                    .bind(r)
                    .execute(&mut *tx)
                    .await?;
                }
                None => {
                    sqlx::query("DELETE FROM sync_state WHERE pair_id = ? AND (rel_path = ? OR rel_path LIKE ? ESCAPE '\\')")
                        .bind(pair_id)
                        .bind(path)
                        .bind(format!("{}/%", like_escape(path)))
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        tx.commit().await?;
        Ok(())
    }
}
