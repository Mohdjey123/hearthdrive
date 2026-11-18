use crate::Database;
use sqlx::AssertSqlSafe;
use nexus_common::types::{Device, Role};
use nexus_common::{NexusError, Result, new_id, time::now_ms};

const COLUMNS: &str = "id, name, device_type, public_key, network_address, is_local, last_seen, status, created_at, \n                       role, addresses, latency_ms, paired_at, version, allowed_roots, introduced_by";
const LOCAL_DEVICE_KEY: &str = "local_device_id";

impl Database {
    /// Returns this computer's device record, creating it on first run.
    pub async fn ensure_local_device(&self, default_name: &str, device_type: &str) -> Result<Device> {
        if let Some(id) = self.get_setting(LOCAL_DEVICE_KEY).await?
            && let Ok(device) = self.get_device(&id).await {
                return Ok(device);
            }
        let device = Device {
            id: new_id(),
            name: default_name.to_string(),
            device_type: device_type.to_string(),
            public_key: None,
            network_address: None,
            is_local: true,
            last_seen: Some(now_ms()),
            status: "online".into(),
            created_at: now_ms(),
            role: Role::Owner,
            addresses: "[]".into(),
            latency_ms: None,
            paired_at: None,
            version: None,
            allowed_roots: None,
            introduced_by: None,
        };
        sqlx::query(
            "INSERT INTO devices (id, name, device_type, network_address, is_local, last_seen, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&device.id)
        .bind(&device.name)
        .bind(&device.device_type)
        .bind(&device.network_address)
        .bind(device.is_local)
        .bind(device.last_seen)
        .bind(&device.status)
        .bind(device.created_at)
        .execute(&self.pool)
        .await?;
        self.set_setting(LOCAL_DEVICE_KEY, &device.id).await?;
        Ok(device)
    }

    pub async fn get_device(&self, id: &str) -> Result<Device> {
        sqlx::query_as::<_, Device>(AssertSqlSafe(format!("SELECT {COLUMNS} FROM devices WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("device".into()))
    }

    pub async fn list_devices(&self) -> Result<Vec<Device>> {
        Ok(sqlx::query_as::<_, Device>(AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM devices ORDER BY is_local DESC, created_at"
        )))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn rename_device(&self, id: &str, name: &str) -> Result<()> {
        sqlx::query("UPDATE devices SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn touch_device(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE devices SET last_seen = ?, status = 'online' WHERE id = ?")
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records this node's own public key and version.
    pub async fn set_local_identity(&self, id: &str, public_key: &str, version: &str) -> Result<()> {
        sqlx::query("UPDATE devices SET public_key = ?, version = ? WHERE id = ? AND is_local = 1")
            .bind(public_key)
            .bind(version)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Paired devices only (never this node).
    pub async fn list_peer_devices(&self) -> Result<Vec<Device>> {
        Ok(sqlx::query_as::<_, Device>(AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM devices WHERE is_local = 0 AND public_key IS NOT NULL ORDER BY name COLLATE NOCASE"
        )))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Adds a paired device, or refreshes it when it pairs again.
    pub async fn upsert_paired_device(&self, peer: &NewPeer<'_>) -> Result<()> {
        if self.get_setting(LOCAL_DEVICE_KEY).await?.as_deref() == Some(peer.id) {
            return Err(NexusError::invalid("A device cannot pair with itself"));
        }
        let now = now_ms();
        let addresses = serde_json::to_string(peer.addresses).map_err(NexusError::internal)?;
        sqlx::query(
            "INSERT INTO devices (id, name, device_type, public_key, network_address, is_local, last_seen, status,                                   created_at, role, addresses, paired_at, version)              VALUES (?, ?, ?, ?, ?, 0, NULL, 'offline', ?, ?, ?, ?, ?)              ON CONFLICT(id) DO UPDATE SET name = excluded.name, device_type = excluded.device_type,                 public_key = excluded.public_key, network_address = excluded.network_address,                 role = excluded.role, addresses = excluded.addresses, paired_at = excluded.paired_at,                 version = excluded.version, status = 'offline', introduced_by = NULL              WHERE devices.is_local = 0",
        )
        .bind(peer.id)
        .bind(peer.name)
        .bind(peer.device_type)
        .bind(peer.public_key)
        .bind(peer.addresses.first())
        .bind(now)
        .bind(peer.role)
        .bind(addresses)
        .bind(now)
        .bind(peer.version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes a paired device. Its signatures stop being accepted at once.
    pub async fn delete_peer_device(&self, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM devices WHERE id = ? AND is_local = 0")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn set_device_role(&self, id: &str, role: Role) -> Result<()> {
        sqlx::query("UPDATE devices SET role = ? WHERE id = ? AND is_local = 0")
            .bind(role)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records a successful contact with a peer.
    pub async fn mark_peer_online(
        &self,
        id: &str,
        address: Option<&str>,
        latency_ms: Option<i64>,
        name: Option<&str>,
        addresses: Option<&[String]>,
        version: Option<&str>,
    ) -> Result<()> {
        let addresses = match addresses {
            Some(a) if !a.is_empty() => Some(serde_json::to_string(a).map_err(NexusError::internal)?),
            _ => None,
        };
        sqlx::query(
            "UPDATE devices SET status = 'online', last_seen = ?,                 network_address = COALESCE(?, network_address), latency_ms = COALESCE(?, latency_ms),                 name = COALESCE(?, name), addresses = COALESCE(?, addresses), version = COALESCE(?, version)              WHERE id = ? AND is_local = 0",
        )
        .bind(now_ms())
        .bind(address)
        .bind(latency_ms)
        .bind(name)
        .bind(addresses)
        .bind(version)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn mark_peer_offline(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE devices SET status = 'offline', latency_ms = NULL WHERE id = ? AND is_local = 0")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

/// A device being added through pairing.
pub struct NewPeer<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub device_type: &'a str,
    pub public_key: &'a str,
    pub addresses: &'a [String],
    pub role: Role,
    pub version: Option<&'a str>,
}

impl Database {
    /// Adds or refreshes a device introduced by the paired device `by`, with
    /// its role and storage limits. A device paired directly, or introduced
    /// by another device, is left alone. Returns whether it was new.
    pub async fn upsert_introduced_device(&self, peer: &NewPeer<'_>, by: &str, roots: Option<&[String]>) -> Result<bool> {
        if self.get_setting(LOCAL_DEVICE_KEY).await?.as_deref() == Some(peer.id) {
            return Ok(false);
        }
        let existing: Option<(Option<String>,)> = sqlx::query_as("SELECT introduced_by FROM devices WHERE id = ?")
            .bind(peer.id)
            .fetch_optional(&self.pool)
            .await?;
        if let Some((owner,)) = &existing
            && owner.as_deref() != Some(by)
        {
            return Ok(false);
        }
        let addresses = serde_json::to_string(peer.addresses).map_err(NexusError::internal)?;
        let roots = roots.map(|r| serde_json::to_string(r).unwrap_or_else(|_| "[]".into()));
        let now = now_ms();
        sqlx::query(
            "INSERT INTO devices (id, name, device_type, public_key, network_address, is_local, last_seen, status,                                   created_at, role, addresses, paired_at, version, allowed_roots, introduced_by)              VALUES (?, ?, ?, ?, ?, 0, NULL, 'offline', ?, ?, ?, ?, ?, ?, ?)              ON CONFLICT(id) DO UPDATE SET name = excluded.name, device_type = excluded.device_type,                 public_key = excluded.public_key, role = excluded.role, addresses = excluded.addresses,                 allowed_roots = excluded.allowed_roots              WHERE devices.is_local = 0 AND devices.introduced_by = excluded.introduced_by",
        )
        .bind(peer.id)
        .bind(peer.name)
        .bind(peer.device_type)
        .bind(peer.public_key)
        .bind(peer.addresses.first())
        .bind(now)
        .bind(peer.role)
        .bind(addresses)
        .bind(now)
        .bind(peer.version)
        .bind(roots)
        .bind(by)
        .execute(&self.pool)
        .await?;
        Ok(existing.is_none())
    }

    /// Forgets the devices `by` introduced, except `keep`. Returns the ids
    /// forgotten.
    pub async fn forget_introduced(&self, by: &str, keep: &[String]) -> Result<Vec<String>> {
        let introduced: Vec<(String,)> = sqlx::query_as("SELECT id FROM devices WHERE introduced_by = ? AND is_local = 0")
            .bind(by)
            .fetch_all(&self.pool)
            .await?;
        let mut gone = Vec::new();
        for (id,) in introduced {
            if !keep.contains(&id) {
                sqlx::query("DELETE FROM devices WHERE id = ? AND introduced_by = ?").bind(&id).bind(by).execute(&self.pool).await?;
                gone.push(id);
            }
        }
        Ok(gone)
    }

    /// Limits a paired device to some storage locations (`None` = all).
    pub async fn set_device_roots(&self, id: &str, roots: Option<&[String]>) -> Result<()> {
        let json = roots.map(|r| serde_json::to_string(r).unwrap_or_else(|_| "[]".into()));
        sqlx::query("UPDATE devices SET allowed_roots = ? WHERE id = ? AND is_local = 0")
            .bind(json)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
