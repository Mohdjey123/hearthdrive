//! Drive health: full reads of storage locations, the files they found
//! damaged, and when each location went missing or came back.

use crate::Database;
use nexus_common::{Result, time::now_ms};
use serde::Serialize;

/// The last full read of a storage location.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RootCheck {
    pub root_id: String,
    pub checked_at: i64,
    pub files: i64,
    pub bytes: i64,
    pub damaged: i64,
    pub result: String,
}

/// A file whose content no longer matches its hash although its size and
/// date are unchanged.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DamagedFile {
    pub file_id: String,
    pub root_id: String,
    pub relative_path: String,
    pub size: i64,
    pub expected_hash: String,
    pub found_hash: Option<String>,
    pub found_at: i64,
}

/// A file with a content hash, as the index knows it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HashedFile {
    pub id: String,
    pub relative_path: String,
    pub size: i64,
    pub modified_at: Option<i64>,
    pub content_hash: String,
}

impl Database {
    /// Hashed files of a root after `after` (by path), `limit` at a time.
    pub async fn hashed_files(&self, root_id: &str, after: &str, limit: i64) -> Result<Vec<HashedFile>> {
        Ok(sqlx::query_as::<_, HashedFile>(
            "SELECT id, relative_path, size, modified_at, content_hash FROM files \
             WHERE root_id = ? AND is_dir = 0 AND content_hash IS NOT NULL AND relative_path > ? \
             ORDER BY relative_path LIMIT ?",
        )
        .bind(root_id)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn root_checks(&self) -> Result<Vec<RootCheck>> {
        Ok(sqlx::query_as::<_, RootCheck>("SELECT root_id, checked_at, files, bytes, damaged, result FROM root_checks")
            .fetch_all(&self.pool)
            .await?)
    }

    /// Records a finished full read and replaces the root's damaged files.
    pub async fn record_root_check(&self, check: &RootCheck, damaged: &[DamagedFile]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO root_checks (root_id, checked_at, files, bytes, damaged, result) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (root_id) DO UPDATE SET checked_at = excluded.checked_at, files = excluded.files, \
               bytes = excluded.bytes, damaged = excluded.damaged, result = excluded.result",
        )
        .bind(&check.root_id)
        .bind(check.checked_at)
        .bind(check.files)
        .bind(check.bytes)
        .bind(check.damaged)
        .bind(&check.result)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM damaged_files WHERE root_id = ?").bind(&check.root_id).execute(&mut *tx).await?;
        for d in damaged {
            sqlx::query(
                "INSERT INTO damaged_files (file_id, root_id, relative_path, size, expected_hash, found_hash, found_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT (file_id) DO NOTHING",
            )
            .bind(&d.file_id)
            .bind(&d.root_id)
            .bind(&d.relative_path)
            .bind(d.size)
            .bind(&d.expected_hash)
            .bind(&d.found_hash)
            .bind(d.found_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn damaged_files(&self) -> Result<Vec<DamagedFile>> {
        Ok(sqlx::query_as::<_, DamagedFile>(
            "SELECT file_id, root_id, relative_path, size, expected_hash, found_hash, found_at FROM damaged_files \
             ORDER BY root_id, relative_path",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn damaged_file(&self, file_id: &str) -> Result<Option<DamagedFile>> {
        Ok(sqlx::query_as::<_, DamagedFile>(
            "SELECT file_id, root_id, relative_path, size, expected_hash, found_hash, found_at FROM damaged_files WHERE file_id = ?",
        )
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// The file is fine again (repaired, accepted as it is, or gone).
    pub async fn clear_damaged(&self, file_id: &str) -> Result<()> {
        let root: Option<(String,)> = sqlx::query_as("DELETE FROM damaged_files WHERE file_id = ? RETURNING root_id")
            .bind(file_id)
            .fetch_optional(&self.pool)
            .await?;
        if let Some((root_id,)) = root {
            sqlx::query("UPDATE root_checks SET damaged = MAX(damaged - 1, 0) WHERE root_id = ?")
                .bind(root_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Whether each root was there when last looked at, and since when.
    pub async fn root_presence(&self) -> Result<Vec<(String, bool, i64)>> {
        Ok(sqlx::query_as("SELECT root_id, online, since FROM root_presence").fetch_all(&self.pool).await?)
    }

    pub async fn set_root_presence(&self, root_id: &str, online: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO root_presence (root_id, online, since) VALUES (?, ?, ?) \
             ON CONFLICT (root_id) DO UPDATE SET online = excluded.online, since = excluded.since",
        )
        .bind(root_id)
        .bind(online)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
