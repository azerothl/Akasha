//! Safe PDF text extraction: patched `lopdf` via `pdf-extract` 0.12+, plus panic guard.

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Extract text from PDF bytes. Returns `Err` on parse/extraction failure or if the extractor panics.
pub fn extract_text_from_mem(bytes: &[u8]) -> Result<String, String> {
    let bytes = bytes.to_vec();
    match catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(&bytes))) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => {
            tracing::warn!("panic caught during PDF extraction");
            Err("Couldn't read this PDF (the file may be damaged or unsupported).".to_string())
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
        assert!(result.is_err(), "expected extraction error, got {:?}", result);
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
            result.is_err() || result.as_ref().map(|s| s.is_empty()).unwrap_or(false),
            "expected err or empty text, got {:?}",
            result
        );
    }
}
