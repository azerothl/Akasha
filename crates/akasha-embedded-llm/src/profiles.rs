//! Static profile tiers from spec/dev/quality/embedded_profiles.json.

use crate::hardware::HardwareProfile;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct ProfilesDocument {
    pub version: u32,
    #[serde(default)]
    pub description: String,
    pub tiers: Vec<ProfileTier>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileTier {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub default_engine: String,
    #[serde(default)]
    pub default_n_gpu_layers: u32,
    #[serde(default)]
    pub model_priority: Vec<String>,
    #[serde(default)]
    pub calibration_ngl_options: Vec<u32>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StaticCandidate {
    pub model_id: String,
    pub n_gpu_layers: u32,
    pub engine: String,
}

pub fn embedded_profiles_path() -> PathBuf {
    if let Ok(v) = std::env::var("AKASHA_SPEC_DIR") {
        let p = PathBuf::from(v.trim())
            .join("dev")
            .join("quality")
            .join("embedded_profiles.json");
        if p.is_file() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for rel in [
                "spec/dev/quality/embedded_profiles.json",
                "../spec/dev/quality/embedded_profiles.json",
            ] {
                let p = exe_dir.join(rel);
                if p.is_file() {
                    return p;
                }
            }
        }
    }
    for c in [
        PathBuf::from("spec/dev/quality/embedded_profiles.json"),
        PathBuf::from("../spec/dev/quality/embedded_profiles.json"),
    ] {
        if c.is_file() {
            return c;
        }
    }
    PathBuf::from("spec/dev/quality/embedded_profiles.json")
}

pub fn load_profiles() -> Result<ProfilesDocument, String> {
    let path = embedded_profiles_path();
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("read profiles {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse profiles: {e}"))
}

pub fn tier_for_id<'a>(doc: &'a ProfilesDocument, tier_id: &str) -> Option<&'a ProfileTier> {
    doc.tiers.iter().find(|t| t.id == tier_id)
}

/// Build up to `max_configs` calibration candidates for a hardware profile.
pub fn calibration_candidates(
    profile: &HardwareProfile,
    max_configs: usize,
) -> Result<Vec<StaticCandidate>, String> {
    let doc = load_profiles()?;
    let tier = tier_for_id(&doc, &profile.tier_id)
        .ok_or_else(|| format!("unknown tier {}", profile.tier_id))?;

    #[cfg(feature = "download")]
    let manifest = crate::download::load_manifest().ok();

    let mut out = Vec::new();
    'models: for model_id in &tier.model_priority {
        #[cfg(feature = "download")]
        if let Some(ref m) = manifest {
            if let Some(entry) = m.models.iter().find(|e| e.id == *model_id) {
                if !entry.profile_tiers.is_empty()
                    && !entry.profile_tiers.iter().any(|t| t == &profile.tier_id)
                {
                    continue;
                }
            }
        }
        for &ngl in &tier.calibration_ngl_options {
            if ngl > 0 && !profile.cuda_runtime {
                continue;
            }
            #[cfg(feature = "download")]
            if ngl > 0 {
                if let Some(ref m) = manifest {
                    if let Some(entry) = m.models.iter().find(|e| e.id == *model_id) {
                        if let Some(vram) = profile.vram_mb {
                            if vram < entry.min_vram_mb {
                                continue;
                            }
                        }
                    }
                }
            }
            let engine = if ngl > 0 {
                "llama_cpp_cuda".to_string()
            } else {
                "llama_cpp_cpu".to_string()
            };
            out.push(StaticCandidate {
                model_id: model_id.clone(),
                n_gpu_layers: ngl,
                engine,
            });
            if out.len() >= max_configs {
                break 'models;
            }
        }
        if out.len() >= max_configs {
            break;
        }
    }

    if out.is_empty() {
        let ngl = tier.default_n_gpu_layers;
        let engine = tier.default_engine.clone();
        let model_id = tier
            .model_priority
            .first()
            .cloned()
            .unwrap_or_else(|| "smollm2-360m-instruct-q4".to_string());
        out.push(StaticCandidate {
            model_id,
            n_gpu_layers: ngl,
            engine,
        });
    }
    Ok(out)
}

/// Models from manifest that match the user's tier (for wizard filtering).
pub fn models_for_tier(tier_id: &str) -> Result<Vec<String>, String> {
    #[cfg(feature = "download")]
    {
        let manifest = crate::download::load_manifest()?;
        Ok(manifest
            .models
            .iter()
            .filter(|m| {
                m.profile_tiers.is_empty() || m.profile_tiers.iter().any(|t| t == tier_id)
            })
            .map(|m| m.id.clone())
            .collect())
    }
    #[cfg(not(feature = "download"))]
    {
        let _ = tier_id;
        Err("download feature not enabled".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::HardwareProfile;

    #[test]
    fn profiles_json_parses() {
        if !embedded_profiles_path().is_file() {
            return;
        }
        let doc = load_profiles().expect("profiles");
        assert!(!doc.tiers.is_empty());
    }

    #[test]
    fn gpu_low_candidates_prefer_cpu_ngl() {
        if !embedded_profiles_path().is_file() {
            return;
        }
        let profile = HardwareProfile {
            ram_gb: 32,
            vram_mb: Some(4096),
            gpu_name: Some("RTX 3050 Ti".into()),
            cuda_compiled: true,
            cuda_runtime: true,
            cpu_model: None,
            tier_id: "gpu_low_4gb".to_string(),
        };
        let c = calibration_candidates(&profile, 3).expect("candidates");
        assert!(!c.is_empty());
        assert!(c.iter().any(|x| x.n_gpu_layers == 0));
    }
}
