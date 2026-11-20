//! Reading the text inside documents so search finds them by what they
//! say: plain text and code, PDFs (their text, or for scanned ones the
//! pages read with Windows' text recognition), Word, Excel, PowerPoint and
//! OpenDocument files, and optionally text in pictures.
//!
//! Documents also get tags from what they say ("invoice", "receipt"…),
//! which people can keep, remove or add to.

use crate::state::SharedState;
use crate::winrt;
use nexus_common::kinds;
use nexus_common::time::now_ms;
use nexus_common::types::FileEntry;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

const BATCH: i64 = 50;
/// The most text kept per document.
const MAX_CHARS: usize = 200_000;
const MAX_TEXT_FILE: u64 = 4 * 1024 * 1024;
const MAX_DOC_FILE: u64 = 200 * 1024 * 1024;
/// Scanned PDFs: pages read with text recognition.
const OCR_PAGES: u32 = 10;
/// Read text in pictures too (off by default: it takes a while).
pub const OCR_IMAGES_SETTING: &str = "documents.ocr_images";

const PLAIN: &[&str] = &["txt", "md", "csv", "tex", "log", "rtf"];
const OFFICE: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp", "epub"];
const PICTURES: &[&str] = &["png", "jpg", "jpeg", "tif", "tiff", "bmp"];

fn readable(ocr_images: bool) -> Vec<&'static str> {
    let mut v: Vec<&str> = [PLAIN, OFFICE, kinds::CODE, &["pdf"]].concat();
    if ocr_images {
        v.extend_from_slice(PICTURES);
    }
    v
}

fn clip(mut text: String) -> String {
    if text.len() > MAX_CHARS {
        let mut cut = MAX_CHARS;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// The text between XML tags, with paragraph and cell ends as spaces.
pub fn xml_text(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 3);
    let mut in_tag = false;
    let mut tag = String::new();
    for c in xml.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let t = tag.trim_start_matches('/');
                if t.starts_with("w:p") && !t.starts_with("w:pPr")
                    || t.starts_with("a:p")
                    || t.starts_with("text:p")
                    || t.starts_with("text:h")
                    || t.starts_with("w:tab")
                    || t.starts_with("w:br")
                    || t.starts_with("si")
                    || t.starts_with("c ")
                    || t == "p"
                    || t.starts_with("br")
                {
                    if !out.ends_with(['\n', ' ']) {
                        out.push(if t.starts_with("w:tab") || t.starts_with("c ") { ' ' } else { '\n' });
                    }
                }
            }
            _ if in_tag => tag.push(c),
            _ => out.push(c),
        }
    }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
}

fn office_text(path: &Path, ext: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    let all: Vec<String> = zip.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).collect();
    let mut names: Vec<String> = all
        .into_iter()
        .filter(|n| match ext {
            "docx" => *n == "word/document.xml" || n.starts_with("word/footnotes") || n.starts_with("word/header"),
            "pptx" => n.starts_with("ppt/slides/slide") && n.ends_with(".xml"),
            "xlsx" => *n == "xl/sharedStrings.xml",
            "epub" => n.ends_with(".xhtml") || n.ends_with(".html"),
            _ => *n == "content.xml",
        })
        .collect();
    // Slides in order: slide2 before slide10.
    names.sort_by_key(|n| (n.len(), n.clone()));
    let mut out = String::new();
    for name in names {
        let mut entry = zip.by_name(&name).map_err(|e| e.to_string())?;
        let mut xml = String::new();
        entry.by_ref().take(32 * 1024 * 1024).read_to_string(&mut xml).map_err(|e| e.to_string())?;
        out.push_str(&xml_text(&xml));
        out.push('\n');
        if out.len() > MAX_CHARS {
            break;
        }
    }
    Ok(out)
}

fn pdf_text(path: &Path) -> Result<String, String> {
    let path = path.to_path_buf();
    // The PDF reader can panic on damaged files; that must not take the
    // worker down.
    std::panic::catch_unwind(move || pdf_extract::extract_text(&path).map_err(|e| e.to_string()))
        .unwrap_or_else(|_| Err("The PDF could not be read".into()))
}

/// Reads a document's text. Returns how it was read and the text.
pub fn extract(path: &Path, ext: &str, size: u64) -> (&'static str, Result<String, String>) {
    if PLAIN.contains(&ext) || kinds::CODE.contains(&ext) {
        if size > MAX_TEXT_FILE {
            return ("text", Err("Too big to read".into()));
        }
        let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| e.to_string());
        return ("text", text);
    }
    if size > MAX_DOC_FILE {
        return ("none", Err("Too big to read".into()));
    }
    if OFFICE.contains(&ext) {
        return ("office", office_text(path, ext));
    }
    if ext == "pdf" {
        let text = pdf_text(path);
        let words = text.as_ref().map(|t| t.split_whitespace().count()).unwrap_or(0);
        // Scanned: no text layer. Read the pages instead.
        if words < 20 && winrt::available() {
            return match winrt::ocr_pdf(path, OCR_PAGES) {
                Ok(t) if t.split_whitespace().count() > words => ("ocr", Ok(t)),
                _ => ("pdf", text),
            };
        }
        return ("pdf", text);
    }
    if PICTURES.contains(&ext) {
        return ("ocr", winrt::ocr_image(path));
    }
    ("none", Ok(String::new()))
}

/// Tags a document gets from what it says.
pub fn auto_tags(text: &str) -> Vec<String> {
    const RULES: &[(&str, &[&str])] = &[
        ("invoice", &["invoice", "rechnung", "factura", "facture", "fattura"]),
        ("receipt", &["receipt", "quittung", "kassenbon", "recibo", "reçu"]),
        ("contract", &["contract", "agreement", "vertrag", "contrato", "contrat"]),
        ("bank statement", &["bank statement", "account statement", "kontoauszug", "relevé de compte"]),
        ("tax", &["tax return", "steuererklärung", "w-2", "1099", "income tax", "einkommensteuer"]),
        ("payslip", &["payslip", "pay slip", "pay stub", "gehaltsabrechnung", "lohnabrechnung"]),
        ("insurance", &["insurance policy", "policy number", "versicherungsschein", "versicherungsnummer"]),
        ("medical", &["diagnosis", "prescription", "patient name", "befund", "rezept"]),
        ("utility", &["electricity", "kwh", "natural gas", "water bill", "utility bill", "stromrechnung", "électricité"]),
    ];
    let lower = text.chars().take(20_000).collect::<String>().to_lowercase();
    RULES
        .iter()
        .filter(|(_, words)| words.iter().any(|w| lower.contains(w)))
        .map(|(tag, _)| tag.to_string())
        .collect()
}

/// Reads new and changed documents in the background.
pub fn spawn(state: SharedState) {
    if std::env::var_os("NEXUS_NO_DOCUMENTS").is_some() {
        return;
    }
    tokio::spawn(async move {
        let mut events = state.events.subscribe();
        loop {
            let done = run_batch(&state).await;
            if done >= BATCH as usize {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
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
                _ = wait => tokio::time::sleep(Duration::from_secs(10)).await,
                _ = tokio::time::sleep(Duration::from_secs(900)) => {}
            }
        }
    });
}

async fn run_batch(state: &SharedState) -> usize {
    let ocr_images = state.db.get_setting(OCR_IMAGES_SETTING).await.ok().flatten().as_deref() == Some("true");
    let todo = match state.db.doc_todo(&readable(ocr_images), BATCH).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "could not list documents to read");
            return 0;
        }
    };
    let n = todo.len();
    for file in todo {
        read_one(state, file).await;
    }
    n + backfill(state).await
}

async fn read_one(state: &SharedState, file: FileEntry) {
    let Ok(root) = state.db.get_root(&file.root_id).await else { return };
    if state.storage.ensure_online(&root).is_err() {
        return;
    }
    let Ok(path) = state.storage.resolve(&root, &file.relative_path) else { return };
    let ext = file.extension.as_deref().unwrap_or_default().to_ascii_lowercase();
    let size = file.size.max(0) as u64;
    let is_code = kinds::CODE.contains(&ext.as_str());
    let (method, text) = tokio::task::spawn_blocking(move || {
        let (m, t) = extract(&path, &ext, size);
        (m, t.map(clip))
    })
    .await
    .unwrap_or(("none", Err("Reading the document failed".into())));
    let now = now_ms();
    let (body, error) = match text {
        Ok(t) => (t, None),
        Err(e) => (String::new(), Some(e)),
    };
    if let Err(e) = state.db.save_doc_text(&file, method, &body, error.as_deref(), now).await {
        tracing::debug!(error = %e, file = %file.relative_path, "could not save document text");
        return;
    }
    if !is_code {
        let tags = auto_tags(&body);
        let _ = state.db.set_file_tags(&file, &tags, true, "nexus", now).await;
        if state.db.is_document(&file.id).await.unwrap_or(false) {
            save_found(state, &file, &body).await;
        }
    }
}

/// Keeps the date, sender and amount a document's text mentions.
async fn save_found(state: &SharedState, file: &FileEntry, body: &str) {
    let (date, sender, amount) =
        (crate::docmeta::find_date(body), crate::docmeta::find_sender(body), crate::docmeta::find_amount(body));
    if let Err(e) = state.db.save_doc_found(file, date.as_deref(), sender.as_deref(), amount.as_deref()).await {
        tracing::debug!(error = %e, file = %file.relative_path, "could not save document details");
    }
}

/// Fills in details for documents read before HearthDrive looked for them.
async fn backfill(state: &SharedState) -> usize {
    let Ok(todo) = state.db.doc_meta_todo(BATCH).await else { return 0 };
    let n = todo.len();
    for (file, body) in todo {
        save_found(state, &file, &body).await;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_out_of_office_xml() {
        let xml = r#"<w:document><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:tab/><w:r><w:t>world &amp; more</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p></w:body></w:document>"#;
        let t = xml_text(xml);
        assert!(t.contains("Hello world & more"), "{t:?}");
        assert!(t.contains("\nSecond"), "{t:?}");
    }

    #[test]
    fn tags_documents_by_what_they_say() {
        assert_eq!(auto_tags("INVOICE No. 42\nTotal due"), ["invoice"]);
        assert!(auto_tags("Ihr Kontoauszug für März").contains(&"bank statement".to_string()));
        assert!(auto_tags("a shopping list").is_empty());
    }

    #[test]
    fn reads_a_word_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.docx");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, b"<w:document><w:p><w:t>Rental agreement</w:t></w:p></w:document>").unwrap();
        zip.finish().unwrap();
        let (method, text) = extract(&path, "docx", 10);
        assert_eq!(method, "office");
        assert!(text.unwrap().contains("Rental agreement"));
    }
}
