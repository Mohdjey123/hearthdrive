use crate::Database;
use nexus_common::types::User;
use nexus_common::{Result, new_id, time::now_ms};

impl Database {
    pub async fn create_session(
        &self,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
        client: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (id, user_id, token_hash, client, created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(user_id)
        .bind(token_hash)
        .bind(client)
        .bind(now_ms())
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The user owning an unexpired session, if any.
    pub async fn session_user(&self, token_hash: &str) -> Result<Option<User>> {
        Ok(sqlx::query_as::<_, User>(
            "SELECT u.id, u.username, u.role, u.created_at FROM sessions s \
             JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires_at > ?",
        )
        .bind(token_hash)
        .bind(now_ms())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn purge_expired_sessions(&self) -> Result<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}
