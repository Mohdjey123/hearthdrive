use crate::Database;
use nexus_common::types::{Role, User};
use nexus_common::{NexusError, Result, new_id, time::now_ms};

impl Database {
    pub async fn count_users(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&self.pool).await?)
    }

    /// Creates the owner account, atomically refusing if any user exists.
    pub async fn create_first_owner(&self, username: &str, password_hash: &str) -> Result<Option<User>> {
        let id = new_id();
        let now = now_ms();
        let result = sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at) \
             SELECT ?, ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users)",
        )
        .bind(&id)
        .bind(username)
        .bind(password_hash)
        .bind(Role::Owner)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Ok(None);
        }
        Ok(Some(User { id, username: username.to_string(), role: Role::Owner, created_at: now }))
    }

    /// Returns the user and their Argon2id password hash.
    pub async fn find_credentials(&self, username: &str) -> Result<Option<(User, String)>> {
        let row: Option<(String, String, Role, i64, Option<String>)> = sqlx::query_as(
            "SELECT id, username, role, created_at, password_hash FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(id, username, role, created_at, hash)| {
            hash.map(|h| (User { id, username, role, created_at }, h))
        }))
    }

    pub async fn get_user(&self, id: &str) -> Result<User> {
        sqlx::query_as::<_, User>("SELECT id, username, role, created_at FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("user".into()))
    }

    pub async fn first_owner(&self) -> Result<Option<User>> {
        Ok(sqlx::query_as::<_, User>(
            "SELECT id, username, role, created_at FROM users WHERE role = 'owner' ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?)
    }
}
