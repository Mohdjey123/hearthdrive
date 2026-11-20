//! What a document's text says about it: its date, who sent it and the
//! amount it is for. These are guesses for the Documents page; people can
//! correct them, and what they type is kept.

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use std::sync::LazyLock;

/// Only the start of a document is looked at: letterheads, dates and the
/// sender are near the top, and the total is rarely far from it.
const LOOK_AT: usize = 8_000;

const MONTHS: &[(&str, u32)] = &[
    ("january", 1), ("jan", 1), ("januar", 1), ("janvier", 1),
    ("february", 2), ("feb", 2), ("februar", 2), ("février", 2), ("fevrier", 2),
    ("march", 3), ("mar", 3), ("märz", 3), ("maerz", 3), ("mars", 3),
    ("april", 4), ("apr", 4), ("avril", 4),
    ("may", 5), ("mai", 5),
    ("june", 6), ("jun", 6), ("juni", 6), ("juin", 6),
    ("july", 7), ("jul", 7), ("juli", 7), ("juillet", 7),
    ("august", 8), ("aug", 8), ("août", 8), ("aout", 8),
    ("september", 9), ("sep", 9), ("sept", 9), ("septembre", 9),
    ("october", 10), ("oct", 10), ("oktober", 10), ("octobre", 10),
    ("november", 11), ("nov", 11), ("novembre", 11),
    ("december", 12), ("dec", 12), ("dezember", 12), ("décembre", 12), ("decembre", 12),
];

fn month(name: &str) -> Option<u32> {
    let name = name.trim_end_matches('.').to_lowercase();
    MONTHS.iter().find(|(n, _)| *n == name).map(|(_, m)| *m)
}

static NUMERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(\d{4})[-/.](\d{1,2})[-/.](\d{1,2})\b|\b(\d{1,2})([./-])(\d{1,2})[./-](\d{4})\b").unwrap()
});
static WORDY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(\d{1,2})\.?\s+([a-zäéû]{3,9})\.?,?\s+(\d{4})\b|\b([a-zäéû]{3,9})\.?\s+(\d{1,2})(?:st|nd|rd|th)?,?\s+(\d{4})\b")
        .unwrap()
});

fn plausible(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
    let date = NaiveDate::from_ymd_opt(y, m, d)?;
    let latest = chrono::Local::now().date_naive().year() + 1;
    (1970..=latest).contains(&y).then_some(date)
}

/// The first date the document mentions, as `YYYY-MM-DD`.
///
/// `05.03.2026` is day first (the dot is European). With slashes or dashes
/// a part over 12 settles it; otherwise month first, as in North America.
pub fn find_date(text: &str) -> Option<String> {
    let text = head(text);
    let mut best: Option<(usize, NaiveDate)> = None;
    let mut keep = |at: usize, date: Option<NaiveDate>| {
        if let Some(date) = date
            && best.is_none_or(|(b, _)| at < b)
        {
            best = Some((at, date));
        }
    };
    for c in NUMERIC.captures_iter(text) {
        let at = c.get(0).unwrap().start();
        let n = |i: usize| c.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
        if let (Some(y), Some(m), Some(d)) = (n(1), n(2), n(3)) {
            keep(at, plausible(y as i32, m, d));
        } else if let (Some(a), Some(b), Some(y)) = (n(4), n(6), n(7)) {
            let dot = c.get(5).is_some_and(|s| s.as_str() == ".");
            let (m, d) = if dot || a > 12 { (b, a) } else { (a, b) };
            keep(at, plausible(y as i32, m, d));
        }
    }
    for c in WORDY.captures_iter(text) {
        let at = c.get(0).unwrap().start();
        let s = |i: usize| c.get(i).map(|m| m.as_str());
        let date = if let (Some(d), Some(mo), Some(y)) = (s(1), s(2), s(3)) {
            month(mo).and_then(|m| plausible(y.parse().ok()?, m, d.parse().ok()?))
        } else if let (Some(mo), Some(d), Some(y)) = (s(4), s(5), s(6)) {
            month(mo).and_then(|m| plausible(y.parse().ok()?, m, d.parse().ok()?))
        } else {
            None
        };
        keep(at, date);
    }
    best.map(|(_, d)| d.format("%Y-%m-%d").to_string())
}

/// Words that mark a line as something other than the sender.
const NOT_A_SENDER: &[&str] = &[
    "invoice", "receipt", "statement", "page", "date", "total", "bill to", "ship to", "to:", "dear", "phone", "tel",
    "fax", "email", "e-mail", "www.", "http", "@", "rechnung", "seite", "datum", "facture", "customer", "account",
    "order", "number", "no.", "#",
];
const COMPANY_ENDINGS: &[&str] = &[
    " inc", " inc.", " ltd", " ltd.", " llc", " llp", " corp", " corp.", " co.", " company", " gmbh", " ag", " sa",
    " s.a.", " bv", " plc", " limited", " incorporated", " corporation",
];

/// Who the document is from: a line that names a company, a `From:` line,
/// or else the first line of the letterhead that reads like a name.
pub fn find_sender(text: &str) -> Option<String> {
    let lines: Vec<&str> = head(text).lines().map(str::trim).filter(|l| !l.is_empty()).take(30).collect();
    let clean = |l: &str| {
        let l = l.trim_matches(|c: char| c == ':' || c == ',' || c == '-' || c.is_whitespace());
        (l.chars().count() >= 3 && l.chars().count() <= 60).then(|| l.to_string())
    };
    for l in &lines {
        let lower = l.to_lowercase();
        for key in ["from:", "von:", "de :", "de:", "issued by:", "seller:", "vendor:"] {
            if let Some(rest) = lower.strip_prefix(key) {
                return clean(&l[l.len() - rest.len()..]);
            }
        }
    }
    for l in &lines {
        let lower = format!("{} ", l.to_lowercase());
        if COMPANY_ENDINGS.iter().any(|e| lower.contains(&format!("{e} ")) || lower.contains(&format!("{e},")))
            && !NOT_A_SENDER.iter().any(|w| lower.contains(w))
        {
            return clean(after_lead_in(l));
        }
    }
    lines.iter().take(6).find_map(|l| {
        let lower = l.to_lowercase();
        let letters = l.chars().filter(|c| c.is_alphabetic()).count();
        let digits = l.chars().filter(|c| c.is_ascii_digit()).count();
        let words = l.split_whitespace().count();
        (letters >= 3 && digits == 0 && words <= 6 && !NOT_A_SENDER.iter().any(|w| lower.contains(w)))
            .then(|| clean(l))
            .flatten()
    })
}

/// "between Oakwood Properties Ltd" names the company after the lead-in.
fn after_lead_in(line: &str) -> &str {
    const LEAD_INS: &[&str] = &["between ", "by ", "with ", "and ", "from ", "of ", "to "];
    let lower = line.to_lowercase();
    let mut start = 0;
    for w in LEAD_INS {
        if let Some(i) = lower.rfind(w)
            && lower[..i].split_whitespace().count() <= 6
        {
            start = start.max(i + w.len());
        }
    }
    line.get(start..).unwrap_or(line)
}

static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:(?P<pre>[$€£¥]|CHF|CAD|USD|EUR|GBP)\s?)?(?P<num>\d{1,3}(?:[,.\s']\d{3})*[.,]\d{2}|\d+[.,]\d{2})(?:\s?(?P<post>[$€£¥]|CHF|CAD|USD|EUR|GBP|€))?")
        .unwrap()
});
const TOTAL_WORDS: &[&str] = &[
    "grand total", "amount due", "balance due", "total due", "total amount", "amount payable", "total", "summe",
    "gesamtbetrag", "gesamt", "endbetrag", "montant total", "total ttc", "importe total", "totale", "monthly rent",
    "rent:", "miete",
];

/// The amount the document is for: the last money amount on a line about
/// the total, as written (with its currency when there is one).
pub fn find_amount(text: &str) -> Option<String> {
    let mut found = None;
    for line in head(text).lines() {
        let lower = line.to_lowercase();
        if lower.contains("subtotal") || lower.contains("sub-total") || lower.contains("zwischensumme") {
            continue;
        }
        if !TOTAL_WORDS.iter().any(|w| lower.contains(w)) {
            continue;
        }
        if let Some(c) = AMOUNT.captures_iter(line).last() {
            let num = c.name("num").unwrap().as_str().trim();
            let cur = c.name("pre").or_else(|| c.name("post")).map(|m| m.as_str().to_uppercase());
            found = Some(match cur.as_deref() {
                Some(sym @ ("$" | "€" | "£" | "¥")) => format!("{sym}{num}"),
                Some(code) => format!("{num} {code}"),
                None => num.to_string(),
            });
        }
    }
    found
}

fn head(text: &str) -> &str {
    match text.char_indices().nth(LOOK_AT) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_date() {
        assert_eq!(find_date("Invoice\nDate: 2026-03-05\nDue 2026-04-05").as_deref(), Some("2026-03-05"));
        assert_eq!(find_date("Rechnungsdatum 05.03.2026").as_deref(), Some("2026-03-05"));
        assert_eq!(find_date("Issued 03/05/2026").as_deref(), Some("2026-03-05"));
        assert_eq!(find_date("Issued 25/12/2025").as_deref(), Some("2025-12-25"));
        assert_eq!(find_date("March 5, 2026 at noon").as_deref(), Some("2026-03-05"));
        assert_eq!(find_date("le 5 mars 2026").as_deref(), Some("2026-03-05"));
        assert_eq!(find_date("Order 12.34.5678 and 99/99/2026"), None);
        assert_eq!(find_date("nothing here"), None);
    }

    #[test]
    fn finds_who_sent_it() {
        assert_eq!(find_sender("Riverside Dental Inc.\n12 Main St\nINVOICE").as_deref(), Some("Riverside Dental Inc."));
        assert_eq!(find_sender("Page 1\nFrom: Hill Street Bakery\nTo: Sam").as_deref(), Some("Hill Street Bakery"));
        assert_eq!(find_sender("Northwind Energy\n42 Power Road\nAccount 1234").as_deref(), Some("Northwind Energy"));
        assert_eq!(find_sender("INVOICE #42\n2026-01-01"), None);
        assert_eq!(
            find_sender("Lease Agreement\nmade on 2026-01-15\nbetween Oakwood Properties Ltd\nand Sam").as_deref(),
            Some("Oakwood Properties Ltd")
        );
    }

    #[test]
    fn finds_the_total() {
        let receipt = "Coffee 4.50\nSubtotal 9.00\nTax 1.17\nTotal $10.17\nThank you";
        assert_eq!(find_amount(receipt).as_deref(), Some("$10.17"));
        assert_eq!(find_amount("Gesamtbetrag 1.234,56 €").as_deref(), Some("€1.234,56"));
        assert_eq!(find_amount("Amount due: 2,500.00 CAD").as_deref(), Some("2,500.00 CAD"));
        assert_eq!(find_amount("coffee 12.00, thanks"), None);
        assert_eq!(find_amount("Monthly rent: 1,450.00 CAD").as_deref(), Some("1,450.00 CAD"));
    }
}
