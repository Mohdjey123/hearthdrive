use crate::{Database, like_escape};
use sqlx::{AssertSqlSafe, QueryBuilder, Sqlite};
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

/// What a search looks for. Every filter is optional.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub query: String,
    /// Also match text inside documents and tags.
    pub content: bool,
    pub root_id: Option<String>,
    /// Only these storage locations (the ones that are connected).
    pub roots: Option<Vec<String>>,
    /// `image`, `video`, `audio`, `document`, `archive`, `code`, `other` or `folder`.
    pub kind: Option<String>,
    pub modified_after: Option<i64>,
    pub modified_before: Option<i64>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub tag: Option<String>,
    /// Only files that can be opened now.
    pub available: bool,
}

/// Words typed in a search box as an FTS5 query: every word must appear,
/// each as a prefix. Quotes keep FTS syntax characters harmless.
pub fn fts_query(text: &str) -> String {
    text.split_whitespace()
        .map(|w| format!("\"{}\"*", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn subtree_pattern(rel: &str) -> String {
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

    /// Case-insensitive filename search with filters. Returns up to `limit` items.
    pub async fn search(&self, f: &SearchFilter, offset: i64, limit: i64) -> Result<Vec<FileEntry>> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!("SELECT {FILE_COLUMNS} FROM files WHERE 1 = 1"));
        let text = f.query.trim();
        if !text.is_empty() {
            q.push(" AND (filename LIKE ").push_bind(format!("%{}%", like_escape(text))).push(" ESCAPE '\\'");
            if f.content {
                q.push(" OR id IN (SELECT file_id FROM doc_text WHERE doc_text MATCH ")
                    .push_bind(fts_query(text))
                    .push(")");
                q.push(" OR id IN (SELECT file_id FROM file_tags WHERE tag LIKE ")
                    .push_bind(format!("%{}%", like_escape(text)))
                    .push(" ESCAPE '\\')");
            }
            q.push(")");
        }
        if let Some(root) = &f.root_id {
            q.push(" AND root_id = ").push_bind(root.clone());
        }
        if let Some(roots) = &f.roots {
            if roots.is_empty() {
                return Ok(Vec::new());
            }
            q.push(" AND root_id IN (");
            let mut sep = q.separated(", ");
            for r in roots {
                sep.push_bind(r.clone());
            }
            q.push(")");
        }
        if let Some(kind) = &f.kind {
            let (list, negate) = match nexus_common::kinds::extensions(kind) {
                Some(list) => (list, false),
                None if kind == "folder" => {
                    q.push(" AND is_dir = 1");
                    (Vec::new(), false)
                }
                None => (nexus_common::kinds::known(), true),
            };
            if !list.is_empty() {
                q.push(if negate { " AND is_dir = 0 AND (extension IS NULL OR extension NOT IN (" } else { " AND is_dir = 0 AND extension IN (" });
                let mut sep = q.separated(", ");
                for e in list {
                    sep.push_bind(e);
                }
                q.push(if negate { "))" } else { ")" });
            }
        }
        if let Some(t) = f.modified_after {
            q.push(" AND modified_at >= ").push_bind(t);
        }
        if let Some(t) = f.modified_before {
            q.push(" AND modified_at < ").push_bind(t);
        }
        if let Some(n) = f.min_size {
            q.push(" AND is_dir = 0 AND size >= ").push_bind(n);
        }
        if let Some(n) = f.max_size {
            q.push(" AND is_dir = 0 AND size <= ").push_bind(n);
        }
        if let Some(tag) = &f.tag {
            q.push(" AND id IN (SELECT file_id FROM file_tags WHERE tag = ").push_bind(tag.clone()).push(")");
        }
        if f.available {
            q.push(" AND availability = 'online'");
        }
        if text.is_empty() {
            q.push(" ORDER BY is_dir DESC, modified_at DESC");
        } else {
            q.push(" ORDER BY is_dir DESC, filename COLLATE NOCASE");
        }
        q.push(" LIMIT ").push_bind(limit).push(" OFFSET ").push_bind(offset);
        Ok(q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?)
    }

    /// Files with this content (BLAKE3 hash and size).
    pub async fn files_with_hash(&self, hash: &str, size: i64, limit: i64) -> Result<Vec<FileEntry>> {
        Ok(sqlx::query_as::<_, FileEntry>(AssertSqlSafe(format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE is_dir = 0 AND content_hash = ? AND size = ? LIMIT ?"
        )))
        .bind(hash)
        .bind(size)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Files with the same content (same hash), biggest savings first.
    pub async fn duplicate_groups(&self, root_id: Option<&str>, min_size: i64, limit: i64) -> Result<Vec<Vec<FileEntry>>> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT content_hash FROM files WHERE is_dir = 0 AND content_hash IS NOT NULL AND size >= ",
        );
        q.push_bind(min_size.max(1));
        if let Some(root) = root_id {
            q.push(" AND root_id = ").push_bind(root.to_string());
        }
        q.push(" GROUP BY content_hash HAVING COUNT(*) > 1 ORDER BY MAX(size) * (COUNT(*) - 1) DESC LIMIT ")
            .push_bind(limit);
        let hashes: Vec<String> = q.build_query_scalar().fetch_all(&self.pool).await?;
        let mut groups = Vec::with_capacity(hashes.len());
        for hash in hashes {
            let mut q: QueryBuilder<Sqlite> =
                QueryBuilder::new(format!("SELECT {FILE_COLUMNS} FROM files WHERE is_dir = 0 AND content_hash = "));
            q.push_bind(hash);
            if let Some(root) = root_id {
                q.push(" AND root_id = ").push_bind(root.to_string());
            }
            q.push(" ORDER BY modified_at ASC");
            groups.push(q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?);
        }
        Ok(groups)
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
