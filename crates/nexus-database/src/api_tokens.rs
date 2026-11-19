//! Tokens for programs: scripts using the API, WebDAV and S3 clients.

use crate::Database;
use nexus_common::{NexusError, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ApiToken {
    /// Also the S3 access key id.
    pub id: String,
    pub user_id: String,
    pub name: String,
    /// The secret, sealed with this device's key.
    #[serde(skip)]
    pub secret: String,
    pub read_only: bool,
    pub created_at: i64,
    pub last_used: Option<i64>,
    pub expires_at: Option<i64>,
}

impl Database {
    pub async fn create_api_token(&self, t: &ApiToken) -> Result<()> {
        sqlx::query(
            "INSERT INTO api_tokens (id, user_id, name, secret, read_only, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&t.id)
        .bind(&t.user_id)
        .bind(&t.name)
        .bind(&t.secret)
        .bind(t.read_only)
        .bind(t.created_at)
        .bind(t.expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A person's tokens, or everyone's (`None`).
    pub async fn list_api_tokens(&self, user_id: Option<&str>) -> Result<Vec<ApiToken>> {
        Ok(match user_id {
            Some(u) => sqlx::query_as::<_, ApiToken>("SELECT * FROM api_tokens WHERE user_id = ? ORDER BY created_at")
                .bind(u)
                .fetch_all(&self.pool)
                .await?,
            None => sqlx::query_as::<_, ApiToken>("SELECT * FROM api_tokens ORDER BY created_at").fetch_all(&self.pool).await?,
        })
    }

    pub async fn get_api_token(&self, id: &str) -> Result<ApiToken> {
        sqlx::query_as::<_, ApiToken>("SELECT * FROM api_tokens WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("token".into()))
    }

    pub async fn delete_api_token(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM api_tokens WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn api_token_used(&self, id: &str, at: i64) -> Result<()> {
        sqlx::query("UPDATE api_tokens SET last_used = ? WHERE id = ?").bind(at).bind(id).execute(&self.pool).await?;
        Ok(())
    }
}

impl Database {
    /// Files (not folders) whose path starts with `prefix`, after `after`
    /// in byte order: S3 listings without a delimiter.
    pub async fn object_list(&self, root_id: &str, prefix: &str, after: &str, limit: i64) -> Result<Vec<nexus_common::types::FileEntry>> {
        Ok(sqlx::query_as::<_, nexus_common::types::FileEntry>(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM files WHERE root_id = ? AND is_dir = 0 AND availability = 'online' \
             AND relative_path LIKE ? ESCAPE '\\' AND relative_path > ? COLLATE BINARY \
             ORDER BY relative_path COLLATE BINARY LIMIT ?",
            crate::files::FILE_COLUMNS
        )))
        .bind(root_id)
        .bind(format!("{}%", crate::like_escape(prefix)))
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Everything directly in a folder, for listings by folder.
    pub async fn folder_items(&self, root_id: &str, parent: &str, limit: i64) -> Result<Vec<nexus_common::types::FileEntry>> {
        Ok(sqlx::query_as::<_, nexus_common::types::FileEntry>(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM files WHERE root_id = ? AND parent_path = ? AND availability = 'online' LIMIT ?",
            crate::files::FILE_COLUMNS
        )))
        .bind(root_id)
        .bind(parent)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
}
