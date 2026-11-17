//! Signed requests between paired nodes.
//!
//! The caller signs a canonical description of the request with its device
//! key. The receiver checks the signature against the public key it stored at
//! pairing, the clock window, the nonce (single use) and that the request was
//! addressed to it. Bodies travel over the WireGuard tunnel, which provides
//! integrity, and are streamed rather than signed.

use crate::identity::{NodeIdentity, verify_message};
use std::collections::HashMap;
use std::sync::Mutex;

pub const HDR_DEVICE: &str = "x-nexus-device";
pub const HDR_TIMESTAMP: &str = "x-nexus-timestamp";
pub const HDR_NONCE: &str = "x-nexus-nonce";
pub const HDR_SIGNATURE: &str = "x-nexus-signature";
pub const HDR_ACTING_USER: &str = "x-nexus-acting-user";
pub const HDR_ACTING_ROLE: &str = "x-nexus-acting-role";
/// Storage locations the person is limited to, comma-separated (absent: none
/// of their own limits apply).
pub const HDR_ACTING_ROOTS: &str = "x-nexus-acting-roots";

/// Requests older or newer than this are refused.
pub const MAX_SKEW_MS: i64 = 120_000;

/// Everything a signature covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRequest {
    pub method: String,
    /// Path and query exactly as sent on the wire, such as `/api/v1/files?root_id=…`.
    pub path_and_query: String,
    pub timestamp_ms: i64,
    pub nonce: String,
    /// The device the request is meant for; stops a request captured for one
    /// peer being replayed against another.
    pub target_device: String,
    /// The person on the calling node, and their role there.
    pub acting_user: String,
    pub acting_role: String,
    /// The storage locations that person is limited to, comma-separated;
    /// `None` when they are not limited.
    pub acting_roots: Option<String>,
}

impl SignedRequest {
    pub fn canonical(&self) -> Vec<u8> {
        let mut text = format!(
            "NEXUS-SIG-V1\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.method.to_ascii_uppercase(),
            self.path_and_query,
            self.timestamp_ms,
            self.nonce,
            self.target_device,
            self.acting_role,
            self.acting_user,
        );
        // Only limited people add a line, so requests from people without
        // limits read exactly as before.
        if let Some(roots) = &self.acting_roots {
            text.push_str("
roots:");
            text.push_str(roots);
        }
        text.into_bytes()
    }

    /// The acting person's storage limits as a list.
    pub fn acting_root_list(&self) -> Option<Vec<String>> {
        self.acting_roots
            .as_deref()
            .map(|r| r.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
    }

    pub fn sign(&self, identity: &NodeIdentity) -> String {
        identity.sign(&self.canonical())
    }

    pub fn verify(&self, public_key: &str, signature: &str) -> bool {
        verify_message(public_key, &self.canonical(), signature)
    }
}

/// Remembers recent nonces so a captured request cannot be replayed.
#[derive(Default)]
pub struct ReplayGuard {
    seen: Mutex<HashMap<String, i64>>,
}

impl ReplayGuard {
    /// Accepts a nonce once, and only for a timestamp within the window.
    pub fn check(&self, nonce: &str, timestamp_ms: i64, now_ms: i64) -> bool {
        if nonce.len() < 16 || nonce.len() > 64 || (now_ms - timestamp_ms).abs() > MAX_SKEW_MS {
            return false;
        }
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.len() > 4096 {
            seen.retain(|_, at| now_ms - *at <= 2 * MAX_SKEW_MS);
        }
        if seen.contains_key(nonce) {
            return false;
        }
        seen.insert(nonce.to_string(), now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> SignedRequest {
        SignedRequest {
            method: "GET".into(),
            path_and_query: "/api/v1/files?root_id=r1".into(),
            timestamp_ms: 1_000_000,
            nonce: "0123456789abcdef0123".into(),
            target_device: "node-a".into(),
            acting_user: "alex".into(),
            acting_role: "owner".into(),
            acting_roots: None,
        }
    }

    #[test]
    fn any_changed_field_breaks_the_signature() {
        let id = NodeIdentity::generate();
        let req = request();
        let sig = req.sign(&id);
        assert!(req.verify(id.public_key(), &sig));
        let variants = [
            SignedRequest { method: "DELETE".into(), ..request() },
            SignedRequest { path_and_query: "/api/v1/files?root_id=r2".into(), ..request() },
            SignedRequest { target_device: "node-b".into(), ..request() },
            SignedRequest { acting_role: "viewer".into(), ..request() },
            SignedRequest { acting_roots: Some("r1".into()), ..request() },
            SignedRequest { nonce: "fedcba9876543210fedc".into(), ..request() },
        ];
        for v in variants {
            assert!(!v.verify(id.public_key(), &sig), "{v:?}");
        }
    }

    #[test]
    fn limits_are_signed() {
        let id = NodeIdentity::generate();
        let limited = SignedRequest { acting_roots: Some("r1,r2".into()), ..request() };
        let sig = limited.sign(&id);
        assert!(limited.verify(id.public_key(), &sig));
        // Dropping or widening the limits breaks the signature.
        assert!(!request().verify(id.public_key(), &sig));
        assert!(!SignedRequest { acting_roots: Some("r1,r2,r3".into()), ..request() }.verify(id.public_key(), &sig));
        assert_eq!(limited.acting_root_list(), Some(vec!["r1".to_string(), "r2".to_string()]));
        assert_eq!(SignedRequest { acting_roots: Some(String::new()), ..request() }.acting_root_list(), Some(vec![]));
    }

    #[test]
    fn replay_guard_rejects_reuse_and_stale_requests() {
        let guard = ReplayGuard::default();
        let now = 10_000_000;
        assert!(guard.check("nonce-aaaaaaaaaaaaaa", now, now));
        assert!(!guard.check("nonce-aaaaaaaaaaaaaa", now, now));
        assert!(!guard.check("nonce-bbbbbbbbbbbbbb", now - MAX_SKEW_MS - 1, now));
        assert!(!guard.check("nonce-cccccccccccccc", now + MAX_SKEW_MS + 1, now));
        assert!(!guard.check("short", now, now));
    }
}
