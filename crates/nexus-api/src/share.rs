//! Share links: a file uploaded to a link service, so anyone with the link
//! can download it from anywhere.
//!
//! **Send** (the default) is end-to-end encrypted. The file is encrypted
//! here with a random key before it leaves this device; the key travels only
//! in the link's `#` part, which browsers never send to the server. The
//! service stores data it cannot read, and the recipient's browser decrypts
//! it. This follows the protocol of [Send](https://github.com/timvisee/send):
//! keys from HKDF-SHA256, metadata in AES-128-GCM, content in
//! `aes128gcm` encrypted records (RFC 8188) of 64 KiB.
//!
//! **temp.sh** takes larger files for longer but is not encrypted: the
//! service can read what is uploaded.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures_util::{SinkExt, StreamExt};
use nexus_common::{NexusError, Result};
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{hkdf, pbkdf2};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::Message;

/// The public Send instance used unless another is set.
pub const DEFAULT_SEND: &str = "https://send.vis.ee";
pub const TEMPSH: &str = "https://temp.sh";
/// temp.sh keeps files for 3 days and takes up to 4 GB.
pub const TEMPSH_MAX_SIZE: u64 = 4_000_000_000;
pub const TEMPSH_EXPIRE_SECS: u64 = 3 * 24 * 3600;

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// A silent connection during an upload is given up after this long.
const STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Notified as an upload makes progress; asked whether to stop.
pub trait Watch: Send + Sync {
    /// Bytes of the original file sent so far.
    fn sent(&self, bytes: u64);
    fn cancelled(&self) -> bool;
}

fn unavailable(service: &str, e: impl std::fmt::Display) -> NexusError {
    NexusError::Unavailable(format!("{service}: {e}"))
}

fn stopped() -> NexusError {
    NexusError::Unavailable("Cancelled".into())
}

// ---------------------------------------------------------------------------
// Keys

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64_decode(text: &str) -> Vec<u8> {
    URL_SAFE_NO_PAD.decode(text).unwrap_or_default()
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

/// HKDF-SHA256 into `out`.
fn hkdf_into(ikm: &[u8], salt: &[u8], info: &[u8], out: &mut [u8]) {
    let info = [info];
    hkdf::Salt::new(hkdf::HKDF_SHA256, salt)
        .extract(ikm)
        .expand(&info, Len(out.len()))
        .and_then(|okm| okm.fill(out))
        .expect("HKDF output length is valid");
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut out = [0u8; N];
    SystemRandom::new().fill(&mut out).map_err(|_| NexusError::internal("no secure random numbers"))?;
    Ok(out)
}

/// The link secret and the keys derived from it.
pub struct Keys {
    secret: [u8; 16],
    meta_key: [u8; 16],
    /// Proves to the service that a downloader holds the link (or the
    /// password, once one is set). An HMAC-SHA256 key: 64 bytes.
    auth_key: [u8; 64],
}

impl Keys {
    pub fn new() -> Result<Self> {
        Ok(Self::from_secret(random()?))
    }

    pub fn from_secret(secret: [u8; 16]) -> Self {
        let mut meta_key = [0u8; 16];
        hkdf_into(&secret, &[], b"metadata", &mut meta_key);
        let mut auth_key = [0u8; 64];
        hkdf_into(&secret, &[], b"authentication", &mut auth_key);
        Self { secret, meta_key, auth_key }
    }

    /// The part of the link after `#`.
    pub fn secret_b64(&self) -> String {
        b64(&self.secret)
    }

    pub fn auth_b64(&self) -> String {
        b64(&self.auth_key)
    }

    /// What recipients see before downloading: the name, size and type,
    /// encrypted. Send uses one key per file, so a fixed nonce is safe here.
    pub fn encrypt_metadata(&self, name: &str, size: u64, mime: &str) -> Vec<u8> {
        #[derive(Serialize)]
        struct Entry<'a> {
            name: &'a str,
            size: u64,
            #[serde(rename = "type")]
            mime: &'a str,
        }
        #[derive(Serialize)]
        struct Manifest<'a> {
            files: [Entry<'a>; 1],
        }
        #[derive(Serialize)]
        struct Meta<'a> {
            name: &'a str,
            size: u64,
            #[serde(rename = "type")]
            mime: &'a str,
            manifest: Manifest<'a>,
        }
        let entry = || Entry { name, size, mime };
        let meta = Meta { name, size, mime, manifest: Manifest { files: [entry()] } };
        let mut data = serde_json::to_vec(&meta).expect("plain data");
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &self.meta_key).expect("16-byte key"));
        key.seal_in_place_append_tag(Nonce::assume_unique_for_key([0u8; 12]), Aad::empty(), &mut data)
            .expect("metadata fits in one message");
        data
    }
}

/// The key that replaces the link's own when a password is set: recipients
/// derive it from the password and the full link.
pub fn password_auth_key(password: &str, share_url: &str) -> [u8; 64] {
    let mut out = [0u8; 64];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(100).expect("nonzero"),
        share_url.as_bytes(),
        password.as_bytes(),
        &mut out,
    );
    out
}

// ---------------------------------------------------------------------------
// Content encryption (aes128gcm records)

/// Encrypted record size.
pub const RS: usize = 64 * 1024;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = 21;
/// Plaintext per record: one byte goes to the padding delimiter.
pub const CHUNK: usize = RS - TAG_LEN - 1;

/// Bytes uploaded for a file of `size` bytes.
pub fn encrypted_size(size: u64) -> u64 {
    let chunk = CHUNK as u64;
    HEADER_LEN as u64 + size + (TAG_LEN as u64 + 1) * size.div_ceil(chunk)
}

pub struct Encryptor {
    salt: [u8; 16],
    key: LessSafeKey,
    nonce_base: [u8; 12],
    seq: u32,
}

impl Encryptor {
    pub fn new(keys: &Keys) -> Result<Self> {
        Ok(Self::with_salt(&keys.secret, random()?))
    }

    fn with_salt(secret: &[u8; 16], salt: [u8; 16]) -> Self {
        let mut key = [0u8; 16];
        hkdf_into(secret, &salt, b"Content-Encoding: aes128gcm\0", &mut key);
        let mut nonce_base = [0u8; 12];
        hkdf_into(secret, &salt, b"Content-Encoding: nonce\0", &mut nonce_base);
        let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &key).expect("16-byte key"));
        Self { salt, key, nonce_base, seq: 0 }
    }

    /// Comes first: the salt, the record size and an empty key id.
    pub fn header(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(HEADER_LEN);
        h.extend_from_slice(&self.salt);
        h.extend_from_slice(&(RS as u32).to_be_bytes());
        h.push(0);
        h
    }

    fn nonce(&self) -> [u8; 12] {
        let mut n = self.nonce_base;
        let tail = u32::from_be_bytes([n[8], n[9], n[10], n[11]]) ^ self.seq;
        n[8..].copy_from_slice(&tail.to_be_bytes());
        n
    }

    /// Encrypts one record of at most [`CHUNK`] bytes. Every record but the
    /// last is padded to the full record size.
    pub fn seal(&mut self, chunk: &[u8], last: bool) -> Vec<u8> {
        assert!(chunk.len() <= CHUNK, "record too large");
        let mut rec = Vec::with_capacity(RS);
        rec.extend_from_slice(chunk);
        if last {
            rec.push(2);
        } else {
            rec.push(1);
            rec.resize(RS - TAG_LEN, 0);
        }
        let nonce = Nonce::assume_unique_for_key(self.nonce());
        self.key.seal_in_place_append_tag(nonce, Aad::empty(), &mut rec).expect("record fits");
        self.seq += 1;
        rec
    }
}

fn secret_of(secret_b64: &str) -> Option<[u8; 16]> {
    URL_SAFE_NO_PAD.decode(secret_b64).ok()?.try_into().ok()
}

/// Reads content the way a recipient's browser does, with the secret from
/// the link. `None` if anything does not check out.
pub fn decrypt(secret_b64: &str, data: &[u8]) -> Option<Vec<u8>> {
    let secret = secret_of(secret_b64)?;
    let salt: [u8; 16] = data.get(..16)?.try_into().ok()?;
    if u32::from_be_bytes(data.get(16..20)?.try_into().ok()?) as usize != RS || *data.get(20)? != 0 {
        return None;
    }
    let mut dec = Encryptor::with_salt(&secret, salt);
    let records: Vec<&[u8]> = data[HEADER_LEN..].chunks(RS).collect();
    let mut out = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        let mut rec = rec.to_vec();
        let plain = dec.key.open_in_place(Nonce::assume_unique_for_key(dec.nonce()), Aad::empty(), &mut rec).ok()?;
        let end = plain.iter().rposition(|&b| b != 0)?;
        let last = i + 1 == records.len();
        if plain[end] != if last { 2 } else { 1 } {
            return None;
        }
        out.extend_from_slice(&plain[..end]);
        dec.seq += 1;
    }
    Some(out)
}

/// Reads the name, size and type recipients see.
pub fn decrypt_metadata(secret_b64: &str, data: &[u8]) -> Option<serde_json::Value> {
    let keys = Keys::from_secret(secret_of(secret_b64)?);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &keys.meta_key).ok()?);
    let mut data = data.to_vec();
    let plain = key.open_in_place(Nonce::assume_unique_for_key([0u8; 12]), Aad::empty(), &mut data).ok()?;
    serde_json::from_slice(plain).ok()
}

// ---------------------------------------------------------------------------
// The client

/// TLS with the built-in root certificates, so it works the same on every
/// platform (phones included).
fn tls() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .tls_backend_preconfigured(tls())
            .user_agent(concat!("NEXUS-Cloud/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("HTTP client")
    })
}

/// Checks a link service address set in Settings: HTTPS, or plain HTTP on
/// this computer (a self-hosted instance for testing). Returns it without a
/// trailing slash.
pub fn service_url(url: &str) -> Result<String> {
    let url = url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(url).map_err(|_| NexusError::invalid("Enter a web address, like https://send.vis.ee"))?;
    let local = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let ok = match parsed.scheme() {
        "https" => true,
        "http" => local,
        _ => false,
    };
    if !ok || parsed.host_str().is_none() || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(NexusError::invalid("The address must start with https://"));
    }
    Ok(url.to_string())
}

/// What a Send instance allows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendLimits {
    #[serde(rename = "MAX_FILE_SIZE")]
    pub max_file_size: u64,
    #[serde(rename = "MAX_DOWNLOADS")]
    pub max_downloads: u32,
    #[serde(rename = "MAX_EXPIRE_SECONDS")]
    pub max_expire_seconds: u64,
}

pub async fn send_limits(base: &str) -> Result<SendLimits> {
    #[derive(Deserialize)]
    struct Config {
        #[serde(rename = "LIMITS")]
        limits: SendLimits,
    }
    let res = http()
        .get(format!("{base}/config"))
        .timeout(CALL_TIMEOUT)
        .send()
        .await
        .map_err(|e| unavailable("Send", e))?;
    if !res.status().is_success() {
        return Err(unavailable("Send", format!("{base} answered {}", res.status())));
    }
    let config: Config = res.json().await.map_err(|_| unavailable("Send", format!("{base} is not a Send service")))?;
    Ok(config.limits)
}

pub struct SendFile<'a> {
    pub path: &'a Path,
    pub size: u64,
    pub name: &'a str,
    pub mime: &'a str,
    pub expire_secs: u64,
    pub downloads: u32,
    pub password: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Uploaded {
    /// The full link, key included.
    pub url: String,
    pub id: String,
    pub owner_token: String,
}

#[derive(Deserialize)]
struct WsReply {
    url: Option<String>,
    #[serde(rename = "ownerToken")]
    owner_token: Option<String>,
    id: Option<String>,
    ok: Option<bool>,
    error: Option<serde_json::Value>,
}

fn send_error(code: &serde_json::Value) -> NexusError {
    match code.as_u64() {
        Some(413) => NexusError::invalid("This file is larger than the Send service accepts"),
        Some(400) => NexusError::invalid("The Send service refused these link settings"),
        Some(401) => NexusError::Unavailable("This Send service needs an account; choose another in Settings".into()),
        _ => NexusError::Unavailable(format!("The Send service could not store the file ({code})")),
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_reply(ws: &mut Ws, wait: Duration) -> Result<WsReply> {
    loop {
        let msg = tokio::time::timeout(wait, ws.next())
            .await
            .map_err(|_| unavailable("Send", "no answer"))?
            .ok_or_else(|| unavailable("Send", "the connection closed"))?
            .map_err(|e| unavailable("Send", e))?;
        match msg {
            Message::Text(text) => {
                return serde_json::from_str(&text).map_err(|_| unavailable("Send", "unexpected answer"));
            }
            Message::Close(_) => return Err(unavailable("Send", "the connection closed")),
            _ => continue,
        }
    }
}

async fn send(ws: &mut Ws, data: Vec<u8>) -> Result<()> {
    tokio::time::timeout(STALL_TIMEOUT, ws.send(Message::Binary(data.into())))
        .await
        .map_err(|_| unavailable("Send", "the upload stalled"))?
        .map_err(|e| unavailable("Send", e))
}

/// Encrypts and uploads a file to a Send service.
pub async fn send_upload(base: &str, file: &SendFile<'_>, watch: &dyn Watch) -> Result<Uploaded> {
    let keys = Keys::new()?;
    let meta = keys.encrypt_metadata(file.name, file.size, file.mime);

    let ws_url = format!("{}/api/ws", base.replacen("https://", "wss://", 1).replacen("http://", "ws://", 1));
    let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls()));
    let (mut ws, _) = tokio::time::timeout(
        CALL_TIMEOUT,
        tokio_tungstenite::connect_async_tls_with_config(ws_url.as_str(), None, false, Some(connector)),
    )
    .await
    .map_err(|_| unavailable("Send", "took too long to answer"))?
    .map_err(|e| unavailable("Send", e))?;

    let hello = json!({
        "fileMetadata": b64(&meta),
        "authorization": format!("send-v1 {}", keys.auth_b64()),
        "timeLimit": file.expire_secs,
        "dlimit": file.downloads,
    });
    ws.send(Message::Text(hello.to_string().into())).await.map_err(|e| unavailable("Send", e))?;
    let reply = ws_reply(&mut ws, CALL_TIMEOUT).await?;
    if let Some(code) = reply.error {
        return Err(send_error(&code));
    }
    let (Some(url), Some(owner_token), Some(id)) = (reply.url, reply.owner_token, reply.id) else {
        return Err(unavailable("Send", "unexpected answer"));
    };
    // The link is shown to people: only accept one on the service itself.
    if !url.starts_with(&format!("{base}/")) || url.contains('#') {
        return Err(unavailable("Send", "it answered with a link elsewhere"));
    }

    let mut enc = Encryptor::new(&keys)?;
    send(&mut ws, enc.header()).await?;

    let mut source = tokio::fs::File::open(file.path).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    loop {
        if watch.cancelled() {
            let _ = ws.close(None).await;
            return Err(stopped());
        }
        let want = (file.size - done).min(CHUNK as u64) as usize;
        source.read_exact(&mut buf[..want]).await.map_err(|_| NexusError::Conflict("The file changed while it was being shared".into()))?;
        done += want as u64;
        let last = done == file.size;
        send(&mut ws, enc.seal(&buf[..want], last)).await?;
        watch.sent(done);
        if last {
            break;
        }
    }
    // A single zero byte ends the upload.
    send(&mut ws, vec![0]).await?;
    let end = ws_reply(&mut ws, Duration::from_secs(600)).await?;
    let _ = ws.close(None).await;
    if let Some(code) = end.error {
        return Err(send_error(&code));
    }
    if end.ok != Some(true) {
        return Err(unavailable("Send", "the upload did not finish"));
    }

    let link = format!("{url}#{}", keys.secret_b64());
    if let Some(password) = file.password.filter(|p| !p.is_empty()) {
        let auth = password_auth_key(password, &link);
        let set = owner_call(base, "password", &id, json!({ "owner_token": owner_token, "auth": b64(&auth) })).await;
        if let Err(e) = set {
            // Without the password the link would be open to anyone who has it.
            let _ = send_delete(base, &id, &owner_token).await;
            return Err(e);
        }
    }
    Ok(Uploaded { url: link, id, owner_token })
}

async fn owner_call(base: &str, what: &str, id: &str, body: serde_json::Value) -> Result<reqwest::Response> {
    let res = http()
        .post(format!("{base}/api/{what}/{id}"))
        .json(&body)
        .timeout(CALL_TIMEOUT)
        .send()
        .await
        .map_err(|e| unavailable("Send", e))?;
    match res.status() {
        s if s.is_success() => Ok(res),
        reqwest::StatusCode::NOT_FOUND => Err(NexusError::NotFound("Link".into())),
        s => Err(unavailable("Send", format!("answered {s}"))),
    }
}

/// Deletes the file from the service. Already gone counts as done.
pub async fn send_delete(base: &str, id: &str, owner_token: &str) -> Result<()> {
    match owner_call(base, "delete", id, json!({ "owner_token": owner_token })).await {
        Ok(_) | Err(NexusError::NotFound(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SendInfo {
    /// Downloads so far.
    #[serde(rename = "dtotal", alias = "dl")]
    pub dl: u32,
    pub dlimit: u32,
    /// Milliseconds left.
    pub ttl: i64,
}

/// How the link stands; `None` once it expired or ran out of downloads.
pub async fn send_info(base: &str, id: &str, owner_token: &str) -> Result<Option<SendInfo>> {
    match owner_call(base, "info", id, json!({ "owner_token": owner_token })).await {
        Ok(res) => Ok(Some(res.json().await.map_err(|_| unavailable("Send", "unexpected answer"))?)),
        Err(NexusError::NotFound(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// temp.sh

/// Uploads a file to temp.sh as it is (not encrypted). Returns the link.
pub async fn tempsh_upload(path: &Path, size: u64, name: &str, mime: &str, watch: Arc<dyn Watch>) -> Result<String> {
    let file = tokio::fs::File::open(path).await?;
    let stream = futures_util::stream::unfold((file, 0u64, watch), |(mut file, sent, watch)| async move {
        if watch.cancelled() {
            return Some((Err(std::io::Error::other("cancelled")), (file, sent, watch)));
        }
        let mut buf = vec![0u8; 256 * 1024];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                let sent = sent + n as u64;
                watch.sent(sent);
                Some((Ok::<_, std::io::Error>(buf), (file, sent, watch)))
            }
            Err(e) => Some((Err(e), (file, sent, watch))),
        }
    });
    let part = reqwest::multipart::Part::stream_with_length(reqwest::Body::wrap_stream(stream), size)
        .file_name(name.to_string())
        .mime_str(mime)
        .map_err(|e| unavailable("temp.sh", e))?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let res = http()
        .post(format!("{TEMPSH}/upload"))
        .multipart(form)
        .send()
        .await
        .map_err(|e| unavailable("temp.sh", e))?;
    let status = res.status();
    let text = res.text().await.map_err(|e| unavailable("temp.sh", e))?;
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        return Err(NexusError::invalid("This file is larger than temp.sh accepts"));
    }
    if !status.is_success() {
        return Err(unavailable("temp.sh", format!("answered {status}")));
    }
    let link = text.trim().replacen("http://", "https://", 1);
    if !link.starts_with("https://temp.sh/") || link.contains(char::is_whitespace) {
        return Err(unavailable("temp.sh", "unexpected answer"));
    }
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(secret: &[u8; 16], data: &[u8]) -> Vec<u8> {
        decrypt(&b64(secret), data).expect("authentic content")
    }

    fn encrypt(secret: &[u8; 16], salt: [u8; 16], data: &[u8]) -> Vec<u8> {
        let mut enc = Encryptor::with_salt(secret, salt);
        let mut out = enc.header();
        let chunks: Vec<&[u8]> = data.chunks(CHUNK).collect();
        for (i, c) in chunks.iter().enumerate() {
            out.extend(enc.seal(c, i + 1 == chunks.len()));
        }
        out
    }

    #[test]
    fn content_round_trips_and_has_the_expected_size() {
        let secret = [7u8; 16];
        for size in [1, 100, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 5] {
            let data: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            let sealed = encrypt(&secret, [9u8; 16], &data);
            assert_eq!(sealed.len() as u64, encrypted_size(size as u64), "size {size}");
            assert_eq!(open(&secret, &sealed), data, "size {size}");
        }
    }

    #[test]
    fn a_tampered_record_is_refused() {
        let secret = [1u8; 16];
        let mut sealed = encrypt(&secret, [2u8; 16], b"hello");
        assert_eq!(decrypt(&b64(&secret), &sealed).unwrap(), b"hello");
        sealed[HEADER_LEN + 1] ^= 1;
        assert!(decrypt(&b64(&secret), &sealed).is_none());
        assert!(decrypt(&b64(&[3u8; 16]), &encrypt(&secret, [2u8; 16], b"hello")).is_none(), "wrong key");
    }

    /// Values computed with the Web Crypto API exactly as Send's own web
    /// page does it (keychain.js and ece.js), for a fixed secret and salt.
    #[test]
    fn matches_send_in_the_browser() {
        let secret: [u8; 16] = core::array::from_fn(|i| i as u8);
        let keys = Keys::from_secret(secret);
        assert_eq!(keys.secret_b64(), "AAECAwQFBgcICQoLDA0ODw");
        assert_eq!(keys.auth_b64(), WEB_AUTH);
        assert_eq!(b64(&keys.encrypt_metadata("a.txt", 5, "text/plain")), WEB_META);
        let salt: [u8; 16] = core::array::from_fn(|i| (100 + i) as u8);
        assert_eq!(b64(&encrypt(&secret, salt, b"hello")), WEB_CONTENT);
        assert_eq!(b64(&password_auth_key("pw", "https://send.example/download/abc/#AAECAwQFBgcICQoLDA0ODw")), WEB_PASSWORD);
    }

    const WEB_AUTH: &str = "kQ93M995gaAZVUQIc_hb2GSavYMMJAt5u-tT55jauhvgtaL35F3FDba5oPgZFb44gpXP7mS1ENvs1EcEtb8lng";
    const WEB_META: &str = "JJo8SSnwoCHctQEYKEbNau_LPnaN2u9BQgdf5ozgswPoTZfxwvpcwNKXz-tWc44dS02sdYmFwrRMO6f_VvIZ9y8cfceXy7QO-ksTJPT-8mORZxs6kpSqeHoTB8QDdyq9RTquxTbIkMhu92sBdfs5aSWiMHronnuKo8V4duSh3MR_RQ";
    const WEB_CONTENT: &str = "ZGVmZ2hpamtsbW5vcHFycwABAAAAjJyLEzDEHG0miXE8iugoNXyTZ4otKQ";
    const WEB_PASSWORD: &str = "HotQ5xJB0wiZtiKyHkRArHzsHsmhXpJTo8kivW26bVGv6SpbL-YHZhG5vfE0aCHBrcp9yWjdm0dqznEZgrBHKw";

    struct Quiet;
    impl Watch for Quiet {
        fn sent(&self, _: u64) {}
        fn cancelled(&self) -> bool {
            false
        }
    }

    /// Uploads a small generated file to the public Send service, checks it,
    /// and deletes it. Needs the internet: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_send_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        let data: Vec<u8> = (0..(CHUNK + 1000)).map(|i| b'a' + (i % 26) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let file = SendFile {
            path: &path,
            size: data.len() as u64,
            name: "nexus-test.txt",
            mime: "text/plain",
            expire_secs: 300,
            downloads: 1,
            password: Some("test"),
        };
        let up = send_upload(DEFAULT_SEND, &file, &Quiet).await.unwrap();
        assert!(up.url.starts_with("https://send.vis.ee/download/"));
        let info = send_info(DEFAULT_SEND, &up.id, &up.owner_token).await.unwrap().unwrap();
        assert_eq!((info.dl, info.dlimit), (0, 1));
        assert!(info.ttl > 0);
        send_delete(DEFAULT_SEND, &up.id, &up.owner_token).await.unwrap();
        assert!(send_info(DEFAULT_SEND, &up.id, &up.owner_token).await.unwrap().is_none());
    }

    #[test]
    fn service_addresses_are_checked() {
        assert_eq!(service_url("https://send.vis.ee/").unwrap(), "https://send.vis.ee");
        assert!(service_url("http://send.vis.ee").is_err());
        assert!(service_url("http://127.0.0.1:1443").is_ok());
        assert!(service_url("ftp://x").is_err());
        assert!(service_url("https://x/?a=b").is_err());
    }
}
