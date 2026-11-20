//! Reading what is inside photos, videos and music: dates, cameras and
//! places (EXIF), small previews, a perceptual hash for photos that look
//! alike, music tags, and film and episode names from file names.
//!
//! A background worker reads files that are new or changed. Everything here
//! is derived from the files and read again when they change.

use crate::state::SharedState;
use image::{DynamicImage, ImageReader};
use nexus_common::kinds;
use nexus_common::time::now_ms;
use nexus_common::types::FileEntry;
use nexus_database::MediaInfo;
use std::io::{BufReader, Cursor};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Preview sizes the API serves (longest side, pixels).
pub const SIZES: &[u32] = &[256, 1024];
const BATCH: i64 = 100;
/// Files bigger than this are not decoded for previews (RAW previews come
/// from their embedded JPEG, which is read on its own).
const MAX_DECODE: u64 = 200 * 1024 * 1024;

pub fn is_photo(ext: &str) -> bool {
    kinds::IMAGE.contains(&ext) || kinds::RAW.contains(&ext)
}

/// Extensions the worker reads.
fn wanted() -> Vec<&'static str> {
    [kinds::IMAGE, kinds::RAW, kinds::VIDEO, kinds::AUDIO].concat().into_iter().filter(|e| *e != "svg" && *e != "ico").collect()
}

// ---------------------------------------------------------------------------
// EXIF

struct Exif {
    inner: exif::Exif,
}

fn ascii(f: &exif::Field) -> Option<String> {
    match &f.value {
        exif::Value::Ascii(v) => {
            let s = String::from_utf8_lossy(v.first()?).trim_end_matches('\0').trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        _ => None,
    }
}

impl Exif {
    fn read(path: &Path) -> Option<Self> {
        let file = std::fs::File::open(path).ok()?;
        let inner = exif::Reader::new().read_from_container(&mut BufReader::new(file)).ok()?;
        Some(Self { inner })
    }

    fn text(&self, tag: exif::Tag) -> Option<String> {
        ascii(self.inner.get_field(tag, exif::In::PRIMARY)?)
    }

    fn uint(&self, tag: exif::Tag, ifd: exif::In) -> Option<u32> {
        self.inner.get_field(tag, ifd)?.value.get_uint(0)
    }

    /// When the photo was taken, as local time stored as if it were UTC.
    fn taken_at(&self) -> Option<i64> {
        for tag in [exif::Tag::DateTimeOriginal, exif::Tag::DateTimeDigitized, exif::Tag::DateTime] {
            let Some(field) = self.inner.get_field(tag, exif::In::PRIMARY) else { continue };
            let exif::Value::Ascii(v) = &field.value else { continue };
            let Some(raw) = v.first() else { continue };
            let Ok(dt) = exif::DateTime::from_ascii(raw) else { continue };
            let date = chrono::NaiveDate::from_ymd_opt(dt.year as i32, dt.month as u32, dt.day as u32)?;
            let time = chrono::NaiveTime::from_hms_opt(dt.hour as u32, dt.minute as u32, dt.second as u32)?;
            if dt.year < 1900 {
                continue;
            }
            return Some(chrono::NaiveDateTime::new(date, time).and_utc().timestamp_millis());
        }
        None
    }

    fn coordinate(&self, value: exif::Tag, reference: exif::Tag) -> Option<f64> {
        let field = self.inner.get_field(value, exif::In::PRIMARY)?;
        let exif::Value::Rational(parts) = &field.value else { return None };
        if parts.len() < 3 || parts.iter().any(|r| r.denom == 0) {
            return None;
        }
        let v = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
        let negative = self
            .inner
            .get_field(reference, exif::In::PRIMARY)
            .and_then(ascii)
            .is_some_and(|r| r.starts_with('S') || r.starts_with('W'));
        Some(if negative { -v } else { v })
    }

    fn place(&self) -> Option<(f64, f64)> {
        let lat = self.coordinate(exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef)?;
        let lon = self.coordinate(exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef)?;
        let valid = (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) && !(lat == 0.0 && lon == 0.0);
        valid.then_some((lat, lon))
    }

    fn orientation(&self) -> u32 {
        self.uint(exif::Tag::Orientation, exif::In::PRIMARY).unwrap_or(1)
    }

    /// The biggest JPEG preview stored in the file (camera raw files keep
    /// one, often full size).
    fn embedded_jpeg(&self) -> Option<Vec<u8>> {
        let buf = self.inner.buf();
        let mut best: Option<&[u8]> = None;
        let mut consider = |offset: Option<u32>, len: Option<u32>| {
            let (Some(o), Some(l)) = (offset, len) else { return };
            let (o, l) = (o as usize, l as usize);
            if l < 1024 || o.checked_add(l).is_none_or(|end| end > buf.len()) {
                return;
            }
            let data = &buf[o..o + l];
            if data.starts_with(&[0xFF, 0xD8]) && best.is_none_or(|b| b.len() < data.len()) {
                best = Some(data);
            }
        };
        for ifd in [exif::In::PRIMARY, exif::In::THUMBNAIL] {
            consider(
                self.uint(exif::Tag::JPEGInterchangeFormat, ifd),
                self.uint(exif::Tag::JPEGInterchangeFormatLength, ifd),
            );
            consider(self.uint(exif::Tag::StripOffsets, ifd), self.uint(exif::Tag::StripByteCounts, ifd));
        }
        best.map(<[u8]>::to_vec)
    }
}

// ---------------------------------------------------------------------------
// Previews

fn orient(img: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Decodes a photo (or a raw photo's embedded preview), turned upright.
fn decode(path: &Path, ext: &str, exif: Option<&Exif>) -> Result<DynamicImage, String> {
    let orientation = exif.map(Exif::orientation).unwrap_or(1);
    let embedded = exif.and_then(Exif::embedded_jpeg);
    let img = if kinds::RAW.contains(&ext) || matches!(ext, "heic" | "heif" | "avif") {
        let jpeg = embedded.ok_or("This photo has no preview inside it")?;
        image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).map_err(|e| e.to_string())?
    } else {
        let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
        if size > MAX_DECODE {
            return Err("The photo is too big to preview".into());
        }
        let reader = ImageReader::open(path).map_err(|e| e.to_string())?.with_guessed_format().map_err(|e| e.to_string())?;
        let mut reader = reader;
        reader.limits(image::Limits::default());
        match reader.decode() {
            Ok(img) => img,
            // Some files the decoder cannot read still carry a preview.
            Err(e) => match embedded {
                Some(jpeg) => image::load_from_memory(&jpeg).map_err(|e| e.to_string())?,
                None => return Err(e.to_string()),
            },
        }
    };
    Ok(orient(img, orientation))
}

fn jpeg_bytes(img: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let rgb = img.to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut out), quality)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

/// A 64-bit difference hash: photos that look alike have hashes that differ
/// in few bits.
pub fn dhash(img: &DynamicImage) -> String {
    let small = img.resize_exact(9, 8, image::imageops::FilterType::Triangle).to_luma8();
    let mut bits: u64 = 0;
    for y in 0..8 {
        for x in 0..8 {
            bits <<= 1;
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                bits |= 1;
            }
        }
    }
    format!("{bits:016x}")
}

pub fn hash_distance(a: &str, b: &str) -> Option<u32> {
    Some((u64::from_str_radix(a, 16).ok()? ^ u64::from_str_radix(b, 16).ok()?).count_ones())
}

/// Where a file's preview of `size` is kept.
pub fn preview_path(data_dir: &Path, file: &FileEntry, size: u32) -> PathBuf {
    let shard = file.id.get(..2).unwrap_or("00");
    data_dir.join("thumbs").join(shard).join(format!("{}-{}-{size}.jpg", file.id, file.modified_at.unwrap_or(0)))
}

/// Removes every preview kept for a file (all sizes and versions).
pub fn forget_previews(data_dir: &Path, file_id: &str) {
    let dir = data_dir.join("thumbs").join(file_id.get(..2).unwrap_or("00"));
    let prefix = format!("{file_id}-");
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Makes the previews of a photo. Returns the hash and the decoded size.
fn make_previews(data_dir: &Path, file: &FileEntry, path: &Path, ext: &str, exif: Option<&Exif>) -> Result<(String, u32, u32), String> {
    let img = decode(path, ext, exif)?;
    let (w, h) = (img.width(), img.height());
    let hash = dhash(&img);
    let dir = preview_path(data_dir, file, 0).parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Previews of an older version of the file go.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with(&format!("{}-", file.id)) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    for &size in SIZES {
        let scaled = if w.max(h) > size { img.thumbnail(size, size) } else { img.clone() };
        let bytes = jpeg_bytes(&scaled, if size <= 256 { 78 } else { 85 })?;
        std::fs::write(preview_path(data_dir, file, size), bytes).map_err(|e| e.to_string())?;
    }
    Ok((hash, w, h))
}

/// A photo's preview of `size` (one of [`SIZES`]), made now if missing.
pub fn preview(data_dir: &Path, file: &FileEntry, path: &Path, size: u32) -> Result<PathBuf, String> {
    let size = SIZES.iter().copied().find(|s| *s >= size).unwrap_or(SIZES[SIZES.len() - 1]);
    let target = preview_path(data_dir, file, size);
    if target.exists() {
        return Ok(target);
    }
    let ext = file.extension.as_deref().unwrap_or_default().to_ascii_lowercase();
    if ext == "pdf" {
        // The first page, drawn by Windows, becomes the preview.
        let png = crate::winrt::pdf_first_page(path, SIZES[SIZES.len() - 1])?;
        let tmp = data_dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let page = tmp.join(format!("{}-page.png", file.id));
        std::fs::write(&page, png).map_err(|e| e.to_string())?;
        let made = make_previews(data_dir, file, &page, "png", None);
        let _ = std::fs::remove_file(&page);
        made?;
        return Ok(target);
    }
    let exif = Exif::read(path);
    make_previews(data_dir, file, path, &ext, exif.as_ref())?;
    Ok(target)
}

/// Whether [`preview`] can draw this kind of file.
pub fn has_preview(ext: &str) -> bool {
    is_photo(ext) || (cfg!(windows) && ext == "pdf")
}

// ---------------------------------------------------------------------------
// Films, episodes and music

/// Reads "Show.Name.S01E02…" or "Show Name - 1x02" as an episode, and
/// "Film Name (2019)" or "Film.Name.2019.1080p" as a film with its year.
pub fn parse_video_name(filename: &str) -> (Option<String>, Option<i64>, Option<i64>, Option<i64>, &'static str) {
    let stem = filename.rsplit_once('.').map(|(s, _)| s).unwrap_or(filename);
    let clean = |s: &str| {
        let s = s.replace(['.', '_'], " ");
        let s = s.trim().trim_end_matches(['-', ' ', '(', '[']).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    let lower = stem.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    // SxxEyy
    for i in 0..bytes.len().saturating_sub(3) {
        if bytes[i] == b's' && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let rest = &lower[i + 1..];
            let season: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let after = &rest[season.len()..];
            if !season.is_empty() && season.len() <= 2 && after.starts_with('e') {
                let episode: String = after[1..].chars().take_while(char::is_ascii_digit).collect();
                if !episode.is_empty() && episode.len() <= 3 {
                    return (clean(&stem[..i]), None, season.parse().ok(), episode.parse().ok(), "episode");
                }
            }
        }
    }
    // 1x02
    for (i, _) in lower.match_indices('x') {
        let before: String = lower[..i].chars().rev().take_while(char::is_ascii_digit).collect::<String>().chars().rev().collect();
        let after: String = lower[i + 1..].chars().take_while(char::is_ascii_digit).collect();
        let start = i - before.len();
        if (1..=2).contains(&before.len())
            && (2..=3).contains(&after.len())
            && (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
        {
            return (clean(&stem[..start]), None, before.parse().ok(), after.parse().ok(), "episode");
        }
    }
    // A year: "(2019)" or ".2019."
    let chars: Vec<char> = stem.chars().collect();
    for i in 0..chars.len().saturating_sub(3) {
        let year: String = chars[i..i + 4].iter().collect();
        let boundary_before = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let boundary_after = i + 4 == chars.len() || !chars[i + 4].is_ascii_alphanumeric();
        if i > 0
            && boundary_before
            && boundary_after
            && year.chars().all(|c| c.is_ascii_digit())
            && (1900..=2100).contains(&year.parse::<i64>().unwrap_or(0))
        {
            let title: String = chars[..i].iter().collect();
            return (clean(&title), year.parse().ok(), None, None, "film");
        }
    }
    (None, None, None, None, "home_video")
}

fn read_tags(path: &Path, m: &mut MediaInfo) {
    use lofty::prelude::*;
    let Ok(tagged) = lofty::read_from_path(path) else { return };
    let duration = tagged.properties().duration().as_millis() as i64;
    if duration > 0 {
        m.duration_ms = Some(duration);
    }
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else { return };
    let text = |v: Option<std::borrow::Cow<'_, str>>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    m.title = text(tag.title());
    m.artist = text(tag.artist());
    m.album = text(tag.album());
    m.genre = text(tag.genre());
    m.album_artist = tag.get_string(&lofty::tag::ItemKey::AlbumArtist).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    m.track = tag.track().map(i64::from);
    m.disc = tag.disk().map(i64::from);
    m.year = tag.year().map(i64::from).filter(|y| (1000..3000).contains(y));
}

/// Reads one file. Never fails: problems are kept in `error`.
pub fn read_file(data_dir: &Path, file: &FileEntry, path: &Path) -> MediaInfo {
    let ext = file.extension.as_deref().unwrap_or_default().to_ascii_lowercase();
    let mut m = MediaInfo {
        file_id: file.id.clone(),
        root_id: file.root_id.clone(),
        modified_at: file.modified_at,
        read_at: now_ms(),
        ..Default::default()
    };
    if is_photo(&ext) {
        m.kind = "photo".into();
        let exif = Exif::read(path);
        if let Some(x) = &exif {
            m.taken_at = x.taken_at();
            let make = x.text(exif::Tag::Make);
            let model = x.text(exif::Tag::Model);
            m.camera = match (make, model) {
                (Some(make), Some(model)) if model.to_lowercase().starts_with(&make.to_lowercase().split(' ').next().unwrap_or("").to_string()) => Some(model),
                (Some(make), Some(model)) => Some(format!("{make} {model}")),
                (a, b) => a.or(b),
            };
            m.lens = x.text(exif::Tag::LensModel);
            if let Some((lat, lon)) = x.place() {
                m.latitude = Some(lat);
                m.longitude = Some(lon);
            }
        }
        match make_previews(data_dir, file, path, &ext, exif.as_ref()) {
            Ok((hash, w, h)) => {
                m.phash = Some(hash);
                m.width = Some(w as i64);
                m.height = Some(h as i64);
                // Faces, with Windows' built-in face finder (nothing is
                // downloaded; elsewhere this stays unknown).
                if let Ok(jpeg) = std::fs::read(preview_path(data_dir, file, 1024)) {
                    m.faces = crate::winrt::count_faces(&jpeg).ok().map(i64::from);
                }
            }
            Err(e) => m.error = Some(e),
        }
    } else if kinds::VIDEO.contains(&ext.as_str()) {
        m.kind = "video".into();
        read_tags(path, &mut m);
        let (name, year, season, episode, kind) = parse_video_name(&file.filename);
        m.media_type = Some(kind.into());
        match kind {
            "episode" => {
                m.show_name = name.or_else(|| folder_name(file));
                m.season = season;
                m.episode = episode;
            }
            "film" => {
                m.title = name;
                m.year = year;
            }
            _ => {}
        }
    } else if kinds::AUDIO.contains(&ext.as_str()) {
        m.kind = "music".into();
        m.media_type = Some("music".into());
        read_tags(path, &mut m);
    } else {
        m.kind = "other".into();
    }
    if m.taken_at.is_none() && m.kind != "music" {
        m.taken_at = file.modified_at;
    }
    m
}

fn folder_name(file: &FileEntry) -> Option<String> {
    let parent = file.parent_path.trim_end_matches('/');
    let name = parent.rsplit('/').next()?.trim();
    // "Season 1" folders: the show is the folder above.
    if name.to_ascii_lowercase().starts_with("season") {
        let above = parent.rsplit_once('/')?.0;
        return above.rsplit('/').next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    }
    (!name.is_empty()).then(|| name.to_string())
}

// ---------------------------------------------------------------------------
// The worker

/// Reads new and changed photos, videos and music in the background.
pub fn spawn(state: SharedState) {
    if std::env::var_os("NEXUS_NO_MEDIA").is_some() {
        return;
    }
    tokio::spawn(async move {
        let mut events = state.events.subscribe();
        loop {
            let done = run_batch(&state).await;
            if done >= BATCH as usize {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
            // Wait for files to change, then let them settle.
            let wait = async {
                loop {
                    match events.recv().await {
                        Ok(nexus_common::events::Event::FileCreated { .. })
                        | Ok(nexus_common::events::Event::FileModified { .. })
                        | Ok(nexus_common::events::Event::ScanCompleted { .. }) => break,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                        _ => {}
                    }
                }
            };
            tokio::select! {
                _ = wait => tokio::time::sleep(Duration::from_secs(5)).await,
                _ = tokio::time::sleep(Duration::from_secs(600)) => {}
            }
        }
    });
}

async fn run_batch(state: &SharedState) -> usize {
    let todo = match state.db.media_todo(&wanted(), BATCH).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "could not list files to read");
            return 0;
        }
    };
    let n = todo.len();
    for file in todo {
        let Ok(root) = state.db.get_root(&file.root_id).await else { continue };
        if state.storage.ensure_online(&root).is_err() {
            continue;
        }
        let Ok(path) = state.storage.resolve(&root, &file.relative_path) else { continue };
        let data_dir = state.data_dir.clone();
        let f = file.clone();
        let info = match tokio::task::spawn_blocking(move || read_file(&data_dir, &f, &path)).await {
            Ok(info) => info,
            Err(_) => MediaInfo {
                file_id: file.id.clone(),
                root_id: file.root_id.clone(),
                modified_at: file.modified_at,
                kind: "other".into(),
                read_at: now_ms(),
                error: Some("Reading the file failed".into()),
                ..Default::default()
            },
        };
        if let Err(e) = state.db.save_media(&info).await {
            tracing::debug!(error = %e, file = %file.relative_path, "could not save media details");
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_episode_and_film_names() {
        let (show, _, s, e, kind) = parse_video_name("The.Office.US.S02E05.720p.mkv");
        assert_eq!((show.as_deref(), s, e, kind), (Some("The Office US"), Some(2), Some(5), "episode"));
        let (show, _, s, e, _) = parse_video_name("Lost - 1x04 - Walkabout.avi");
        assert_eq!((show.as_deref(), s, e), (Some("Lost"), Some(1), Some(4)));
        let (title, year, _, _, kind) = parse_video_name("Blade Runner (1982).mp4");
        assert_eq!((title.as_deref(), year, kind), (Some("Blade Runner"), Some(1982), "film"));
        let (title, year, _, _, _) = parse_video_name("Arrival.2016.1080p.BluRay.mkv");
        assert_eq!((title.as_deref(), year), (Some("Arrival"), Some(2016)));
        assert_eq!(parse_video_name("VID_20230101_120000.mp4").4, "home_video");
    }

    #[test]
    fn photos_that_look_alike_hash_close() {
        let mut a = image::RgbImage::new(64, 48);
        for (x, y, p) in a.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 4) as u8, (y * 5) as u8, 90]);
        }
        let b = DynamicImage::ImageRgb8(a.clone()).brighten(10);
        let a = DynamicImage::ImageRgb8(a);
        let flipped = a.fliph();
        assert!(hash_distance(&dhash(&a), &dhash(&b)).unwrap() <= 4);
        assert!(hash_distance(&dhash(&a), &dhash(&flipped)).unwrap() > 20);
    }

    #[test]
    fn makes_previews_of_a_photo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.png");
        image::RgbImage::from_pixel(2000, 1000, image::Rgb([200, 10, 10])).save(&path).unwrap();
        let file = FileEntry {
            id: "abcdef".into(),
            root_id: "r".into(),
            relative_path: "p.png".into(),
            parent_path: String::new(),
            filename: "p.png".into(),
            is_dir: false,
            extension: Some("png".into()),
            mime_type: None,
            size: 1,
            created_at: None,
            modified_at: Some(5),
            content_hash: None,
            indexed_at: 0,
            availability: nexus_common::types::Availability::Online,
        };
        let info = read_file(dir.path(), &file, &path);
        assert_eq!(info.kind, "photo");
        assert_eq!((info.width, info.height), (Some(2000), Some(1000)));
        assert!(info.phash.is_some(), "{:?}", info.error);
        let small = image::open(preview_path(dir.path(), &file, 256)).unwrap();
        assert_eq!(small.width(), 256);
    }
}
