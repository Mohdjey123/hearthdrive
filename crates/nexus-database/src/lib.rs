//! SQLite access for a single NEXUS node.
//!
//! Every node owns its own database file. The database is never shared over
//! the network; nodes exchange metadata events instead.

mod activity;
mod devices;
mod pairing;
mod files;
mod roots;
mod sessions;
mod settings;
mod trash;
mod users;

pub use devices::NewPeer;
pub use pairing::NewPairingRequest;
pub use files::{RootStats, SortField, UnhashedFile};

use nexus_common::Result;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
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
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
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
