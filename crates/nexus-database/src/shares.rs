//! Share links made on this node.

use crate::Database;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::Serialize;
use sqlx::AssertSqlSafe;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Share {
    pub id: String,
    pub file_id: String,
    pub root_id: String,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub provider: String,
    pub service: String,
    pub url: Option<String>,
    #[serde(skip)]
    pub remote_id: Option<String>,
    #[serde(skip)]
    pub owner_token: Option<String>,
    pub expires_at: i64,
    pub downloads: Option<i64>,
    pub downloaded: i64,
    pub has_password: bool,
    pub status: String,
    pub error: Option<String>,
    pub job_id: Option<String>,
    #[serde(skip)]
    pub owner_key: String,
    pub created_by: String,
    pub created_at: i64,
}

pub struct NewShare<'a> {
    pub file_id: &'a str,
    pub root_id: &'a str,
    pub name: &'a str,
    pub is_dir: bool,
    pub size: i64,
    pub provider: &'a str,
    pub service: &'a str,
    pub expires_at: i64,
    pub downloads: Option<i64>,
    pub has_password: bool,
    pub job_id: &'a str,
    pub owner_key: &'a str,
    pub created_by: &'a str,
}

const COLUMNS: &str = "id, file_id, root_id, name, is_dir, size, provider, service, url, remote_id, owner_token, \
                       expires_at, downloads, downloaded, has_password, status, error, job_id, owner_key, created_by, created_at";

impl Database {
    pub async fn create_share(&self, s: &NewShare<'_>) -> Result<Share> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO shares (id, file_id, root_id, name, is_dir, size, provider, service, expires_at, downloads, \
             has_password, status, job_id, owner_key, created_by, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'uploading', ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(s.file_id)
        .bind(s.root_id)
        .bind(s.name)
        .bind(s.is_dir)
        .bind(s.size)
        .bind(s.provider)
        .bind(s.service)
        .bind(s.expires_at)
        .bind(s.downloads)
        .bind(s.has_password)
        .bind(s.job_id)
        .bind(s.owner_key)
        .bind(s.created_by)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        self.get_share(&id).await
    }

    pub async fn get_share(&self, id: &str) -> Result<Share> {
        sqlx::query_as::<_, Share>(AssertSqlSafe(format!("SELECT {COLUMNS} FROM shares WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("Link".into()))
    }

    /// Links newest first: everyone's, or one person's.
    pub async fn list_shares(&self, owner_key: Option<&str>) -> Result<Vec<Share>> {
        let rows = match owner_key {
            Some(key) => {
                sqlx::query_as::<_, Share>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM shares WHERE owner_key = ? AND status != 'deleted' ORDER BY created_at DESC LIMIT 500"
                )))
                .bind(key)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, Share>(AssertSqlSafe(format!(
                    "SELECT {COLUMNS} FROM shares WHERE status != 'deleted' ORDER BY created_at DESC LIMIT 500"
                )))
                .fetch_all(&self.pool)
                .await?
            }
        };
        Ok(rows)
    }

    /// The upload finished: the link works from now on.
    pub async fn share_ready(&self, id: &str, url: &str, remote_id: Option<&str>, owner_token: Option<&str>, size: i64) -> Result<()> {
        sqlx::query("UPDATE shares SET status = 'active', url = ?, remote_id = ?, owner_token = ?, size = ?, error = NULL WHERE id = ?")
            .bind(url)
            .bind(remote_id)
            .bind(owner_token)
            .bind(size)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_share_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE shares SET status = ?, error = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// What the service last said about downloads and time left.
    pub async fn set_share_usage(&self, id: &str, downloaded: i64, expires_at: i64) -> Result<()> {
        sqlx::query("UPDATE shares SET downloaded = ?, expires_at = ? WHERE id = ?")
            .bind(downloaded)
            .bind(expires_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Forgets a link, its key with it.
    pub async fn forget_share(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM shares WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Links that were still uploading when the node stopped.
    pub async fn fail_unfinished_shares(&self) -> Result<u64> {
        let r = sqlx::query("UPDATE shares SET status = 'failed', error = 'Stopped before the upload finished' WHERE status = 'uploading'")
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Marks links whose time is up as expired, and drops the key.
    pub async fn expire_shares(&self) -> Result<u64> {
        let r = sqlx::query("UPDATE shares SET status = 'expired', owner_token = NULL WHERE status = 'active' AND expires_at <= ?")
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}
