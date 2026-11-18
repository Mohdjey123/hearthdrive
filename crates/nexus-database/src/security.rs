//! Two-step sign-in, passkeys, locked accounts and the sessions people can
//! see and end.

use crate::Database;
use nexus_common::types::ActivityEntry;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::Serialize;

/// A person's authenticator-app setup.
#[derive(Debug, Clone, Default)]
pub struct TotpState {
    /// The secret as stored (protected by the caller).
    pub secret: Option<Vec<u8>>,
    /// Two-step sign-in is on since then; `None` while being set up.
    pub enabled_at: Option<i64>,
    pub last_step: Option<i64>,
}

/// A passkey someone added.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Passkey {
    pub id: String,
    #[serde(skip)]
    pub user_id: String,
    #[serde(skip)]
    pub credential_id: String,
    #[serde(skip)]
    pub public_key: Vec<u8>,
    #[serde(skip)]
    pub sign_count: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// One place someone is signed in.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct SessionInfo {
    pub id: String,
    #[serde(skip)]
    pub token_hash: String,
    /// The browser's user agent.
    pub client: Option<String>,
    pub method: Option<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    pub expires_at: i64,
}

/// What People shows about each account's sign-in.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserSecurity {
    pub user_id: String,
    pub two_factor: bool,
    pub locked_at: Option<i64>,
    pub passkeys: i64,
}

/// Secret, when it was turned on, last step used.
type TotpRow = (Option<Vec<u8>>, Option<i64>, Option<i64>);

const PASSKEY_COLUMNS: &str = "id, user_id, credential_id, public_key, sign_count, name, created_at, last_used_at";

impl Database {
    pub async fn totp_state(&self, user_id: &str) -> Result<TotpState> {
        let row: Option<TotpRow> =
            sqlx::query_as("SELECT totp_secret, totp_enabled_at, totp_last_step FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        let (secret, enabled_at, last_step) = row.ok_or_else(|| NexusError::NotFound("user".into()))?;
        Ok(TotpState { secret, enabled_at, last_step })
    }

    /// Stores a new secret while two-step sign-in is being set up; whatever
    /// was set up before stays in force until the new one is confirmed.
    pub async fn set_totp_pending(&self, user_id: &str, secret: &[u8]) -> Result<()> {
        sqlx::query("UPDATE users SET totp_secret = ?, totp_enabled_at = NULL, totp_last_step = NULL WHERE id = ? AND totp_enabled_at IS NULL")
            .bind(secret)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Turns two-step sign-in on with the code just accepted and a fresh set
    /// of recovery codes (stored hashed).
    pub async fn enable_totp(&self, user_id: &str, step: i64, code_hashes: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE users SET totp_enabled_at = ?, totp_last_step = ? WHERE id = ?")
            .bind(now_ms())
            .bind(step)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
        for hash in code_hashes {
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?, ?)").bind(user_id).bind(hash).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Turns two-step sign-in off and forgets the secret and recovery codes.
    pub async fn disable_totp(&self, user_id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE users SET totp_secret = NULL, totp_enabled_at = NULL, totp_last_step = NULL WHERE id = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records the time step of an accepted code. `false` if a code of this
    /// or a later step was already used (each code works once).
    pub async fn use_totp_step(&self, user_id: &str, step: i64) -> Result<bool> {
        let r = sqlx::query("UPDATE users SET totp_last_step = ? WHERE id = ? AND (totp_last_step IS NULL OR totp_last_step < ?)")
            .bind(step)
            .bind(user_id)
            .bind(step)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn replace_recovery_codes(&self, user_id: &str, code_hashes: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?").bind(user_id).execute(&mut *tx).await?;
        for hash in code_hashes {
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?, ?)").bind(user_id).bind(hash).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Uses up a recovery code. `false` if it is not one of the person's
    /// unused codes.
    pub async fn use_recovery_code(&self, user_id: &str, code_hash: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE recovery_codes SET used_at = ? WHERE user_id = ? AND code_hash = ? AND used_at IS NULL")
            .bind(now_ms())
            .bind(user_id)
            .bind(code_hash)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn recovery_codes_left(&self, user_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM recovery_codes WHERE user_id = ? AND used_at IS NULL")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn add_passkey(&self, user_id: &str, credential_id: &str, public_key: &[u8], algorithm: i64, sign_count: u32, name: &str) -> Result<Passkey> {
        let id = new_id();
        let now = now_ms();
        let result = sqlx::query(
            "INSERT INTO passkeys (id, user_id, credential_id, public_key, algorithm, sign_count, name, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (credential_id) DO NOTHING",
        )
        .bind(&id)
        .bind(user_id)
        .bind(credential_id)
        .bind(public_key)
        .bind(algorithm)
        .bind(i64::from(sign_count))
        .bind(name)
        .bind(now)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(NexusError::Conflict("This passkey was already added".into()));
        }
        Ok(Passkey {
            id,
            user_id: user_id.into(),
            credential_id: credential_id.into(),
            public_key: public_key.to_vec(),
            sign_count: i64::from(sign_count),
            name: name.into(),
            created_at: now,
            last_used_at: None,
        })
    }

    pub async fn passkeys_of(&self, user_id: &str) -> Result<Vec<Passkey>> {
        Ok(sqlx::query_as::<_, Passkey>(sqlx::AssertSqlSafe(format!(
            "SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE user_id = ? ORDER BY created_at"
        )))
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn passkey_by_credential(&self, credential_id: &str) -> Result<Option<Passkey>> {
        Ok(sqlx::query_as::<_, Passkey>(sqlx::AssertSqlSafe(format!("SELECT {PASSKEY_COLUMNS} FROM passkeys WHERE credential_id = ?")))
            .bind(credential_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Records a sign-in with a passkey. `false` if another sign-in with it
    /// moved the counter meanwhile.
    pub async fn use_passkey(&self, id: &str, old_count: i64, new_count: u32) -> Result<bool> {
        let r = sqlx::query("UPDATE passkeys SET sign_count = ?, last_used_at = ? WHERE id = ? AND sign_count = ?")
            .bind(i64::from(new_count))
            .bind(now_ms())
            .bind(id)
            .bind(old_count)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Removes one of a person's passkeys; `None` if they have no such one.
    pub async fn remove_passkey(&self, user_id: &str, id: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar("DELETE FROM passkeys WHERE id = ? AND user_id = ? RETURNING name")
            .bind(id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Where a person is signed in, most recently used first.
    pub async fn sessions_of(&self, user_id: &str) -> Result<Vec<SessionInfo>> {
        Ok(sqlx::query_as::<_, SessionInfo>(
            "SELECT id, token_hash, client, method, created_at, last_used_at, expires_at FROM sessions \
             WHERE user_id = ? AND expires_at > ? ORDER BY COALESCE(last_used_at, created_at) DESC",
        )
        .bind(user_id)
        .bind(now_ms())
        .fetch_all(&self.pool)
        .await?)
    }

    /// Ends one of a person's sessions; `false` if they have no such one.
    pub async fn end_session(&self, user_id: &str, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM sessions WHERE id = ? AND user_id = ?").bind(id).bind(user_id).execute(&self.pool).await?;
        Ok(r.rows_affected() == 1)
    }

    /// Ends every session of a person except `keep` (a token hash).
    pub async fn end_other_sessions(&self, user_id: &str, keep: Option<&str>) -> Result<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash IS NOT ?")
            .bind(user_id)
            .bind(keep)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Locks an account (ending its sessions) or unlocks it.
    pub async fn set_user_locked(&self, id: &str, locked: bool) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE users SET locked_at = ? WHERE id = ?").bind(locked.then(now_ms)).bind(id).execute(&mut *tx).await?;
        if locked {
            sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn user_locked(&self, id: &str) -> Result<bool> {
        let locked: Option<Option<i64>> = sqlx::query_scalar("SELECT locked_at FROM users WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        Ok(locked.flatten().is_some())
    }

    /// Owners that are not locked: at least one must stay.
    pub async fn count_unlocked_owners(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'owner' AND locked_at IS NULL").fetch_one(&self.pool).await?)
    }

    /// Two-step sign-in, lock and passkeys of every account.
    pub async fn user_security(&self) -> Result<Vec<UserSecurity>> {
        Ok(sqlx::query_as::<_, UserSecurity>(
            "SELECT u.id AS user_id, u.totp_enabled_at IS NOT NULL AS two_factor, u.locked_at, \
                    (SELECT COUNT(*) FROM passkeys p WHERE p.user_id = u.id) AS passkeys \
             FROM users u",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Sign-ins, account and device changes: the security log.
    pub async fn security_activity(&self, limit: i64) -> Result<Vec<ActivityEntry>> {
        Ok(sqlx::query_as::<_, ActivityEntry>(
            "SELECT a.id, a.action, a.resource, \
                    COALESCE(u.username, CASE WHEN d.is_local = 0 THEN d.name END) AS username, a.timestamp \
             FROM activity_log a LEFT JOIN users u ON u.id = a.user_id LEFT JOIN devices d ON d.id = a.device_id \
             WHERE a.action LIKE 'auth.%' OR a.action LIKE 'user.%' OR a.action LIKE 'device.%' \
                OR a.action LIKE 'pairing.%' OR a.action LIKE 'setup.%' OR a.action = 'backup.key_shown' \
             ORDER BY a.timestamp DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
}
