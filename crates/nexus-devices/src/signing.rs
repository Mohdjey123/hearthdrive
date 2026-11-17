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
}

impl SignedRequest {
    pub fn canonical(&self) -> Vec<u8> {
        format!(
            "NEXUS-SIG-V1\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.method.to_ascii_uppercase(),
            self.path_and_query,
            self.timestamp_ms,
            self.nonce,
            self.target_device,
            self.acting_role,
            self.acting_user,
        )
        .into_bytes()
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
            SignedRequest { nonce: "fedcba9876543210fedc".into(), ..request() },
        ];
        for v in variants {
            assert!(!v.verify(id.public_key(), &sig), "{v:?}");
        }
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
