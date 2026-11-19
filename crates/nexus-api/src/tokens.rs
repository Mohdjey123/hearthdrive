//! Tokens for programs: scripts calling the API, WebDAV clients and S3
//! tools. A token acts as the person who made it (read-only if they chose),
//! and never manages people, tokens, pairing or sign-in.
//!
//! The token is `nxt_{id}_{secret}`. For S3 tools the id is the access key
//! and the secret the secret key, so the secret is kept (sealed with this
//! device's key) rather than only a hash of it.

use crate::auth::AuthUser;
use crate::routes::sealed;
use crate::state::AppState;
use base64::Engine;
use nexus_common::time::now_ms;
use nexus_common::types::Role;
use nexus_common::{NexusError, Result};
use nexus_database::ApiToken;

pub const PREFIX: &str = "nxt_";
const ID_LEN: usize = 20;
const SECRET_LEN: usize = 40;
pub(crate) const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
pub(crate) const SECRET_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz0123456789";

pub(crate) fn random_string(len: usize, alphabet: &[u8]) -> String {
    let mut bytes = vec![0u8; len];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).expect("random");
    // 256 is not a multiple of the alphabet size; the bias is negligible for
    // keys this long.
    bytes.iter().map(|b| alphabet[*b as usize % alphabet.len()] as char).collect()
}

fn key(state: &AppState) -> [u8; 32] {
    blake3::derive_key("NEXUS 2026-11 tokens for programs", &state.identity.secret_bytes())
}

pub fn seal(state: &AppState, secret: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(sealed::seal_bytes(&key(state), secret.as_bytes()))
}

pub fn open(state: &AppState, sealed_secret: &str) -> Result<String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(sealed_secret)
        .map_err(|_| NexusError::internal("a token secret is damaged"))?;
    let plain = sealed::open_bytes(&key(state), &raw).map_err(|_| NexusError::internal("a token secret is damaged"))?;
    String::from_utf8(plain).map_err(|_| NexusError::internal("a token secret is damaged"))
}

/// A new token: what is stored, and the full token to show once.
pub fn generate(state: &AppState, user_id: &str, name: &str, read_only: bool, expires_at: Option<i64>) -> (ApiToken, String) {
    let id = format!("NX{}", random_string(ID_LEN - 2, ALPHABET));
    let secret = random_string(SECRET_LEN, SECRET_ALPHABET);
    let token = ApiToken {
        id: id.clone(),
        user_id: user_id.to_string(),
        name: name.to_string(),
        secret: seal(state, &secret),
        read_only,
        created_at: now_ms(),
        last_used: None,
        expires_at,
    };
    (token, format!("{PREFIX}{id}_{secret}"))
}

/// The id and secret of a full token.
pub fn split(token: &str) -> Option<(&str, &str)> {
    let rest = token.strip_prefix(PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    (id.len() == ID_LEN && secret.len() == SECRET_LEN).then_some((id, secret))
}

pub(crate) fn same(a: &str, b: &str) -> bool {
    blake3::hash(a.as_bytes()) == blake3::hash(b.as_bytes())
}

/// The person a token acts as, given its id and (checked) secret.
pub async fn user_for(state: &AppState, id: &str, check_secret: Option<&str>) -> Result<(AuthUser, ApiToken, String)> {
    let token = state.db.get_api_token(id).await.map_err(|_| NexusError::Unauthorized)?;
    let secret = open(state, &token.secret)?;
    if let Some(given) = check_secret
        && !same(given, &secret)
    {
        return Err(NexusError::Unauthorized);
    }
    let now = now_ms();
    if token.expires_at.is_some_and(|e| e <= now) {
        return Err(NexusError::Unauthorized);
    }
    let person = state.db.get_user(&token.user_id).await.map_err(|_| NexusError::Unauthorized)?;
    if state.db.user_locked(&person.id).await? {
        return Err(NexusError::Unauthorized);
    }
    let mut user = AuthUser::person(person).with_shared(state).await?;
    user.via_token = true;
    if token.read_only {
        user.role = user.role.least(Role::Viewer);
    }
    if token.last_used.is_none_or(|t| now - t > 60_000) {
        let _ = state.db.api_token_used(&token.id, now).await;
    }
    Ok((user, token, secret))
}

/// Signs in with a full token (`Authorization: Bearer nxt_…`).
pub async fn authenticate(state: &AppState, token: &str) -> Result<AuthUser> {
    let (id, secret) = split(token).ok_or(NexusError::Unauthorized)?;
    Ok(user_for(state, id, Some(secret)).await?.0)
}

/// Parts of the API a token may not use.
pub fn forbidden_path(path: &str) -> bool {
    let p = path.trim_start_matches("/api/v1");
    ["/users", "/auth/", "/tokens", "/devices/pair", "/pairing", "/setup", "/programs"]
        .iter()
        .any(|x| p.starts_with(x))
        && p != "/auth/me"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_into_id_and_secret() {
        let id = format!("NX{}", random_string(ID_LEN - 2, ALPHABET));
        let secret = random_string(SECRET_LEN, SECRET_ALPHABET);
        let token = format!("{PREFIX}{id}_{secret}");
        assert_eq!(split(&token), Some((id.as_str(), secret.as_str())));
        assert_eq!(split("nxt_short_x"), None);
        assert!(forbidden_path("/api/v1/users"));
        assert!(forbidden_path("/api/v1/auth/2fa/setup"));
        assert!(!forbidden_path("/api/v1/auth/me"));
        assert!(!forbidden_path("/api/v1/files"));
    }
}
