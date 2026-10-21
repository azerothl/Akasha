//! Safe PDF text extraction: patched `lopdf` via `pdf-extract` 0.12+, plus panic guard.

use std::panic::{catch_unwind, AssertUnwindSafe};

/// User-visible failure kind for PDF text extraction (localized via [`pdf_extract_user_message`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfExtractFailure {
    /// Parse/extraction error or panic in the extractor.
    Unreadable,
    /// Extraction succeeded but text is empty or whitespace-only (e.g. scanned PDF).
    EmptyText,
}

/// Resolve application locale first, then POSIX override precedence: `LC_ALL`, `LC_MESSAGES`, `LANG`.
fn locale_is_en(
    akasha_lang: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
) -> bool {
    for locale in [akasha_lang, lc_all, lc_messages, lang].into_iter().flatten() {
        let locale = locale.trim();
        if !locale.is_empty() {
            return locale.to_ascii_lowercase().starts_with("en");
        }
    }
    false
}

pub fn daemon_locale_is_en() -> bool {
    let akasha_lang = std::env::var("AKASHA_LANG").ok();
    let lc_all = std::env::var("LC_ALL").ok();
    let lc_messages = std::env::var("LC_MESSAGES").ok();
    let lang = std::env::var("LANG").ok();
    locale_is_en(
        akasha_lang.as_deref(),
        lc_all.as_deref(),
        lc_messages.as_deref(),
        lang.as_deref(),
    )
}
/// Localized user-facing message for a PDF extraction failure (exact copy; do not reword).
pub fn pdf_extract_user_message(failure: PdfExtractFailure, is_en: bool) -> &'static str {
    match (failure, is_en) {
        (PdfExtractFailure::Unreadable, true) => {
            "Couldn't read this PDF (the file may be damaged or unsupported)."
        }
        (PdfExtractFailure::Unreadable, false) => {
            "Impossible de lire ce PDF (le fichier est peut-être endommagé ou non pris en charge)."
        }
        (PdfExtractFailure::EmptyText, true) => {
            "This PDF has no readable text (it may be a scan)."
        }
        (PdfExtractFailure::EmptyText, false) => {
            "Ce PDF ne contient pas de texte lisible (c'est peut-être un scan)."
        }
    }
}

/// Bracket-wrapped message for inline prompt injection (e.g. `/api/message` attachments).
pub fn pdf_extract_bracketed_user_message(failure: PdfExtractFailure) -> String {
    format!(
        "[{}]",
        pdf_extract_user_message(failure, daemon_locale_is_en())
    )
}

fn ok_or_empty_text_failure(text: String) -> Result<String, PdfExtractFailure> {
    if text.trim().is_empty() {
        tracing::warn!("PDF extraction returned empty or whitespace-only text");
        Err(PdfExtractFailure::EmptyText)
    } else {
        Ok(text)
    }
}

/// Extract text from PDF bytes. Returns `Err` on parse/extraction failure, panic, or empty text.
pub fn extract_text_from_mem(bytes: &[u8]) -> Result<String, PdfExtractFailure> {
    let bytes = bytes.to_vec();
    match catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(&bytes))) {
        Ok(Ok(text)) => ok_or_empty_text_failure(text),
        Ok(Err(e)) => {
            tracing::warn!("PDF extraction failed: {}", e);
            Err(PdfExtractFailure::Unreadable)
        }
        Err(_) => {
            tracing::warn!("panic caught during PDF extraction");
            Err(PdfExtractFailure::Unreadable)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Truncated PDF header only — must not panic or stack-overflow (RUSTSEC-2026-0187 regression).
    #[test]
    fn malformed_truncated_pdf_returns_err_without_panic() {
        let bytes = b"%PDF-1.4\n% truncated fixture for lopdf regression\n";
        let result = extract_text_from_mem(bytes);
        assert_eq!(result, Err(PdfExtractFailure::Unreadable));
    }

    /// Deeply nested Kids tree (shallow sample) — must not panic.
    #[test]
    fn malformed_nested_pages_pdf_returns_err_without_panic() {
        let mut pdf = String::from("%PDF-1.4\n");
        // Chain 40 page-tree nodes referencing the next (malformed but hostile-shaped).
        for i in 1..40 {
            let next = i + 1;
            pdf.push_str(&format!(
                "{i} 0 obj << /Type /Pages /Kids [{next} 0 R] /Count 1 >> endobj\n"
            ));
        }
        pdf.push_str("40 0 obj << /Type /Pages /Kids [] /Count 0 >> endobj\n");
        pdf.push_str("xref\n0 41\n0000000000 65535 f \n");
        pdf.push_str("trailer << /Root 1 0 R /Size 41 >>\nstartxref\n0\n%%EOF\n");
        let result = extract_text_from_mem(pdf.as_bytes());
        assert!(
            result == Err(PdfExtractFailure::Unreadable)
                || result == Err(PdfExtractFailure::EmptyText),
            "expected unreadable or empty, got {:?}",
            result
        );
    }

    #[test]
    fn user_messages_unreadable_en_fr() {
        assert_eq!(
            pdf_extract_user_message(PdfExtractFailure::Unreadable, true),
            "Couldn't read this PDF (the file may be damaged or unsupported)."
        );
        assert_eq!(
            pdf_extract_user_message(PdfExtractFailure::Unreadable, false),
            "Impossible de lire ce PDF (le fichier est peut-être endommagé ou non pris en charge)."
        );
    }

    #[test]
    fn user_messages_empty_en_fr() {
        assert_eq!(
            pdf_extract_user_message(PdfExtractFailure::EmptyText, true),
            "This PDF has no readable text (it may be a scan)."
        );
        assert_eq!(
            pdf_extract_user_message(PdfExtractFailure::EmptyText, false),
            "Ce PDF ne contient pas de texte lisible (c'est peut-être un scan)."
        );
    }

    #[test]
    fn empty_whitespace_maps_to_empty_text_not_unreadable() {
        assert_eq!(ok_or_empty_text_failure(String::new()), Err(PdfExtractFailure::EmptyText));
        assert_eq!(
            ok_or_empty_text_failure("   \n\t  ".to_string()),
            Err(PdfExtractFailure::EmptyText)
        );
        assert_eq!(
            ok_or_empty_text_failure("hello".to_string()),
            Ok("hello".to_string())
        );
        assert_ne!(
            pdf_extract_user_message(PdfExtractFailure::EmptyText, true),
            pdf_extract_user_message(PdfExtractFailure::Unreadable, true)
        );
    }

    #[test]
    fn daemon_locale_precedence_honors_overrides_before_lang() {
        assert!(!locale_is_en(Some("fr_FR"), Some("en_US.UTF-8"), None, Some("en_US")));
        assert!(locale_is_en(None, Some("en_US.UTF-8"), None, Some("C.UTF-8")));
        assert!(locale_is_en(None, None, Some("en_US.UTF-8"), Some("C.UTF-8")));
        assert!(!locale_is_en(None, Some("fr_FR.UTF-8"), Some("en_US"), Some("en_US")));
        assert!(locale_is_en(None, Some(""), None, Some("en_US.UTF-8")));
        assert!(!locale_is_en(None, None, None, Some("C.UTF-8")));
    }}
