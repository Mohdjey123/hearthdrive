//! Upload links: pages anyone with the link can add files to a folder from.

use crate::Database;
use nexus_common::{NexusError, Result, time::now_ms};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct UploadLink {
    pub id: String,
    /// Sealed with the node's key; never sent as is.
    #[serde(skip)]
    pub secret: String,
    pub root_id: String,
    pub path: String,
    pub title: String,
    pub message: String,
    pub user_id: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub max_bytes: i64,
    pub received_bytes: i64,
    pub received_files: i64,
    pub last_used_at: Option<i64>,
}

pub struct NewUploadLink<'a> {
    pub id: &'a str,
    pub secret: &'a str,
    pub root_id: &'a str,
    pub path: &'a str,
    pub title: &'a str,
    pub message: &'a str,
    pub user_id: &'a str,
    pub created_by: &'a str,
    pub expires_at: Option<i64>,
    pub max_bytes: i64,
}

const COLUMNS: &str = "id, secret, root_id, path, title, message, user_id, created_by, created_at, expires_at, max_bytes, \
                       received_bytes, received_files, last_used_at";

impl Database {
    pub async fn create_upload_link(&self, l: NewUploadLink<'_>) -> Result<UploadLink> {
        sqlx::query(
            "INSERT INTO upload_links (id, secret, root_id, path, title, message, user_id, created_by, created_at, expires_at, max_bytes) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(l.id)
        .bind(l.secret)
        .bind(l.root_id)
        .bind(l.path)
        .bind(l.title)
        .bind(l.message)
        .bind(l.user_id)
        .bind(l.created_by)
        .bind(now_ms())
        .bind(l.expires_at)
        .bind(l.max_bytes)
        .execute(&self.pool)
        .await?;
        self.get_upload_link(l.id).await
    }

    pub async fn get_upload_link(&self, id: &str) -> Result<UploadLink> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLUMNS} FROM upload_links WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("Upload link".into()))
    }

    /// Every upload link, or the ones `user_id` made.
    pub async fn list_upload_links(&self, user_id: Option<&str>) -> Result<Vec<UploadLink>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM upload_links WHERE ?1 IS NULL OR user_id = ?1 ORDER BY created_at DESC"
        );
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(user_id).fetch_all(&self.pool).await?)
    }

    pub async fn delete_upload_link(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM upload_links WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Takes room for `bytes` more on a link, if its limit allows. Taken
    /// before the upload starts, so two uploads at once cannot both pass.
    pub async fn reserve_upload_room(&self, id: &str, bytes: i64) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE upload_links SET received_bytes = received_bytes + ?1, last_used_at = ?2 \
             WHERE id = ?3 AND received_bytes + ?1 <= max_bytes",
        )
        .bind(bytes)
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    /// Gives back room taken for an upload that failed.
    pub async fn release_upload_room(&self, id: &str, bytes: i64) -> Result<()> {
        sqlx::query("UPDATE upload_links SET received_bytes = MAX(0, received_bytes - ?) WHERE id = ?")
            .bind(bytes)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn upload_link_received(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE upload_links SET received_files = received_files + 1 WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
}
