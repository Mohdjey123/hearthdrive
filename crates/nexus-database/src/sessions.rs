use crate::Database;
use nexus_common::types::{Role, User};
use nexus_common::{Result, new_id, time::now_ms};

/// A session row joined with its user, and when it was last used.
type SessionRow = (String, String, Role, i64, Option<String>, Option<i64>);

impl Database {
    /// Stores a new session. `method` says how the person signed in
    /// (`password`, `password+code`, `password+recovery`, `passkey`, `setup`).
    pub async fn create_session(
        &self,
        user_id: &str,
        token_hash: &str,
        expires_at: i64,
        client: Option<&str>,
        method: &str,
    ) -> Result<()> {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO sessions (id, user_id, token_hash, client, created_at, expires_at, method, last_used_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(user_id)
        .bind(token_hash)
        .bind(client.map(|c| c.chars().take(300).collect::<String>()))
        .bind(now)
        .bind(expires_at)
        .bind(method)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The user owning an unexpired session, if any (never a locked
    /// account). Notes when the session was used, at most every few minutes.
    pub async fn session_user(&self, token_hash: &str) -> Result<Option<User>> {
        let now = now_ms();
        let row: Option<SessionRow> = sqlx::query_as(
            "SELECT u.id, u.username, u.role, u.created_at, u.allowed_roots, s.last_used_at FROM sessions s \
             JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND s.expires_at > ? AND u.locked_at IS NULL",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, username, role, created_at, allowed_roots, last_used)) = row else { return Ok(None) };
        if last_used.is_none_or(|t| now - t > 5 * 60 * 1000) {
            sqlx::query("UPDATE sessions SET last_used_at = ? WHERE token_hash = ?").bind(now).bind(token_hash).execute(&self.pool).await?;
        }
        Ok(Some(User { id, username, role, created_at, allowed_roots }))
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
