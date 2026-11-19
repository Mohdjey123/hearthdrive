//! Encrypted folders: a folder locked with a password keeps every file in
//! it encrypted on the disk, so a stolen or lost drive gives nothing away.
//! Unlocking it with the password turns the files back into ordinary ones.
//!
//! The password gives a 32-byte key (PBKDF2-HMAC-SHA256, 600 000 rounds,
//! with a random salt kept in the folder's `.nexus-vault` file, next to a
//! check value that tells a wrong password from a right one). Each file gets
//! its own key from that key and 16 random bytes stored at its start, and
//! is sealed in 64 KiB chunks with ChaCha20-Poly1305 (the backup format), so
//! changed, swapped or cut-short files are noticed. A locked file is named
//! like the original with `.nxlocked` added: names stay readable.

use crate::auth::{AuthUser, Need};
use crate::error::ApiResult;
use crate::routes::sealed;
use crate::routes::transfers::finish_job;
use crate::state::SharedState;
use axum::Json;
use axum::extract::{Path as UrlPath, State};
use base64::Engine;
use futures_util::StreamExt;
use nexus_common::{NexusError, Result};
use nexus_security::Action;
use nexus_storage::paths;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub const MARKER: &str = paths::VAULT_MARKER;
pub const LOCKED_EXT: &str = "nxlocked";
const FILE_MAGIC: &[u8; 4] = b"NXV1";
const ROUNDS: u32 = 600_000;
const CHECK_TEXT: &[u8] = b"NEXUS encrypted folder";

#[derive(Serialize, Deserialize)]
struct Marker {
    v: u32,
    salt: String,
    rounds: u32,
    /// [`CHECK_TEXT`] sealed with the folder key.
    check: String,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).expect("random");
    b
}

fn derive(password: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(rounds.max(1)).unwrap(),
        salt,
        password.as_bytes(),
        &mut key,
    );
    key
}

fn file_key(folder_key: &[u8; 32], salt: &[u8; 16]) -> [u8; 32] {
    let mut material = folder_key.to_vec();
    material.extend_from_slice(salt);
    blake3::derive_key("NEXUS 2026 encrypted folder file", &material)
}

async fn read_marker(dir: &Path) -> Option<Marker> {
    let raw = tokio::fs::read(dir.join(MARKER)).await.ok()?;
    serde_json::from_slice(&raw).ok()
}

/// The folder key for a password: checked against the folder's marker, or a
/// new marker made if the folder has none yet.
async fn folder_key(dir: &Path, password: &str, create: bool) -> Result<[u8; 32]> {
    match read_marker(dir).await {
        Some(m) => {
            let salt = b64().decode(&m.salt).map_err(|_| NexusError::internal("the folder's key file is damaged"))?;
            let key = tokio::task::spawn_blocking({
                let password = password.to_string();
                move || derive(&password, &salt, m.rounds)
            })
            .await
            .map_err(NexusError::internal)?;
            let check = b64().decode(&m.check).map_err(|_| NexusError::internal("the folder's key file is damaged"))?;
            match sealed::open_bytes(&key, &check) {
                Ok(text) if text == CHECK_TEXT => Ok(key),
                _ => Err(NexusError::PermissionDenied("That is not this folder's password".into())),
            }
        }
        None if create => {
            if password.chars().count() < 8 {
                return Err(NexusError::invalid("Use a password of at least 8 characters"));
            }
            let salt: [u8; 16] = random();
            let key = tokio::task::spawn_blocking({
                let password = password.to_string();
                move || derive(&password, &salt, ROUNDS)
            })
            .await
            .map_err(NexusError::internal)?;
            let marker = Marker { v: 1, salt: b64().encode(salt), rounds: ROUNDS, check: b64().encode(sealed::seal_bytes(&key, CHECK_TEXT)) };
            tokio::fs::write(dir.join(MARKER), serde_json::to_vec_pretty(&marker).unwrap()).await?;
            Ok(key)
        }
        None => Err(NexusError::invalid("This folder is not encrypted")),
    }
}

/// Every file under `dir` (not the marker), with whether it is locked.
fn files_under(dir: &Path) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            let path = e.path();
            if t.is_symlink() {
                continue;
            }
            if t.is_dir() {
                if e.file_name().to_string_lossy().eq_ignore_ascii_case(paths::INTERNAL_DIR) {
                    continue;
                }
                stack.push(path);
            } else if t.is_file() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == MARKER || name.ends_with(".nxpart") {
                    continue;
                }
                let locked = path.extension().is_some_and(|x| x.eq_ignore_ascii_case(LOCKED_EXT));
                out.push((path, locked));
            }
        }
    }
    out
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".nxpart");
    target.with_file_name(name)
}

/// Encrypts one file into `name.nxlocked`, then removes the original.
async fn lock_file(path: &Path, key: &[u8; 32]) -> Result<u64> {
    let plain = tokio::fs::metadata(path).await?.len();
    let salt: [u8; 16] = random();
    let mut target = path.as_os_str().to_os_string();
    target.push(format!(".{LOCKED_EXT}"));
    let target = PathBuf::from(target);
    let part = part_path(&target);
    let result: Result<()> = async {
        let mut out = tokio::fs::File::create(&part).await?;
        out.write_all(FILE_MAGIC).await?;
        out.write_all(&salt).await?;
        let mut stream = Box::pin(sealed::seal_file(path.to_path_buf(), file_key(key, &salt), plain, 0, sealed::PlainHash::default()).await?);
        while let Some(chunk) = stream.next().await {
            out.write_all(&chunk?).await?;
        }
        out.sync_all().await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, &target).await?;
    tokio::fs::remove_file(path).await?;
    Ok(plain)
}

/// Decrypts `name.nxlocked` back into `name`, then removes the locked file.
async fn unlock_file(path: &Path, key: &[u8; 32]) -> Result<u64> {
    let total = tokio::fs::metadata(path).await?.len();
    let mut file = tokio::fs::File::open(path).await?;
    let mut head = [0u8; 20];
    tokio::io::AsyncReadExt::read_exact(&mut file, &mut head).await.map_err(|_| NexusError::invalid("not an encrypted file"))?;
    if &head[..4] != FILE_MAGIC {
        return Err(NexusError::invalid("not an encrypted file"));
    }
    let salt: [u8; 16] = head[4..20].try_into().unwrap();
    let plain = sealed::plain_len(total - 20).ok_or_else(|| NexusError::invalid("the encrypted file is cut short"))?;
    // Skip the sealed form's own header.
    let mut magic = [0u8; 4];
    tokio::io::AsyncReadExt::read_exact(&mut file, &mut magic).await?;
    let target = path.with_extension("");
    let part = part_path(&target);
    let body = futures_util::stream::unfold(file, |mut f| async move {
        let mut buf = vec![0u8; 256 * 1024];
        match tokio::io::AsyncReadExt::read(&mut f, &mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(axum::body::Bytes::from(buf)), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let result: Result<()> = async {
        let mut out = tokio::fs::File::create(&part).await?;
        let mut opened = Box::pin(sealed::open_stream(Box::pin(body), file_key(key, &salt), plain, 0, 0));
        while let Some(chunk) = opened.next().await {
            out.write_all(&chunk.map_err(|_| NexusError::invalid("the encrypted file was changed or damaged"))?).await?;
        }
        out.sync_all().await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    if tokio::fs::symlink_metadata(&target).await.is_ok() {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(NexusError::Conflict("a file with the original name is already there".into()));
    }
    tokio::fs::rename(&part, &target).await?;
    tokio::fs::remove_file(path).await?;
    Ok(plain)
}

#[derive(Serialize)]
pub struct VaultState {
    encrypted: bool,
    locked_files: usize,
    open_files: usize,
}

async fn folder(s: &SharedState, user: &AuthUser, id: &str, need: Need) -> Result<(nexus_common::types::StorageRoot, nexus_common::types::FileEntry, PathBuf)> {
    let file = s.db.get_file(id).await?;
    if !file.is_dir {
        return Err(NexusError::invalid("Choose a folder"));
    }
    user.require_access(&file.root_id, &file.relative_path, need).map_err(|e| e.0)?;
    let root = s.db.get_root(&file.root_id).await?;
    s.storage.ensure_online(&root)?;
    let dir = s.storage.resolve(&root, &file.relative_path)?;
    Ok((root, file, dir))
}

/// `GET /files/{id}/encryption`
pub async fn state(State(s): State<SharedState>, user: AuthUser, UrlPath(id): UrlPath<String>) -> ApiResult<Json<VaultState>> {
    user.require(Action::Read)?;
    let (_, _, dir) = folder(&s, &user, &id, Need::Read).await?;
    let encrypted = read_marker(&dir).await.is_some();
    let files = tokio::task::spawn_blocking(move || files_under(&dir)).await.map_err(NexusError::internal)?;
    let locked = files.iter().filter(|f| f.1).count();
    Ok(Json(VaultState { encrypted, locked_files: locked, open_files: files.len() - locked }))
}

#[derive(Deserialize)]
pub struct PasswordBody {
    password: String,
}

/// `POST /files/{id}/encryption/lock`: encrypts every file in the folder
/// (the first time, the password becomes the folder's password).
pub async fn lock(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Json<crate::routes::transfers::JobView>> {
    run(s, user, id, body.password, true).await
}

/// `POST /files/{id}/encryption/unlock`: decrypts them again.
pub async fn unlock(
    State(s): State<SharedState>,
    user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Json<crate::routes::transfers::JobView>> {
    run(s, user, id, body.password, false).await
}

async fn run(s: SharedState, user: AuthUser, id: String, password: String, locking: bool) -> ApiResult<Json<crate::routes::transfers::JobView>> {
    user.require_person()?;
    user.require(Action::Write)?;
    let (root, entry, dir) = folder(&s, &user, &id, Need::Change).await?;
    if root.read_only {
        return Err(NexusError::PermissionDenied("This storage is read-only".into()).into());
    }
    let key = folder_key(&dir, &password, locking).await?;
    let verb = if locking { "Encrypt" } else { "Decrypt" };
    let job = s.jobs.add("check", format!("{verb} {}", entry.filename), "This computer".into(), user.username.clone());
    let view = job.view();
    let state = s.clone();
    crate::routes::audit(&s, &user, if locking { "folder.encrypted" } else { "folder.decrypted" }, Some(&entry.relative_path)).await;
    tokio::spawn(async move {
        let Some(_slot) = state.jobs.start(&state, &job).await else { return };
        let known: Vec<String> = state
            .db
            .object_list(&root.id, &format!("{}/", entry.relative_path), "", 10_000_000)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|f| f.id)
            .collect();
        let all = {
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || files_under(&dir)).await.unwrap_or_default()
        };
        let todo: Vec<PathBuf> = all.into_iter().filter(|(_, l)| *l != locking).map(|(p, _)| p).collect();
        let mut sizes = 0u64;
        for p in &todo {
            sizes += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        }
        job.update(|i| {
            i.files_total = todo.len() as u64;
            i.bytes_total = sizes;
        });
        let mut bytes = 0u64;
        for path in todo {
            if !state.jobs.checkpoint(&state, &job).await {
                break;
            }
            let shown = path.strip_prefix(&dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            job.update(|i| i.current = Some(shown.clone()));
            let done = if locking { lock_file(&path, &key).await } else { unlock_file(&path, &key).await };
            match done {
                Ok(n) => {
                    bytes += n;
                    job.set_bytes(bytes);
                    job.update(|i| i.files_done += 1);
                }
                Err(e) => job.fail_file(&shown, &e),
            }
        }
        let _ = state.indexer.reindex_path(&root, &entry.relative_path).await;
        if locking {
            // Photo previews of what is now encrypted must not stay readable.
            for id in &known {
                crate::media::forget_previews(&state.data_dir, id);
            }
        }
        finish_job(&state, &job);
    });
    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn locks_and_unlocks_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.path().join("a.txt"), b"secret plans").unwrap();
        std::fs::write(dir.path().join("sub").join("b.bin"), &big).unwrap();
        std::fs::write(dir.path().join("empty"), b"").unwrap();

        assert!(folder_key(dir.path(), "short", true).await.is_err());
        let key = folder_key(dir.path(), "correct horse", true).await.unwrap();
        for (p, _) in files_under(dir.path()) {
            lock_file(&p, &key).await.unwrap();
        }
        assert!(!dir.path().join("a.txt").exists());
        let locked = std::fs::read(dir.path().join("a.txt.nxlocked")).unwrap();
        assert!(!locked.windows(6).any(|w| w == b"secret"), "the content is encrypted");

        assert!(folder_key(dir.path(), "wrong horse", false).await.is_err());
        let key = folder_key(dir.path(), "correct horse", false).await.unwrap();
        // A changed file is refused and left as it is.
        let mut tampered = locked.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        std::fs::write(dir.path().join("a.txt.nxlocked"), &tampered).unwrap();
        assert!(unlock_file(&dir.path().join("a.txt.nxlocked"), &key).await.is_err());
        std::fs::write(dir.path().join("a.txt.nxlocked"), &locked).unwrap();

        for (p, l) in files_under(dir.path()) {
            assert!(l);
            unlock_file(&p, &key).await.unwrap();
        }
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"secret plans");
        assert_eq!(std::fs::read(dir.path().join("sub").join("b.bin")).unwrap(), big);
        assert_eq!(std::fs::read(dir.path().join("empty")).unwrap(), b"");
    }
}
