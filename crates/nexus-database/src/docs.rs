//! Text read from inside documents, and tags on files.

use crate::Database;
use crate::files::FILE_COLUMNS;
use nexus_common::Result;
use nexus_common::types::FileEntry;
use serde::Serialize;
use sqlx::{QueryBuilder, Sqlite};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FileTag {
    pub tag: String,
    /// Given by HearthDrive from what the document says, not by a person.
    pub auto: bool,
    pub added_by: String,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TagCount {
    pub tag: String,
    pub files: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DocState {
    pub method: Option<String>,
    pub chars: i64,
    pub read_at: i64,
    pub error: Option<String>,
}

impl Database {
    /// Documents of these extensions not read yet, or changed since.
    pub async fn doc_todo(&self, extensions: &[&str], limit: i64) -> Result<Vec<FileEntry>> {
        let cols = FILE_COLUMNS.split(',').map(|c| format!("f.{}", c.trim())).collect::<Vec<_>>().join(", ");
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT {cols} FROM files f LEFT JOIN doc_state d ON d.file_id = f.id \
             WHERE f.is_dir = 0 AND f.availability = 'online' AND (d.file_id IS NULL OR d.modified_at IS NOT f.modified_at) \
             AND lower(f.extension) IN ("
        ));
        let mut sep = q.separated(", ");
        for e in extensions {
            sep.push_bind(e.to_string());
        }
        sep.push_unseparated(")");
        q.push(" ORDER BY f.modified_at DESC LIMIT ").push_bind(limit);
        Ok(q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?)
    }

    /// Keeps the text read from a file (replacing earlier text).
    pub async fn save_doc_text(
        &self,
        file: &FileEntry,
        method: &str,
        text: &str,
        error: Option<&str>,
        at: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM doc_text WHERE file_id = ?").bind(&file.id).execute(&mut *tx).await?;
        if !text.trim().is_empty() {
            sqlx::query("INSERT INTO doc_text (file_id, body) VALUES (?, ?)")
                .bind(&file.id)
                .bind(text)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "INSERT OR REPLACE INTO doc_state (file_id, root_id, modified_at, method, chars, read_at, error) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&file.id)
        .bind(&file.root_id)
        .bind(file.modified_at)
        .bind(method)
        .bind(text.chars().count() as i64)
        .bind(at)
        .bind(error)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn doc_state(&self, file_id: &str) -> Result<Option<DocState>> {
        Ok(sqlx::query_as::<_, DocState>("SELECT method, chars, read_at, error FROM doc_state WHERE file_id = ?")
            .bind(file_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// The start of a document's text, for showing where a search matched.
    pub async fn doc_snippet(&self, file_id: &str, query: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT snippet(doc_text, 1, '[', ']', ' … ', 24) FROM doc_text WHERE doc_text MATCH ? AND file_id = ?",
        )
        .bind(crate::files::fts_query(query))
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await
        .unwrap_or(None))
    }

    pub async fn file_tags(&self, file_id: &str) -> Result<Vec<FileTag>> {
        Ok(sqlx::query_as::<_, FileTag>(
            "SELECT tag, auto, added_by, added_at FROM file_tags WHERE file_id = ? ORDER BY auto, tag COLLATE NOCASE",
        )
        .bind(file_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Replaces the tags people gave a file (`auto` false) or the ones HearthDrive
    /// gave it (`auto` true).
    pub async fn set_file_tags(&self, file: &FileEntry, tags: &[String], auto: bool, by: &str, at: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM file_tags WHERE file_id = ? AND auto = ?")
            .bind(&file.id)
            .bind(auto)
            .execute(&mut *tx)
            .await?;
        for tag in tags {
            sqlx::query(
                "INSERT OR IGNORE INTO file_tags (file_id, root_id, tag, added_by, auto, added_at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&file.id)
            .bind(&file.root_id)
            .bind(tag)
            .bind(by)
            .bind(auto)
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Adds tags to a file, keeping the ones it has. They stay when the
    /// document is read again (unlike the tags it gets from its text).
    pub async fn add_file_tags(&self, file: &FileEntry, tags: &[String], by: &str, at: i64) -> Result<()> {
        for tag in tags {
            sqlx::query(
                "INSERT OR IGNORE INTO file_tags (file_id, root_id, tag, added_by, auto, added_at) VALUES (?, ?, ?, ?, 0, ?)",
            )
            .bind(&file.id)
            .bind(&file.root_id)
            .bind(tag)
            .bind(by)
            .bind(at)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Every tag in use, most used first (only in `roots`, if given).
    pub async fn all_tags(&self, roots: Option<&[String]>) -> Result<Vec<TagCount>> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT tag, COUNT(DISTINCT file_id) AS files FROM file_tags WHERE 1 = 1");
        if let Some(roots) = roots {
            if roots.is_empty() {
                return Ok(Vec::new());
            }
            q.push(" AND root_id IN (");
            let mut sep = q.separated(", ");
            for r in roots {
                sep.push_bind(r.clone());
            }
            sep.push_unseparated(")");
        }
        q.push(" GROUP BY tag COLLATE NOCASE ORDER BY files DESC, tag LIMIT 500");
        Ok(q.build_query_as::<TagCount>().fetch_all(&self.pool).await?)
    }
}
