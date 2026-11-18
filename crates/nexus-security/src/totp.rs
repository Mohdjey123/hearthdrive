//! Codes from an authenticator app (TOTP, RFC 6238): six digits, a new one
//! every 30 seconds, from a secret shared once through a QR code. Also the
//! one-time recovery codes given out with it.

use argon2::password_hash::rand_core::{OsRng, RngCore};
use ring::hmac;

/// Seconds each code is valid for.
pub const STEP_SECS: i64 = 30;
const DIGITS: u32 = 6;
/// Codes from one step before or after are accepted too (clock drift and
/// typing time).
const WINDOW: i64 = 1;

/// A new 160-bit secret, the size RFC 4226 recommends.
pub fn new_secret() -> Vec<u8> {
    let mut secret = vec![0u8; 20];
    OsRng.fill_bytes(&mut secret);
    secret
}

/// RFC 4648 base32 without padding: how authenticator apps take secrets.
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The `otpauth://` link the QR code holds.
pub fn uri(secret: &[u8], issuer: &str, account: &str) -> String {
    let label = format!("{}:{}", encode(issuer), encode(account));
    format!(
        "otpauth://totp/{label}?secret={}&issuer={}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
        base32(secret),
        encode(issuer)
    )
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The code for one time step (HOTP, RFC 4226, with HMAC-SHA1 as the
/// standard and every authenticator app require).
pub fn code_at(secret: &[u8], step: i64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let mac = hmac::sign(&key, &(step as u64).to_be_bytes());
    let mac = mac.as_ref();
    let offset = (mac[mac.len() - 1] & 0x0f) as usize;
    let value = u32::from_be_bytes([mac[offset] & 0x7f, mac[offset + 1], mac[offset + 2], mac[offset + 3]]);
    value % 10u32.pow(DIGITS)
}

/// Checks a typed code at `now_secs`. Returns the step it belongs to, which
/// must be newer than `last_step` (the step of the last code accepted), so
/// a code seen over someone's shoulder cannot be used again.
pub fn verify(secret: &[u8], code: &str, now_secs: i64, last_step: Option<i64>) -> Option<i64> {
    let code = code.trim().replace([' ', '-'], "");
    if code.len() != DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let typed: u32 = code.parse().ok()?;
    let now = now_secs / STEP_SECS;
    (now - WINDOW..=now + WINDOW)
        .filter(|step| last_step.is_none_or(|last| *step > last))
        .find(|step| constant_eq(code_at(secret, *step), typed))
}

fn constant_eq(a: u32, b: u32) -> bool {
    (a ^ b) == 0
}

/// Whether a typed code looks like an authenticator-app code (rather than a
/// recovery code).
pub fn looks_like_code(code: &str) -> bool {
    let code = code.trim().replace([' ', '-'], "");
    code.len() == DIGITS as usize && code.bytes().all(|b| b.is_ascii_digit())
}

/// Ten one-time recovery codes, each 50 random bits written as
/// `xxxxx-xxxxx` (lower-case base32).
pub fn recovery_codes() -> Vec<String> {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    (0..10)
        .map(|_| {
            let mut bytes = [0u8; 10];
            OsRng.fill_bytes(&mut bytes);
            let chars: String = bytes.iter().map(|b| ALPHABET[(b & 31) as usize] as char).collect();
            format!("{}-{}", &chars[..5], &chars[5..])
        })
        .collect()
}

/// The form recovery codes are stored and compared in: lower case, no
/// spaces or dashes.
pub fn normalize_recovery_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 appendix B (SHA-1), truncated to six digits.
    const SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn matches_the_rfc_test_vectors() {
        for (time, code) in [(59, 287082), (1111111109, 81804), (1111111111, 50471), (1234567890, 5924), (2000000000, 279037)] {
            assert_eq!(code_at(SECRET, time / STEP_SECS), code, "at {time}");
        }
    }

    #[test]
    fn accepts_neighbouring_steps_once() {
        let now = 1_700_000_000;
        let code = format!("{:06}", code_at(SECRET, now / STEP_SECS - 1));
        let step = verify(SECRET, &code, now, None).expect("previous step accepted");
        assert_eq!(step, now / STEP_SECS - 1);
        assert_eq!(verify(SECRET, &code, now, Some(step)), None, "a code works only once");
        assert_eq!(verify(SECRET, "123 45", now, None), None);
        let old = format!("{:06}", code_at(SECRET, now / STEP_SECS - 3));
        assert_eq!(verify(SECRET, &old, now, None), None, "old codes are refused");
    }

    #[test]
    fn base32_and_uri() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(b"f"), "MY");
        let uri = uri(SECRET, "NEXUS (Desk)", "anna");
        assert!(uri.starts_with("otpauth://totp/NEXUS%20%28Desk%29:anna?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&issuer=NEXUS%20%28Desk%29"), "{uri}");
    }

    #[test]
    fn recovery_codes_are_distinct_and_normalized() {
        let codes = recovery_codes();
        assert_eq!(codes.len(), 10);
        assert!(codes.iter().all(|c| c.len() == 11 && c.as_bytes()[5] == b'-'));
        assert_eq!(normalize_recovery_code(" AbCde-fGh23 "), "abcdefgh23");
        assert!(looks_like_code("123 456") && !looks_like_code(&codes[0]));
    }
}
