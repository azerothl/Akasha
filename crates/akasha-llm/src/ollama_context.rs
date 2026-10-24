//! Native capacity and allocated context are different: never allocate the native maximum implicitly.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OllamaContextLimits {
    pub native_max: Option<u32>,
    pub model_default: Option<u32>,
    pub running_context: Option<u32>,
}

fn positive(value: Option<u64>) -> Option<u32> {
    value
        .filter(|value| *value > 0)
        .map(|value| value.min(u32::MAX as u64) as u32)
}

fn same_model(left: &str, right: &str) -> bool {
    left == right
        || left.strip_suffix(":latest").unwrap_or(left)
            == right.strip_suffix(":latest").unwrap_or(right)
}

impl OllamaContextLimits {
    pub fn from_metadata(show: &Value, running: &Value, model: &str) -> Self {
        let info = &show["model_info"];
        let architecture = info["general.architecture"].as_str();
        let native_max = architecture
            .and_then(|name| positive(info[format!("{name}.context_length")].as_u64()))
            .or_else(|| {
                // Older servers may omit architecture. Choose conservatively when ambiguous.
                info.as_object()?
                    .iter()
                    .filter(|(key, _)| key.ends_with(".context_length"))
                    .filter_map(|(_, value)| positive(value.as_u64()))
                    .min()
            });
        let model_default = show["parameters"].as_str().and_then(|text| {
            text.lines().find_map(|line| {
                let mut parts = line.split_whitespace();
                if parts.next()? != "num_ctx" {
                    return None;
                }
                positive(parts.next()?.parse().ok())
            })
        });
        let running_context = running["models"].as_array().and_then(|models| {
            models.iter().find_map(|entry| {
                let matches = [entry["name"].as_str(), entry["model"].as_str()]
                    .into_iter()
                    .flatten()
                    .any(|name| same_model(name, model));
                matches
                    .then(|| positive(entry["context_length"].as_u64()))
                    .flatten()
            })
        });
        Self {
            native_max,
            model_default,
            running_context,
        }
    }

    pub fn effective(&self, requested: Option<u32>) -> (u32, &'static str) {
        let (value, source) = if let Some(value) = requested.filter(|value| *value > 0) {
            (value, "configured")
        } else if let Some(value) = self.running_context {
            (value, "running")
        } else if let Some(value) = self.model_default {
            (value, "model_default")
        } else {
            (8192, "conservative_default")
        };
        (
            self.native_max.map_or(value, |maximum| value.min(maximum)),
            source,
        )
    }
}

async fn metadata(response: Result<reqwest::Response, reqwest::Error>) -> Option<Value> {
    let mut response = response.ok()?.error_for_status().ok()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > 8 * 1024 * 1024 {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).ok()
}

pub async fn discover(base_url: &str, model: &str, timeout: Duration) -> OllamaContextLimits {
    let client = reqwest::Client::new();
    let timeout = timeout.min(Duration::from_secs(2));
    let base = base_url.trim_end_matches('/');
    let (show, running) = tokio::join!(
        async {
            metadata(
                client
                    .post(format!("{base}/api/show"))
                    .json(&serde_json::json!({"model":model,"verbose":false}))
                    .timeout(timeout)
                    .send()
                    .await,
            )
            .await
        },
        async {
            metadata(
                client
                    .get(format!("{base}/api/ps"))
                    .timeout(timeout)
                    .send()
                    .await,
            )
            .await
        }
    );
    OllamaContextLimits::from_metadata(
        &show.unwrap_or(Value::Null),
        &running.unwrap_or(Value::Null),
        model,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn native_limit_does_not_replace_the_allocated_window() {
        let limits = OllamaContextLimits::from_metadata(
            &json!({"model_info":{"general.architecture":"gemma","gemma.context_length":131072,"gemma.vision.context_length":1024},"parameters":"num_ctx 8192"}),
            &json!({"models":[{"name":"gemma:latest","context_length":32768}]}),
            "gemma",
        );
        assert_eq!(limits.native_max, Some(131072));
        assert_eq!(limits.effective(None), (32768, "running"));
        assert_eq!(limits.effective(Some(4096)), (4096, "configured"));
        assert_eq!(limits.effective(Some(262144)).0, 131072);
    }
    #[test]
    fn missing_metadata_and_other_model_allocations_are_conservative() {
        let limits = OllamaContextLimits::from_metadata(
            &json!({}),
            &json!({"models":[{"name":"qwen:other","context_length":65536}]}),
            "qwen",
        );
        assert_eq!(limits.effective(None), (8192, "conservative_default"));
        let limits = OllamaContextLimits::from_metadata(
            &json!({"parameters":"num_ctx_extra 99999\nnum_ctx 4096"}),
            &Value::Null,
            "qwen",
        );
        assert_eq!(limits.effective(None), (4096, "model_default"));
    }
}
