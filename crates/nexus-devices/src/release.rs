//! Signed update packages.
//!
//! A release describes one installer (its platform, version, size and
//! BLAKE3 hash) and is signed with the project's update key, which never
//! leaves the computer releases are built on. Devices pass releases and
//! packages to each other, but only install, keep or offer one whose
//! signature checks out against a key built into the app, and whose
//! package matches the signed size and hash. A paired device can therefore
//! delay an update, but never change one.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Public keys whose releases devices accept. The newest key comes first;
/// an old key stays listed for a while after it is replaced.
///
/// The second is the spare: its secret half is kept offline and has never
/// signed anything. If the first key is lost or leaks, releases are signed
/// with the spare instead (and the first is taken out of this list), so
/// devices keep updating without anyone reinstalling NEXUS.
pub const TRUSTED_KEYS: &[&str] = &["Jx9BMwt9fve5bFgowIuSFnq2k7bRWMhmsD09tidtT8c", "xXlKnZ-eLFRyQU0IjV9ci3apfDnRLkFonkXyUeWkqpY"];

/// The channel of a release that only devices taking beta versions get.
/// Stable releases have no channel.
pub const BETA: &str = "beta";

/// Platforms releases are made for.
pub const PLATFORMS: &[&str] = &["windows-x86_64", "android-aarch64"];

/// Packages larger than this are never downloaded.
pub const MAX_PACKAGE_SIZE: u64 = 1024 * 1024 * 1024;

/// Separates release signatures from every other signature the same key
/// could make.
const CONTEXT: &[u8] = b"nexus-release-v1\n";

/// One installer for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub platform: String,
    pub version: String,
    /// The installer's file name, for display and downloads.
    pub file: String,
    pub size: u64,
    /// BLAKE3 hash of the installer, in hex.
    pub blake3: String,
    #[serde(default)]
    pub notes: String,
    pub published_at: i64,
    /// Empty for a stable release, [`BETA`] for a beta. Signed with the rest,
    /// so a beta can never pass for a stable release. Releases signed before
    /// channels existed have none, and stay valid.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub channel: String,
}

impl Release {
    pub fn is_beta(&self) -> bool {
        !self.channel.is_empty()
    }
}

/// A release and its signature. The release is kept as the exact JSON text
/// that was signed, so nothing about it can change on the way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRelease {
    pub release: String,
    pub signature: String,
}

impl SignedRelease {
    /// Signs `release` with a key from [`generate_key`].
    pub fn sign(release: &Release, secret: &str) -> Option<Self> {
        let key = signing_key(secret)?;
        let text = serde_json::to_string(release).ok()?;
        let signature = key.sign(&message(&text));
        Some(Self { release: text, signature: crate::b64_encode(&signature.to_bytes()) })
    }

    /// The release, if one of `trusted` signed it and it describes a known
    /// platform and a usable version.
    pub fn verify(&self, trusted: &[impl AsRef<str>]) -> Option<Release> {
        let signature = crate::b64_decode(&self.signature)?;
        let signature = Signature::from_slice(&signature).ok()?;
        let message = message(&self.release);
        let signed = trusted.iter().any(|key| {
            crate::identity::parse_public_key(key.as_ref()).is_some_and(|k| k.verify_strict(&message, &signature).is_ok())
        });
        if !signed {
            return None;
        }
        let release: Release = serde_json::from_str(&self.release).ok()?;
        let valid = PLATFORMS.contains(&release.platform.as_str())
            && parse_version(&release.version).is_some()
            && release.size > 0
            && release.size <= MAX_PACKAGE_SIZE
            && release.blake3.len() == 64
            && release.blake3.bytes().all(|b| b.is_ascii_hexdigit())
            && (release.channel.is_empty() || release.channel == BETA);
        valid.then_some(release)
    }
}

fn message(text: &str) -> Vec<u8> {
    [CONTEXT, text.as_bytes()].concat()
}

fn signing_key(secret: &str) -> Option<SigningKey> {
    let bytes: [u8; 32] = crate::b64_decode(secret)?.try_into().ok()?;
    Some(SigningKey::from_bytes(&bytes))
}

/// A new update key: `(secret, public)`, both base64url. The secret signs
/// releases and must stay with whoever publishes them.
pub fn generate_key() -> (String, String) {
    use rand_core::{OsRng, RngCore};
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let key = SigningKey::from_bytes(&seed);
    (crate::b64_encode(&key.to_bytes()), crate::b64_encode(VerifyingKey::from(&key).as_bytes()))
}

/// The public half of a secret update key.
pub fn public_key_of(secret: &str) -> Option<String> {
    signing_key(secret).map(|k| crate::b64_encode(VerifyingKey::from(&k).as_bytes()))
}

/// `major.minor.patch`, ignoring anything after a `-` or `+`.
pub fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// Whether `candidate` is a later version than `current`. A version that
/// cannot be read is older than every other.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

/// The platform this program was built for, if releases are made for it.
pub fn this_platform() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else if cfg!(all(target_os = "android", target_arch = "aarch64")) {
        Some("android-aarch64")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> Release {
        Release {
            platform: "windows-x86_64".into(),
            version: "1.2.3".into(),
            file: "NEXUS-Cloud_1.2.3_x64-setup.exe".into(),
            size: 10,
            blake3: blake3::hash(b"0123456789").to_hex().to_string(),
            notes: "Fixes".into(),
            published_at: 1,
            channel: String::new(),
        }
    }

    #[test]
    fn only_trusted_keys_sign_releases() {
        let (secret, public) = generate_key();
        let (_, stranger) = generate_key();
        assert_eq!(public_key_of(&secret).as_deref(), Some(public.as_str()));
        let signed = SignedRelease::sign(&release(), &secret).unwrap();
        assert_eq!(signed.verify(&[&public]), Some(release()));
        assert_eq!(signed.verify(&[&stranger, &public]), Some(release()));
        assert_eq!(signed.verify(&[&stranger]), None);

        // Any change to the signed text breaks the signature.
        let mut changed = signed.clone();
        changed.release = changed.release.replace("1.2.3", "9.2.3");
        assert_eq!(changed.verify(&[&public]), None);

        // A signed release must still make sense.
        let mut odd = release();
        odd.platform = "plan9".into();
        assert_eq!(SignedRelease::sign(&odd, &secret).unwrap().verify(&[&public]), None);
    }

    #[test]
    fn channels_are_signed_and_only_beta_is_known() {
        let (secret, public) = generate_key();
        // A stable release is signed without a channel, exactly as before
        // channels existed, so older devices read it the same way.
        let stable = SignedRelease::sign(&release(), &secret).unwrap();
        assert!(!stable.release.contains("channel"));
        assert!(!stable.verify(&[&public]).unwrap().is_beta());

        let mut beta = release();
        beta.channel = BETA.into();
        let signed = SignedRelease::sign(&beta, &secret).unwrap();
        assert!(signed.verify(&[&public]).unwrap().is_beta());
        // Taking the channel out breaks the signature: a beta never passes
        // for a stable release.
        let mut stripped = signed.clone();
        stripped.release = stripped.release.replace(",\"channel\":\"beta\"", "");
        assert_ne!(stripped.release, signed.release);
        assert_eq!(stripped.verify(&[&public]), None);

        let mut odd = release();
        odd.channel = "nightly".into();
        assert_eq!(SignedRelease::sign(&odd, &secret).unwrap().verify(&[&public]), None);
    }

    #[test]
    fn the_spare_key_is_trusted_too() {
        assert!(TRUSTED_KEYS.len() >= 2);
        for key in TRUSTED_KEYS {
            assert!(crate::identity::parse_public_key(key).is_some(), "{key}");
        }
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
        assert!(is_newer("0.1.0", "test"));
        assert!(!is_newer("nonsense", "0.1.0"));
        assert_eq!(parse_version("0.1.2-beta"), Some((0, 1, 2)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
    }
}
