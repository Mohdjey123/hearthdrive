use crate::Database;
use sqlx::AssertSqlSafe;
use nexus_common::types::DeletedFile;
use nexus_common::{NexusError, Result};

const COLUMNS: &str =
    "id, root_id, original_path, filename, is_dir, size, trash_name, deleted_at, deleted_by";

impl Database {
    pub async fn insert_deleted(&self, d: &DeletedFile) -> Result<()> {
        sqlx::query(
            "INSERT INTO deleted_files (id, root_id, original_path, filename, is_dir, size, trash_name, deleted_at, deleted_by) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&d.id)
        .bind(&d.root_id)
        .bind(&d.original_path)
        .bind(&d.filename)
        .bind(d.is_dir)
        .bind(d.size)
        .bind(&d.trash_name)
        .bind(d.deleted_at)
        .bind(&d.deleted_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_deleted(&self) -> Result<Vec<DeletedFile>> {
        Ok(sqlx::query_as::<_, DeletedFile>(AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM deleted_files ORDER BY deleted_at DESC"
        )))
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_deleted(&self, id: &str) -> Result<DeletedFile> {
        sqlx::query_as::<_, DeletedFile>(AssertSqlSafe(format!("SELECT {COLUMNS} FROM deleted_files WHERE id = ?")))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("deleted item".into()))
    }

    pub async fn remove_deleted(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM deleted_files WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
