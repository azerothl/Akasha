//! User profile: first name, last name, how to address the user.
//! Persisted in data_dir/user_profile.json and used for greetings and personalization.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserProfile {
    /// User's first name (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    /// User's last name (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    /// How the agent should address the user (e.g. first name or nickname). Used for greetings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub how_to_call: Option<String>,
    /// Whether onboarding (collecting user name) has been completed.
    #[serde(default)]
    pub onboarding_completed: bool,
    /// If true, show a proactive check-in message when user returns after a period of inactivity.
    #[serde(default)]
    pub proactive_check_in_enabled: bool,
    /// Minimum days without user message before showing proactive check-in (0 = disabled).
    #[serde(default)]
    pub proactive_check_in_interval_days: u32,
}

const FILENAME: &str = "user_profile.json";
const LAST_ACTIVITY_FILENAME: &str = "last_activity.json";

impl UserProfile {
    /// True if we have at least how_to_call (enough to personalize greetings).
    pub fn has_how_to_call(&self) -> bool {
        self.how_to_call
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    /// True if onboarding is done (user has provided how_to_call).
    pub fn is_onboarding_completed(&self) -> bool {
        self.onboarding_completed && self.has_how_to_call()
    }

    /// Load profile from data_dir/user_profile.json. Returns default empty profile if file missing or invalid.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILENAME);
        let Ok(data) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&data).unwrap_or_default()
    }

    /// Save profile to data_dir/user_profile.json.
    pub fn save(&self, data_dir: &Path) -> Result<(), std::io::Error> {
        let path = data_dir.join(FILENAME);
        std::fs::create_dir_all(data_dir)?;
        let json = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(path, json)
    }

    /// Load last activity timestamp from data_dir/last_activity.json.
    pub fn load_last_activity(data_dir: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
        let path = data_dir.join(LAST_ACTIVITY_FILENAME);
        let data = std::fs::read_to_string(&path).ok()?;
        let v: serde_json::Value = serde_json::from_str(&data).ok()?;
        let s = v.get("last_user_message_at")?.as_str()?;
        chrono::DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&chrono::Utc))
    }

    /// Save last activity timestamp to data_dir/last_activity.json.
    pub fn save_last_activity(data_dir: &Path, at: chrono::DateTime<chrono::Utc>) -> Result<(), std::io::Error> {
        let path = data_dir.join(LAST_ACTIVITY_FILENAME);
        std::fs::create_dir_all(data_dir)?;
        let json = serde_json::json!({ "last_user_message_at": at.to_rfc3339() }).to_string();
        std::fs::write(path, json)
    }

    /// Build a line for the user identity block in the prompt (how to address the user).
    pub fn format_for_prompt(&self) -> String {
        if let Some(ref call) = self.how_to_call {
            let call = call.trim();
            if !call.is_empty() {
                let name = self
                    .first_name
                    .as_deref()
                    .unwrap_or(call)
                    .trim();
                return format!(
                    "The user's name is {}; address them as \"{}\" in greetings and when speaking to them.\n",
                    if name.is_empty() { call } else { name },
                    call
                );
            }
        }
        String::new()
    }
}
