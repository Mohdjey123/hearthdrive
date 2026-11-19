//! What NEXUS read from inside photos, videos and music, and photo albums.

use crate::Database;
use crate::files::FILE_COLUMNS;
use nexus_common::types::FileEntry;
use nexus_common::{NexusError, Result};
use serde::Serialize;
use sqlx::{AssertSqlSafe, QueryBuilder, Sqlite};

/// Details read from inside a file. Empty fields were not found.
#[derive(Debug, Clone, Default, Serialize, sqlx::FromRow)]
pub struct MediaInfo {
    pub file_id: String,
    pub root_id: String,
    /// The file's modification time when it was read.
    pub modified_at: Option<i64>,
    /// `photo` | `video` | `music` | `other`
    pub kind: String,
    pub taken_at: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub duration_ms: Option<i64>,
    pub phash: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track: Option<i64>,
    pub disc: Option<i64>,
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub show_name: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    /// `film` | `episode` | `music` | `home_video`
    pub media_type: Option<String>,
    pub faces: Option<i64>,
    pub labels: Option<String>,
    pub read_at: i64,
    pub error: Option<String>,
}

/// A file with what was read from it.
#[derive(Debug, Clone, Serialize)]
pub struct MediaItem {
    #[serde(flatten)]
    pub file: FileEntry,
    pub media: MediaInfo,
}

/// Which photos to list.
#[derive(Debug, Clone, Default)]
pub struct PhotoFilter {
    /// Only these storage locations (`None`: all).
    pub roots: Option<Vec<String>>,
    pub kind: Option<String>,
    /// Taken in `[from, to)` (ms).
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub with_place: bool,
    pub camera: Option<String>,
    pub album: Option<String>,
    /// Taken on this month and day (`MM-DD`) in an earlier year.
    pub on_day: Option<(String, i64)>,
    /// Only photos with people in them.
    pub with_people: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub created_by: String,
    pub created_at: i64,
    pub cover_id: Option<String>,
    pub items: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AlbumMember {
    pub member: String,
    pub can_add: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MonthCount {
    /// `YYYY-MM`
    pub month: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Place {
    pub file_id: String,
    pub root_id: String,
    pub latitude: f64,
    pub longitude: f64,
    pub taken_at: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PhotoHash {
    pub file_id: String,
    pub root_id: String,
    pub phash: String,
}

fn prefixed(table: &str) -> String {
    FILE_COLUMNS.split(',').map(|c| format!("{table}.{}", c.trim())).collect::<Vec<_>>().join(", ")
}

fn push_list(q: &mut QueryBuilder<Sqlite>, column: &str, values: &[String]) {
    q.push(format!(" AND {column} IN ("));
    let mut sep = q.separated(", ");
    for v in values {
        sep.push_bind(v.clone());
    }
    sep.push_unseparated(")");
}

impl Database {
    /// Files of these extensions not read yet, or changed since.
    pub async fn media_todo(&self, extensions: &[&str], limit: i64) -> Result<Vec<FileEntry>> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT {} FROM files f LEFT JOIN media_info m ON m.file_id = f.id \
             WHERE f.is_dir = 0 AND f.availability = 'online' AND (m.file_id IS NULL OR m.modified_at IS NOT f.modified_at)",
            prefixed("f")
        ));
        let exts: Vec<String> = extensions.iter().map(|e| e.to_string()).collect();
        push_list(&mut q, "lower(f.extension)", &exts);
        q.push(" LIMIT ").push_bind(limit);
        Ok(q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?)
    }

    pub async fn save_media(&self, m: &MediaInfo) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO media_info (file_id, root_id, modified_at, kind, taken_at, width, height, camera, lens, \
             latitude, longitude, duration_ms, phash, title, artist, album, album_artist, track, disc, year, genre, \
             show_name, season, episode, media_type, faces, labels, read_at, error) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&m.file_id)
        .bind(&m.root_id)
        .bind(m.modified_at)
        .bind(&m.kind)
        .bind(m.taken_at)
        .bind(m.width)
        .bind(m.height)
        .bind(&m.camera)
        .bind(&m.lens)
        .bind(m.latitude)
        .bind(m.longitude)
        .bind(m.duration_ms)
        .bind(&m.phash)
        .bind(&m.title)
        .bind(&m.artist)
        .bind(&m.album)
        .bind(&m.album_artist)
        .bind(m.track)
        .bind(m.disc)
        .bind(m.year)
        .bind(&m.genre)
        .bind(&m.show_name)
        .bind(m.season)
        .bind(m.episode)
        .bind(&m.media_type)
        .bind(m.faces)
        .bind(&m.labels)
        .bind(m.read_at)
        .bind(&m.error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_media(&self, file_id: &str) -> Result<Option<MediaInfo>> {
        Ok(sqlx::query_as::<_, MediaInfo>("SELECT * FROM media_info WHERE file_id = ?")
            .bind(file_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    async fn with_media(&self, files: Vec<FileEntry>) -> Result<Vec<MediaItem>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new("SELECT * FROM media_info WHERE 1 = 1");
        let ids: Vec<String> = files.iter().map(|f| f.id.clone()).collect();
        push_list(&mut q, "file_id", &ids);
        let media: Vec<MediaInfo> = q.build_query_as().fetch_all(&self.pool).await?;
        let mut by_id: std::collections::HashMap<String, MediaInfo> = media.into_iter().map(|m| (m.file_id.clone(), m)).collect();
        Ok(files
            .into_iter()
            .filter_map(|file| by_id.remove(&file.id).map(|media| MediaItem { file, media }))
            .collect())
    }

    fn photo_query(&self, select: &str, f: &PhotoFilter) -> QueryBuilder<Sqlite> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT {select} FROM media_info m JOIN files f ON f.id = m.file_id WHERE m.kind = "
        ));
        q.push_bind(f.kind.clone().unwrap_or_else(|| "photo".into()));
        if let Some(roots) = &f.roots {
            if roots.is_empty() {
                q.push(" AND 0");
            } else {
                push_list(&mut q, "m.root_id", roots);
            }
        }
        if let Some(from) = f.from {
            q.push(" AND COALESCE(m.taken_at, f.modified_at) >= ").push_bind(from);
        }
        if let Some(to) = f.to {
            q.push(" AND COALESCE(m.taken_at, f.modified_at) < ").push_bind(to);
        }
        if f.with_place {
            q.push(" AND m.latitude IS NOT NULL");
        }
        if f.with_people {
            q.push(" AND m.faces > 0");
        }
        if let Some(camera) = &f.camera {
            q.push(" AND m.camera = ").push_bind(camera.clone());
        }
        if let Some(album) = &f.album {
            q.push(" AND m.file_id IN (SELECT file_id FROM album_items WHERE album_id = ").push_bind(album.clone()).push(")");
        }
        if let Some((day, before)) = &f.on_day {
            q.push(" AND m.taken_at IS NOT NULL AND strftime('%m-%d', m.taken_at / 1000, 'unixepoch') = ")
                .push_bind(day.clone())
                .push(" AND m.taken_at < ")
                .push_bind(*before);
        }
        q
    }

    /// Photos (or videos), newest first.
    pub async fn photos(&self, f: &PhotoFilter, offset: i64, limit: i64) -> Result<Vec<MediaItem>> {
        let mut q = self.photo_query(&prefixed("f"), f);
        q.push(" ORDER BY COALESCE(m.taken_at, f.modified_at) DESC, f.id LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let files = q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?;
        self.with_media(files).await
    }

    /// How many photos each month has, newest first.
    pub async fn photo_months(&self, f: &PhotoFilter) -> Result<Vec<MonthCount>> {
        let mut q = self.photo_query(
            "strftime('%Y-%m', COALESCE(m.taken_at, f.modified_at) / 1000, 'unixepoch') AS month, COUNT(*) AS count",
            f,
        );
        q.push(" GROUP BY month ORDER BY month DESC");
        Ok(q.build_query_as::<MonthCount>().fetch_all(&self.pool).await?)
    }

    /// Where photos were taken.
    pub async fn photo_places(&self, f: &PhotoFilter, limit: i64) -> Result<Vec<Place>> {
        let f = PhotoFilter { with_place: true, ..f.clone() };
        let mut q = self.photo_query("m.file_id, m.root_id, m.latitude, m.longitude, m.taken_at", &f);
        q.push(" ORDER BY m.taken_at DESC LIMIT ").push_bind(limit);
        Ok(q.build_query_as::<Place>().fetch_all(&self.pool).await?)
    }

    /// The cameras photos were taken with, most used first.
    pub async fn photo_cameras(&self, f: &PhotoFilter) -> Result<Vec<(String, i64)>> {
        let mut q = self.photo_query("m.camera, COUNT(*)", f);
        q.push(" AND m.camera IS NOT NULL GROUP BY m.camera ORDER BY COUNT(*) DESC LIMIT 50");
        Ok(q.build_query_as::<(String, i64)>().fetch_all(&self.pool).await?)
    }

    /// Photo hashes, to find photos that look alike.
    pub async fn photo_hashes(&self, roots: Option<&[String]>) -> Result<Vec<PhotoHash>> {
        let mut q: QueryBuilder<Sqlite> =
            QueryBuilder::new("SELECT file_id, root_id, phash FROM media_info WHERE kind = 'photo' AND phash IS NOT NULL");
        if let Some(roots) = roots {
            if roots.is_empty() {
                return Ok(Vec::new());
            }
            push_list(&mut q, "root_id", roots);
        }
        Ok(q.build_query_as::<PhotoHash>().fetch_all(&self.pool).await?)
    }

    pub async fn media_items(&self, ids: &[String]) -> Result<Vec<MediaItem>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!("SELECT {FILE_COLUMNS} FROM files WHERE 1 = 1"));
        push_list(&mut q, "id", ids);
        let files = q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?;
        self.with_media(files).await
    }

    // -- Albums -------------------------------------------------------------

    pub async fn create_album(&self, id: &str, name: &str, by: &str, at: i64) -> Result<()> {
        sqlx::query("INSERT INTO albums (id, name, created_by, created_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(name)
            .bind(by)
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    const ALBUM_SELECT: &str = "SELECT a.id, a.name, a.created_by, a.created_at, \
        COALESCE(a.cover_id, (SELECT file_id FROM album_items i WHERE i.album_id = a.id ORDER BY added_at DESC LIMIT 1)) AS cover_id, \
        (SELECT COUNT(*) FROM album_items i WHERE i.album_id = a.id) AS items FROM albums a";

    /// Albums `who` made or that are shared with `who` (`None`: all).
    pub async fn list_albums(&self, who: Option<&str>) -> Result<Vec<Album>> {
        let rows = match who {
            Some(who) => {
                sqlx::query_as::<_, Album>(AssertSqlSafe(format!(
                    "{} WHERE a.created_by = ? OR a.id IN (SELECT album_id FROM album_members WHERE member = ?) ORDER BY a.created_at DESC",
                    Self::ALBUM_SELECT
                )))
                .bind(who)
                .bind(who)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, Album>(AssertSqlSafe(format!("{} ORDER BY a.created_at DESC", Self::ALBUM_SELECT)))
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows)
    }

    pub async fn get_album(&self, id: &str) -> Result<Album> {
        sqlx::query_as::<_, Album>(AssertSqlSafe(format!("{} WHERE a.id = ?", Self::ALBUM_SELECT)))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| NexusError::NotFound("album".into()))
    }

    pub async fn rename_album(&self, id: &str, name: &str, cover: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE albums SET name = ?, cover_id = COALESCE(?, cover_id) WHERE id = ?")
            .bind(name)
            .bind(cover)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_album(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM albums WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn add_to_album(&self, album: &str, file_id: &str, root_id: &str, by: &str, at: i64) -> Result<bool> {
        let r = sqlx::query(
            "INSERT OR IGNORE INTO album_items (album_id, file_id, root_id, added_by, added_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(album)
        .bind(file_id)
        .bind(root_id)
        .bind(by)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn remove_from_album(&self, album: &str, file_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM album_items WHERE album_id = ? AND file_id = ?")
            .bind(album)
            .bind(file_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn album_members(&self, album: &str) -> Result<Vec<AlbumMember>> {
        Ok(sqlx::query_as::<_, AlbumMember>("SELECT member, can_add FROM album_members WHERE album_id = ? ORDER BY member")
            .bind(album)
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn set_album_members(&self, album: &str, members: &[(String, bool)]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM album_members WHERE album_id = ?").bind(album).execute(&mut *tx).await?;
        for (member, can_add) in members {
            sqlx::query("INSERT INTO album_members (album_id, member, can_add) VALUES (?, ?, ?)")
                .bind(album)
                .bind(member)
                .bind(can_add)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether a file is in an album shared with `who`.
    pub async fn in_shared_album(&self, file_id: &str, who: &str) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM album_items i JOIN albums a ON a.id = i.album_id \
             WHERE i.file_id = ? AND (a.created_by = ? OR a.id IN (SELECT album_id FROM album_members WHERE member = ?))",
        )
        .bind(file_id)
        .bind(who)
        .bind(who)
        .fetch_one(&self.pool)
        .await?;
        Ok(n > 0)
    }

    // -- Media library ------------------------------------------------------

    /// Music albums: album, album artist (or artist), year, tracks.
    pub async fn music_albums(&self, roots: Option<&[String]>) -> Result<Vec<MusicAlbum>> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT COALESCE(album, '') AS album, COALESCE(album_artist, artist, '') AS artist, MAX(year) AS year,              COUNT(*) AS tracks, SUM(COALESCE(duration_ms, 0)) AS duration_ms, MIN(file_id) AS first_file              FROM media_info WHERE kind = 'music'",
        );
        if let Some(roots) = roots {
            if roots.is_empty() {
                return Ok(Vec::new());
            }
            push_list(&mut q, "root_id", roots);
        }
        q.push(" GROUP BY COALESCE(album, ''), COALESCE(album_artist, artist, '') ORDER BY artist COLLATE NOCASE, year, album COLLATE NOCASE LIMIT 5000");
        Ok(q.build_query_as::<MusicAlbum>().fetch_all(&self.pool).await?)
    }

    /// Songs of an album (`album` and `artist` as [`Database::music_albums`] gives them), or
    /// films, episodes or home videos (`media_type`).
    pub async fn library_items(&self, f: &LibraryFilter, limit: i64) -> Result<Vec<MediaItem>> {
        let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!(
            "SELECT {} FROM media_info m JOIN files f ON f.id = m.file_id WHERE 1 = 1",
            prefixed("f")
        ));
        if let Some(roots) = &f.roots {
            if roots.is_empty() {
                return Ok(Vec::new());
            }
            push_list(&mut q, "m.root_id", roots);
        }
        if let Some(t) = &f.media_type {
            q.push(" AND m.media_type = ").push_bind(t.clone());
        }
        if let Some(album) = &f.album {
            q.push(" AND m.kind = 'music' AND COALESCE(m.album, '') = ").push_bind(album.clone());
        }
        if let Some(artist) = &f.artist {
            q.push(" AND COALESCE(m.album_artist, m.artist, '') = ").push_bind(artist.clone());
        }
        if let Some(show) = &f.show {
            q.push(" AND m.show_name = ").push_bind(show.clone());
        }
        q.push(match f.media_type.as_deref() {
            Some("episode") => " ORDER BY m.show_name COLLATE NOCASE, m.season, m.episode, f.filename",
            Some("film") => " ORDER BY COALESCE(m.title, f.filename) COLLATE NOCASE",
            Some("home_video") => " ORDER BY f.modified_at DESC",
            _ => " ORDER BY m.disc, m.track, f.filename",
        });
        q.push(" LIMIT ").push_bind(limit);
        let files = q.build_query_as::<FileEntry>().fetch_all(&self.pool).await?;
        let mut items = self.with_media(files).await?;
        // `with_media` keeps the order of the files.
        items.truncate(limit as usize);
        Ok(items)
    }

    /// Where a viewer stopped in a file.
    pub async fn save_playback(&self, viewer: &str, file_id: &str, position_ms: i64, duration_ms: Option<i64>, device: &str, at: i64) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO playback (viewer, file_id, position_ms, duration_ms, device_name, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(viewer)
        .bind(file_id)
        .bind(position_ms)
        .bind(duration_ms)
        .bind(device)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_playback(&self, viewer: &str, file_id: &str) -> Result<Option<Playback>> {
        Ok(sqlx::query_as::<_, Playback>(
            "SELECT file_id, position_ms, duration_ms, device_name, updated_at FROM playback WHERE viewer = ? AND file_id = ?",
        )
        .bind(viewer)
        .bind(file_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// What a viewer started and did not finish, latest first.
    pub async fn continue_watching(&self, viewer: &str, limit: i64) -> Result<Vec<Playback>> {
        Ok(sqlx::query_as::<_, Playback>(
            "SELECT file_id, position_ms, duration_ms, device_name, updated_at FROM playback              WHERE viewer = ? AND position_ms > 10000 AND (duration_ms IS NULL OR position_ms < duration_ms * 0.95)              ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(viewer)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MusicAlbum {
    pub album: String,
    pub artist: String,
    pub year: Option<i64>,
    pub tracks: i64,
    pub duration_ms: i64,
    pub first_file: String,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryFilter {
    pub roots: Option<Vec<String>>,
    pub media_type: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub show: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Playback {
    pub file_id: String,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub device_name: Option<String>,
    pub updated_at: i64,
}
