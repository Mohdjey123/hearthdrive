//! Shared folders and comments on files.

use crate::Database;
use crate::files::subtree_pattern;
use nexus_common::{NexusError, Result, new_id, time::now_ms};
use serde::{Deserialize, Serialize};

/// What a member of a shared folder may do in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderAccess {
    /// Add new files and folders, without seeing what is there (a drop folder).
    Add,
    /// Open and download.
    View,
    /// View, add, change and delete.
    Edit,
}

impl FolderAccess {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "add" => Some(Self::Add),
            "view" => Some(Self::View),
            "edit" => Some(Self::Edit),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::View => "view",
            Self::Edit => "edit",
        }
    }
}

/// A person (`user:<id>`) or paired device (`device:<id>`) a folder is shared with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderMember {
    pub member: String,
    pub access: FolderAccess,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedFolder {
    pub id: String,
    pub root_id: String,
    pub path: String,
    pub name: String,
    pub created_by: String,
    pub created_at: i64,
    pub members: Vec<FolderMember>,
}

/// A shared folder as one member has it.
#[derive(Debug, Clone, Serialize)]
pub struct FolderGrant {
    pub folder_id: String,
    pub root_id: String,
    pub path: String,
    pub name: String,
    pub access: FolderAccess,
    pub created_by: String,
    /// Who it was given to: a person or a device.
    pub member: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Comment {
    pub id: String,
    pub file_id: String,
    #[serde(skip)]
    pub root_id: String,
    pub author: String,
    #[serde(skip)]
    pub author_key: String,
    pub body: String,
    pub created_at: i64,
}

type FolderRow = (String, String, String, String, String, i64);
type MemberRow = (String, String, String, i64);
type GrantRow = (String, String, String, String, String, String, String);

const FOLDER_COLUMNS: &str = "id, root_id, path, name, created_by, created_at";

impl Database {
    async fn with_members(&self, rows: Vec<FolderRow>) -> Result<Vec<SharedFolder>> {
        let members: Vec<MemberRow> =
            sqlx::query_as("SELECT folder_id, member, access, added_at FROM folder_members ORDER BY added_at")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|(id, root_id, path, name, created_by, created_at)| {
                let members = members
                    .iter()
                    .filter(|m| m.0 == id)
                    .filter_map(|(_, member, access, added_at)| {
                        Some(FolderMember { member: member.clone(), access: FolderAccess::parse(access)?, added_at: *added_at })
                    })
                    .collect();
                SharedFolder { id, root_id, path, name, created_by, created_at, members }
            })
            .collect())
    }

    pub async fn list_shared_folders(&self) -> Result<Vec<SharedFolder>> {
        let rows: Vec<FolderRow> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {FOLDER_COLUMNS} FROM shared_folders ORDER BY name COLLATE NOCASE")))
                .fetch_all(&self.pool)
                .await?;
        self.with_members(rows).await
    }

    pub async fn get_shared_folder(&self, id: &str) -> Result<SharedFolder> {
        let row: Option<FolderRow> =
            sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {FOLDER_COLUMNS} FROM shared_folders WHERE id = ?")))
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let row = row.ok_or_else(|| NexusError::NotFound("Shared folder".into()))?;
        Ok(self.with_members(vec![row]).await?.remove(0))
    }

    pub async fn shared_folder_at(&self, root_id: &str, path: &str) -> Result<Option<SharedFolder>> {
        let id: Option<String> = sqlx::query_scalar("SELECT id FROM shared_folders WHERE root_id = ? AND path = ?")
            .bind(root_id)
            .bind(path)
            .fetch_optional(&self.pool)
            .await?;
        match id {
            Some(id) => Ok(Some(self.get_shared_folder(&id).await?)),
            None => Ok(None),
        }
    }

    pub async fn create_shared_folder(&self, root_id: &str, path: &str, name: &str, created_by: &str) -> Result<SharedFolder> {
        if self.shared_folder_at(root_id, path).await?.is_some() {
            return Err(NexusError::Conflict("This folder is already shared".into()));
        }
        let id = new_id();
        sqlx::query("INSERT INTO shared_folders (id, root_id, path, name, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(root_id)
            .bind(path)
            .bind(name)
            .bind(created_by)
            .bind(now_ms())
            .execute(&self.pool)
            .await?;
        self.get_shared_folder(&id).await
    }

    pub async fn rename_shared_folder(&self, id: &str, name: &str) -> Result<()> {
        sqlx::query("UPDATE shared_folders SET name = ? WHERE id = ?").bind(name).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Replaces who a folder is shared with. Members who stay keep the date
    /// they were added.
    pub async fn set_folder_members(&self, id: &str, members: &[(String, FolderAccess)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let current: Vec<String> = sqlx::query_scalar("SELECT member FROM folder_members WHERE folder_id = ?")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
        for gone in current.iter().filter(|m| !members.iter().any(|(n, _)| n == *m)) {
            sqlx::query("DELETE FROM folder_members WHERE folder_id = ? AND member = ?")
                .bind(id)
                .bind(gone)
                .execute(&mut *tx)
                .await?;
        }
        let now = now_ms();
        for (member, access) in members {
            sqlx::query(
                "INSERT INTO folder_members (folder_id, member, access, added_at) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(folder_id, member) DO UPDATE SET access = excluded.access",
            )
            .bind(id)
            .bind(member)
            .bind(access.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_shared_folder(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM shared_folders WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// The shared folders given to any of `members`.
    pub async fn grants_for(&self, members: &[String]) -> Result<Vec<FolderGrant>> {
        if members.is_empty() {
            return Ok(vec![]);
        }
        let marks = vec!["?"; members.len()].join(", ");
        let sql = format!(
            "SELECT f.id, f.root_id, f.path, f.name, m.access, f.created_by, m.member FROM folder_members m \
             JOIN shared_folders f ON f.id = m.folder_id WHERE m.member IN ({marks}) ORDER BY f.name COLLATE NOCASE"
        );
        let mut query = sqlx::query_as::<_, GrantRow>(sqlx::AssertSqlSafe(sql));
        for m in members {
            query = query.bind(m);
        }
        Ok(query
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .filter_map(|(folder_id, root_id, path, name, access, created_by, member)| {
                Some(FolderGrant { folder_id, root_id, path, name, access: FolderAccess::parse(&access)?, created_by, member })
            })
            .collect())
    }

    /// Keeps shared folders pointing at the same folder after Hearthdrive renamed
    /// or moved it (or a folder above it).
    pub async fn move_shared_paths(&self, root_id: &str, old_rel: &str, new_rel: &str) -> Result<()> {
        if old_rel.is_empty() {
            return Ok(());
        }
        let old_len = old_rel.chars().count() as i64;
        sqlx::query(
            "UPDATE shared_folders SET path = ?1 || substr(path, ?2 + 1) \
             WHERE root_id = ?3 AND (path = ?4 OR path LIKE ?5 ESCAPE '\\')",
        )
        .bind(new_rel)
        .bind(old_len)
        .bind(root_id)
        .bind(old_rel)
        .bind(subtree_pattern(old_rel))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // -- comments -------------------------------------------------------------

    pub async fn add_comment(&self, file_id: &str, root_id: &str, author: &str, author_key: &str, body: &str) -> Result<Comment> {
        let comment = Comment {
            id: new_id(),
            file_id: file_id.into(),
            root_id: root_id.into(),
            author: author.into(),
            author_key: author_key.into(),
            body: body.into(),
            created_at: now_ms(),
        };
        sqlx::query(
            "INSERT INTO file_comments (id, file_id, root_id, author, author_key, body, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&comment.id)
        .bind(&comment.file_id)
        .bind(&comment.root_id)
        .bind(&comment.author)
        .bind(&comment.author_key)
        .bind(&comment.body)
        .bind(comment.created_at)
        .execute(&self.pool)
        .await?;
        Ok(comment)
    }

    pub async fn comments_of(&self, file_id: &str) -> Result<Vec<Comment>> {
        Ok(sqlx::query_as::<_, Comment>(
            "SELECT id, file_id, root_id, author, author_key, body, created_at FROM file_comments \
             WHERE file_id = ? ORDER BY created_at, id",
        )
        .bind(file_id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_comment(&self, id: &str) -> Result<Comment> {
        sqlx::query_as::<_, Comment>(
            "SELECT id, file_id, root_id, author, author_key, body, created_at FROM file_comments WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| NexusError::NotFound("Comment".into()))
    }

    pub async fn delete_comment(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM file_comments WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Forgets the comments on a file that was deleted for good.
    pub async fn delete_comments_of(&self, file_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM file_comments WHERE file_id = ?").bind(file_id).execute(&self.pool).await?;
        Ok(())
    }
}
