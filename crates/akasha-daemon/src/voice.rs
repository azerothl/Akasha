//! Voice (TTS/STT) via external services (Kyutai Unmute, Pocket TTS, moshi-server).
//! Config in data_dir/voice_router.yaml; services are called over HTTP.
//! See plan: Kyutai TTS/STT, delayed-streams-modeling, Unmute.

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::Path;

const VOICE_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceProviderConfig {
    /// Base URL of the service (e.g. http://localhost:8765 for Pocket TTS or moshi-server).
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceRouterConfig {
    #[serde(default)]
    pub tts: VoiceProviderConfig,
    #[serde(default)]
    pub stt: VoiceProviderConfig,
}

impl VoiceRouterConfig {
    pub fn load_from_path(path: &Path) -> Option<Self> {
        let s = std::fs::read_to_string(path).ok()?;
        serde_yaml::from_str(&s).ok()
    }

    pub fn tts_configured(&self) -> bool {
        self.tts
            .base_url
            .as_ref()
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false)
    }

    pub fn stt_configured(&self) -> bool {
        self.stt
            .base_url
            .as_ref()
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false)
    }
}

fn voice_config_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("voice_router.yaml")
}

/// Load voice config from data_dir/voice_router.yaml. Returns None if file missing or invalid.
pub fn load_voice_config(data_dir: &Path) -> Option<VoiceRouterConfig> {
    VoiceRouterConfig::load_from_path(&voice_config_path(data_dir))
}

/// TTS: synthesize text to audio. Returns (message, data_url) with data_url = data:audio/wav;base64,....
pub async fn speech_synthesize_impl(
    data_dir: &Path,
    text: &str,
) -> Result<(String, String), String> {
    let config = match load_voice_config(data_dir) {
        Some(c) => c,
        None => {
            return Err(
                "[speech_synthesize] Voice non configuré : créez data_dir/voice_router.yaml avec tts.base_url (ex. http://localhost:8765)."
                    .to_string(),
            );
        }
    };
    if !config.tts_configured() {
        return Err(
            "[speech_synthesize] TTS non configuré : ajoutez tts.base_url dans voice_router.yaml."
                .to_string(),
        );
    }
    let base_url = config
        .tts
        .base_url
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/');
    let url = format!("{}/tts", base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(VOICE_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("[speech_synthesize] reqwest: {}", e))?;
    let body = serde_json::json!({ "text": text });
    let res = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("[speech_synthesize] request: {}", e))?;
    if !res.status().is_success() {
        return Err(format!(
            "[speech_synthesize] HTTP {}: {}",
            res.status(),
            res.text().await.unwrap_or_default()
        ));
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| format!("[speech_synthesize] body: {}", e))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes.as_ref());
    let data_url = format!("data:audio/wav;base64,{}", b64);
    Ok((
        "Audio synthétisé.".to_string(),
        data_url,
    ))
}

/// Decode optional data URL (data:audio/...;base64,...) to raw bytes. If input is already raw base64, decode it.
/// Extract the mime type and raw bytes from a data URL or plain base64 audio input.
/// Returns (mime_type, bytes). If not a data URL, defaults to "audio/wav".
fn decode_audio_input(input: &str) -> Result<(String, Vec<u8>), String> {
    let input = input.trim();
    if input.starts_with("data:") {
        let comma = input
            .find(',')
            .ok_or_else(|| "[speech_transcribe] data URL sans virgule.".to_string())?;
        // MIME is between "data:" (5 chars) and the first ';' or ',' — whichever comes first.
        let mime_end = input.find(';').unwrap_or(comma).min(comma);
        let mime = if mime_end > 5 {
            input[5..mime_end].to_string()
        } else {
            "audio/wav".to_string()
        };
        let b64 = input[comma + 1..].trim();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("[speech_transcribe] base64 decode: {}", e))?;
        Ok((mime, bytes))
    } else {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(input)
            .map_err(|e| format!("[speech_transcribe] base64 decode: {}", e))?;
        Ok(("audio/wav".to_string(), bytes))
    }
}

/// STT: transcribe audio to text. `audio_input` is either a data URL (data:audio/...;base64,...) or raw base64.
pub async fn speech_transcribe_impl(
    data_dir: &Path,
    audio_input: &str,
) -> Result<String, String> {
    if audio_input.trim().is_empty() {
        return Err(
            "[speech_transcribe] Fournissez une data URL audio ou du base64 (ex. après device_invoke local_media microphone record)."
                .to_string(),
        );
    }
    let config = match load_voice_config(data_dir) {
        Some(c) => c,
        None => {
            return Err(
                "[speech_transcribe] Voice non configuré : créez data_dir/voice_router.yaml avec stt.base_url."
                    .to_string(),
            );
        }
    };
    if !config.stt_configured() {
        return Err(
            "[speech_transcribe] STT non configuré : ajoutez stt.base_url dans voice_router.yaml."
                .to_string(),
        );
    }
    let (mime_type, audio_bytes) = decode_audio_input(audio_input)?;
    let base_url = config
        .stt
        .base_url
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/');
    let url = format!("{}/stt", base_url);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(VOICE_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("[speech_transcribe] reqwest: {}", e))?;
    let res = client
        .post(&url)
        .header("Content-Type", mime_type)
        .body(audio_bytes)
        .send()
        .await
        .map_err(|e| format!("[speech_transcribe] request: {}", e))?;
    if !res.status().is_success() {
        return Err(format!(
            "[speech_transcribe] HTTP {}: {}",
            res.status(),
            res.text().await.unwrap_or_default()
        ));
    }
    let json: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("[speech_transcribe] response JSON: {}", e))?;
    let text = json
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(text)
}
