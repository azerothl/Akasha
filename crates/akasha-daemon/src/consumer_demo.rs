//! Immutable synthetic files are embedded in the candidate, no network required.
pub fn files() -> serde_json::Value {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    let files: [(&str, &str, &[u8]); 3] = [
        (
            "facture-fictive.pdf",
            "application/pdf",
            include_bytes!("../../../spec/consumer_demo/facture-fictive.pdf"),
        ),
        (
            "photo-facture-fictive.jpg",
            "image/jpeg",
            include_bytes!("../../../spec/consumer_demo/photo-facture-fictive.jpg"),
        ),
        (
            "facture-fictive.txt",
            "text/plain",
            include_bytes!("../../../spec/consumer_demo/facture-fictive.txt"),
        ),
    ];
    serde_json::json!({"synthetic":true,"expected":{"amount_eur":42,"due_date":"2026-11-10"},"files":files.into_iter().map(|(name,mime,bytes)|serde_json::json!({"name":name,"mime_type":mime,"bytes":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes)),"content_base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect::<Vec<_>>()})
}
