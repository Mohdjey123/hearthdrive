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

/// Extensions that are documents whatever they say.
pub const DOCUMENT_EXTENSIONS: &[&str] = &["pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp"];
/// Tags that make a picture a document (a photo of a receipt, say).
pub const DOCUMENT_TAGS: &[&str] =
    &["invoice", "receipt", "contract", "bank statement", "tax", "payslip", "insurance", "medical", "utility"];

/// A document's details: what was found in its text and what people typed.
#[derive(Debug, Clone, Default, Serialize, sqlx::FromRow)]
pub struct DocMeta {
    pub found_date: Option<String>,
    pub found_sender: Option<String>,
    pub found_amount: Option<String>,
    pub title: Option<String>,
    pub doc_date: Option<String>,
    pub sender: Option<String>,
    pub amount: Option<String>,
    pub note: Option<String>,
    pub edited_by: Option<String>,
    pub edited_at: Option<i64>,
}

/// What people can set on a document; `None` clears a value.
#[derive(Debug, Clone, Default)]
pub struct DocEdit {
    pub title: Option<String>,
    pub doc_date: Option<String>,
    pub sender: Option<String>,
    pub amount: Option<String>,
    pub note: Option<String>,
}

/// One document in a list: the file and the details to show.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DocRow {
    #[sqlx(flatten)]
    #[serde(flatten)]
    pub file: FileEntry,
    pub title: Option<String>,
    /// Typed, else found in the text (`YYYY-MM-DD`).
    pub doc_date: Option<String>,
    pub sender: Option<String>,
    pub amount: Option<String>,
    pub has_note: bool,
    /// Tags separated by `|`.
    pub tags: Option<String>,
    pub method: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DocFilter {
    /// Only these storage locations (`None`: all).
    pub roots: Option<Vec<String>>,
    /// Words in the document's text, name, title, sender or note.
    pub query: Option<String>,
    pub tag: Option<String>,
    /// `YYYY`.
    pub year: Option<String>,
    pub sender: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Count {
    pub value: String,
    pub count: i64,
}

/// A document's date: typed, found, or else the file's own.
const DOC_DATE: &str = "COALESCE(dm.doc_date, dm.found_date, date(f.modified_at / 1000, 'unixepoch'))";

impl Database {
    fn document_query(&self, select: &str, f: &DocFilter) -> QueryBuilder<Sqlite> {
        self.document_query_joined(select, "", f)
    }

    fn document_query_joined(&self, select: &str, join: &str, f: &DocFilter) -> QueryBuilder<Sqlite> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT {select} FROM files f LEFT JOIN doc_meta dm ON dm.file_id = f.id \
             LEFT JOIN doc_state ds ON ds.file_id = f.id{join} WHERE f.is_dir = 0 AND ("
        ));
        q.push("lower(f.extension) IN (");
        let mut sep = q.separated(", ");
        for e in DOCUMENT_EXTENSIONS {
            sep.push_bind(*e);
        }
        sep.push_unseparated(") OR f.id IN (SELECT file_id FROM file_tags WHERE tag IN (");
        let mut sep = q.separated(", ");
        for t in DOCUMENT_TAGS {
            sep.push_bind(*t);
        }
        sep.push_unseparated(")))");
        if let Some(roots) = &f.roots {
            if roots.is_empty() {
                q.push(" AND 0");
            } else {
                q.push(" AND f.root_id IN (");
                let mut sep = q.separated(", ");
                for r in roots {
                    sep.push_bind(r.clone());
                }
                sep.push_unseparated(")");
            }
        }
        if let Some(text) = f.query.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            let like = format!("%{}%", text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            q.push(" AND (f.id IN (SELECT file_id FROM doc_text WHERE doc_text MATCH ")
                .push_bind(crate::files::fts_query(text))
                .push(")");
            for col in ["f.filename", "dm.title", "COALESCE(dm.sender, dm.found_sender)", "dm.note"] {
                q.push(format!(" OR {col} LIKE ")).push_bind(like.clone()).push(" ESCAPE '\\'");
            }
            q.push(")");
        }
        if let Some(tag) = &f.tag {
            q.push(" AND f.id IN (SELECT file_id FROM file_tags WHERE tag = ").push_bind(tag.clone()).push(")");
        }
        if let Some(year) = &f.year {
            q.push(format!(" AND substr({DOC_DATE}, 1, 4) = ")).push_bind(year.clone());
        }
        if let Some(sender) = &f.sender {
            q.push(" AND COALESCE(dm.sender, dm.found_sender) = ").push_bind(sender.clone());
        }
        q
    }

    /// Documents, newest first by their date.
    pub async fn documents(&self, f: &DocFilter, offset: i64, limit: i64) -> Result<Vec<DocRow>> {
        let cols = FILE_COLUMNS.split(',').map(|c| format!("f.{}", c.trim())).collect::<Vec<_>>().join(", ");
        let select = format!(
            "{cols}, dm.title, {DOC_DATE} AS doc_date, COALESCE(dm.sender, dm.found_sender) AS sender, \
             COALESCE(dm.amount, dm.found_amount) AS amount, (dm.note IS NOT NULL AND dm.note != '') AS has_note, \
             (SELECT group_concat(tag, '|') FROM file_tags t WHERE t.file_id = f.id) AS tags, ds.method"
        );
        let mut q = self.document_query(&select, f);
        q.push(format!(" ORDER BY {DOC_DATE} DESC, f.filename LIMIT "))
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        Ok(q.build_query_as::<DocRow>().fetch_all(&self.pool).await?)
    }

    /// How many documents there are per year, per sender and per tag, for filters.
    pub async fn document_counts(&self, f: &DocFilter) -> Result<(Vec<Count>, Vec<Count>, Vec<Count>)> {
        let base = DocFilter { year: None, sender: None, tag: None, ..f.clone() };
        let mut q = self.document_query(&format!("substr({DOC_DATE}, 1, 4) AS value, COUNT(*) AS count"), &base);
        q.push(" GROUP BY value ORDER BY value DESC");
        let years = q.build_query_as::<Count>().fetch_all(&self.pool).await?;
        let mut q = self.document_query("COALESCE(dm.sender, dm.found_sender) AS value, COUNT(*) AS count", &base);
        q.push(" AND COALESCE(dm.sender, dm.found_sender) IS NOT NULL GROUP BY value ORDER BY count DESC, value LIMIT 30");
        let senders = q.build_query_as::<Count>().fetch_all(&self.pool).await?;
        let mut q = self.document_query_joined(
            "t.tag AS value, COUNT(DISTINCT f.id) AS count",
            " JOIN file_tags t ON t.file_id = f.id",
            &base,
        );
        q.push(" GROUP BY t.tag COLLATE NOCASE ORDER BY count DESC, t.tag LIMIT 40");
        let tags = q.build_query_as::<Count>().fetch_all(&self.pool).await?;
        Ok((years, senders, tags))
    }

    pub async fn doc_meta(&self, file_id: &str) -> Result<Option<DocMeta>> {
        Ok(sqlx::query_as::<_, DocMeta>(
            "SELECT found_date, found_sender, found_amount, title, doc_date, sender, amount, note, edited_by, edited_at \
             FROM doc_meta WHERE file_id = ?",
        )
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Keeps what was found in a document's text, leaving what people typed.
    pub async fn save_doc_found(
        &self,
        file: &FileEntry,
        date: Option<&str>,
        sender: Option<&str>,
        amount: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO doc_meta (file_id, root_id, found_date, found_sender, found_amount) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (file_id) DO UPDATE SET found_date = excluded.found_date, found_sender = excluded.found_sender, \
             found_amount = excluded.found_amount",
        )
        .bind(&file.id)
        .bind(&file.root_id)
        .bind(date)
        .bind(sender)
        .bind(amount)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Sets what people typed about a document.
    pub async fn set_doc_meta(&self, file: &FileEntry, e: &DocEdit, by: &str, at: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO doc_meta (file_id, root_id, title, doc_date, sender, amount, note, edited_by, edited_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (file_id) DO UPDATE SET title = excluded.title, \
             doc_date = excluded.doc_date, sender = excluded.sender, amount = excluded.amount, note = excluded.note, \
             edited_by = excluded.edited_by, edited_at = excluded.edited_at",
        )
        .bind(&file.id)
        .bind(&file.root_id)
        .bind(&e.title)
        .bind(&e.doc_date)
        .bind(&e.sender)
        .bind(&e.amount)
        .bind(&e.note)
        .bind(by)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Documents read before their date, sender and amount were looked for,
    /// with their text.
    pub async fn doc_meta_todo(&self, limit: i64) -> Result<Vec<(FileEntry, String)>> {
        let cols = FILE_COLUMNS.split(',').map(|c| format!("f.{}", c.trim())).collect::<Vec<_>>().join(", ");
        let mut q = self.document_query(&format!("{cols}"), &DocFilter::default());
        q.push(" AND ds.chars > 0 AND dm.file_id IS NULL LIMIT ").push_bind(limit);
        let files = q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?;
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            let body = sqlx::query_scalar::<_, String>("SELECT body FROM doc_text WHERE file_id = ?")
                .bind(&f.id)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or_default();
            out.push((f, body));
        }
        Ok(out)
    }

    /// Whether a file counts as a document (for the Documents page).
    pub async fn is_document(&self, file_id: &str) -> Result<bool> {
        let mut q = self.document_query("COUNT(*)", &DocFilter::default());
        q.push(" AND f.id = ").push_bind(file_id.to_string());
        Ok(q.build_query_scalar::<i64>().fetch_one(&self.pool).await? > 0)
    }

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
