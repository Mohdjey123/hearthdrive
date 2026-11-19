//! Pairing invitations, shown as a QR code or a copyable link.
//!
//! An invitation tells the joining device how to reach the host and which
//! public key to expect, and carries a single-use code with 128 bits of
//! entropy that expires after a few minutes. It never contains a private key,
//! and the host still has to approve the request.

use crate::{b64_decode, b64_encode};
use nexus_common::{NexusError, Result};
use serde::{Deserialize, Serialize};

pub const SCHEME: &str = "nexus-pair:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInvite {
    pub v: u8,
    pub device_id: String,
    pub name: String,
    pub public_key: String,
    /// Ways to reach the host, best first: `relay:<url>` or `ip:port`.
    pub addrs: Vec<String>,
    pub code: String,
    pub expires_at: i64,
}

impl PairingInvite {
    pub fn to_link(&self) -> String {
        let json = serde_json::to_vec(self).expect("invite serializes");
        format!("{SCHEME}{}", b64_encode(&json))
    }

    /// Parses a link, tolerating surrounding whitespace and a missing scheme.
    pub fn parse(link: &str) -> Result<Self> {
        let body = link.trim();
        let body = body.strip_prefix(SCHEME).unwrap_or(body);
        let bytes = b64_decode(body).ok_or_else(|| NexusError::invalid("This is not a Hearthdrive pairing link"))?;
        let invite: Self =
            serde_json::from_slice(&bytes).map_err(|_| NexusError::invalid("This is not a Hearthdrive pairing link"))?;
        if invite.v != 1 {
            return Err(NexusError::invalid("This pairing link needs a newer version of Hearthdrive"));
        }
        if crate::identity::parse_public_key(&invite.public_key).is_none() {
            return Err(NexusError::invalid("This pairing link is incomplete"));
        }
        Ok(invite)
    }
}

/// What the joining device signs to prove it holds the key it presents.
pub fn request_proof_message(host_device: &str, code: &str, device_id: &str, public_key: &str) -> Vec<u8> {
    format!("NEXUS-PAIR-REQUEST-V1\n{host_device}\n{code}\n{device_id}\n{public_key}").into_bytes()
}

/// What the host signs when it accepts a request for review, proving to the
/// joiner that it reached the device named in the invitation.
pub fn ack_message(request_id: &str, device_id: &str, public_key: &str, code: &str) -> Vec<u8> {
    format!("NEXUS-PAIR-ACK-V1\n{request_id}\n{device_id}\n{public_key}\n{code}").into_bytes()
}

/// What the host signs when it reports the outcome of a request.
pub fn status_message(request_id: &str, status: &str) -> Vec<u8> {
    format!("NEXUS-PAIR-STATUS-V1\n{request_id}\n{status}").into_bytes()
}

/// What a peer signs in its `hello` reply, bound to the caller's nonce.
pub fn hello_message(nonce: &str, device_id: &str) -> Vec<u8> {
    format!("NEXUS-HELLO-V1\n{nonce}\n{device_id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeIdentity;

    #[test]
    fn round_trips_through_a_link() {
        let id = NodeIdentity::generate();
        let invite = PairingInvite {
            v: 1,
            device_id: "dev-1".into(),
            name: "Studio PC".into(),
            public_key: id.public_key().into(),
            addrs: vec!["100.64.0.1:47100".into()],
            code: crate::random_code(16),
            expires_at: 123,
        };
        let link = invite.to_link();
        assert!(link.starts_with(SCHEME));
        assert_eq!(PairingInvite::parse(&format!("  {link}\n")).unwrap(), invite);
        assert!(PairingInvite::parse("nexus-pair:garbage").is_err());
        assert!(PairingInvite::parse("https://example.com").is_err());
    }
}
