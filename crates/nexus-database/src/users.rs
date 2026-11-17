use crate::Database;
use nexus_common::types::{Role, User};
use nexus_common::{NexusError, Result, new_id, time::now_ms};

/// A user row with their allowed storage and password hash.
type CredentialRow = (String, String, Role, i64, Option<String>, Option<String>);

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
        Ok(Some(User { id, username: username.to_string(), role: Role::Owner, created_at: now, allowed_roots: None }))
    }

    /// Returns the user and their Argon2id password hash.
    pub async fn find_credentials(&self, username: &str) -> Result<Option<(User, String)>> {
        let row: Option<CredentialRow> = sqlx::query_as(
            "SELECT id, username, role, created_at, allowed_roots, password_hash FROM users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(id, username, role, created_at, allowed_roots, hash)| {
            hash.map(|h| (User { id, username, role, created_at, allowed_roots }, h))
        }))
    }

    pub async fn get_user(&self, id: &str) -> Result<User> {
        sqlx::query_as::<_, User>("SELECT id, username, role, created_at, allowed_roots FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("user".into()))
    }

    pub async fn first_owner(&self) -> Result<Option<User>> {
        Ok(sqlx::query_as::<_, User>(
            "SELECT id, username, role, created_at, allowed_roots FROM users WHERE role = 'owner' ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn list_users(&self) -> Result<Vec<User>> {
        Ok(sqlx::query_as::<_, User>("SELECT id, username, role, created_at, allowed_roots FROM users ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?)
    }

    /// Adds an account. Fails with a conflict if the name is taken.
    pub async fn create_user(&self, username: &str, password_hash: &str, role: Role) -> Result<User> {
        let id = new_id();
        let now = now_ms();
        let result = sqlx::query(
            "INSERT INTO users (id, username, password_hash, role, created_at) \
             SELECT ?, ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users WHERE username = ?)",
        )
        .bind(&id)
        .bind(username)
        .bind(password_hash)
        .bind(role)
        .bind(now)
        .bind(username)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(NexusError::Conflict(format!("The name \"{username}\" is already taken")));
        }
        Ok(User { id, username: username.to_string(), role, created_at: now, allowed_roots: None })
    }

    pub async fn count_owners(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'owner'").fetch_one(&self.pool).await?)
    }

    /// Limits a person to some storage locations (`None`: all of them).
    pub async fn set_user_roots(&self, id: &str, roots: Option<&[String]>) -> Result<()> {
        let json = roots.map(|r| serde_json::to_string(r).unwrap_or_else(|_| "[]".into()));
        sqlx::query("UPDATE users SET allowed_roots = ? WHERE id = ?").bind(json).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn set_user_role(&self, id: &str, role: Role) -> Result<()> {
        sqlx::query("UPDATE users SET role = ? WHERE id = ?").bind(role).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Changes a password and signs the account out everywhere.
    pub async fn set_user_password(&self, id: &str, password_hash: &str) -> Result<()> {
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(password_hash)
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Removes an account and its sessions.
    pub async fn delete_user(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
}
