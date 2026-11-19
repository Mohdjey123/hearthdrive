//! Passkeys (WebAuthn): signing in with Windows Hello, a phone or a security
//! key instead of a password.
//!
//! Only what a node on this computer needs: the relying party is always
//! `localhost` (the API only answers on loopback), passkeys are discoverable
//! and must verify the person (PIN, fingerprint or face), and attestation is
//! not asked for, so any authenticator works. Signatures are checked with
//! `ring`: ES256 (P-256), EdDSA (Ed25519) and RS256.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde::Deserialize;

/// The relying party id every passkey is made for.
pub const RP_ID: &str = "localhost";

/// COSE algorithm ids offered to the browser, most preferred first.
pub const ALGORITHMS: [i64; 3] = [-7, -8, -257];

const FLAG_USER_PRESENT: u8 = 0x01;
const FLAG_USER_VERIFIED: u8 = 0x04;
const FLAG_ATTESTED: u8 = 0x40;

/// Why a passkey was refused (shown to the person).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub &'static str);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Refused {}

type Checked<T> = Result<T, Refused>;

/// A passkey just created, to be stored.
#[derive(Debug, Clone)]
pub struct NewPasskey {
    pub credential_id: Vec<u8>,
    /// The public key as the authenticator gave it (a COSE key).
    pub public_key: Vec<u8>,
    pub algorithm: i64,
    pub sign_count: u32,
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &str) -> Checked<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(text.trim_end_matches('=')).map_err(|_| Refused("The passkey's answer is damaged"))
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: bool,
}

/// Checks what the browser says it signed: the ceremony, our challenge and
/// the page it was asked from.
fn check_client_data(json: &[u8], kind: &str, challenge: &[u8], origin: &str) -> Checked<()> {
    let data: ClientData = serde_json::from_slice(json).map_err(|_| Refused("The passkey's answer is damaged"))?;
    if data.kind != kind {
        return Err(Refused("The passkey answered a different request"));
    }
    if unb64(&data.challenge)? != challenge {
        return Err(Refused("The passkey answered an old or different request"));
    }
    if data.origin != origin || data.cross_origin {
        return Err(Refused("The passkey was used from another page"));
    }
    Ok(())
}

struct AuthData {
    rp_id_hash: [u8; 32],
    flags: u8,
    sign_count: u32,
    /// Credential id and COSE public key, when a passkey was just created.
    attested: Option<(Vec<u8>, Vec<u8>)>,
}

fn parse_auth_data(data: &[u8]) -> Checked<AuthData> {
    let damaged = Refused("The passkey's answer is damaged");
    if data.len() < 37 {
        return Err(damaged);
    }
    let rp_id_hash: [u8; 32] = data[..32].try_into().map_err(|_| damaged.clone())?;
    let flags = data[32];
    let sign_count = u32::from_be_bytes([data[33], data[34], data[35], data[36]]);
    let attested = if flags & FLAG_ATTESTED != 0 {
        let rest = &data[37..];
        // AAGUID (16 bytes), credential id length (2), credential id, key.
        if rest.len() < 18 {
            return Err(damaged);
        }
        let len = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        let rest = &rest[18..];
        if len == 0 || len > 1023 || rest.len() < len {
            return Err(damaged);
        }
        let (id, rest) = rest.split_at(len);
        let (_, used) = cbor::decode(rest).map_err(|_| damaged.clone())?;
        Some((id.to_vec(), rest[..used].to_vec()))
    } else {
        None
    };
    Ok(AuthData { rp_id_hash, flags, sign_count, attested })
}

fn check_flags(auth: &AuthData) -> Checked<()> {
    if auth.rp_id_hash[..] != digest(&SHA256, RP_ID.as_bytes()).as_ref()[..] {
        return Err(Refused("This passkey belongs to another site"));
    }
    if auth.flags & FLAG_USER_PRESENT == 0 || auth.flags & FLAG_USER_VERIFIED == 0 {
        return Err(Refused("The passkey did not check it was you (PIN, fingerprint or face)"));
    }
    Ok(())
}

enum PublicKey {
    P256(Vec<u8>),
    Ed25519(Vec<u8>),
    Rsa { n: Vec<u8>, e: Vec<u8> },
}

/// Reads a COSE key (RFC 9053) of a kind this module can check.
fn parse_cose_key(bytes: &[u8]) -> Checked<(PublicKey, i64)> {
    use cbor::Value;
    let unsupported = Refused("This passkey uses a kind of key Hearthdrive does not support");
    let (Value::Map(map), _) = cbor::decode(bytes).map_err(|_| Refused("The passkey's key is damaged"))? else {
        return Err(Refused("The passkey's key is damaged"));
    };
    let get = |k: i128| map.iter().find(|(key, _)| *key == Value::Int(k)).map(|(_, v)| v);
    let int = |k: i128| match get(k) {
        Some(Value::Int(v)) => Some(*v),
        _ => None,
    };
    let bytes_of = |k: i128| match get(k) {
        Some(Value::Bytes(v)) => Some(v.clone()),
        _ => None,
    };
    let (kty, alg) = (int(1), int(3));
    match (kty, alg) {
        // EC2, ES256, curve P-256.
        (Some(2), Some(-7)) if int(-1) == Some(1) => {
            let (x, y) = (bytes_of(-2).ok_or(unsupported.clone())?, bytes_of(-3).ok_or(unsupported.clone())?);
            if x.len() != 32 || y.len() != 32 {
                return Err(unsupported);
            }
            let mut point = vec![0x04];
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            Ok((PublicKey::P256(point), -7))
        }
        // OKP, EdDSA, curve Ed25519.
        (Some(1), Some(-8)) if int(-1) == Some(6) => {
            let x = bytes_of(-2).filter(|x| x.len() == 32).ok_or(unsupported)?;
            Ok((PublicKey::Ed25519(x), -8))
        }
        // RSA, RS256.
        (Some(3), Some(-257)) => {
            let n = bytes_of(-1).ok_or(unsupported.clone())?;
            let e = bytes_of(-2).ok_or(unsupported)?;
            let strip = |v: Vec<u8>| v.into_iter().skip_while(|b| *b == 0).collect::<Vec<u8>>();
            Ok((PublicKey::Rsa { n: strip(n), e: strip(e) }, -257))
        }
        _ => Err(unsupported),
    }
}

fn verify_signature(key: &PublicKey, message: &[u8], sig: &[u8]) -> bool {
    match key {
        PublicKey::P256(point) => UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point).verify(message, sig).is_ok(),
        PublicKey::Ed25519(x) => UnparsedPublicKey::new(&signature::ED25519, x).verify(message, sig).is_ok(),
        PublicKey::Rsa { n, e } => RsaPublicKeyComponents { n, e }.verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, sig).is_ok(),
    }
}

/// Checks a new passkey (`navigator.credentials.create`) made for
/// `challenge` on the page at `origin`.
pub fn verify_registration(client_data_json: &[u8], attestation_object: &[u8], challenge: &[u8], origin: &str) -> Checked<NewPasskey> {
    use cbor::Value;
    check_client_data(client_data_json, "webauthn.create", challenge, origin)?;
    let (Value::Map(map), _) = cbor::decode(attestation_object).map_err(|_| Refused("The passkey's answer is damaged"))? else {
        return Err(Refused("The passkey's answer is damaged"));
    };
    // Attestation was not asked for, so whatever statement came is ignored.
    let auth_data = map
        .iter()
        .find(|(k, _)| *k == Value::Text("authData".into()))
        .and_then(|(_, v)| match v {
            Value::Bytes(b) => Some(b.as_slice()),
            _ => None,
        })
        .ok_or(Refused("The passkey's answer is damaged"))?;
    let auth = parse_auth_data(auth_data)?;
    check_flags(&auth)?;
    let (credential_id, public_key) = auth.attested.ok_or(Refused("The passkey sent no key"))?;
    let (_, algorithm) = parse_cose_key(&public_key)?;
    Ok(NewPasskey { credential_id, public_key, algorithm, sign_count: auth.sign_count })
}

/// Checks a sign-in with a stored passkey (`navigator.credentials.get`).
/// Returns the authenticator's new signature counter.
pub fn verify_assertion(
    public_key: &[u8],
    stored_count: u32,
    client_data_json: &[u8],
    authenticator_data: &[u8],
    sig: &[u8],
    challenge: &[u8],
    origin: &str,
) -> Checked<u32> {
    check_client_data(client_data_json, "webauthn.get", challenge, origin)?;
    let auth = parse_auth_data(authenticator_data)?;
    check_flags(&auth)?;
    let (key, _) = parse_cose_key(public_key)?;
    let mut message = authenticator_data.to_vec();
    message.extend_from_slice(digest(&SHA256, client_data_json).as_ref());
    if !verify_signature(&key, &message, sig) {
        return Err(Refused("The passkey's signature is wrong"));
    }
    // Authenticators that count must count up; going back means the key was
    // copied. Most passkeys always send 0.
    if (auth.sign_count != 0 || stored_count != 0) && auth.sign_count <= stored_count {
        return Err(Refused("This passkey may have been copied; remove it and add it again"));
    }
    Ok(auth.sign_count)
}

/// Just enough CBOR (RFC 8949) to read authenticator data and COSE keys.
pub mod cbor {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Value {
        Int(i128),
        Bytes(Vec<u8>),
        Text(String),
        Array(Vec<Value>),
        Map(Vec<(Value, Value)>),
        Bool(bool),
        Null,
        /// Floats and simple values nothing here needs.
        Other,
    }

    #[derive(Debug)]
    pub struct Malformed;

    /// Decodes one item; returns it and how many bytes it took.
    pub fn decode(data: &[u8]) -> Result<(Value, usize), Malformed> {
        let mut r = Reader { data, pos: 0 };
        let value = r.item(0)?;
        Ok((value, r.pos))
    }

    struct Reader<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl Reader<'_> {
        fn take(&mut self, n: usize) -> Result<&[u8], Malformed> {
            let end = self.pos.checked_add(n).filter(|end| *end <= self.data.len()).ok_or(Malformed)?;
            let slice = &self.data[self.pos..end];
            self.pos = end;
            Ok(slice)
        }

        fn arg(&mut self, info: u8) -> Result<u64, Malformed> {
            Ok(match info {
                0..=23 => u64::from(info),
                24 => u64::from(self.take(1)?[0]),
                25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| Malformed)?)),
                26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| Malformed)?)),
                27 => u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| Malformed)?),
                // Indefinite lengths are not used by authenticators.
                _ => return Err(Malformed),
            })
        }

        fn len(&mut self, info: u8) -> Result<usize, Malformed> {
            let n = self.arg(info)?;
            // Never more items or bytes than are left.
            if n > (self.data.len() - self.pos) as u64 {
                return Err(Malformed);
            }
            Ok(n as usize)
        }

        fn item(&mut self, depth: u8) -> Result<Value, Malformed> {
            if depth > 16 {
                return Err(Malformed);
            }
            let head = self.take(1)?[0];
            let (major, info) = (head >> 5, head & 0x1f);
            Ok(match major {
                0 => Value::Int(i128::from(self.arg(info)?)),
                1 => Value::Int(-1 - i128::from(self.arg(info)?)),
                2 => {
                    let n = self.len(info)?;
                    Value::Bytes(self.take(n)?.to_vec())
                }
                3 => {
                    let n = self.len(info)?;
                    Value::Text(String::from_utf8(self.take(n)?.to_vec()).map_err(|_| Malformed)?)
                }
                4 => {
                    let n = self.len(info)?;
                    let mut items = Vec::with_capacity(n);
                    for _ in 0..n {
                        items.push(self.item(depth + 1)?);
                    }
                    Value::Array(items)
                }
                5 => {
                    let n = self.len(info)?;
                    let mut entries = Vec::with_capacity(n);
                    for _ in 0..n {
                        let k = self.item(depth + 1)?;
                        let v = self.item(depth + 1)?;
                        entries.push((k, v));
                    }
                    Value::Map(entries)
                }
                // A tag: the tagged item is all that matters here.
                6 => {
                    self.arg(info)?;
                    self.item(depth + 1)?
                }
                _ => match info {
                    20 => Value::Bool(false),
                    21 => Value::Bool(true),
                    22 | 23 => Value::Null,
                    24 => {
                        self.take(1)?;
                        Value::Other
                    }
                    25..=27 => {
                        self.take(1 << (info - 24))?;
                        Value::Other
                    }
                    0..=19 => Value::Other,
                    _ => return Err(Malformed),
                },
            })
        }
    }

    /// Encodes the values used by authenticators (tests and the software
    /// authenticator only).
    #[cfg(any(test, feature = "soft-authenticator"))]
    pub fn encode(value: &Value) -> Vec<u8> {
        fn head(out: &mut Vec<u8>, major: u8, n: u64) {
            let m = major << 5;
            match n {
                0..=23 => out.push(m | n as u8),
                24..=0xff => out.extend([m | 24, n as u8]),
                0x100..=0xffff => {
                    out.push(m | 25);
                    out.extend((n as u16).to_be_bytes());
                }
                0x1_0000..=0xffff_ffff => {
                    out.push(m | 26);
                    out.extend((n as u32).to_be_bytes());
                }
                _ => {
                    out.push(m | 27);
                    out.extend(n.to_be_bytes());
                }
            }
        }
        fn go(out: &mut Vec<u8>, v: &Value) {
            match v {
                Value::Int(i) if *i >= 0 => head(out, 0, *i as u64),
                Value::Int(i) => head(out, 1, (-1 - *i) as u64),
                Value::Bytes(b) => {
                    head(out, 2, b.len() as u64);
                    out.extend(b);
                }
                Value::Text(t) => {
                    head(out, 3, t.len() as u64);
                    out.extend(t.as_bytes());
                }
                Value::Array(items) => {
                    head(out, 4, items.len() as u64);
                    items.iter().for_each(|i| go(out, i));
                }
                Value::Map(entries) => {
                    head(out, 5, entries.len() as u64);
                    for (k, v) in entries {
                        go(out, k);
                        go(out, v);
                    }
                }
                Value::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
                Value::Null | Value::Other => out.push(0xf6),
            }
        }
        let mut out = Vec::new();
        go(&mut out, value);
        out
    }
}

/// A passkey kept in memory, standing in for a browser and authenticator
/// in tests.
#[cfg(any(test, feature = "soft-authenticator"))]
pub mod soft {
    use super::cbor::{Value, encode};
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

    pub struct SoftPasskey {
        key: EcdsaKeyPair,
        pub credential_id: Vec<u8>,
        pub user_handle: Vec<u8>,
        pub count: u32,
        rng: SystemRandom,
    }

    /// What the browser sends back for a new passkey (all base64url).
    pub struct Created {
        pub credential_id: String,
        pub client_data: String,
        pub attestation_object: String,
    }

    /// What the browser sends back when signing in (all base64url).
    pub struct Assertion {
        pub credential_id: String,
        pub client_data: String,
        pub authenticator_data: String,
        pub signature: String,
        pub user_handle: String,
    }

    fn client_data(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
        serde_json::json!({ "type": kind, "challenge": b64(challenge), "origin": origin, "crossOrigin": false }).to_string().into_bytes()
    }

    impl SoftPasskey {
        pub fn new(user_handle: &[u8]) -> Self {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).expect("key");
            let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).expect("key");
            let mut credential_id = vec![0u8; 16];
            ring::rand::SecureRandom::fill(&rng, &mut credential_id).expect("random");
            Self { key, credential_id, user_handle: user_handle.to_vec(), count: 0, rng }
        }

        fn auth_data(&self, rp_id: &str, attested: bool) -> Vec<u8> {
            let mut data = digest(&SHA256, rp_id.as_bytes()).as_ref().to_vec();
            data.push(FLAG_USER_PRESENT | FLAG_USER_VERIFIED | if attested { FLAG_ATTESTED } else { 0 });
            data.extend(self.count.to_be_bytes());
            if attested {
                let point = self.key.public_key().as_ref();
                let cose = Value::Map(vec![
                    (Value::Int(1), Value::Int(2)),
                    (Value::Int(3), Value::Int(-7)),
                    (Value::Int(-1), Value::Int(1)),
                    (Value::Int(-2), Value::Bytes(point[1..33].to_vec())),
                    (Value::Int(-3), Value::Bytes(point[33..65].to_vec())),
                ]);
                data.extend([0u8; 16]);
                data.extend((self.credential_id.len() as u16).to_be_bytes());
                data.extend(&self.credential_id);
                data.extend(encode(&cose));
            }
            data
        }

        pub fn create(&self, challenge: &[u8], origin: &str) -> Created {
            let attestation = Value::Map(vec![
                (Value::Text("fmt".into()), Value::Text("none".into())),
                (Value::Text("attStmt".into()), Value::Map(vec![])),
                (Value::Text("authData".into()), Value::Bytes(self.auth_data(RP_ID, true))),
            ]);
            Created {
                credential_id: b64(&self.credential_id),
                client_data: b64(&client_data("webauthn.create", challenge, origin)),
                attestation_object: b64(&encode(&attestation)),
            }
        }

        pub fn sign(&mut self, challenge: &[u8], origin: &str) -> Assertion {
            self.count += 1;
            let auth = self.auth_data(RP_ID, false);
            let client = client_data("webauthn.get", challenge, origin);
            let mut message = auth.clone();
            message.extend_from_slice(digest(&SHA256, &client).as_ref());
            let sig = self.key.sign(&self.rng, &message).expect("sign");
            Assertion {
                credential_id: b64(&self.credential_id),
                client_data: b64(&client),
                authenticator_data: b64(&auth),
                signature: b64(sig.as_ref()),
                user_handle: b64(&self.user_handle),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cbor::{Value, encode};
    use super::soft::SoftPasskey;
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    const ORIGIN: &str = "http://localhost:7464";

    #[test]
    fn registers_and_signs_in_with_a_p256_passkey() {
        let mut key = SoftPasskey::new(b"user-1");
        let challenge = b"register challenge";
        let created = key.create(challenge, ORIGIN);
        let passkey = verify_registration(&unb64(&created.client_data).unwrap(), &unb64(&created.attestation_object).unwrap(), challenge, ORIGIN)
            .expect("registered");
        assert_eq!(passkey.credential_id, key.credential_id);
        assert_eq!(passkey.algorithm, -7);

        let challenge = b"login challenge";
        let a = key.sign(challenge, ORIGIN);
        let (client, auth, sig) = (unb64(&a.client_data).unwrap(), unb64(&a.authenticator_data).unwrap(), unb64(&a.signature).unwrap());
        let count = verify_assertion(&passkey.public_key, 0, &client, &auth, &sig, challenge, ORIGIN).expect("signed in");
        assert_eq!(count, 1);

        // Replayed (counter did not move), wrong challenge, wrong page, tampered.
        assert!(verify_assertion(&passkey.public_key, 1, &client, &auth, &sig, challenge, ORIGIN).is_err());
        assert!(verify_assertion(&passkey.public_key, 0, &client, &auth, &sig, b"other", ORIGIN).is_err());
        assert!(verify_assertion(&passkey.public_key, 0, &client, &auth, &sig, challenge, "http://localhost:1").is_err());
        let mut bad = auth.clone();
        bad[36] ^= 1;
        assert_eq!(
            verify_assertion(&passkey.public_key, 0, &client, &bad, &sig, challenge, ORIGIN),
            Err(Refused("The passkey's signature is wrong"))
        );
        // A create answer is not a sign-in.
        assert!(verify_registration(&client, &unb64(&created.attestation_object).unwrap(), challenge, ORIGIN).is_err());
    }

    #[test]
    fn checks_ed25519_signatures() {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let cose = encode(&Value::Map(vec![
            (Value::Int(1), Value::Int(1)),
            (Value::Int(3), Value::Int(-8)),
            (Value::Int(-1), Value::Int(6)),
            (Value::Int(-2), Value::Bytes(pair.public_key().as_ref().to_vec())),
        ]));
        let mut auth = digest(&SHA256, RP_ID.as_bytes()).as_ref().to_vec();
        auth.push(FLAG_USER_PRESENT | FLAG_USER_VERIFIED);
        auth.extend(0u32.to_be_bytes());
        let client = serde_json::json!({ "type": "webauthn.get", "challenge": b64(b"c"), "origin": ORIGIN }).to_string();
        let mut message = auth.clone();
        message.extend_from_slice(digest(&SHA256, client.as_bytes()).as_ref());
        let sig = pair.sign(&message);
        assert_eq!(verify_assertion(&cose, 0, client.as_bytes(), &auth, sig.as_ref(), b"c", ORIGIN), Ok(0));
    }

    #[test]
    fn refuses_unverified_people_and_other_sites() {
        let key = SoftPasskey::new(b"u");
        let created = key.create(b"c", ORIGIN);
        let attestation = unb64(&created.attestation_object).unwrap();
        let (Value::Map(mut map), _) = cbor::decode(&attestation).unwrap() else { panic!() };
        let Value::Bytes(auth) = &mut map[2].1 else { panic!() };
        auth[32] &= !FLAG_USER_VERIFIED;
        let unverified = encode(&Value::Map(map.clone()));
        assert!(verify_registration(&unb64(&created.client_data).unwrap(), &unverified, b"c", ORIGIN).unwrap_err().0.contains("check it was you"));
        let Value::Bytes(auth) = &mut map[2].1 else { panic!() };
        auth[32] |= FLAG_USER_VERIFIED;
        auth[0] ^= 1;
        let elsewhere = encode(&Value::Map(map));
        assert_eq!(
            verify_registration(&unb64(&created.client_data).unwrap(), &elsewhere, b"c", ORIGIN).unwrap_err(),
            Refused("This passkey belongs to another site")
        );
    }

    #[test]
    fn cbor_rejects_truncated_and_oversized_input() {
        assert!(cbor::decode(&[0x5a, 0xff, 0xff, 0xff, 0xff]).is_err(), "length beyond the data");
        assert!(cbor::decode(&[0x82, 0x01]).is_err(), "array cut short");
        assert!(cbor::decode(&[0x9f]).is_err(), "indefinite length");
        let deep = vec![0x81; 40];
        assert!(cbor::decode(&deep).is_err(), "nesting too deep");
        assert_eq!(cbor::decode(&[0x38, 0x18]).unwrap().0, Value::Int(-25));
    }
}
