//! Relative-path normalisation and containment checks.
//!
//! Relative paths inside a root are '/'-separated with no leading slash; the
//! empty string means the root itself.

use nexus_common::{NexusError, Result};
use std::path::{Path, PathBuf};

/// NEXUS bookkeeping directory inside each writable root (trash, staging).
pub const INTERNAL_DIR: &str = ".nexus";

/// Device names Windows reserves in every folder (with any extension),
/// including the superscript-digit ports and the console streams.
const RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
    "COM7", "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
];

/// Top-level directories that are never indexed or exposed.
const SKIPPED_TOP_LEVEL: &[&str] = &[INTERNAL_DIR, "$RECYCLE.BIN", "System Volume Information"];

fn rejected(msg: impl Into<String>) -> NexusError {
    NexusError::PathRejected(msg.into())
}

/// Validates a single file or folder name for Windows filesystems.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(rejected("Enter a valid name"));
    }
    if name.len() > 255 {
        return Err(rejected("Names must be 255 bytes or fewer"));
    }
    if let Some(c) = name
        .chars()
        .find(|c| (*c as u32) < 0x20 || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
    {
        return Err(rejected(format!("Names cannot contain {c:?}")));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(rejected("Names cannot end with a space or a period"));
    }
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(rejected(format!("{name} is a reserved name on Windows")));
    }
    Ok(())
}

/// Normalises a client-supplied relative path, rejecting traversal (`..`),
/// drive or UNC prefixes, alternate data streams and invalid names.
pub fn normalize_relative(input: &str) -> Result<String> {
    let unified = input.replace('\\', "/");
    let mut parts = Vec::new();
    for part in unified.split('/') {
        if part.is_empty() {
            continue;
        }
        validate_name(part)?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

/// The file that makes a folder encrypted (its salt and password check):
/// never shown, and never changed through NEXUS, since losing it loses the
/// folder's files.
pub const VAULT_MARKER: &str = ".nexus-vault";

/// True for paths inside folders NEXUS never exposes (its own bookkeeping
/// directory, the recycle bin, volume metadata) and for encrypted folders'
/// marker files.
pub fn is_hidden_internal(rel: &str) -> bool {
    let first = rel.split('/').next().unwrap_or("");
    let last = rel.rsplit('/').next().unwrap_or("");
    SKIPPED_TOP_LEVEL.iter().any(|s| first.eq_ignore_ascii_case(s)) || last.eq_ignore_ascii_case(VAULT_MARKER)
}

pub fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

/// Splits `a/b/c` into (`a/b`, `c`).
pub fn split_parent(rel: &str) -> (String, String) {
    match rel.rsplit_once('/') {
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None => (String::new(), rel.to_string()),
    }
}

/// Lower-cased extension and guessed MIME type for a file name.
pub fn describe(name: &str, is_dir: bool) -> (Option<String>, Option<String>) {
    if is_dir {
        return (None, None);
    }
    let extension = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase());
    let mime = mime_guess::from_path(name).first_raw().map(str::to_string);
    (extension, mime)
}

/// Converts an absolute path under `base` into a NEXUS relative path.
pub fn relative_of(base: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(base).ok()?;
    let parts: Vec<String> = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Resolves `rel` inside `root`, guaranteeing the result cannot escape it.
///
/// * the relative path is normalised and validated first;
/// * NEXUS-internal folders are not addressable;
/// * the nearest existing ancestor is canonicalised (resolving symlinks and
///   junctions) and must still be inside the canonical root;
/// * an existing symlink or junction at the target itself is refused.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = normalize_relative(rel)?;
    if is_hidden_internal(&rel) {
        return Err(rejected("This location is reserved"));
    }
    let canonical_root = dunce::canonicalize(root)
        .map_err(|_| NexusError::Unavailable("The storage root is not accessible".into()))?;

    let mut target = canonical_root.clone();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        target.push(part);
    }

    let mut probe: &Path = &target;
    loop {
        match std::fs::symlink_metadata(probe) {
            Ok(meta) => {
                if probe != canonical_root && meta.file_type().is_symlink() {
                    return Err(rejected("Symbolic links and junctions are not followed"));
                }
                let resolved = dunce::canonicalize(probe)?;
                if !resolved.starts_with(&canonical_root) {
                    return Err(rejected("The path is outside the storage root"));
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                probe = probe
                    .parent()
                    .ok_or_else(|| rejected("The path is outside the storage root"))?;
                if !probe.starts_with(&canonical_root) {
                    return Err(rejected("The path is outside the storage root"));
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(target)
}

/// "report.pdf" -> "report (1).pdf", "report (2).pdf", ...
pub fn numbered_name(name: &str, n: usize, is_dir: bool) -> String {
    if n == 0 {
        return name.to_string();
    }
    let (stem, ext) = match (is_dir, name.rfind('.')) {
        (false, Some(i)) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    format!("{stem} ({n}){ext}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_separators_and_slashes() {
        assert_eq!(normalize_relative("").unwrap(), "");
        assert_eq!(normalize_relative("/a//b/").unwrap(), "a/b");
        assert_eq!(normalize_relative("a\\b\\c.txt").unwrap(), "a/b/c.txt");
    }

    #[test]
    fn rejects_traversal_and_prefixes() {
        for bad in ["..", "a/../b", "a/./b", "C:/Windows", "C:", "file.txt:stream", "a/con", "a/nul.txt", "a/b ", "x."] {
            assert!(normalize_relative(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn rejects_invalid_names() {
        assert!(validate_name("ok name.txt").is_ok());
        assert!(validate_name("a*b").is_err());
        assert!(validate_name("a\u{1}b").is_err());
        assert!(validate_name("COM1.log").is_err());
        assert!(validate_name("conin$").is_err());
        assert!(validate_name("COM¹.txt").is_err());
        assert!(validate_name("COM10").is_ok());
        assert!(validate_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn numbered_names() {
        assert_eq!(numbered_name("a.txt", 0, false), "a.txt");
        assert_eq!(numbered_name("a.txt", 2, false), "a (2).txt");
        assert_eq!(numbered_name(".env", 1, false), ".env (1)");
        assert_eq!(numbered_name("v1.2", 1, true), "v1.2 (1)");
    }

    #[test]
    fn resolve_stays_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let ok = resolve(dir.path(), "sub/new.txt").unwrap();
        assert!(ok.ends_with("sub/new.txt") || ok.ends_with("sub\\new.txt"));
        assert!(resolve(dir.path(), "../outside").is_err());
        assert!(resolve(dir.path(), ".nexus/trash").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn resolve_refuses_junction_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = dir.path().join("escape");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .output()
            .unwrap();
        if !status.status.success() {
            return; // junction creation unavailable in this environment
        }
        assert!(resolve(dir.path(), "escape").is_err());
        assert!(resolve(dir.path(), "escape/file.txt").is_err());
    }
}
