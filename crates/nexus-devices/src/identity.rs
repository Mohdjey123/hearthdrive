use crate::{b64_decode, b64_encode, dpapi};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use nexus_common::{NexusError, Result};
use std::path::Path;

pub const IDENTITY_FILE: &str = "identity.key";

/// This node's long-term Ed25519 key pair.
pub struct NodeIdentity {
    key: SigningKey,
    public_key: String,
    fingerprint: String,
}

impl NodeIdentity {
    /// Loads the key from `<data_dir>/identity.key`, creating it on first run.
    pub fn load_or_create(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(IDENTITY_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::decode(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate();
                let protected = dpapi::protect(&identity.key.to_bytes())?;
                let text = format!("{}:{}\n", dpapi::SCHEME, b64_encode(&protected));
                let tmp = path.with_extension("key.tmp");
                let _ = std::fs::remove_file(&tmp);
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                // Without DPAPI (phones, development) the key is protected by
                // the file's permissions: its owner only.
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
                std::io::Write::write_all(&mut options.open(&tmp)?, text.as_bytes())?;
                std::fs::rename(&tmp, &path)?;
                tracing::info!(fingerprint = %identity.fingerprint, "created device identity");
                Ok(identity)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn decode(text: &str) -> Result<Self> {
        let (scheme, body) = text
            .trim()
            .split_once(':')
            .ok_or_else(|| NexusError::internal("identity.key is malformed"))?;
        let sealed = b64_decode(body).ok_or_else(|| NexusError::internal("identity.key is malformed"))?;
        let secret = match scheme {
            "dpapi" => dpapi::unprotect(&sealed).map_err(|e| {
                NexusError::internal(format!(
                    "identity.key could not be decrypted ({e}); it belongs to another Windows user or computer"
                ))
            })?,
            "plain" => sealed,
            other => return Err(NexusError::internal(format!("unknown identity.key scheme {other}"))),
        };
        let bytes: [u8; 32] = secret
            .as_slice()
            .try_into()
            .map_err(|_| NexusError::internal("identity.key has the wrong length"))?;
        Ok(Self::from_key(SigningKey::from_bytes(&bytes)))
    }

    /// A fresh, unsaved identity (tests and first run).
    pub fn generate() -> Self {
        use rand_core::{OsRng, RngCore};
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        Self::from_key(SigningKey::from_bytes(&seed))
    }

    fn from_key(key: SigningKey) -> Self {
        let public = key.verifying_key().to_bytes();
        Self { public_key: b64_encode(&public), fingerprint: fingerprint_bytes(&public), key }
    }

    /// Base64url (no padding) of the 32-byte public key.
    pub fn public_key(&self) -> &str {
        &self.public_key
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The raw secret key, for the peer-to-peer transport: the network
    /// endpoint uses the same key, so a device's public key is its address.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.key.to_bytes()
    }

    /// Base64url signature over `message`.
    pub fn sign(&self, message: &[u8]) -> String {
        b64_encode(&self.key.sign(message).to_bytes())
    }
}

/// Verifies a base64url signature by a base64url public key.
pub fn verify_message(public_key: &str, message: &[u8], signature: &str) -> bool {
    let Some(key) = parse_public_key(public_key) else { return false };
    let Some(sig) = b64_decode(signature).and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok()) else {
        return false;
    };
    key.verify_strict(message, &Signature::from_bytes(&sig)).is_ok()
}

pub fn parse_public_key(public_key: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = b64_decode(public_key)?.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

/// Human-comparable fingerprint of a base64url public key, such as
/// `7F3A-09C1-…` (128 bits of BLAKE3). Returns `None` for an invalid key.
pub fn fingerprint(public_key: &str) -> Option<String> {
    parse_public_key(public_key).map(|k| fingerprint_bytes(&k.to_bytes()))
}

fn fingerprint_bytes(public: &[u8]) -> String {
    let hash = blake3::hash(public);
    hash.as_bytes()[..16]
        .chunks(2)
        .map(|pair| format!("{:02X}{:02X}", pair[0], pair[1]))
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_reloads_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let a = NodeIdentity::load_or_create(dir.path()).unwrap();
        let b = NodeIdentity::load_or_create(dir.path()).unwrap();
        assert_eq!(a.public_key(), b.public_key());
        let text = std::fs::read_to_string(dir.path().join(IDENTITY_FILE)).unwrap();
        assert!(text.starts_with(dpapi::SCHEME));
        // The private key never appears in the clear on Windows.
        if cfg!(windows) {
            assert!(!text.contains(&b64_encode(&a.key.to_bytes())));
        }
    }

    #[test]
    fn signatures_verify_only_for_the_signed_message() {
        let id = NodeIdentity::generate();
        let sig = id.sign(b"hello");
        assert!(verify_message(id.public_key(), b"hello", &sig));
        assert!(!verify_message(id.public_key(), b"hellO", &sig));
        assert!(!verify_message(NodeIdentity::generate().public_key(), b"hello", &sig));
        assert!(!verify_message("not-a-key", b"hello", &sig));
        assert_eq!(fingerprint(id.public_key()).as_deref(), Some(id.fingerprint()));
        assert_eq!(id.fingerprint().len(), 39);
    }
}
