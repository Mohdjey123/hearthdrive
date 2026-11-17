use crate::{Database, like_escape};
use sqlx::AssertSqlSafe;
use nexus_common::types::{FileEntry, NewEntry};
use nexus_common::{NexusError, Result};
use serde::Serialize;

pub(crate) const FILE_COLUMNS: &str = "id, root_id, relative_path, parent_path, filename, is_dir, \
     extension, mime_type, size, created_at, modified_at, content_hash, indexed_at, availability";

/// Insert-or-update keyed on (root_id, relative_path) so a file keeps its id
/// across rescans. The content hash is kept only while size and mtime match.
/// `indexed_at` never moves backwards, which lets concurrent scans and watcher
/// updates use it as a "seen since" marker.
const UPSERT: &str = "INSERT INTO files (id, root_id, relative_path, parent_path, filename, is_dir, \
        extension, mime_type, size, created_at, modified_at, indexed_at, availability) \
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'online') \
     ON CONFLICT (root_id, relative_path) DO UPDATE SET \
        relative_path = excluded.relative_path, \
        parent_path = excluded.parent_path, \
        filename = excluded.filename, \
        is_dir = excluded.is_dir, \
        extension = excluded.extension, \
        mime_type = excluded.mime_type, \
        content_hash = CASE WHEN files.size = excluded.size AND files.modified_at IS excluded.modified_at \
            AND files.is_dir = excluded.is_dir THEN files.content_hash ELSE NULL END, \
        hashed_at = CASE WHEN files.size = excluded.size AND files.modified_at IS excluded.modified_at \
            AND files.is_dir = excluded.is_dir THEN files.hashed_at ELSE NULL END, \
        size = excluded.size, \
        created_at = excluded.created_at, \
        modified_at = excluded.modified_at, \
        indexed_at = MAX(files.indexed_at, excluded.indexed_at), \
        availability = 'online'";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortField {
    Name,
    Size,
    Modified,
    Type,
}

impl SortField {
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("name") {
            "size" => Self::Size,
            "modified" => Self::Modified,
            "type" => Self::Type,
            _ => Self::Name,
        }
    }

    fn sql(self) -> &'static str {
        match self {
            Self::Name => "filename COLLATE NOCASE",
            Self::Size => "size",
            Self::Modified => "modified_at",
            Self::Type => "extension COLLATE NOCASE",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct RootStats {
    pub files: i64,
    pub dirs: i64,
    pub bytes: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UnhashedFile {
    pub id: String,
    pub relative_path: String,
    pub size: i64,
    pub modified_at: Option<i64>,
}

fn subtree_pattern(rel: &str) -> String {
    format!("{}/%", like_escape(rel))
}

impl Database {
    pub async fn upsert_entries(&self, root_id: &str, entries: &[NewEntry], indexed_at: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for e in entries {
            sqlx::query(UPSERT)
                .bind(nexus_common::new_id())
                .bind(root_id)
                .bind(&e.relative_path)
                .bind(&e.parent_path)
                .bind(&e.filename)
                .bind(e.is_dir)
                .bind(&e.extension)
                .bind(&e.mime_type)
                .bind(e.size)
                .bind(e.created_at)
                .bind(e.modified_at)
                .bind(indexed_at)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Removes entries under `prefix` ('' = whole root) not seen since `before`.
    pub async fn remove_stale(&self, root_id: &str, prefix: &str, before: i64) -> Result<u64> {
        let result = if prefix.is_empty() {
            sqlx::query("DELETE FROM files WHERE root_id = ? AND indexed_at < ?")
                .bind(root_id)
                .bind(before)
                .execute(&self.pool)
                .await?
        } else {
            sqlx::query(
                "DELETE FROM files WHERE root_id = ? AND relative_path LIKE ? ESCAPE '\\' AND indexed_at < ?",
            )
            .bind(root_id)
            .bind(subtree_pattern(prefix))
            .bind(before)
            .execute(&self.pool)
            .await?
        };
        Ok(result.rows_affected())
    }

    /// Removes an entry and, for folders, everything beneath it.
    pub async fn delete_subtree(&self, root_id: &str, rel: &str) -> Result<u64> {
        let result = sqlx::query(
            "DELETE FROM files WHERE root_id = ? AND (relative_path = ? OR relative_path LIKE ? ESCAPE '\\')",
        )
        .bind(root_id)
        .bind(rel)
        .bind(subtree_pattern(rel))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn subtree_size(&self, root_id: &str, rel: &str) -> Result<i64> {
        let size: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(size), 0) FROM files WHERE root_id = ? AND relative_path LIKE ? ESCAPE '\\'",
        )
        .bind(root_id)
        .bind(subtree_pattern(rel))
        .fetch_one(&self.pool)
        .await?;
        Ok(size)
    }

    pub async fn list_children(
        &self,
        root_id: &str,
        parent: &str,
        sort: SortField,
        descending: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<FileEntry>, i64)> {
        let direction = if descending { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE root_id = ? AND parent_path = ? \
             ORDER BY is_dir DESC, {} {direction}, filename COLLATE NOCASE ASC LIMIT ? OFFSET ?",
            sort.sql()
        );
        let items = sqlx::query_as::<_, FileEntry>(AssertSqlSafe(sql))
            .bind(root_id)
            .bind(parent)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM files WHERE root_id = ? AND parent_path = ?")
            .bind(root_id)
            .bind(parent)
            .fetch_one(&self.pool)
            .await?;
        Ok((items, total))
    }

    pub async fn get_file(&self, id: &str) -> Result<FileEntry> {
        sqlx::query_as::<_, FileEntry>(AssertSqlSafe(format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("file".into()))
    }

    pub async fn get_file_by_path(&self, root_id: &str, rel: &str) -> Result<Option<FileEntry>> {
        Ok(sqlx::query_as::<_, FileEntry>(AssertSqlSafe(format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE root_id = ? AND relative_path = ?"
        )))
        .bind(root_id)
        .bind(rel)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Case-insensitive filename search. Returns up to `limit` items.
    pub async fn search(&self, query: &str, root_id: Option<&str>, offset: i64, limit: i64) -> Result<Vec<FileEntry>> {
        let pattern = format!("%{}%", like_escape(query.trim()));
        let root_clause = if root_id.is_some() { "AND root_id = ?" } else { "" };
        let sql = format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE filename LIKE ? ESCAPE '\\' {root_clause} \
             ORDER BY is_dir DESC, filename COLLATE NOCASE LIMIT ? OFFSET ?"
        );
        let mut q = sqlx::query_as::<_, FileEntry>(AssertSqlSafe(sql)).bind(pattern);
        if let Some(root) = root_id {
            q = q.bind(root);
        }
        Ok(q.bind(limit).bind(offset).fetch_all(&self.pool).await?)
    }

    pub async fn recent_files(&self, limit: i64) -> Result<Vec<FileEntry>> {
        Ok(sqlx::query_as::<_, FileEntry>(AssertSqlSafe(format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE is_dir = 0 ORDER BY modified_at DESC LIMIT ?"
        )))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn root_stats(&self, root_id: &str) -> Result<RootStats> {
        if let Some((stats, fresh_until)) = self.stats_cache.lock().unwrap_or_else(|e| e.into_inner()).get(root_id)
            && std::time::Instant::now() < *fresh_until {
                return Ok(*stats);
            }
        let started = std::time::Instant::now();
        let (files, dirs, bytes): (i64, i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(1 - is_dir), 0), COALESCE(SUM(is_dir), 0), COALESCE(SUM(size), 0) \
             FROM files WHERE root_id = ?",
        )
        .bind(root_id)
        .fetch_one(&self.pool)
        .await?;
        let stats = RootStats { files, dirs, bytes };
        // A small root is counted in a millisecond and stays live. A root
        // with millions of files takes seconds, so its totals are reused for
        // up to a minute instead of being recounted on every request.
        let ttl = (started.elapsed() * 20).min(std::time::Duration::from_secs(60));
        self.stats_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(root_id.to_string(), (stats, std::time::Instant::now() + ttl));
        Ok(stats)
    }

    pub async fn unhashed_files(&self, root_id: &str, limit: i64) -> Result<Vec<UnhashedFile>> {
        Ok(sqlx::query_as::<_, UnhashedFile>(
            "SELECT id, relative_path, size, modified_at FROM files \
             WHERE root_id = ? AND hashed_at IS NULL AND is_dir = 0 LIMIT ?",
        )
        .bind(root_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Records a content hash, unless the file changed since it was read.
    /// `None` marks the file as unhashable so it is not retried endlessly.
    pub async fn set_hash(&self, id: &str, hash: Option<&str>, expected_modified: Option<i64>, at: i64) -> Result<()> {
        sqlx::query("UPDATE files SET content_hash = ?, hashed_at = ? WHERE id = ? AND modified_at IS ?")
            .bind(hash)
            .bind(at)
            .bind(id)
            .bind(expected_modified)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Updates the index after a rename or move, keeping every file id.
    #[allow(clippy::too_many_arguments)]
    pub async fn move_entry(
        &self,
        root_id: &str,
        old_rel: &str,
        new_rel: &str,
        new_parent: &str,
        new_name: &str,
        extension: Option<String>,
        mime_type: Option<String>,
        is_dir: bool,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        if is_dir {
            let old_len = old_rel.chars().count() as i64;
            sqlx::query(
                "UPDATE files SET \
                    relative_path = ?1 || substr(relative_path, ?2 + 1), \
                    parent_path = ?1 || substr(parent_path, ?2 + 1) \
                 WHERE root_id = ?3 AND relative_path LIKE ?4 ESCAPE '\\'",
            )
            .bind(new_rel)
            .bind(old_len)
            .bind(root_id)
            .bind(subtree_pattern(old_rel))
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE files SET relative_path = ?, parent_path = ?, filename = ?, extension = ?, mime_type = ? \
             WHERE root_id = ? AND relative_path = ?",
        )
        .bind(new_rel)
        .bind(new_parent)
        .bind(new_name)
        .bind(extension)
        .bind(mime_type)
        .bind(root_id)
        .bind(old_rel)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Re-attaches a previous id to an entry (used when restoring from trash).
    pub async fn set_id_for_path(&self, root_id: &str, rel: &str, id: &str) -> Result<()> {
        sqlx::query("UPDATE files SET id = ? WHERE root_id = ? AND relative_path = ?")
            .bind(id)
            .bind(root_id)
            .bind(rel)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
