//! Voice (TTS/STT) via external services (Kyutai Unmute, Pocket TTS, moshi-server).
//! Config in data_dir/voice_router.yaml; services are called over HTTP.
//! See plan: Kyutai TTS/STT, delayed-streams-modeling, Unmute.

use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;

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

/// Strip markdown / markup so TTS does not read `**`, backticks, links, etc. aloud.
pub fn plain_text_for_speech(text: &str) -> String {
    static RE_FENCE: OnceLock<Regex> = OnceLock::new();
    static RE_INLINE_CODE: OnceLock<Regex> = OnceLock::new();
    static RE_LINK: OnceLock<Regex> = OnceLock::new();
    static RE_IMAGE: OnceLock<Regex> = OnceLock::new();
    static RE_BOLD: OnceLock<Regex> = OnceLock::new();
    static RE_ITALIC: OnceLock<Regex> = OnceLock::new();
    static RE_HEADING: OnceLock<Regex> = OnceLock::new();
    static RE_QUOTE: OnceLock<Regex> = OnceLock::new();
    static RE_LIST: OnceLock<Regex> = OnceLock::new();
    static RE_HTML: OnceLock<Regex> = OnceLock::new();
    static RE_WS: OnceLock<Regex> = OnceLock::new();

    let re_fence = RE_FENCE.get_or_init(|| {
        Regex::new(r"(?s)```[^\n]*\n.*?```|```.*?```").expect("fence regex")
    });
    let re_inline_code =
        RE_INLINE_CODE.get_or_init(|| Regex::new(r"`([^`]+)`").expect("inline code"));
    let re_image = RE_IMAGE.get_or_init(|| Regex::new(r"!\[([^\]]*)\]\([^)]+\)").expect("image"));
    let re_link = RE_LINK.get_or_init(|| Regex::new(r"\[([^\]]+)\]\([^)]+\)").expect("link"));
    let re_bold = RE_BOLD.get_or_init(|| {
        Regex::new(r"\*\*(.+?)\*\*|__(.+?)__").expect("bold")
    });
    let re_italic = RE_ITALIC.get_or_init(|| {
        Regex::new(r"\*([^*\n]+)\*|_([^_\n]+)_").expect("italic")
    });
    let re_heading = RE_HEADING.get_or_init(|| Regex::new(r"(?m)^#{1,6}\s*").expect("heading"));
    let re_quote = RE_QUOTE.get_or_init(|| Regex::new(r"(?m)^>\s?").expect("quote"));
    let re_list = RE_LIST.get_or_init(|| Regex::new(r"(?m)^(?:[-*+]|\d+\.)\s+").expect("list"));
    let re_html = RE_HTML.get_or_init(|| Regex::new(r"<[^>]+>").expect("html"));
    let re_ws = RE_WS.get_or_init(|| Regex::new(r"[ \t]+\n").expect("ws"));

    let mut s = re_fence.replace_all(text, " ").into_owned();
    s = re_image.replace_all(&s, "$1").into_owned();
    s = re_link.replace_all(&s, "$1").into_owned();
    s = re_inline_code.replace_all(&s, "$1").into_owned();
    s = re_bold.replace_all(&s, |caps: &regex::Captures| {
        caps.get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_string()
    })
    .into_owned();
    s = re_italic
        .replace_all(&s, |caps: &regex::Captures| {
            caps.get(1)
                .or_else(|| caps.get(2))
                .map(|m| m.as_str())
                .unwrap_or("")
                .to_string()
        })
        .into_owned();
    s = re_heading.replace_all(&s, "").into_owned();
    s = re_quote.replace_all(&s, "").into_owned();
    s = re_list.replace_all(&s, "").into_owned();
    s = re_html.replace_all(&s, " ").into_owned();
    s = s.replace("~~", "");
    s = s.replace("---", ". ");
    s = s.replace("***", " ");
    s = re_ws.replace_all(&s, "\n").into_owned();
    // Collapse runs of whitespace / blank lines into single spaces for speech.
    let collapsed = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.trim().to_string()
}

/// TTS: synthesize text to audio. Returns (message, data_url) with data_url = data:audio/wav;base64,....
pub async fn speech_synthesize_impl(
    data_dir: &Path,
    text: &str,
) -> Result<(String, String), String> {
    let spoken = plain_text_for_speech(text);
    let text = if spoken.is_empty() { text.trim() } else { spoken.as_str() };
    if text.is_empty() {
        return Err("[speech_synthesize] texte vide après nettoyage.".to_string());
    }
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

#[cfg(test)]
mod tests {
    use super::plain_text_for_speech;

    #[test]
    fn strips_common_markdown_for_speech() {
        let raw = "## Titre\n\nVoici **gras** et *italique*, un [lien](https://ex.com),\n- item un\n- item deux\n\n```rust\nfn x() {}\n```\nEt `code`.";
        let plain = plain_text_for_speech(raw);
        assert!(!plain.contains('#'), "{plain}");
        assert!(!plain.contains('*'), "{plain}");
        assert!(!plain.contains('`'), "{plain}");
        assert!(!plain.contains("https://"), "{plain}");
        assert!(plain.contains("Titre"), "{plain}");
        assert!(plain.contains("gras"), "{plain}");
        assert!(plain.contains("lien"), "{plain}");
        assert!(plain.contains("code"), "{plain}");
    }
}
