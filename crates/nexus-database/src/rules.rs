//! Automation rules: what to do with files that match, and when.

use crate::files::{FILE_COLUMNS, fts_query};
use crate::{Database, like_escape};
use nexus_common::types::FileEntry;
use nexus_common::{NexusError, Result};
use serde::Serialize;
use sqlx::{QueryBuilder, Sqlite};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// `added` | `hourly` | `daily`
    pub trigger: String,
    pub root_id: String,
    /// The folder the rule looks in (`''`: the whole storage location).
    pub path: String,
    /// JSON: which files.
    pub conditions: String,
    /// JSON: what happens to them.
    pub action: String,
    pub created_by: String,
    pub created_at: i64,
    pub last_run: Option<i64>,
    pub last_result: Option<String>,
    pub files_done: i64,
}

/// Which files a rule looks at.
#[derive(Debug, Clone, Default)]
pub struct RuleScope {
    pub root_id: String,
    pub path: String,
    pub subfolders: bool,
    pub extensions: Vec<String>,
    pub name_contains: Option<String>,
    pub text_contains: Option<String>,
    pub tag: Option<String>,
    /// Modified before this time (ms).
    pub modified_before: Option<i64>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    /// Only files indexed at or after this time (ms): new files.
    pub indexed_after: Option<i64>,
    /// Leave out files this rule has already dealt with.
    pub skip_done_for: Option<String>,
}

impl Database {
    pub async fn list_rules(&self) -> Result<Vec<Rule>> {
        Ok(sqlx::query_as::<_, Rule>("SELECT * FROM rules ORDER BY created_at").fetch_all(&self.pool).await?)
    }

    pub async fn get_rule(&self, id: &str) -> Result<Rule> {
        sqlx::query_as::<_, Rule>("SELECT * FROM rules WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("rule".into()))
    }

    pub async fn save_rule(&self, r: &Rule) -> Result<()> {
        sqlx::query(
            "INSERT INTO rules (id, name, enabled, trigger, root_id, path, conditions, action, created_by, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET name = excluded.name, enabled = excluded.enabled, trigger = excluded.trigger, \
             root_id = excluded.root_id, path = excluded.path, conditions = excluded.conditions, action = excluded.action",
        )
        .bind(&r.id)
        .bind(&r.name)
        .bind(r.enabled)
        .bind(&r.trigger)
        .bind(&r.root_id)
        .bind(&r.path)
        .bind(&r.conditions)
        .bind(&r.action)
        .bind(&r.created_by)
        .bind(r.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete_rule(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM rule_done WHERE rule_id = ?").bind(id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM rules WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn rule_done(&self, rule_id: &str, file_id: &str, at: i64) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO rule_done (rule_id, file_id, done_at) VALUES (?, ?, ?)")
            .bind(rule_id)
            .bind(file_id)
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn rule_undone(&self, rule_id: &str, file_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM rule_done WHERE rule_id = ? AND file_id = ?").bind(rule_id).bind(file_id).execute(&self.pool).await?;
        Ok(())
    }

    /// Forget which files a rule dealt with (after its conditions change).
    pub async fn reset_rule(&self, rule_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM rule_done WHERE rule_id = ?").bind(rule_id).execute(&self.pool).await?;
        Ok(())
    }

    /// Space taken by extra copies of the same content: (bytes, files).
    pub async fn duplicate_waste(&self, root_id: &str) -> Result<(i64, i64)> {
        let row: (Option<i64>, Option<i64>) = sqlx::query_as(
            "SELECT SUM(size * (n - 1)), SUM(n - 1) FROM \
             (SELECT MAX(size) AS size, COUNT(*) AS n FROM files WHERE root_id = ? AND is_dir = 0 \
             AND content_hash IS NOT NULL AND size > 0 GROUP BY content_hash HAVING COUNT(*) > 1)",
        )
        .bind(root_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((row.0.unwrap_or(0), row.1.unwrap_or(0)))
    }

    /// Files with a tag that sit outside `folder` (and its subfolders).
    pub async fn tagged_outside(&self, root_id: &str, tag: &str, folder: &str) -> Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM files f JOIN file_tags t ON t.file_id = f.id \
             WHERE f.root_id = ? AND t.tag = ? AND f.relative_path NOT LIKE ? ESCAPE '\\'",
        )
        .bind(root_id)
        .bind(tag)
        .bind(format!("{}/%", like_escape(folder)))
        .fetch_one(&self.pool)
        .await?;
        Ok(n)
    }

    pub async fn rule_ran(&self, id: &str, at: i64, result: &str, files: i64) -> Result<()> {
        sqlx::query("UPDATE rules SET last_run = ?, last_result = ?, files_done = files_done + ? WHERE id = ?")
            .bind(at)
            .bind(result)
            .bind(files)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Files a rule applies to.
    pub async fn rule_files(&self, s: &RuleScope, limit: i64) -> Result<Vec<FileEntry>> {
        let mut q: QueryBuilder<Sqlite> =
            QueryBuilder::new(format!("SELECT {FILE_COLUMNS} FROM files WHERE is_dir = 0 AND availability = 'online' AND root_id = "));
        q.push_bind(s.root_id.clone());
        if let Some(after) = s.indexed_after {
            q.push(" AND indexed_at >= ").push_bind(after);
        }
        if let Some(rule) = &s.skip_done_for {
            q.push(" AND id NOT IN (SELECT file_id FROM rule_done WHERE rule_id = ").push_bind(rule.clone()).push(")");
        }
        let path = s.path.trim_matches('/').to_string();
        if s.subfolders {
            if !path.is_empty() {
                q.push(" AND relative_path LIKE ").push_bind(format!("{}/%", like_escape(&path))).push(" ESCAPE '\\'");
            }
        } else {
            q.push(" AND parent_path = ").push_bind(path);
        }
        if !s.extensions.is_empty() {
            q.push(" AND lower(extension) IN (");
            let mut sep = q.separated(", ");
            for e in &s.extensions {
                sep.push_bind(e.to_lowercase());
            }
            sep.push_unseparated(")");
        }
        if let Some(name) = &s.name_contains {
            q.push(" AND filename LIKE ").push_bind(format!("%{}%", like_escape(name))).push(" ESCAPE '\\'");
        }
        if let Some(text) = &s.text_contains {
            q.push(" AND id IN (SELECT file_id FROM doc_text WHERE doc_text MATCH ").push_bind(fts_query(text)).push(")");
        }
        if let Some(tag) = &s.tag {
            q.push(" AND id IN (SELECT file_id FROM file_tags WHERE tag = ").push_bind(tag.clone()).push(")");
        }
        if let Some(before) = s.modified_before {
            q.push(" AND modified_at < ").push_bind(before);
        }
        if let Some(min) = s.min_size {
            q.push(" AND size >= ").push_bind(min);
        }
        if let Some(max) = s.max_size {
            q.push(" AND size <= ").push_bind(max);
        }
        q.push(" ORDER BY modified_at LIMIT ").push_bind(limit);
        Ok(q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?)
    }

    /// The biggest files not changed since `before`, in a storage location.
    pub async fn big_old_files(&self, root_id: &str, before: i64, limit: i64) -> Result<Vec<FileEntry>> {
        Ok(sqlx::query_as::<_, FileEntry>(sqlx::AssertSqlSafe(format!(
            "SELECT {FILE_COLUMNS} FROM files WHERE root_id = ? AND is_dir = 0 AND modified_at < ? ORDER BY size DESC LIMIT ?"
        )))
        .bind(root_id)
        .bind(before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
}
