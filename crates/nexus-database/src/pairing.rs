use crate::Database;
use nexus_common::types::{PairingRequest, PairingSession};
use nexus_common::{NexusError, Result, new_id, time::now_ms};

const REQUEST_COLUMNS: &str = "id, session_id, device_id, name, device_type, public_key, addresses, version, \
                               remote_ip, status, created_at, decided_at";

/// Fields of an incoming pairing request.
pub struct NewPairingRequest<'a> {
    pub session_id: &'a str,
    pub device_id: &'a str,
    pub name: &'a str,
    pub device_type: &'a str,
    pub public_key: &'a str,
    pub addresses: &'a [String],
    pub version: Option<&'a str>,
    pub remote_ip: Option<&'a str>,
}

impl Database {
    /// Opens an invitation. Earlier open invitations are closed, so only the
    /// newest QR code works.
    pub async fn create_pairing_session(&self, code_hash: &str, created_by: Option<&str>, expires_at: i64) -> Result<String> {
        let id = new_id();
        let now = now_ms();
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE pairing_sessions SET closed_at = ? WHERE closed_at IS NULL")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO pairing_sessions (id, code_hash, created_by, created_at, expires_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(code_hash)
            .bind(created_by)
            .bind(now)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// The open, unexpired invitation with this code hash.
    pub async fn open_pairing_session(&self, code_hash: &str) -> Result<Option<PairingSession>> {
        Ok(sqlx::query_as::<_, PairingSession>(
            "SELECT id, expires_at, closed_at FROM pairing_sessions \
             WHERE code_hash = ? AND closed_at IS NULL AND expires_at > ?",
        )
        .bind(code_hash)
        .bind(now_ms())
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn close_pairing_session(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE pairing_sessions SET closed_at = COALESCE(closed_at, ?) WHERE id = ?")
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn count_session_requests(&self, session_id: &str) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM pairing_requests WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn insert_pairing_request(&self, req: &NewPairingRequest<'_>) -> Result<PairingRequest> {
        let id = new_id();
        let addresses = serde_json::to_string(req.addresses).map_err(NexusError::internal)?;
        sqlx::query(
            "INSERT INTO pairing_requests (id, session_id, device_id, name, device_type, public_key, addresses, \
                                           version, remote_ip, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
        )
        .bind(&id)
        .bind(req.session_id)
        .bind(req.device_id)
        .bind(req.name)
        .bind(req.device_type)
        .bind(req.public_key)
        .bind(addresses)
        .bind(req.version)
        .bind(req.remote_ip)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        self.get_pairing_request(&id).await
    }

    pub async fn get_pairing_request(&self, id: &str) -> Result<PairingRequest> {
        self.expire_pairing_requests().await?;
        sqlx::query_as::<_, PairingRequest>(&format!("SELECT {REQUEST_COLUMNS} FROM pairing_requests WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("pairing request".into()))
    }

    pub async fn pending_pairing_requests(&self) -> Result<Vec<PairingRequest>> {
        self.expire_pairing_requests().await?;
        Ok(sqlx::query_as::<_, PairingRequest>(&format!(
            "SELECT {REQUEST_COLUMNS} FROM pairing_requests WHERE status = 'pending' ORDER BY created_at"
        ))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Moves a pending request to `approved` or `rejected`. Returns false if
    /// it was no longer pending.
    pub async fn decide_pairing_request(&self, id: &str, status: &str, decided_by: Option<&str>) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE pairing_requests SET status = ?, decided_at = ?, decided_by = ? WHERE id = ? AND status = 'pending'",
        )
        .bind(status)
        .bind(now_ms())
        .bind(decided_by)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Requests waiting longer than their invitation's lifetime (plus grace
    /// for the owner to respond) expire.
    async fn expire_pairing_requests(&self) -> Result<()> {
        const GRACE_MS: i64 = 15 * 60 * 1000;
        sqlx::query(
            "UPDATE pairing_requests SET status = 'expired' WHERE status = 'pending' AND session_id IN \
             (SELECT id FROM pairing_sessions WHERE expires_at + ? < ?)",
        )
        .bind(GRACE_MS)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
