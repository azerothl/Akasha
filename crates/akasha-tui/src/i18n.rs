//! TUI i18n: locale detection (AKASHA_LANG / LANG) and embedded translations (fr.json, en.json).

use std::collections::HashMap;
use std::sync::Arc;

/// Detect locale from environment. Returns "en" if LANG/LC_ALL/AKASHA_LANG starts with "en", else "fr".
pub fn detect_locale() -> &'static str {
    if let Ok(lang) = std::env::var("AKASHA_LANG") {
        if lang.trim().to_lowercase().starts_with("en") {
            return "en";
        }
        return "fr";
    }
    for key in ["LANG", "LC_ALL", "LC_MESSAGES"] {
        if let Ok(lang) = std::env::var(key) {
            if lang.trim().to_lowercase().starts_with("en") {
                return "en";
            }
            break;
        }
    }
    "fr"
}

/// Load translations for the given locale. Uses embedded JSON (include_str!).
pub fn load(locale: &str) -> I18n {
    let json_str = match locale {
        "en" => include_str!("../locales/en.json"),
        _ => include_str!("../locales/fr.json"),
    };
    let map: HashMap<String, String> = serde_json::from_str(json_str).unwrap_or_default();
    I18n {
        map: Arc::new(map),
    }
}

/// Translations: key -> string for the current locale.
#[derive(Clone)]
pub struct I18n {
    map: Arc<HashMap<String, String>>,
}

impl I18n {
    /// Return the translation for `key`, or the key itself if missing.
    pub fn t(&self, key: &str) -> String {
        self.map
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string())
    }
}
