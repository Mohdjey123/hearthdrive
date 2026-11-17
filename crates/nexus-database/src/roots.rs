use crate::Database;
use nexus_common::types::{ScanState, StorageRoot};
use nexus_common::{NexusError, Result};

const ROOT_COLUMNS: &str = "id, device_id, name, absolute_path, root_type, read_only, enabled, created_at";

impl Database {
    pub async fn insert_root(&self, root: &StorageRoot) -> Result<()> {
        sqlx::query(
            "INSERT INTO storage_roots (id, device_id, name, absolute_path, root_type, read_only, enabled, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&root.id)
        .bind(&root.device_id)
        .bind(&root.name)
        .bind(&root.absolute_path)
        .bind(root.root_type)
        .bind(root.read_only)
        .bind(root.enabled)
        .bind(root.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_roots(&self) -> Result<Vec<StorageRoot>> {
        Ok(sqlx::query_as::<_, StorageRoot>(&format!(
            "SELECT {ROOT_COLUMNS} FROM storage_roots ORDER BY created_at"
        ))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_root(&self, id: &str) -> Result<StorageRoot> {
        sqlx::query_as::<_, StorageRoot>(&format!("SELECT {ROOT_COLUMNS} FROM storage_roots WHERE id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("storage root".into()))
    }

    /// Removes the registration and its index. Never touches files on disk.
    pub async fn delete_root(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM storage_roots WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_scan_state(&self, root_id: &str) -> Result<Option<ScanState>> {
        Ok(sqlx::query_as::<_, ScanState>(
            "SELECT root_id, status, files_seen, bytes_seen, started_at, completed_at, error \
             FROM root_scan_state WHERE root_id = ?",
        )
        .bind(root_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn set_scan_state(&self, s: &ScanState) -> Result<()> {
        sqlx::query(
            "INSERT INTO root_scan_state (root_id, status, files_seen, bytes_seen, started_at, completed_at, error) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (root_id) DO UPDATE SET status = excluded.status, files_seen = excluded.files_seen, \
               bytes_seen = excluded.bytes_seen, started_at = excluded.started_at, \
               completed_at = excluded.completed_at, error = excluded.error",
        )
        .bind(&s.root_id)
        .bind(&s.status)
        .bind(s.files_seen)
        .bind(s.bytes_seen)
        .bind(s.started_at)
        .bind(s.completed_at)
        .bind(&s.error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
