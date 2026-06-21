//! Built-in service discovery profiles.

use super::types::{HttpProbeSpec, ServiceProfile};

pub const OLLAMA: ServiceProfile = ServiceProfile {
    id: "ollama",
    display_name: "Ollama",
    port: 11434,
    local_urls: &[
        "http://127.0.0.1:11434",
        "http://localhost:11434",
        "http://[::1]:11434",
    ],
    probe: HttpProbeSpec {
        path: "/api/tags",
        expect_status: 200,
        body_contains: None,
    },
    install_url: Some("https://ollama.com/download"),
};

pub const HOMEASSISTANT: ServiceProfile = ServiceProfile {
    id: "homeassistant",
    display_name: "Home Assistant",
    port: 8123,
    local_urls: &[
        "http://127.0.0.1:8123",
        "http://localhost:8123",
        "http://homeassistant.local:8123",
        "http://[::1]:8123",
    ],
    probe: HttpProbeSpec {
        path: "/api/",
        expect_status: 200,
        body_contains: Some("API running"),
    },
    install_url: Some("https://www.home-assistant.io/installation/"),
};

static PROFILES: &[ServiceProfile] = &[OLLAMA, HOMEASSISTANT];

pub fn list_profiles() -> &'static [ServiceProfile] {
    PROFILES
}

pub fn profile(id: &str) -> Option<&'static ServiceProfile> {
    PROFILES.iter().find(|p| p.id == id)
}
