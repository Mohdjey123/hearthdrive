//! Authentication primitives. No custom cryptography: Argon2id for
//! passwords, the OS CSPRNG for tokens and keyed BLAKE3 as a MAC.

pub mod totp;
pub mod webauthn;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use nexus_common::types::Role;
use nexus_common::{NexusError, Result, time::now_ms};

/// Hashes a password with Argon2id (default, OWASP-recommended parameters).
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(NexusError::internal)
}

pub fn verify_password(password: &str, phc: &str) -> bool {
    PasswordHash::new(phc)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// 256-bit random bearer token, hex encoded.
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Tokens are stored only as hashes.
pub fn hash_token(token: &str) -> String {
    blake3::hash(token.as_bytes()).to_hex().to_string()
}

/// Constant-time token comparison against a stored hash.
pub fn token_matches(token: &str, expected: &blake3::Hash) -> bool {
    blake3::hash(token.as_bytes()) == *expected
}

/// Short-lived, scope-bound signed tickets for URLs that cannot carry an
/// `Authorization` header (`<img>`, `<video>`, downloads).
/// Format: `<expiry-unix-seconds>.<keyed-blake3-hex>`.
pub struct TicketSigner {
    key: [u8; 32],
}

impl TicketSigner {
    pub fn new() -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        Self { key }
    }

    fn mac(&self, scope: &str, subject: &str, expires: i64) -> blake3::Hash {
        blake3::keyed_hash(&self.key, format!("{scope}\n{subject}\n{expires}").as_bytes())
    }

    pub fn issue(&self, scope: &str, subject: &str, ttl_secs: i64) -> String {
        let expires = now_ms() / 1000 + ttl_secs;
        format!("{expires}.{}", self.mac(scope, subject, expires).to_hex())
    }

    pub fn verify(&self, ticket: &str, scope: &str, subject: &str) -> bool {
        let Some((exp, mac)) = ticket.split_once('.') else { return false };
        let Ok(expires) = exp.parse::<i64>() else { return false };
        if expires < now_ms() / 1000 {
            return false;
        }
        match blake3::Hash::from_hex(mac) {
            Ok(given) => given == self.mac(scope, subject, expires),
            Err(_) => false,
        }
    }
}

impl Default for TicketSigner {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    ManageStorage,
    ManageUsers,
}

/// Role-based authorisation, enforced on the backend for every request.
pub fn role_allows(role: Role, action: Action) -> bool {
    match role {
        Role::Owner | Role::Administrator => true,
        Role::Editor => matches!(action, Action::Read | Action::Write),
        Role::Contributor => matches!(action, Action::Read | Action::Write),
        Role::Viewer | Role::Restricted => action == Action::Read,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        let hash = hash_password("correct horse battery").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn tokens_are_random_and_matchable() {
        let a = generate_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, generate_token());
        assert!(token_matches(&a, &blake3::hash(a.as_bytes())));
        assert!(!token_matches("nope", &blake3::hash(a.as_bytes())));
    }

    #[test]
    fn tickets_are_bound_to_scope_and_subject() {
        let signer = TicketSigner::new();
        let t = signer.issue("content", "file-1", 60);
        assert!(signer.verify(&t, "content", "file-1"));
        assert!(!signer.verify(&t, "content", "file-2"));
        assert!(!signer.verify(&t, "other", "file-1"));
        assert!(!TicketSigner::new().verify(&t, "content", "file-1"));
        let expired = signer.issue("content", "file-1", -5);
        assert!(!signer.verify(&expired, "content", "file-1"));
        assert!(!signer.verify("garbage", "content", "file-1"));
    }
}
