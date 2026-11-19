pub type Result<T, E = NexusError> = std::result::Result<T, E>;

/// The error type shared by every Hearthdrive crate. The API layer maps each
/// variant onto an HTTP status and a stable machine-readable code.
#[derive(Debug, thiserror::Error)]
pub enum NexusError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    InvalidInput(String),
    #[error("{0}")]
    PathRejected(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    PermissionDenied(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    InsufficientStorage(String),
    #[error("too many attempts; try again in a few minutes")]
    RateLimited,
    /// A paired device could not be reached or answered badly.
    #[error("{0}")]
    PeerUnreachable(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("internal error: {0}")]
    Internal(String),
}

impl NexusError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::InvalidInput(_) => "invalid_input",
            Self::PathRejected(_) => "path_rejected",
            Self::Conflict(_) => "conflict",
            Self::PermissionDenied(_) => "permission_denied",
            Self::Unauthorized => "unauthorized",
            Self::Unavailable(_) => "unavailable",
            Self::InsufficientStorage(_) => "insufficient_storage",
            Self::RateLimited => "rate_limited",
            Self::PeerUnreachable(_) => "peer_unreachable",
            Self::Database(_) => "database_error",
            Self::Io(e) => match e.kind() {
                std::io::ErrorKind::NotFound => "not_found",
                std::io::ErrorKind::PermissionDenied => "permission_denied",
                std::io::ErrorKind::AlreadyExists => "conflict",
                _ => "io_error",
            },
            Self::Internal(_) => "internal_error",
        }
    }

    pub fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidInput(msg.into())
    }

    pub fn internal(msg: impl ToString) -> Self {
        Self::Internal(msg.to_string())
    }
}

impl From<sqlx::Error> for NexusError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound("record".into()),
            sqlx::Error::Database(ref db) if db.is_unique_violation() => {
                Self::Conflict("an item with the same identity already exists".into())
            }
            other => Self::Database(other.to_string()),
        }
    }
}

impl From<sqlx::migrate::MigrateError> for NexusError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        Self::Database(format!("migration failed: {e}"))
    }
}
