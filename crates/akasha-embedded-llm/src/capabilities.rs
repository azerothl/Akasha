//! Camelid-inspired evidence-gated capabilities for embedded models.
//!
//! Taxonomy (product):
//! - `supported` — runtime-proven on stock backends for this build
//! - `evidence_only` — bench / community evidence; not product-default yet
//! - `groundwork_only` — docs/manifest only; needs runtime work (mmproj, fork, …)

use crate::EmbeddedLlm;
#[cfg(feature = "download")]
use crate::config;
use serde::Serialize;

/// Compatibility class for a model entry (Camelid-style).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityClass {
    Supported,
    EvidenceOnly,
    GroundworkOnly,
}

impl CompatibilityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::EvidenceOnly => "evidence_only",
            Self::GroundworkOnly => "groundwork_only",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "supported" => Some(Self::Supported),
            "evidence_only" | "evidence-only" => Some(Self::EvidenceOnly),
            "groundwork_only" | "groundwork-only" => Some(Self::GroundworkOnly),
            _ => None,
        }
    }
}

/// Static bench-catalog entries not necessarily present in the wizard manifest.
#[derive(Debug, Clone)]
pub struct BenchCatalogEntry {
    pub id: &'static str,
    pub label: &'static str,
    pub arch: &'static str,
    pub compatibility: CompatibilityClass,
    pub bench_tier: &'static str,
    pub vision: bool,
    pub mmproj_required: bool,
    pub notes: &'static str,
    pub size_bytes_hint: u64,
    pub hf_url_hint: &'static str,
}

/// Tier 1–2 AR candidates + tier-3 watch (docs only) for v0.11 benches.
pub fn bench_catalog() -> Vec<BenchCatalogEntry> {
    vec![
        BenchCatalogEntry {
            id: "qwen3.5-0.8b-q4",
            label: "Qwen3.5 0.8B Q4_K_M",
            arch: "qwen35",
            compatibility: CompatibilityClass::EvidenceOnly,
            bench_tier: "tier1",
            vision: true,
            mmproj_required: true,
            notes: "Best local bench on 4 GB GPU (14.9 tok/s); validate n_batch + mmproj before default swap",
            size_bytes_hint: 529_000_000,
            hf_url_hint: "https://huggingface.co/diodel/Qwen3.5-0.8B-Q4_K_M-GGUF",
        },
        BenchCatalogEntry {
            id: "qwen3-0.6b-q4",
            label: "Qwen3 0.6B Q4_K_M GGUF",
            arch: "qwen3",
            compatibility: CompatibilityClass::EvidenceOnly,
            bench_tier: "tier1",
            vision: false,
            mmproj_required: false,
            notes: "Align famille Qwen3 vs Candle bundlé; download HF may fail — re-try in protocol",
            size_bytes_hint: 400_000_000,
            hf_url_hint: "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF",
        },
        BenchCatalogEntry {
            id: "gemma3-1b-qat-q4",
            label: "Gemma 3 1B IT QAT Q4_K_M",
            arch: "gemma3",
            compatibility: CompatibilityClass::EvidenceOnly,
            bench_tier: "tier2",
            vision: false,
            mmproj_required: false,
            notes: "Bench prioritaire; texte seul en GGUF courant; hors manifeste wizard jusqu'à preuve FR",
            size_bytes_hint: 806_000_000,
            hf_url_hint: "https://huggingface.co/bartowski/google_gemma-3-1b-it-qat-GGUF",
        },
        BenchCatalogEntry {
            id: "qwen3-1.7b-q4",
            label: "Qwen3 1.7B Instruct Q4_K_M",
            arch: "qwen3",
            compatibility: CompatibilityClass::EvidenceOnly,
            bench_tier: "tier2",
            vision: false,
            mmproj_required: false,
            notes: "Légèrement au-dessus contrainte onboarding (~1.28 Go); meilleur FR attendu",
            size_bytes_hint: 1_280_000_000,
            hf_url_hint: "https://huggingface.co/bartowski/Qwen_Qwen3-1.7B-GGUF",
        },
        BenchCatalogEntry {
            id: "phi-4-mini-q4",
            label: "Phi-4-mini Instruct Q4_K_M",
            arch: "phi3",
            compatibility: CompatibilityClass::GroundworkOnly,
            bench_tier: "tier2",
            vision: false,
            mmproj_required: false,
            notes: "Hors onboarding (~2.5 Go); bench qualité code EN seulement",
            size_bytes_hint: 2_500_000_000,
            hf_url_hint: "https://huggingface.co/microsoft/Phi-4-mini-instruct-gguf",
        },
        BenchCatalogEntry {
            id: "qwen3.6-watch",
            label: "Qwen3.6 family (watch)",
            arch: "qwen36",
            compatibility: CompatibilityClass::GroundworkOnly,
            bench_tier: "tier3",
            vision: true,
            mmproj_required: true,
            notes: "Veille: MTP / ROCmFP4 vs stack NVIDIA release — doc only, pas de manifeste",
            size_bytes_hint: 0,
            hf_url_hint: "https://huggingface.co/unsloth/Qwen3.6-27B-GGUF",
        },
    ]
}

#[cfg(feature = "download")]
fn file_present(filename: &str) -> bool {
    let p = config::gguf_path_for_filename(filename);
    p.is_file()
}

#[cfg(feature = "download")]
fn mmproj_path_for(filename: &str) -> std::path::PathBuf {
    config::akasha_data_dir()
        .join("models")
        .join("embedded")
        .join(filename)
}

fn classify_manifest_model(
    id: &str,
    explicit: Option<&str>,
    is_default: bool,
    vision: bool,
    mmproj_required: bool,
    mmproj_on_disk: bool,
) -> CompatibilityClass {
    if let Some(raw) = explicit {
        if let Some(c) = CompatibilityClass::parse(raw) {
            return c;
        }
    }
    if vision && mmproj_required && !mmproj_on_disk {
        return CompatibilityClass::EvidenceOnly;
    }
    if is_default {
        return CompatibilityClass::Supported;
    }
    // Wizard / manifeste variants that ship with v0.10
    if matches!(
        id,
        "qwen2.5-1.5b-instruct-q4" | "smollm2-360m-instruct-q4" | "qwen3.5-0.8b-q4" | "qwen3-0.6b-q4"
    ) {
        if id == "qwen2.5-1.5b-instruct-q4" {
            CompatibilityClass::Supported
        } else if id == "smollm2-360m-instruct-q4" {
            CompatibilityClass::Supported
        } else {
            CompatibilityClass::EvidenceOnly
        }
    } else {
        CompatibilityClass::EvidenceOnly
    }
}

/// Build the JSON body for `GET /api/capabilities`.
pub fn build_capabilities() -> serde_json::Value {
    let snap = EmbeddedLlm::status_snapshot();
    let compiled = snap.compiled_backends.clone();
    let candle_compiled = compiled.iter().any(|b| b == "candle");
    let mmproj_env = std::env::var("AKASHA_EMBEDDED_MMPROJ_PATH")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mmproj_present = mmproj_env
        .as_ref()
        .map(|p| std::path::Path::new(p).is_file())
        .unwrap_or(false);

    let mut models = Vec::new();
    #[cfg(feature = "download")]
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    #[cfg(feature = "download")]
    if let Ok(manifest) = crate::download::load_manifest() {
        for entry in &manifest.models {
            seen.insert(entry.id.clone());
            let gguf_on_disk = file_present(&entry.filename);
            let mmproj_filename = entry.mmproj_filename.clone();
            let mmproj_on_disk = mmproj_filename
                .as_ref()
                .map(|f| mmproj_path_for(f).is_file())
                .unwrap_or(false);
            let vision = entry.vision;
            let mmproj_required = entry.mmproj_required || mmproj_filename.is_some();
            let is_default = entry.id == manifest.default_id;
            let compatibility = classify_manifest_model(
                &entry.id,
                entry.compatibility.as_deref(),
                is_default,
                vision,
                mmproj_required,
                mmproj_on_disk,
            );
            models.push(serde_json::json!({
                "id": entry.id,
                "label": entry.label,
                "arch": entry.arch,
                "gguf": true,
                "gguf_filename": entry.filename,
                "gguf_on_disk": gguf_on_disk,
                "vision": vision,
                "mmproj_required": mmproj_required,
                "mmproj_filename": mmproj_filename,
                "mmproj_on_disk": mmproj_on_disk,
                "compatibility": compatibility.as_str(),
                "evidence": {
                    "source": "manifest",
                    "bench_tier": entry.bench_tier,
                    "bench_role": entry.bench_role,
                    "notes": entry.evidence_notes,
                },
                "engine_candidates": entry.engine_candidates,
                "size_bytes_hint": entry.size_bytes_hint,
                "is_default": is_default,
                "in_wizard_manifest": true,
            }));
        }
    }

    for entry in bench_catalog() {
        #[cfg(feature = "download")]
        if seen.contains(entry.id) {
            continue;
        }
        models.push(serde_json::json!({
            "id": entry.id,
            "label": entry.label,
            "arch": entry.arch,
            "gguf": entry.size_bytes_hint > 0,
            "gguf_on_disk": false,
            "vision": entry.vision,
            "mmproj_required": entry.mmproj_required,
            "mmproj_filename": serde_json::Value::Null,
            "mmproj_on_disk": false,
            "compatibility": entry.compatibility.as_str(),
            "evidence": {
                "source": "bench_catalog",
                "bench_tier": entry.bench_tier,
                "bench_role": "bench_candidate",
                "notes": entry.notes,
                "hf_url_hint": entry.hf_url_hint,
            },
            "engine_candidates": ["llama_cpp_cpu", "llama_cpp_cuda"],
            "size_bytes_hint": entry.size_bytes_hint,
            "is_default": false,
            "in_wizard_manifest": false,
        }));
    }

    let action = snap.action.clone();
    let evidence_flags = serde_json::json!({
        "gguf_present": snap.gguf_present,
        "llama_cpp_compiled": snap.llama_cpp_compiled,
        "candle_compiled": candle_compiled,
        "ready_for_chat": snap.ready_for_chat,
        "calibration_done": snap.calibration_done,
        "mmproj_present": mmproj_present,
        "vision_runtime": false,
        "action": action,
        "active_model_id": snap.active_model_id,
        "hardware_tier": snap.hardware_tier,
        "n_gpu_layers": snap.n_gpu_layers,
        "n_batch": resolved_n_batch_hint(),
    });

    serde_json::json!({
        "schema_version": 1,
        "spec": "spec/dev/roadmap/embedded_models_research_v0.10.md",
        "taxonomy": {
            "supported": "Runtime-proven on stock llama-cpp / Candle for this build; safe to present as ready when evidence flags agree",
            "evidence_only": "Bench or community evidence; not product-default until AR protocol + decision A1",
            "groundwork_only": "Docs / watch only; needs runtime work (mmproj, fork ROCmFP4, size, …)"
        },
        "runtime": {
            "embedded_available": snap.embedded_available,
            "embedded_loaded": snap.embedded_loaded,
            "backend": snap.backend,
            "device": snap.device,
            "compiled_backends": snap.compiled_backends,
            "hint": snap.hint,
            "gguf": snap.gguf_present,
            "vision": false,
            "mmproj": mmproj_present,
            "evidence": evidence_flags,
        },
        "models": models,
        "bench_protocol": {
            "id": "ar_fr_v1",
            "doc": "spec/dev/quality/bench_ar_v0.11.md",
            "protocol_json": "spec/dev/quality/bench_ar_protocol.json",
            "prompts": 5,
            "metrics": ["ttft_s", "tok_per_s", "load_s"],
        }
    })
}

/// Hint for capabilities JSON (shared with llama-cpp backend when feature enabled).
pub fn resolved_n_batch_hint() -> u32 {
    std::env::var("AKASHA_EMBEDDED_N_BATCH")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n >= 256)
        .unwrap_or(2048)
}

/// Infer recommended n_batch for a GGUF arch id (hybrid Qwen3.5 needs generous batch).
pub fn recommended_n_batch_for_arch(arch: &str) -> u32 {
    let arch = arch.trim().to_ascii_lowercase();
    let base = resolved_n_batch_hint();
    if arch.contains("qwen35") || arch.contains("qwen3.5") || arch == "qwen3_5" {
        base.max(2048)
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_parse_roundtrip() {
        assert_eq!(
            CompatibilityClass::parse("supported"),
            Some(CompatibilityClass::Supported)
        );
        assert_eq!(
            CompatibilityClass::parse("evidence-only"),
            Some(CompatibilityClass::EvidenceOnly)
        );
        assert_eq!(
            CompatibilityClass::parse("groundwork_only"),
            Some(CompatibilityClass::GroundworkOnly)
        );
        assert_eq!(CompatibilityClass::parse("nope"), None);
    }

    #[test]
    fn build_capabilities_has_taxonomy_and_runtime() {
        let cap = build_capabilities();
        assert_eq!(cap["schema_version"], 1);
        assert!(cap["taxonomy"]["supported"].as_str().unwrap().contains("Runtime"));
        assert!(cap["runtime"]["evidence"]["gguf_present"].is_boolean());
        assert!(cap["runtime"]["evidence"]["llama_cpp_compiled"].is_boolean());
        assert!(cap["runtime"]["evidence"]["vision_runtime"].as_bool() == Some(false));
        let models = cap["models"].as_array().expect("models array");
        assert!(!models.is_empty());
        // Bench catalog always contributes at least Gemma / Qwen3-1.7B when not in manifest
        let ids: Vec<&str> = models
            .iter()
            .filter_map(|m| m["id"].as_str())
            .collect();
        assert!(
            ids.iter().any(|id| id.contains("gemma") || id.contains("qwen3-1.7b") || id.contains("qwen3.5")),
            "expected AR bench candidates in capabilities, got {ids:?}"
        );
        assert_eq!(cap["bench_protocol"]["prompts"], 5);
    }

    #[test]
    fn recommended_n_batch_boosts_qwen35() {
        std::env::remove_var("AKASHA_EMBEDDED_N_BATCH");
        assert!(recommended_n_batch_for_arch("qwen35") >= 2048);
        assert_eq!(recommended_n_batch_for_arch("qwen2"), resolved_n_batch_hint());
    }

    #[test]
    fn bench_catalog_covers_tier1_and_tier2() {
        let cat = bench_catalog();
        assert!(cat.iter().any(|e| e.bench_tier == "tier1"));
        assert!(cat.iter().any(|e| e.bench_tier == "tier2"));
        assert!(cat.iter().any(|e| e.bench_tier == "tier3"));
        assert!(cat.iter().any(|e| e.compatibility == CompatibilityClass::GroundworkOnly));
    }

    #[test]
    fn classify_default_is_supported() {
        let c = classify_manifest_model(
            "qwen2.5-1.5b-instruct-q4",
            None,
            true,
            false,
            false,
            false,
        );
        assert_eq!(c, CompatibilityClass::Supported);
    }

    #[test]
    fn classify_vision_without_mmproj_is_evidence() {
        let c = classify_manifest_model("qwen3.5-0.8b-q4", None, false, true, true, false);
        assert_eq!(c, CompatibilityClass::EvidenceOnly);
    }
}
