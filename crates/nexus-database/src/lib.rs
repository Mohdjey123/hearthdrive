//! SQLite access for a single NEXUS node.
//!
//! Every node owns its own database file. The database is never shared over
//! the network; nodes exchange metadata events instead.

mod activity;
mod api_tokens;
mod backup;
mod devices;
mod pairing;
mod files;
mod groups;
mod health;
mod docs;
mod media;
mod roots;
mod rules;
mod security;
mod sessions;
mod settings;
mod shares;
mod sharing;
mod snapshots;
mod sync;
mod trash;
mod upload_links;
mod users;

pub use api_tokens::ApiToken;
pub use backup::{Backup, NewBackup};
pub use devices::NewPeer;
pub use pairing::NewPairingRequest;
pub use files::{RootStats, SearchFilter, SortField, UnhashedFile};
pub use groups::Group;
pub use health::{DamagedFile, HashedFile, RootCheck};
pub use docs::{DocState, FileTag, TagCount};
pub use rules::{Rule, RuleScope};
pub use media::{Album, AlbumMember, LibraryFilter, MediaInfo, MediaItem, MonthCount, MusicAlbum, PhotoFilter, PhotoHash, Place, Playback};
pub use security::{Passkey, SessionInfo, TotpState, UserSecurity};
pub use shares::{NewShare, Share};
pub use sharing::{Comment, FolderAccess, FolderGrant, FolderMember, SharedFolder};
pub use upload_links::{NewUploadLink, UploadLink};
pub use snapshots::{NewSnapshotSet, Schedule, Snapshot, SnapshotFile, SnapshotSet};
pub use sync::{FileVersion, Fingerprints, HeldFile, NewSyncPair, StateChange, SyncPair, SyncState, UploadSession};

use nexus_common::Result;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// A storage root's totals and how long they stay fresh.
type StatsCache = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, (crate::files::RootStats, std::time::Instant)>>>;

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    /// Totals per storage root. Counting millions of rows takes seconds, so
    /// results are reused for a while (longer the slower the count was).
    stats_cache: StatsCache,
}

impl Database {
    /// Opens (creating if needed) the node database and applies migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(15));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(options)
            .await?;
        Self::init(pool).await
    }

    /// An isolated in-memory database, used by tests.
    pub async fn open_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        Self::init(pool).await
    }

    async fn init(pool: SqlitePool) -> Result<Self> {
        MIGRATOR.run(&pool).await?;
        Ok(Self { pool, stats_cache: Default::default() })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Moves the write-ahead log back into the database file and shrinks it.
    /// Without this the log can grow without bound while readers are busy.
    pub async fn checkpoint(&self) -> Result<()> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&self.pool).await?;
        Ok(())
    }
}

/// Escapes `%`, `_` and `\` for use in a `LIKE ... ESCAPE '\'` pattern.
pub(crate) fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
