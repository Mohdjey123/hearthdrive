//! Groups of people ("Family", "Office"), for sharing folders with all of
//! them at once.

use crate::Database;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::Serialize;

/// A group and the ids of the people in it.
#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub created_by: String,
    pub created_at: i64,
    pub members: Vec<String>,
}

type GroupRow = (String, String, String, i64);

impl Database {
    pub async fn list_groups(&self) -> Result<Vec<Group>> {
        let rows: Vec<GroupRow> =
            sqlx::query_as("SELECT id, name, created_by, created_at FROM people_groups ORDER BY name COLLATE NOCASE")
                .fetch_all(&self.pool)
                .await?;
        let members: Vec<(String, String)> = sqlx::query_as("SELECT group_id, user_id FROM group_members ORDER BY added_at")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, created_by, created_at)| {
                let members = members.iter().filter(|(g, _)| *g == id).map(|(_, u)| u.clone()).collect();
                Group { id, name, created_by, created_at, members }
            })
            .collect())
    }

    pub async fn get_group(&self, id: &str) -> Result<Group> {
        self.list_groups().await?.into_iter().find(|g| g.id == id).ok_or_else(|| NexusError::NotFound("Group".into()))
    }

    async fn group_name_taken(&self, name: &str, except: Option<&str>) -> Result<bool> {
        let other: Option<String> = sqlx::query_scalar("SELECT id FROM people_groups WHERE name = ? COLLATE NOCASE")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        Ok(other.is_some_and(|o| Some(o.as_str()) != except))
    }

    pub async fn create_group(&self, name: &str, created_by: &str) -> Result<Group> {
        if self.group_name_taken(name, None).await? {
            return Err(NexusError::Conflict(format!("There already is a group called {name}")));
        }
        let id = new_id();
        sqlx::query("INSERT INTO people_groups (id, name, created_by, created_at) VALUES (?, ?, ?, ?)")
            .bind(&id)
            .bind(name)
            .bind(created_by)
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        self.get_group(&id).await
    }

    pub async fn rename_group(&self, id: &str, name: &str) -> Result<()> {
        if self.group_name_taken(name, Some(id)).await? {
            return Err(NexusError::Conflict(format!("There already is a group called {name}")));
        }
        sqlx::query("UPDATE people_groups SET name = ? WHERE id = ?").bind(name).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Replaces who is in a group. People who stay keep the date they were
    /// added.
    pub async fn set_group_members(&self, id: &str, users: &[String]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let current: Vec<String> = sqlx::query_scalar("SELECT user_id FROM group_members WHERE group_id = ?")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
        for gone in current.iter().filter(|u| !users.contains(u)) {
            sqlx::query("DELETE FROM group_members WHERE group_id = ? AND user_id = ?")
                .bind(id)
                .bind(gone)
                .execute(&mut *tx)
                .await?;
        }
        let now = now_ms();
        for user in users {
            sqlx::query("INSERT OR IGNORE INTO group_members (group_id, user_id, added_at) VALUES (?, ?, ?)")
                .bind(id)
                .bind(user)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_group(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM people_groups WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// The ids of the groups a person is in.
    pub async fn groups_of_user(&self, user_id: &str) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT group_id FROM group_members WHERE user_id = ?").bind(user_id).fetch_all(&self.pool).await?)
    }
}
