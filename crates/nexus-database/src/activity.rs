use crate::Database;
use nexus_common::types::ActivityEntry;
use nexus_common::{Result, new_id, time::now_ms};

impl Database {
    pub async fn log_activity(
        &self,
        action: &str,
        resource: Option<&str>,
        user_id: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO activity_log (id, device_id, user_id, action, resource, timestamp) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(new_id())
        .bind(device_id)
        .bind(user_id)
        .bind(action)
        .bind(resource)
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn recent_activity(&self, limit: i64) -> Result<Vec<ActivityEntry>> {
        Ok(sqlx::query_as::<_, ActivityEntry>(
            "SELECT a.id, a.action, a.resource, \
                    COALESCE(u.username, CASE WHEN d.is_local = 0 THEN d.name END) AS username, a.timestamp \
             FROM activity_log a LEFT JOIN users u ON u.id = a.user_id LEFT JOIN devices d ON d.id = a.device_id \
             ORDER BY a.timestamp DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
}
