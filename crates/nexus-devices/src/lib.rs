//! Device identity and trust between HearthDrive nodes.
//!
//! * [`NodeIdentity`]: each node's Ed25519 key, created on first run and
//!   stored encrypted with Windows DPAPI (current user).
//! * [`signing`]: canonical request signatures that peers verify on every call,
//!   plus a replay guard.
//! * [`invite`]: the pairing link and QR payload. It carries a public key and a
//!   one-time code, never a private key.
//! * [`release`]: signed update packages and version numbers.

pub mod dpapi;
pub mod identity;
pub mod invite;
pub mod release;
pub mod signing;

pub use identity::{NodeIdentity, fingerprint, verify_message};
pub use invite::PairingInvite;
pub use signing::{ReplayGuard, SignedRequest};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

pub fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim()).ok()
}

/// A random, URL-safe secret of `bytes` bytes of entropy.
pub fn random_code(bytes: usize) -> String {
    use rand_core::{OsRng, RngCore};
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    b64_encode(&buf)
}
