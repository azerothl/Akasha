//! Built-in CalDAV provider presets (Google, Outlook, etc.).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalDavProviderPreset {
    pub id: String,
    pub name_en: String,
    pub name_fr: String,
    /// Default CalDAV base URL; empty when the user must supply it (Nextcloud, custom).
    pub url: String,
    pub url_editable: bool,
    pub url_placeholder_en: String,
    pub url_placeholder_fr: String,
    pub username_hint_en: String,
    pub username_hint_fr: String,
    pub password_hint_en: String,
    pub password_hint_fr: String,
    pub docs_url: Option<String>,
    /// OAuth sign-in supported when operator configured client id/secret in vault.
    pub oauth_available: bool,
    pub oauth_label_en: String,
    pub oauth_label_fr: String,
}

pub fn caldav_provider_presets() -> Vec<CalDavProviderPreset> {
    vec![
        CalDavProviderPreset {
            id: "google_calendar".into(),
            name_en: "Google Calendar".into(),
            name_fr: "Google Calendar".into(),
            url: "https://apidata.googleusercontent.com/caldav/v2/".into(),
            url_editable: false,
            url_placeholder_en: String::new(),
            url_placeholder_fr: String::new(),
            username_hint_en: "Your Google account email".into(),
            username_hint_fr: "Adresse Gmail / Google".into(),
            password_hint_en: "Google App Password (Account → Security → 2-Step Verification → App passwords). CalDAV must be enabled.".into(),
            password_hint_fr: "Mot de passe d'application Google (Compte → Sécurité → Validation en 2 étapes → Mots de passe des applications).".into(),
            docs_url: Some("https://support.google.com/calendar/answer/37648".into()),
            oauth_available: true,
            oauth_label_en: "Sign in with Google".into(),
            oauth_label_fr: "Se connecter avec Google".into(),
        },
        CalDavProviderPreset {
            id: "outlook".into(),
            name_en: "Outlook / Microsoft 365".into(),
            name_fr: "Outlook / Microsoft 365".into(),
            url: "https://outlook.office365.com/caldav/".into(),
            url_editable: false,
            url_placeholder_en: String::new(),
            url_placeholder_fr: String::new(),
            username_hint_en: "Microsoft account email (Outlook.com or work/school)".into(),
            username_hint_fr: "E-mail Microsoft (Outlook.com ou pro/école)".into(),
            password_hint_en: "App password from Microsoft account security settings (if MFA is on).".into(),
            password_hint_fr: "Mot de passe d'application depuis la sécurité du compte Microsoft (si MFA activée).".into(),
            docs_url: Some("https://support.microsoft.com/en-us/office/outlook-com".into()),
            oauth_available: true,
            oauth_label_en: "Sign in with Microsoft".into(),
            oauth_label_fr: "Se connecter avec Microsoft".into(),
        },
        CalDavProviderPreset {
            id: "apple_icloud".into(),
            name_en: "Apple iCloud".into(),
            name_fr: "Apple iCloud".into(),
            url: "https://caldav.icloud.com/".into(),
            url_editable: false,
            url_placeholder_en: String::new(),
            url_placeholder_fr: String::new(),
            username_hint_en: "Apple ID email".into(),
            username_hint_fr: "Identifiant Apple (e-mail)".into(),
            password_hint_en: "App-specific password from appleid.apple.com → Sign-In and Security.".into(),
            password_hint_fr: "Mot de passe spécifique à l'app depuis appleid.apple.com → Connexion et sécurité.".into(),
            docs_url: Some("https://support.apple.com/en-us/HT204316".into()),
            oauth_available: false,
            oauth_label_en: String::new(),
            oauth_label_fr: String::new(),
        },
        CalDavProviderPreset {
            id: "fastmail".into(),
            name_en: "Fastmail".into(),
            name_fr: "Fastmail".into(),
            url: "https://caldav.fastmail.com/dav/calendars/user/".into(),
            url_editable: true,
            url_placeholder_en: "https://caldav.fastmail.com/dav/calendars/user/you@fastmail.com/".into(),
            url_placeholder_fr: "https://caldav.fastmail.com/dav/calendars/user/vous@fastmail.com/".into(),
            username_hint_en: "Fastmail email address".into(),
            username_hint_fr: "Adresse Fastmail".into(),
            password_hint_en: "App password from Fastmail Settings → Privacy & Security.".into(),
            password_hint_fr: "Mot de passe d'application : Fastmail → Paramètres → Confidentialité.".into(),
            docs_url: Some("https://www.fastmail.com/help/technical/caldav.html".into()),
            oauth_available: false,
            oauth_label_en: String::new(),
            oauth_label_fr: String::new(),
        },
        CalDavProviderPreset {
            id: "nextcloud".into(),
            name_en: "Nextcloud".into(),
            name_fr: "Nextcloud".into(),
            url: String::new(),
            url_editable: true,
            url_placeholder_en: "https://cloud.example.com/remote.php/dav".into(),
            url_placeholder_fr: "https://cloud.example.com/remote.php/dav".into(),
            username_hint_en: "Nextcloud username".into(),
            username_hint_fr: "Nom d'utilisateur Nextcloud".into(),
            password_hint_en: "Account password or app token from Nextcloud security settings.".into(),
            password_hint_fr: "Mot de passe du compte ou jeton d'application Nextcloud.".into(),
            docs_url: Some("https://docs.nextcloud.com/server/latest/user_manual/en/pim/sync_calendar.html".into()),
            oauth_available: false,
            oauth_label_en: String::new(),
            oauth_label_fr: String::new(),
        },
        CalDavProviderPreset {
            id: "custom".into(),
            name_en: "Other / custom CalDAV".into(),
            name_fr: "Autre / CalDAV personnalisé".into(),
            url: String::new(),
            url_editable: true,
            url_placeholder_en: "https://your-server.example.com/caldav/".into(),
            url_placeholder_fr: "https://votre-serveur.example.com/caldav/".into(),
            username_hint_en: "CalDAV username".into(),
            username_hint_fr: "Utilisateur CalDAV".into(),
            password_hint_en: "CalDAV password or app password from your provider.".into(),
            password_hint_fr: "Mot de passe CalDAV ou mot de passe d'application.".into(),
            docs_url: None,
            oauth_available: false,
            oauth_label_en: String::new(),
            oauth_label_fr: String::new(),
        },
    ]
}

pub fn preset_by_id(id: &str) -> Option<CalDavProviderPreset> {
    caldav_provider_presets()
        .into_iter()
        .find(|p| p.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_include_google_and_custom() {
        let presets = caldav_provider_presets();
        assert!(presets.iter().any(|p| p.id == "google_calendar"));
        assert!(presets.iter().any(|p| p.id == "outlook"));
        assert!(presets.iter().any(|p| p.id == "custom"));
        assert!(preset_by_id("google_calendar").unwrap().url.contains("googleusercontent"));
    }
}
