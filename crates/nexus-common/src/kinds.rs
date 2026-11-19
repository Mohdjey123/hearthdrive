//! What kind of file something is, by its (lower-case) extension.
//!
//! Used by search filters, the photo and media libraries and document text.

pub const IMAGE: &[&str] = &[
    "jpg", "jpeg", "jfif", "png", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif", "svg", "ico",
];
/// Camera raw formats. Each embeds a JPEG preview that Hearthdrive shows.
pub const RAW: &[&str] = &[
    "dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "orf", "rw2", "raf", "srw", "pef", "x3f", "3fr",
    "erf", "kdc", "mrw", "iiq", "rwl",
];
pub const VIDEO: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "avi", "wmv", "webm", "flv", "mpg", "mpeg", "3gp", "m2ts", "mts", "ogv",
];
pub const AUDIO: &[&str] = &["mp3", "m4a", "m4b", "aac", "flac", "wav", "ogg", "oga", "opus", "wma", "aif", "aiff", "ape"];
pub const DOCUMENT: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "odp", "epub", "pages",
    "numbers", "key", "tex",
];
pub const ARCHIVE: &[&str] = &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "tbz2", "xz", "txz", "zst", "iso", "cab"];
pub const CODE: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "py", "java", "c", "h", "cpp", "hpp", "cs", "go", "rb", "php", "swift", "kt",
    "sh", "ps1", "bat", "json", "toml", "yaml", "yml", "xml", "html", "htm", "css", "scss", "sql", "ini", "lua", "dart",
];

/// The filter names the API accepts, in display order.
pub const KINDS: &[&str] = &["image", "video", "audio", "document", "archive", "code", "other"];

/// The extensions of a kind. `image` includes raw photos. `other` is
/// everything not in a list, so it has none of its own.
pub fn extensions(kind: &str) -> Option<Vec<&'static str>> {
    Some(match kind {
        "image" => IMAGE.iter().chain(RAW).copied().collect(),
        "video" => VIDEO.to_vec(),
        "audio" => AUDIO.to_vec(),
        "document" => DOCUMENT.to_vec(),
        "archive" => ARCHIVE.to_vec(),
        "code" => CODE.to_vec(),
        _ => return None,
    })
}

/// Every extension that has a kind (for `other`).
pub fn known() -> Vec<&'static str> {
    [IMAGE, RAW, VIDEO, AUDIO, DOCUMENT, ARCHIVE, CODE].concat()
}

pub fn kind_of(extension: Option<&str>) -> &'static str {
    let Some(ext) = extension else { return "other" };
    let ext = ext.to_ascii_lowercase();
    let has = |list: &[&str]| list.contains(&ext.as_str());
    if has(IMAGE) || has(RAW) {
        "image"
    } else if has(VIDEO) {
        "video"
    } else if has(AUDIO) {
        "audio"
    } else if has(DOCUMENT) {
        "document"
    } else if has(ARCHIVE) {
        "archive"
    } else if has(CODE) {
        "code"
    } else {
        "other"
    }
}

pub fn is_raw(extension: Option<&str>) -> bool {
    extension.is_some_and(|e| RAW.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind_of(Some("JPG")), "image");
        assert_eq!(kind_of(Some("nef")), "image");
        assert_eq!(kind_of(Some("mkv")), "video");
        assert_eq!(kind_of(Some("pdf")), "document");
        assert_eq!(kind_of(Some("weird")), "other");
        assert_eq!(kind_of(None), "other");
        assert!(extensions("image").unwrap().contains(&"cr3"));
        assert!(extensions("other").is_none());
    }
}
