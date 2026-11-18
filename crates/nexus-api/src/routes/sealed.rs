//! Encryption for backups.
//!
//! A backup's key is 32 random bytes. Everything else is derived from it
//! with BLAKE3's key derivation:
//!
//! * a stored content's name: BLAKE3 keyed with a names key over the
//!   content's own BLAKE3, so the store does not reveal which known file
//!   it holds;
//! * a content's encryption key: from the backup key and the content's
//!   BLAKE3, so the same content always encrypts the same way (it is still
//!   stored once) and no two contents share a key;
//! * a snapshot list's key: from the backup key and the snapshot's id.
//!
//! The format is `NXB1` and then the content in 64 KiB chunks, each sealed
//! with ChaCha20-Poly1305 under a nonce made of the chunk's number and
//! whether it is the last one, so chunks cannot be reordered, dropped or
//! cut off unnoticed. Any chunk can be produced or opened on its own, which
//! lets a transfer continue from the middle after a dropped connection.
//!
//! The recovery key is the backup key written out for people: Crockford
//! base32 in groups of four, with a short checksum against typing mistakes.

use axum::body::Bytes;
use futures_util::{Stream, StreamExt};
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use std::io;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

pub const MAGIC: &[u8; 4] = b"NXB1";
pub const CHUNK: u64 = 64 * 1024;
const TAG: u64 = 16;

#[derive(Clone)]
pub struct BackupKey([u8; 32]);

impl BackupKey {
    pub fn generate() -> io::Result<Self> {
        let mut k = [0u8; 32];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut k).map_err(|_| io::Error::other("no randomness"))?;
        Ok(Self(k))
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        Some(Self(b.try_into().ok()?))
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    fn derive(&self, context: &str, extra: &[u8]) -> [u8; 32] {
        let mut material = self.0.to_vec();
        material.extend_from_slice(extra);
        blake3::derive_key(context, &material)
    }

    /// The name a content with this BLAKE3 has in the store.
    pub fn object_name(&self, content_hash: &str) -> String {
        let names = self.derive("NEXUS backup 2026 object names", &[]);
        blake3::keyed_hash(&names, content_hash.as_bytes()).to_hex().to_string()
    }

    pub fn content_key(&self, content_hash: &str) -> [u8; 32] {
        self.derive("NEXUS backup 2026 content", content_hash.as_bytes())
    }

    pub fn snapshot_key(&self, snapshot_id: &str) -> [u8; 32] {
        self.derive("NEXUS backup 2026 snapshot", snapshot_id.as_bytes())
    }

    pub fn recovery_key(&self) -> String {
        let mut data = self.0.to_vec();
        data.extend_from_slice(&blake3::hash(&self.0).as_bytes()[..2]);
        let text = base32(&data);
        text.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join("-")
    }

    pub fn from_recovery_key(text: &str) -> Option<Self> {
        let cleaned: String = text.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let data = unbase32(&cleaned)?;
        if data.len() != 34 {
            return None;
        }
        let (key, sum) = data.split_at(32);
        if blake3::hash(key).as_bytes()[..2] != *sum {
            return None;
        }
        Self::from_bytes(key)
    }
}

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn base32(data: &[u8]) -> String {
    let mut out = String::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &b in data {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn unbase32(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.chars() {
        // Crockford: I and L read as 1, O as 0.
        let c = match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            c => c,
        };
        let v = ALPHABET.iter().position(|&a| a as char == c)? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// The format

/// Chunks a content of `plain` bytes is cut into (an empty one has one).
pub fn chunks(plain: u64) -> u64 {
    plain.div_ceil(CHUNK).max(1)
}

/// Size of the sealed form of `plain` bytes.
pub fn sealed_len(plain: u64) -> u64 {
    MAGIC.len() as u64 + plain + TAG * chunks(plain)
}

/// Size of the content a sealed form of `sealed` bytes holds; `None` if no
/// content seals to that size.
pub fn plain_len(sealed: u64) -> Option<u64> {
    let body = sealed.checked_sub(MAGIC.len() as u64)?;
    let n = body.div_ceil(CHUNK + TAG).max(1);
    let plain = body.checked_sub(n * TAG)?;
    (sealed_len(plain) == sealed).then_some(plain)
}

/// Where chunk `i` starts in the sealed form.
pub fn chunk_offset(i: u64) -> u64 {
    MAGIC.len() as u64 + i * (CHUNK + TAG)
}

fn plain_size(i: u64, plain: u64) -> u64 {
    plain.saturating_sub(i * CHUNK).min(CHUNK)
}

fn cipher(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32-byte key"))
}

fn nonce(i: u64, last: bool) -> Nonce {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(&i.to_be_bytes());
    n[11] = last as u8;
    Nonce::assume_unique_for_key(n)
}

fn seal_chunk(c: &LessSafeKey, i: u64, total: u64, mut data: Vec<u8>) -> Vec<u8> {
    c.seal_in_place_append_tag(nonce(i, i + 1 == total), Aad::empty(), &mut data).expect("sealing cannot fail");
    data
}

fn open_chunk(c: &LessSafeKey, i: u64, total: u64, mut data: Vec<u8>) -> io::Result<Vec<u8>> {
    let len = c
        .open_in_place(nonce(i, i + 1 == total), Aad::empty(), &mut data)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "damaged or not encrypted with this key"))?
        .len();
    data.truncate(len);
    Ok(data)
}

/// Seals bytes held in memory (snapshot lists).
pub fn seal_bytes(key: &[u8; 32], data: &[u8]) -> Vec<u8> {
    let c = cipher(key);
    let total = chunks(data.len() as u64);
    let mut out = MAGIC.to_vec();
    for i in 0..total {
        let start = (i * CHUNK) as usize;
        let end = (start + CHUNK as usize).min(data.len());
        out.extend(seal_chunk(&c, i, total, data[start..end].to_vec()));
    }
    out
}

/// Opens what [`seal_bytes`] made.
pub fn open_bytes(key: &[u8; 32], data: &[u8]) -> io::Result<Vec<u8>> {
    let body = data.strip_prefix(MAGIC.as_slice()).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not an encrypted backup file"))?;
    let c = cipher(key);
    let n_chunks = (body.len() as u64).div_ceil(CHUNK + TAG).max(1);
    let plain = (body.len() as u64).checked_sub(n_chunks * TAG).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cut short"))?;
    let total = chunks(plain);
    let mut out = Vec::with_capacity(plain as usize);
    let mut at = 0usize;
    for i in 0..total {
        let len = (plain_size(i, plain) + TAG) as usize;
        let piece = body.get(at..at + len).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "cut short"))?;
        out.extend(open_chunk(&c, i, total, piece.to_vec())?);
        at += len;
    }
    Ok(out)
}

/// BLAKE3 of the content as it was read for sealing: the next chunk it
/// expects and the hasher. A send that continues from the middle reads some
/// chunks twice; each counts once.
#[derive(Clone, Default)]
pub struct PlainHash(Arc<Mutex<(u64, blake3::Hasher)>>);

impl PlainHash {
    fn feed(&self, i: u64, data: &[u8]) {
        let mut g = self.0.lock().unwrap();
        if g.0 == i {
            g.1.update(data);
            g.0 += 1;
        }
    }

    /// The hash, once every chunk of a content of `plain` bytes was read.
    pub fn finish(&self, plain: u64) -> Option<String> {
        let g = self.0.lock().unwrap();
        (g.0 == chunks(plain)).then(|| g.1.finalize().to_hex().to_string())
    }
}

/// The sealed form of a file of `plain` bytes, from `offset` in the sealed
/// form on. The file is read from the chunk that offset falls in, and what
/// is read goes into `hash`.
pub async fn seal_file(
    path: std::path::PathBuf,
    key: [u8; 32],
    plain: u64,
    offset: u64,
    hash: PlainHash,
) -> io::Result<impl Stream<Item = io::Result<Bytes>> + Send> {
    let total = chunks(plain);
    let (first, skip) = if offset < MAGIC.len() as u64 {
        (0, offset)
    } else {
        let past = offset - MAGIC.len() as u64;
        (past / (CHUNK + TAG), past % (CHUNK + TAG))
    };
    let mut file = tokio::fs::File::open(path).await?;
    tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(first * CHUNK)).await?;
    let header = (offset < MAGIC.len() as u64).then(|| Bytes::from_static(MAGIC));
    let c = cipher(&key);
    let body = futures_util::stream::unfold((file, first, c), move |(mut file, i, c)| {
        let hash = hash.clone();
        async move {
        if i >= total {
            return None;
        }
        let mut buf = vec![0u8; plain_size(i, plain) as usize];
        if let Err(e) = file.read_exact(&mut buf).await {
            let e = if e.kind() == io::ErrorKind::UnexpectedEof { io::Error::other("the file got shorter while it was backed up") } else { e };
            return Some((Err(e), (file, total, c)));
        }
        hash.feed(i, &buf);
        let sealed = seal_chunk(&c, i, total, buf);
        Some((Ok(Bytes::from(sealed)), (file, i + 1, c)))
        }
    });
    let stream = futures_util::stream::iter(header.map(Ok)).chain(body);
    Ok(skip_bytes(stream, skip))
}

/// Opens a sealed body that starts at chunk `first` (at [`chunk_offset`]),
/// yielding the content from `skip` bytes into that chunk on.
pub fn open_stream<S, E>(body: S, key: [u8; 32], plain: u64, first: u64, skip: u64) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    S: Stream<Item = Result<Bytes, E>> + Send + Unpin + 'static,
    E: std::fmt::Display,
{
    let total = chunks(plain);
    let c = cipher(&key);
    let opened = futures_util::stream::unfold((body, Vec::<u8>::new(), first, c, false), move |(mut body, mut buf, i, c, done)| async move {
        if done || i >= total {
            return None;
        }
        let need = (plain_size(i, plain) + TAG) as usize;
        while buf.len() < need {
            match body.next().await {
                Some(Ok(bytes)) => buf.extend_from_slice(&bytes),
                Some(Err(e)) => return Some((Err(io::Error::other(e.to_string())), (body, buf, i, c, true))),
                None => return Some((Err(io::Error::new(io::ErrorKind::UnexpectedEof, "cut short")), (body, buf, i, c, true))),
            }
        }
        let rest = buf.split_off(need);
        match open_chunk(&c, i, total, buf) {
            Ok(plain) => Some((Ok(Bytes::from(plain)), (body, rest, i + 1, c, false))),
            Err(e) => Some((Err(e), (body, rest, i, c, true))),
        }
    });
    skip_bytes(opened, skip)
}

/// Drops the first `n` bytes of a stream.
fn skip_bytes<S>(stream: S, n: u64) -> impl Stream<Item = io::Result<Bytes>> + Send
where
    S: Stream<Item = io::Result<Bytes>> + Send,
{
    let mut left = n;
    stream.filter_map(move |item| {
        let out = match item {
            Ok(mut b) if left > 0 => {
                let drop = left.min(b.len() as u64);
                left -= drop;
                let rest = b.split_off(drop as usize);
                (!rest.is_empty()).then_some(Ok(rest))
            }
            other => Some(other),
        };
        std::future::ready(out)
    })
}

/// Where to start reading a sealed content to get its plain bytes from
/// `offset` on: (sealed offset, first chunk, bytes to skip in it).
pub fn plain_start(offset: u64) -> (u64, u64, u64) {
    let first = offset / CHUNK;
    (chunk_offset(first), first, offset % CHUNK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> BackupKey {
        BackupKey([7u8; 32])
    }

    async fn collect(s: impl Stream<Item = io::Result<Bytes>>) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        futures_util::pin_mut!(s);
        while let Some(b) = s.next().await {
            out.extend_from_slice(&b?);
        }
        Ok(out)
    }

    #[test]
    fn recovery_keys_round_trip_and_catch_typos() {
        let k = BackupKey::generate().unwrap();
        let text = k.recovery_key();
        assert_eq!(text.split('-').count(), 14, "{text}");
        assert_eq!(BackupKey::from_recovery_key(&text).unwrap().bytes(), k.bytes());
        assert_eq!(BackupKey::from_recovery_key(&text.to_lowercase().replace('-', " ")).unwrap().bytes(), k.bytes());
        let mut typo: Vec<char> = text.chars().collect();
        typo[3] = if typo[3] == 'A' { 'B' } else { 'A' };
        assert!(BackupKey::from_recovery_key(&typo.into_iter().collect::<String>()).is_none());
        assert!(BackupKey::from_recovery_key("nonsense").is_none());
    }

    #[test]
    fn names_hide_hashes_and_keys_differ() {
        let k = key();
        let h = blake3::hash(b"x").to_hex().to_string();
        assert_ne!(k.object_name(&h), h);
        assert_eq!(k.object_name(&h).len(), 64);
        assert_ne!(k.content_key(&h), k.content_key(&blake3::hash(b"y").to_hex()));
        assert_ne!(BackupKey([8u8; 32]).object_name(&h), k.object_name(&h));
    }

    #[test]
    fn bytes_round_trip_and_detect_damage() {
        let k = [1u8; 32];
        for len in [0usize, 1, CHUNK as usize, CHUNK as usize + 1, 3 * CHUNK as usize + 5] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let sealed = seal_bytes(&k, &data);
            assert_eq!(sealed.len() as u64, sealed_len(len as u64));
            assert_eq!(open_bytes(&k, &sealed).unwrap(), data);
            let mut bad = sealed.clone();
            let at = bad.len() / 2;
            bad[at] ^= 1;
            assert!(open_bytes(&k, &bad).is_err());
            assert!(open_bytes(&[2u8; 32], &sealed).is_err());
        }
        // Cutting off the last chunk is noticed.
        let data = vec![9u8; 2 * CHUNK as usize];
        let sealed = seal_bytes(&k, &data);
        assert!(open_bytes(&k, &sealed[..sealed.len() - (CHUNK + TAG) as usize]).is_err());
    }

    #[test]
    fn sizes_map_back() {
        for plain in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 5 * CHUNK, 5 * CHUNK + 3] {
            assert_eq!(plain_len(sealed_len(plain)), Some(plain), "{plain}");
        }
        assert_eq!(plain_len(3), None);
        assert_eq!(plain_len(10), None);
    }

    #[tokio::test]
    async fn files_seal_from_any_offset_and_open_from_any_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let data: Vec<u8> = (0..(2 * CHUNK + 777)).map(|i| (i * 7 % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let k = [3u8; 32];
        let hash = PlainHash::default();
        let whole = collect(seal_file(path.clone(), k, data.len() as u64, 0, hash.clone()).await.unwrap()).await.unwrap();
        assert_eq!(hash.finish(data.len() as u64).unwrap(), blake3::hash(&data).to_hex().to_string());
        assert_eq!(whole, seal_bytes(&k, &data));
        for offset in [1, 4, 5, CHUNK + 20, chunk_offset(2), whole.len() as u64 - 3] {
            let part = collect(seal_file(path.clone(), k, data.len() as u64, offset, PlainHash::default()).await.unwrap()).await.unwrap();
            assert_eq!(part, whole[offset as usize..], "offset {offset}");
        }
        // A send cut off in the second chunk and continued: hashed once.
        let hash = PlainHash::default();
        let first = collect(seal_file(path.clone(), k, data.len() as u64, 0, hash.clone()).await.unwrap().take(2)).await.unwrap();
        assert!(hash.finish(data.len() as u64).is_none());
        let rest = collect(seal_file(path.clone(), k, data.len() as u64, first.len() as u64 - 9, hash.clone()).await.unwrap()).await.unwrap();
        assert_eq!([&first[..first.len() - 9], &rest[..]].concat(), whole);
        assert_eq!(hash.finish(data.len() as u64).unwrap(), blake3::hash(&data).to_hex().to_string());

        for offset in [0, 10, CHUNK, CHUNK + 1, 2 * CHUNK + 776] {
            let (at, first, skip) = plain_start(offset);
            let body = futures_util::stream::iter(
                whole[at as usize..].chunks(1000).map(|c| Ok::<_, io::Error>(Bytes::copy_from_slice(c))).collect::<Vec<_>>(),
            );
            let plain = collect(open_stream(body, k, data.len() as u64, first, skip)).await.unwrap();
            assert_eq!(plain, data[offset as usize..], "offset {offset}");
        }
    }
}
