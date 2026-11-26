//! The plugins that come with HearthDrive. Each works only through its
//! [`Sandbox`], so it can do no more than it was allowed to.

use super::{Access, ChoiceOption, CommandDef, Plugin, Report, SettingDef, SettingKind, Subject, Target, Values};
use super::sandbox::{Item, Sandbox};
use nexus_common::{NexusError, Result, kinds};
use std::io::Read;
use std::time::SystemTime;

pub fn all() -> Vec<Plugin> {
    vec![web_copies(), no_location(), contact_sheet(), rename_by_date(), document_text(), checksums(), folder_list()]
}

/// Photos HearthDrive can open (raw and HEIC through the preview inside them).
fn photo_exts() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = kinds::IMAGE.iter().copied().filter(|e| !matches!(*e, "svg" | "ico")).collect();
    v.extend_from_slice(kinds::RAW);
    v
}

const JPEG: &[&str] = &["jpg", "jpeg", "jfif"];
const DOCUMENTS: &[&str] = &["pdf", "docx", "odt", "pptx", "odp", "xlsx", "ods", "epub", "rtf"];

/// Folders plugins put their results in; never read as input.
const OUTPUT_FOLDERS: &[&str] = &["Web-sized", "Without location", "Text"];

fn file(id: &'static str, label: &'static str, description: &'static str, extensions: Vec<&'static str>) -> CommandDef {
    CommandDef { id, label, description, target: Target::File, extensions }
}

fn folder(id: &'static str, label: &'static str, description: &'static str) -> CommandDef {
    CommandDef { id, label, description, target: Target::Folder, extensions: Vec::new() }
}

fn toggle(key: &'static str, label: &'static str, help: &'static str, default: bool) -> SettingDef {
    SettingDef { key, label, help, kind: SettingKind::Toggle { default } }
}

fn subfolders_setting(help: &'static str, default: bool) -> SettingDef {
    toggle("subfolders", "Include folders inside it", help, default)
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn local_time(t: SystemTime) -> chrono::NaiveDateTime {
    chrono::DateTime::<chrono::Local>::from(t).naive_local()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn in_output_folder(rel: &str) -> bool {
    rel.split('/').rev().skip(1).any(|part| OUTPUT_FOLDERS.iter().any(|o| o.eq_ignore_ascii_case(part)))
}

/// The files a command works on: the file itself, or the files of these
/// kinds in the folder (and its folders, if asked), leaving out what
/// plugins made.
fn inputs(sb: &Sandbox, subject: &Subject, exts: &[&str], subfolders: bool) -> Result<Vec<Item>> {
    if !subject.is_dir {
        let path = sb.path(&subject.rel)?;
        let meta = std::fs::metadata(&path)?;
        return Ok(vec![Item { rel: subject.rel.clone(), name: subject.name.clone(), is_dir: false, size: meta.len(), modified: meta.modified().ok() }]);
    }
    Ok(sb
        .list(&subject.rel, subfolders)?
        .into_iter()
        .filter(|i| !i.is_dir && !in_output_folder(&i.rel) && (exts.is_empty() || exts.contains(&i.ext().as_str())))
        .collect())
}

fn parent(rel: &str) -> String {
    nexus_storage::paths::split_parent(rel).0
}

fn stopped(report: &mut Report, left: usize) {
    report.stopped_early = true;
    report.say(format!("Stopped after 4 minutes with {} still to do. Run it again to carry on: finished ones are skipped.", plural(left, "file", "files")));
}

// ---------------------------------------------------------------------------
// Web-sized photos

fn web_copies() -> Plugin {
    Plugin {
        id: "web-copies",
        name: "Web-sized photos",
        summary: "Smaller copies of photos for sending or posting",
        description: "Makes JPEG copies of photos at a size that is quick to send, post or put on a website, in a Web-sized folder beside them. The copies carry no location or camera details. The originals are not changed.",
        category: "Photos",
        version: "1.0",
        needs: vec![Access::Read, Access::Add],
        commands: vec![
            file("copy", "Make a web-sized copy", "A smaller JPEG in Web-sized", photo_exts()),
            folder("copy-folder", "Make web-sized copies", "Of every photo in this folder, in Web-sized"),
        ],
        settings: vec![
            SettingDef {
                key: "size",
                label: "Longest side",
                help: "Photos smaller than this keep their size.",
                kind: SettingKind::Choice {
                    options: vec![
                        ChoiceOption { value: "1280", label: "1280 pixels: email" },
                        ChoiceOption { value: "2048", label: "2048 pixels: websites and social media" },
                        ChoiceOption { value: "3840", label: "3840 pixels: 4K screens" },
                    ],
                    default: "2048",
                },
            },
            SettingDef { key: "quality", label: "JPEG quality", help: "Lower makes smaller files.", kind: SettingKind::Number { min: 50, max: 95, default: 85, unit: "%" } },
            subfolders_setting("Copies go in a Web-sized folder in each one.", false),
        ],
        run: run_web_copies,
    }
}

fn run_web_copies(sb: &mut Sandbox, subject: &Subject, v: &Values, _cmd: &str) -> Result<Report> {
    let size: u32 = v.choice("size").parse().unwrap_or(2048);
    let quality = v.number("quality").clamp(50, 95) as u8;
    let exts = photo_exts();
    let photos = inputs(sb, subject, &exts, v.on("subfolders"))?;
    let mut report = Report::default();
    let (mut made, mut skipped, mut failed) = (0, 0, Vec::new());
    for (i, p) in photos.iter().enumerate() {
        if sb.out_of_time() {
            stopped(&mut report, photos.len() - i);
            break;
        }
        let out_folder = nexus_storage::paths::join(&parent(&p.rel), "Web-sized");
        let out_name = format!("{}.jpg", p.stem());
        if sb.exists(&nexus_storage::paths::join(&out_folder, &out_name)) {
            skipped += 1;
            continue;
        }
        let made_one = (|| -> std::result::Result<Vec<u8>, String> {
            let img = crate::media::decode_photo(&sb.path(&p.rel).map_err(|e| e.to_string())?, &p.ext())?;
            let img = if img.width().max(img.height()) > size { img.resize(size, size, image::imageops::FilterType::Lanczos3) } else { img };
            crate::media::jpeg_bytes(&img, quality)
        })();
        match made_one {
            Ok(bytes) => {
                sb.add_new(&out_folder, &out_name, &bytes)?;
                made += 1;
            }
            Err(e) => failed.push(format!("{}: {e}", p.name)),
        }
    }
    if photos.is_empty() {
        report.say("There are no photos here.");
    }
    if made > 0 {
        report.lines.insert(0, format!("Made {} in Web-sized.", plural(made, "copy", "copies")));
    }
    if skipped > 0 {
        report.say(format!("{} already had one.", plural(skipped, "photo", "photos")));
    }
    report.problems = !failed.is_empty();
    report.list(format!("Could not read {}:", plural(failed.len(), "photo", "photos")), &failed);
    Ok(report)
}

// ---------------------------------------------------------------------------
// Remove location

fn no_location() -> Plugin {
    Plugin {
        id: "no-location",
        name: "Remove location from photos",
        summary: "Copies of JPEG photos without where they were taken",
        description: "Makes copies of JPEG photos without the place they were taken, the camera and other hidden details, in a Without location folder beside them. The picture itself is copied exactly, without compressing it again.",
        category: "Photos",
        version: "1.0",
        needs: vec![Access::Read, Access::Add],
        commands: vec![
            file("strip", "Copy without location", "Into Without location", JPEG.to_vec()),
            folder("strip-folder", "Copy photos without location", "Every JPEG here, into Without location"),
        ],
        settings: vec![
            toggle("keep_date", "Keep the date taken", "So the copies still sort by date in photo apps.", true),
            subfolders_setting("Copies go in a Without location folder in each one.", false),
        ],
        run: run_no_location,
    }
}

fn run_no_location(sb: &mut Sandbox, subject: &Subject, v: &Values, _cmd: &str) -> Result<Report> {
    let photos = inputs(sb, subject, JPEG, v.on("subfolders"))?;
    let mut report = Report::default();
    let (mut made, mut skipped, mut failed) = (0, 0, Vec::new());
    for (i, p) in photos.iter().enumerate() {
        if sb.out_of_time() {
            stopped(&mut report, photos.len() - i);
            break;
        }
        let out_folder = nexus_storage::paths::join(&parent(&p.rel), "Without location");
        if sb.exists(&nexus_storage::paths::join(&out_folder, &p.name)) {
            skipped += 1;
            continue;
        }
        match sb.read(&p.rel).map_err(|e| e.to_string()).and_then(|b| strip_jpeg(&b, v.on("keep_date"))) {
            Ok(bytes) => {
                sb.add_new(&out_folder, &p.name, &bytes)?;
                made += 1;
            }
            Err(e) => failed.push(format!("{}: {e}", p.name)),
        }
    }
    if photos.is_empty() {
        report.say("There are no JPEG photos here.");
    }
    if made > 0 {
        report.lines.insert(0, format!("Made {} in Without location.", plural(made, "copy", "copies")));
    }
    if skipped > 0 {
        report.say(format!("{} already had one.", plural(skipped, "photo", "photos")));
    }
    report.problems = !failed.is_empty();
    report.list(format!("Could not copy {}:", plural(failed.len(), "photo", "photos")), &failed);
    Ok(report)
}

/// A small EXIF block with only the orientation and, if given, the date
/// ("YYYY:MM:DD HH:MM:SS").
fn small_exif(orientation: u16, date: Option<&str>) -> Vec<u8> {
    let entries = 1 + date.is_some() as u16;
    let mut tiff = b"MM\0\x2A\0\0\0\x08".to_vec();
    tiff.extend_from_slice(&entries.to_be_bytes());
    // Orientation: SHORT, one value.
    tiff.extend_from_slice(&[0x01, 0x12, 0x00, 0x03, 0, 0, 0, 1]);
    tiff.extend_from_slice(&orientation.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    let data_at = 8 + 2 + entries as u32 * 12 + 4;
    if date.is_some() {
        // DateTime: ASCII, 20 bytes, after the entries.
        tiff.extend_from_slice(&[0x01, 0x32, 0x00, 0x02, 0, 0, 0, 20]);
        tiff.extend_from_slice(&data_at.to_be_bytes());
    }
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    if let Some(d) = date {
        let mut b = d.as_bytes()[..19.min(d.len())].to_vec();
        b.resize(19, b' ');
        b.push(0);
        tiff.extend_from_slice(&b);
    }
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&tiff);
    seg
}

/// The same JPEG without EXIF, XMP and IPTC (where GPS positions, camera
/// serial numbers and the like are kept). The picture data is untouched;
/// the orientation (and the date taken, if `keep_date`) is written back so
/// the photo still shows the right way up.
pub(crate) fn strip_jpeg(data: &[u8], keep_date: bool) -> std::result::Result<Vec<u8>, String> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err("Not a JPEG photo".into());
    }
    let (orientation, date) = match exif::Reader::new().read_from_container(&mut std::io::Cursor::new(data)) {
        Ok(x) => {
            let o = x.get_field(exif::Tag::Orientation, exif::In::PRIMARY).and_then(|f| f.value.get_uint(0)).unwrap_or(1) as u16;
            let d = [exif::Tag::DateTimeOriginal, exif::Tag::DateTime].iter().find_map(|t| match &x.get_field(*t, exif::In::PRIMARY)?.value {
                exif::Value::Ascii(v) => v.first().map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_string()).filter(|s| s.len() >= 19),
                _ => None,
            });
            (o, d)
        }
        Err(_) => (1, None),
    };
    let mut out = vec![0xFF, 0xD8];
    let mut i = 2;
    let mut exif_written = false;
    let write_exif = |out: &mut Vec<u8>| out.extend(small_exif(orientation, if keep_date { date.as_deref() } else { None }));
    loop {
        // Fill bytes before a marker.
        while i < data.len() && data[i] == 0xFF && data.get(i + 1) == Some(&0xFF) {
            i += 1;
        }
        if i + 1 >= data.len() || data[i] != 0xFF {
            return Err("The photo is damaged".into());
        }
        let marker = data[i + 1];
        if marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            out.extend_from_slice(&data[i..i + 2]);
            i += 2;
            if marker == 0xD9 {
                break;
            }
            continue;
        }
        if i + 4 > data.len() {
            return Err("The photo is damaged".into());
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let end = i + 2 + len;
        if len < 2 || end > data.len() {
            return Err("The photo is damaged".into());
        }
        let is_app0 = marker == 0xE0;
        if !is_app0 && !exif_written {
            write_exif(&mut out);
            exif_written = true;
        }
        if marker == 0xDA {
            // Start of the picture data: the rest is copied as it is.
            out.extend_from_slice(&data[i..]);
            break;
        }
        // APP1 (EXIF, XMP) and APP13 (IPTC) go; everything else stays.
        if marker != 0xE1 && marker != 0xED {
            out.extend_from_slice(&data[i..end]);
        }
        i = end;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Contact sheet

fn contact_sheet() -> Plugin {
    Plugin {
        id: "contact-sheet",
        name: "Contact sheet",
        summary: "Every photo in a folder on one picture",
        description: "Puts small pictures of the photos in a folder side by side on one JPEG, to see what a folder holds at a glance, send an overview or print it. It is saved in the folder as Contact sheet.jpg.",
        category: "Photos",
        version: "1.0",
        needs: vec![Access::Read, Access::Add],
        commands: vec![folder("make", "Make a contact sheet", "One picture of every photo here")],
        settings: vec![
            SettingDef { key: "columns", label: "Photos across", help: "", kind: SettingKind::Number { min: 2, max: 12, default: 6, unit: "" } },
            SettingDef {
                key: "tile",
                label: "Size of each photo",
                help: "",
                kind: SettingKind::Choice {
                    options: vec![
                        ChoiceOption { value: "200", label: "Small (200 pixels)" },
                        ChoiceOption { value: "320", label: "Medium (320 pixels)" },
                        ChoiceOption { value: "480", label: "Large (480 pixels)" },
                    ],
                    default: "320",
                },
            },
        ],
        run: run_contact_sheet,
    }
}

const SHEET_MAX: usize = 400;
const SHEET_MAX_HEIGHT: u32 = 20_000;

fn run_contact_sheet(sb: &mut Sandbox, subject: &Subject, v: &Values, _cmd: &str) -> Result<Report> {
    let columns = v.number("columns").clamp(2, 12) as u32;
    let tile: u32 = v.choice("tile").parse().unwrap_or(320);
    let gap = (tile / 20).max(6);
    let cell = tile + gap;
    let max_rows = (SHEET_MAX_HEIGHT - gap) / cell;
    let limit = SHEET_MAX.min((max_rows * columns) as usize);
    let exts = photo_exts();
    let photos = inputs(sb, subject, &exts, false)?;
    let mut report = Report::default();
    if photos.is_empty() {
        report.say("There are no photos in this folder.");
        return Ok(report);
    }
    let mut thumbs = Vec::new();
    let mut failed = Vec::new();
    for p in photos.iter().take(limit) {
        if sb.out_of_time() {
            report.stopped_early = true;
            break;
        }
        match crate::media::decode_photo(&sb.path(&p.rel)?, &p.ext()) {
            Ok(img) => thumbs.push(img.thumbnail(tile, tile).to_rgb8()),
            Err(e) => failed.push(format!("{}: {e}", p.name)),
        }
    }
    if thumbs.is_empty() {
        report.problems = true;
        report.list("None of the photos could be read:".into(), &failed);
        return Ok(report);
    }
    let cols = columns.min(thumbs.len() as u32);
    let rows = (thumbs.len() as u32).div_ceil(cols);
    let mut sheet = image::RgbImage::from_pixel(cols * cell + gap, rows * cell + gap, image::Rgb([255, 255, 255]));
    for (n, t) in thumbs.iter().enumerate() {
        let (c, r) = (n as u32 % cols, n as u32 / cols);
        let x = gap + c * cell + (tile - t.width()) / 2;
        let y = gap + r * cell + (tile - t.height()) / 2;
        image::imageops::overlay(&mut sheet, t, x as i64, y as i64);
    }
    let bytes = crate::media::jpeg_bytes(&image::DynamicImage::ImageRgb8(sheet), 88).map_err(NexusError::Internal)?;
    let rel = sb.add(&subject.folder, "Contact sheet.jpg", &bytes)?;
    report.say(format!("Made {} with {}.", rel.rsplit('/').next().unwrap_or(&rel), plural(thumbs.len(), "photo", "photos")));
    if photos.len() > thumbs.len() + failed.len() {
        report.say(format!("It has the first {} of the {} photos here.", thumbs.len() + failed.len(), photos.len()));
    }
    report.problems = !failed.is_empty();
    report.list(format!("Left out {}:", plural(failed.len(), "photo", "photos")), &failed);
    Ok(report)
}

// ---------------------------------------------------------------------------
// Rename by date taken

fn rename_by_date() -> Plugin {
    Plugin {
        id: "rename-by-date",
        name: "Rename photos by date taken",
        summary: "Names like 2024-05-01 14.32.10.jpg, so photos sort in order",
        description: "Renames photos to the date and time they were taken, read from the photo itself, so they sort in the order they were taken in any folder or program, whichever camera or phone took them. Tags and comments on them stay.",
        category: "Photos",
        version: "1.0",
        needs: vec![Access::Read, Access::Rename],
        commands: vec![
            file("rename", "Rename by date taken", "To when it was taken", photo_exts()),
            folder("rename-folder", "Rename photos by date taken", "Every photo in this folder"),
        ],
        settings: vec![
            SettingDef {
                key: "format",
                label: "Names look like",
                help: "",
                kind: SettingKind::Choice {
                    options: vec![
                        ChoiceOption { value: "spaced", label: "2024-05-01 14.32.10" },
                        ChoiceOption { value: "compact", label: "20240501_143210" },
                    ],
                    default: "spaced",
                },
            },
            toggle("keep_name", "Keep the old name after the date", "For example 2024-05-01 14.32.10 IMG_4410.jpg", false),
            toggle("fallback", "Use the date last changed when a photo has no date inside", "Otherwise those photos keep their names.", false),
            subfolders_setting("", false),
        ],
        run: run_rename_by_date,
    }
}

fn date_name(taken: chrono::NaiveDateTime, format: &str) -> String {
    taken.format(if format == "compact" { "%Y%m%d_%H%M%S" } else { "%Y-%m-%d %H.%M.%S" }).to_string()
}

fn run_rename_by_date(sb: &mut Sandbox, subject: &Subject, v: &Values, _cmd: &str) -> Result<Report> {
    let exts = photo_exts();
    let photos = inputs(sb, subject, &exts, v.on("subfolders"))?;
    let mut report = Report::default();
    let (mut renamed, mut already, mut undated, mut failed) = (0, 0, Vec::new(), Vec::new());
    for (i, p) in photos.iter().enumerate() {
        if sb.out_of_time() {
            stopped(&mut report, photos.len() - i);
            break;
        }
        let taken = crate::media::photo_taken_at(&sb.path(&p.rel)?)
            .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms).map(|d| d.naive_utc()))
            .or_else(|| v.on("fallback").then_some(p.modified).flatten().map(local_time));
        let Some(taken) = taken else {
            undated.push(p.name.clone());
            continue;
        };
        let date = date_name(taken, v.choice("format"));
        if p.name.starts_with(&date) {
            already += 1;
            continue;
        }
        let ext = p.name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        let base = if v.on("keep_name") { format!("{date} {}", p.stem()) } else { date };
        let mut done = None;
        for n in 1..100 {
            let name = if n == 1 { format!("{base}.{ext}") } else { format!("{base} ({n}).{ext}") };
            match sb.rename(&p.rel, &name) {
                Ok(_) => {
                    done = Some(Ok(()));
                    break;
                }
                Err(NexusError::Conflict(_)) => continue,
                Err(e) => {
                    done = Some(Err(e));
                    break;
                }
            }
        }
        match done {
            Some(Ok(())) => renamed += 1,
            Some(Err(e)) => failed.push(format!("{}: {e}", p.name)),
            None => failed.push(format!("{}: too many photos taken at the same second", p.name)),
        }
    }
    if photos.is_empty() {
        report.say("There are no photos here.");
    }
    if renamed > 0 || !photos.is_empty() {
        report.lines.insert(0, format!("Renamed {}.", plural(renamed, "photo", "photos")));
    }
    if already > 0 {
        report.say(format!("{} already had the date as its name.", plural(already, "photo", "photos")));
    }
    report.list(format!("{} have no date inside, so they kept their names:", plural(undated.len(), "photo", "photos")), &undated);
    report.problems = !failed.is_empty();
    report.list(format!("Could not rename {}:", plural(failed.len(), "photo", "photos")), &failed);
    Ok(report)
}

// ---------------------------------------------------------------------------
// Text from documents

fn document_text() -> Plugin {
    Plugin {
        id: "document-text",
        name: "Text from documents",
        summary: "The words in PDFs and Office files as plain text",
        description: "Saves the text of PDFs, Word, PowerPoint, Excel and OpenDocument files as .txt files in a Text folder beside them, to copy from, search or read on any device. On Windows, scanned PDFs are read with Windows' own text recognition.",
        category: "Documents",
        version: "1.0",
        needs: vec![Access::Read, Access::Add],
        commands: vec![
            file("text", "Save the text", "As a .txt file in Text", DOCUMENTS.to_vec()),
            folder("text-folder", "Save the text of documents", "Of every document here, in Text"),
        ],
        settings: vec![subfolders_setting("Text files go in a Text folder in each one.", false)],
        run: run_document_text,
    }
}

fn run_document_text(sb: &mut Sandbox, subject: &Subject, v: &Values, _cmd: &str) -> Result<Report> {
    let docs = inputs(sb, subject, DOCUMENTS, v.on("subfolders"))?;
    let mut report = Report::default();
    let (mut made, mut skipped, mut empty, mut failed) = (0, 0, Vec::new(), Vec::new());
    for (i, d) in docs.iter().enumerate() {
        if sb.out_of_time() {
            stopped(&mut report, docs.len() - i);
            break;
        }
        let out_folder = nexus_storage::paths::join(&parent(&d.rel), "Text");
        let out_name = format!("{}.txt", d.stem());
        if sb.exists(&nexus_storage::paths::join(&out_folder, &out_name)) {
            skipped += 1;
            continue;
        }
        let (_, text) = crate::documents::extract(&sb.path(&d.rel)?, &d.ext(), d.size);
        match text {
            Ok(t) if t.trim().is_empty() => empty.push(d.name.clone()),
            Ok(t) => {
                sb.add_new(&out_folder, &out_name, t.trim().as_bytes())?;
                made += 1;
            }
            Err(e) => failed.push(format!("{}: {e}", d.name)),
        }
    }
    if docs.is_empty() {
        report.say("There are no documents here.");
    }
    if made > 0 {
        report.lines.insert(0, format!("Saved the text of {} in Text.", plural(made, "document", "documents")));
    }
    if skipped > 0 {
        report.say(format!("{} already had a text file.", plural(skipped, "document", "documents")));
    }
    report.list(format!("{} have no text in them:", plural(empty.len(), "document", "documents")), &empty);
    report.problems = !failed.is_empty();
    report.list(format!("Could not read {}:", plural(failed.len(), "document", "documents")), &failed);
    Ok(report)
}

// ---------------------------------------------------------------------------
// Checksums

fn checksums() -> Plugin {
    Plugin {
        id: "checksums",
        name: "Checksums",
        summary: "Find files that changed or went missing",
        description: "Makes a list of a SHA-256 checksum for every file in a folder, and later checks the files against it, to find any that changed, were damaged or went missing, for example after copying them to another drive. The lists also work with sha256sum on Linux and macOS.",
        category: "Storage",
        version: "1.0",
        needs: vec![Access::Read, Access::Add],
        commands: vec![
            folder("write", "Make a checksum list", "Of every file in this folder"),
            file("check", "Check files against this list", "Find changed and missing files", vec!["sha256"]),
        ],
        settings: vec![subfolders_setting("", true)],
        run: run_checksums,
    }
}

fn sha256(sb: &Sandbox, rel: &str) -> Result<String> {
    let mut f = std::fs::File::open(sb.path(rel)?)?;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ctx.update(&buf[..n]);
    }
    Ok(ctx.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

fn relative_to<'a>(rel: &'a str, folder: &str) -> &'a str {
    if folder.is_empty() { rel } else { rel.strip_prefix(folder).map(|r| r.trim_start_matches('/')).unwrap_or(rel) }
}

fn run_checksums(sb: &mut Sandbox, subject: &Subject, v: &Values, cmd: &str) -> Result<Report> {
    let mut report = Report::default();
    if cmd == "write" {
        let files: Vec<Item> = sb.list(&subject.rel, v.on("subfolders"))?.into_iter().filter(|i| !i.is_dir && i.ext() != "sha256").collect();
        if files.is_empty() {
            report.say("There are no files in this folder.");
            return Ok(report);
        }
        let mut out = String::new();
        let mut failed = Vec::new();
        for (i, f) in files.iter().enumerate() {
            if sb.out_of_time() {
                report.stopped_early = true;
                report.problems = true;
                report.say(format!(
                    "Stopped after 4 minutes, at {} of {}. No list was saved: try a smaller folder, or turn off Include folders inside it.",
                    i,
                    plural(files.len(), "file", "files")
                ));
                return Ok(report);
            }
            match sha256(sb, &f.rel) {
                Ok(h) => out.push_str(&format!("{h}  {}\n", relative_to(&f.rel, &subject.rel))),
                Err(e) => failed.push(format!("{}: {e}", relative_to(&f.rel, &subject.rel))),
            }
        }
        let rel = sb.add(&subject.folder, &format!("Checksums {}.sha256", today()), out.as_bytes())?;
        report.say(format!("Saved checksums of {} in {}.", plural(files.len() - failed.len(), "file", "files"), rel.rsplit('/').next().unwrap_or(&rel)));
        report.say("Later, open the puzzle menu on that file and choose Check files against this list.");
        report.problems = !failed.is_empty();
        report.list(format!("Could not read {}:", plural(failed.len(), "file", "files")), &failed);
        return Ok(report);
    }

    // Check a list.
    let text = String::from_utf8_lossy(&sb.read(&subject.rel)?).into_owned();
    let base = subject.folder.clone();
    let (mut matched, mut changed, mut missing, mut unreadable) = (0, Vec::new(), Vec::new(), Vec::new());
    let mut listed = std::collections::HashSet::new();
    let mut entries = 0;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.len() < 66 || line.starts_with('#') {
            continue;
        }
        let (hash, rest) = line.split_at(64);
        let name = rest.strip_prefix("  ").or_else(|| rest.strip_prefix(" *")).unwrap_or("").replace('\\', "/");
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) || name.is_empty() {
            continue;
        }
        entries += 1;
        if sb.out_of_time() {
            report.stopped_early = true;
            report.say("Stopped after 4 minutes: not every file was checked.");
            break;
        }
        let rel = match nexus_storage::paths::normalize_relative(&nexus_storage::paths::join(&base, &name)) {
            Ok(r) => r,
            Err(_) => {
                unreadable.push(format!("{name}: not inside this folder"));
                continue;
            }
        };
        listed.insert(rel.to_lowercase());
        if !sb.exists(&rel) {
            missing.push(name);
            continue;
        }
        match sha256(sb, &rel) {
            Ok(h) if h.eq_ignore_ascii_case(hash) => matched += 1,
            Ok(_) => changed.push(name),
            Err(e) => unreadable.push(format!("{name}: {e}")),
        }
    }
    if entries == 0 {
        report.problems = true;
        report.say("This is not a checksum list.");
        return Ok(report);
    }
    let subfolders = listed.iter().any(|r| relative_to(r, &base.to_lowercase()).contains('/'));
    let new: Vec<String> = sb
        .list(&base, subfolders)?
        .into_iter()
        .filter(|i| !i.is_dir && i.ext() != "sha256" && !listed.contains(&i.rel.to_lowercase()))
        .map(|i| relative_to(&i.rel, &base).to_string())
        .collect();
    let all_good = changed.is_empty() && missing.is_empty() && unreadable.is_empty();
    report.say(if all_good {
        format!("All {} match the list.", plural(matched, "file", "files"))
    } else {
        format!("{} match the list.", plural(matched, "file", "files"))
    });
    report.list(format!("{} changed since the list was made:", plural(changed.len(), "file", "files")), &changed);
    report.list(format!("{} missing:", plural(missing.len(), "file is", "files are")), &missing);
    report.list(format!("Could not read {}:", plural(unreadable.len(), "file", "files")), &unreadable);
    report.list(format!("{} not on the list (added since):", plural(new.len(), "file", "files")), &new);
    report.problems = !all_good;
    Ok(report)
}

// ---------------------------------------------------------------------------
// Folder contents list

fn folder_list() -> Plugin {
    Plugin {
        id: "folder-list",
        name: "Folder contents list",
        summary: "A spreadsheet of everything in a folder",
        description: "Saves a list of everything in a folder, with sizes, kinds and when each file last changed, as a CSV file that opens in Excel, Numbers or LibreOffice. Handy for a record of what is on a drive, an insurance list or comparing two copies.",
        category: "Files",
        version: "1.0",
        needs: vec![Access::Read, Access::Add],
        commands: vec![folder("list", "Make a contents list", "A spreadsheet of what is in this folder")],
        settings: vec![
            subfolders_setting("", true),
            toggle("folders", "List folders too", "Each folder gets its own line.", false),
        ],
        run: run_folder_list,
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["bytes", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if bytes == 1 {
        "1 byte".into()
    } else if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn csv_field(s: &str) -> String {
    // A leading = + - @ would be read as a formula by spreadsheet programs.
    let s = if s.starts_with(['=', '+', '-', '@']) { format!("'{s}") } else { s.to_string() };
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
}

fn run_folder_list(sb: &mut Sandbox, subject: &Subject, v: &Values, _cmd: &str) -> Result<Report> {
    let items: Vec<Item> = sb.list(&subject.rel, v.on("subfolders"))?.into_iter().filter(|i| v.on("folders") || !i.is_dir).collect();
    let mut csv = String::from("\u{feff}Folder,Name,Kind,Size (bytes),Size,Last changed\r\n");
    let mut total = 0u64;
    for i in &items {
        let folder = relative_to(&parent(&i.rel), &subject.rel).to_string();
        let ext = i.ext();
        let kind = if i.is_dir { "folder" } else { kinds::kind_of((!ext.is_empty()).then_some(ext.as_str())) };
        let changed = i.modified.map(|t| local_time(t).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
        let (bytes, size) = if i.is_dir { (String::new(), String::new()) } else { (i.size.to_string(), human_size(i.size)) };
        total += i.size;
        csv.push_str(&[csv_field(&folder), csv_field(&i.name), kind.to_string(), bytes, size, changed].join(","));
        csv.push_str("\r\n");
    }
    let name = format!("Contents of {} {}.csv", if subject.name.is_empty() { "storage" } else { &subject.name }, today());
    let rel = sb.add(&subject.folder, &name, csv.as_bytes())?;
    let mut report = Report::default();
    report.say(format!(
        "Saved {} listing {} ({}).",
        rel.rsplit('/').next().unwrap_or(&rel),
        plural(items.iter().filter(|i| !i.is_dir).count(), "file", "files"),
        human_size(total)
    ));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_common::types::{RootType, StorageRoot};
    use std::time::Duration;

    fn sandbox(dir: &std::path::Path, granted: Vec<Access>) -> Sandbox {
        let root = StorageRoot {
            id: "r".into(),
            device_id: "d".into(),
            name: "Disk".into(),
            absolute_path: dir.to_string_lossy().into_owned(),
            root_type: RootType::Existing,
            read_only: false,
            enabled: true,
            created_at: 0,
        };
        Sandbox::new(root, granted, Duration::from_secs(60))
    }

    fn values(p: &Plugin, pairs: &[(&str, serde_json::Value)]) -> Values {
        let mut m = serde_json::Map::new();
        for (k, v) in pairs {
            m.insert((*k).into(), v.clone());
        }
        Values::new(&p.settings, &m)
    }

    fn folder_subject(rel: &str) -> Subject {
        Subject { rel: rel.into(), name: rel.rsplit('/').next().unwrap_or("").into(), is_dir: true, folder: rel.into() }
    }

    fn file_subject(rel: &str) -> Subject {
        let (folder, name) = nexus_storage::paths::split_parent(rel);
        Subject { rel: rel.into(), name, is_dir: false, folder }
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([200, 40, 40])));
        crate::media::jpeg_bytes(&img, 90).unwrap()
    }

    /// A JPEG with EXIF (orientation 6, a date) and an XMP block naming a place.
    fn jpeg_with_details() -> Vec<u8> {
        let plain = jpeg(40, 20);
        let mut out = plain[..2].to_vec();
        out.extend(small_exif(6, Some("2021:07:04 09:15:30")));
        let xmp = b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta><exif:GPSLatitude>51,30.1N</exif:GPSLatitude></x:xmpmeta>";
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((xmp.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(xmp);
        out.extend_from_slice(&plain[2..]);
        out
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn stripping_keeps_the_picture_and_orientation_but_not_the_place() {
        let src = jpeg_with_details();
        assert!(contains(&src, b"GPSLatitude"));
        let out = strip_jpeg(&src, true).unwrap();
        assert!(!contains(&out, b"GPSLatitude"));
        assert!(image::load_from_memory(&out).is_ok());
        let x = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(&out)).unwrap();
        assert_eq!(x.get_field(exif::Tag::Orientation, exif::In::PRIMARY).unwrap().value.get_uint(0), Some(6));
        let date = x.get_field(exif::Tag::DateTime, exif::In::PRIMARY).unwrap().display_value().to_string();
        assert!(date.starts_with("2021-07-04 09:15:30"), "{date}");
        // Without the date.
        let out = strip_jpeg(&src, false).unwrap();
        let x = exif::Reader::new().read_from_container(&mut std::io::Cursor::new(&out)).unwrap();
        assert!(x.get_field(exif::Tag::DateTime, exif::In::PRIMARY).is_none());
        assert!(strip_jpeg(b"not a photo", true).is_err());
    }

    #[test]
    fn web_copies_are_smaller_and_made_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("trip")).unwrap();
        std::fs::write(dir.path().join("trip/big.jpg"), jpeg(3000, 1500)).unwrap();
        std::fs::write(dir.path().join("trip/small.png"), {
            let mut b = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::new(100, 50)).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Png).unwrap();
            b
        })
        .unwrap();
        std::fs::write(dir.path().join("trip/notes.txt"), "x").unwrap();
        let p = web_copies();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &folder_subject("trip"), &values(&p, &[("size", "1280".into())]), "copy-folder").unwrap();
        assert!(r.lines[0].starts_with("Made 2 copies"), "{:?}", r.lines);
        let big = image::open(dir.path().join("trip/Web-sized/big.jpg")).unwrap();
        assert_eq!((big.width(), big.height()), (1280, 640));
        let small = image::open(dir.path().join("trip/Web-sized/small.jpg")).unwrap();
        assert_eq!(small.width(), 100);
        // Again: nothing new, and its own copies are not copied.
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &folder_subject("trip"), &values(&p, &[("subfolders", true.into())]), "copy-folder").unwrap();
        assert!(sb.added.is_empty(), "{:?}", r.lines);
    }

    #[test]
    fn a_plugin_without_add_access_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), jpeg(50, 50)).unwrap();
        let p = web_copies();
        let mut sb = sandbox(dir.path(), vec![Access::Read]);
        assert!((p.run)(&mut sb, &file_subject("a.jpg"), &values(&p, &[]), "copy").is_err());
        assert!(!dir.path().join("Web-sized").exists());
    }

    #[test]
    fn renames_by_the_date_inside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("IMG_1.jpg"), jpeg_with_details()).unwrap();
        std::fs::write(dir.path().join("IMG_2.jpg"), jpeg_with_details()).unwrap();
        std::fs::write(dir.path().join("plain.jpg"), jpeg(10, 10)).unwrap();
        let p = rename_by_date();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &folder_subject(""), &values(&p, &[]), "rename-folder").unwrap();
        assert_eq!(r.lines[0], "Renamed 2 photos.", "{:?}", r.lines);
        assert!(dir.path().join("2021-07-04 09.15.30.jpg").exists());
        assert!(dir.path().join("2021-07-04 09.15.30 (2).jpg").exists());
        assert!(dir.path().join("plain.jpg").exists());
        assert_eq!(sb.renamed.len(), 2);
        // Run again: nothing to do.
        let mut sb = sandbox(dir.path(), p.needs.clone());
        (p.run)(&mut sb, &folder_subject(""), &values(&p, &[]), "rename-folder").unwrap();
        assert!(sb.renamed.is_empty());
        // Keeping the old name, in the compact form.
        let p2 = rename_by_date();
        std::fs::write(dir.path().join("IMG_3.jpg"), jpeg_with_details()).unwrap();
        let mut sb = sandbox(dir.path(), p2.needs.clone());
        let v = values(&p2, &[("format", "compact".into()), ("keep_name", true.into())]);
        (p2.run)(&mut sb, &file_subject("IMG_3.jpg"), &v, "rename").unwrap();
        assert!(dir.path().join("20210704_091530 IMG_3.jpg").exists());
    }

    #[test]
    fn checksums_find_changed_missing_and_new_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("set/inner")).unwrap();
        std::fs::write(dir.path().join("set/a.txt"), "alpha").unwrap();
        std::fs::write(dir.path().join("set/inner/b.txt"), "beta").unwrap();
        std::fs::write(dir.path().join("set/c.txt"), "gamma").unwrap();
        let p = checksums();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        (p.run)(&mut sb, &folder_subject("set"), &values(&p, &[]), "write").unwrap();
        let list_rel = sb.added[0].clone();
        let list = std::fs::read_to_string(dir.path().join(&list_rel)).unwrap();
        // sha256("alpha"), in sha256sum's format.
        assert!(list.contains("8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8  a.txt"), "{list}");
        assert!(list.contains("  inner/b.txt"));

        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &file_subject(&list_rel), &values(&p, &[]), "check").unwrap();
        assert!(!r.problems, "{:?}", r.lines);
        assert_eq!(r.lines[0], "All 3 files match the list.");

        std::fs::write(dir.path().join("set/a.txt"), "ALPHA").unwrap();
        std::fs::remove_file(dir.path().join("set/c.txt")).unwrap();
        std::fs::write(dir.path().join("set/inner/d.txt"), "new").unwrap();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &file_subject(&list_rel), &values(&p, &[]), "check").unwrap();
        let text = r.lines.join("\n");
        assert!(r.problems);
        assert!(text.contains("1 file changed") && text.contains("  a.txt"), "{text}");
        assert!(text.contains("1 file is missing") && text.contains("  c.txt"), "{text}");
        assert!(text.contains("  inner/d.txt"), "{text}");
        // Checking reads only: nothing was added.
        assert!(sb.added.is_empty());
    }

    #[test]
    fn checksum_lists_cannot_point_outside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("set")).unwrap();
        std::fs::write(dir.path().join("outside.txt"), "x").unwrap();
        std::fs::write(dir.path().join("set/list.sha256"), format!("{}  ../outside.txt\n", "0".repeat(64))).unwrap();
        let p = checksums();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &file_subject("set/list.sha256"), &values(&p, &[]), "check").unwrap();
        assert!(r.lines.join("\n").contains("not inside this folder"), "{:?}", r.lines);
    }

    #[test]
    fn folder_lists_are_spreadsheets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs/old")).unwrap();
        std::fs::write(dir.path().join("docs/report, final.pdf"), vec![0u8; 2500]).unwrap();
        std::fs::write(dir.path().join("docs/old/=cmd.txt"), "x").unwrap();
        let p = folder_list();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        (p.run)(&mut sb, &folder_subject("docs"), &values(&p, &[]), "list").unwrap();
        let csv = std::fs::read_to_string(dir.path().join(&sb.added[0])).unwrap();
        assert!(csv.starts_with("\u{feff}Folder,Name,Kind"));
        assert!(csv.contains(",\"report, final.pdf\",document,2500,2.5 KB,"), "{csv}");
        assert!(csv.contains("old,'=cmd.txt,document,1,1 byte,"), "{csv}");
    }

    #[test]
    fn contact_sheets_hold_every_photo() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..5 {
            std::fs::write(dir.path().join(format!("{n}.jpg")), jpeg(400, 300)).unwrap();
        }
        let p = contact_sheet();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &folder_subject(""), &values(&p, &[("columns", 3.into()), ("tile", "200".into())]), "make").unwrap();
        assert_eq!(r.lines[0], "Made Contact sheet.jpg with 5 photos.");
        let sheet = image::open(dir.path().join("Contact sheet.jpg")).unwrap();
        // 3 across, 2 down, 200-pixel tiles with 10-pixel gaps.
        assert_eq!((sheet.width(), sheet.height()), (3 * 210 + 10, 2 * 210 + 10));
    }

    #[test]
    fn document_text_is_saved_beside() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, b"<w:document><w:body><w:p><w:r><w:t>Hearth minutes</w:t></w:r></w:p></w:body></w:document>").unwrap();
        std::fs::write(dir.path().join("minutes.docx"), zip.finish().unwrap().into_inner()).unwrap();
        let p = document_text();
        let mut sb = sandbox(dir.path(), p.needs.clone());
        let r = (p.run)(&mut sb, &file_subject("minutes.docx"), &values(&p, &[]), "text").unwrap();
        assert!(!r.problems, "{:?}", r.lines);
        assert_eq!(std::fs::read_to_string(dir.path().join("Text/minutes.txt")).unwrap(), "Hearth minutes");
    }
}
