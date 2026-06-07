//! Catalog of device interfaces and discoverable devices for settings UI and device_discover.

use serde_json::{json, Value};

/// JSON catalog: interfaces grouped by category (local, network, usb, other).
pub fn device_interface_catalog() -> Value {
    json!({
        "interfaces": [
            {
                "id": "local_media",
                "category": "local",
                "supported": true,
                "devices": [
                    { "id": "camera", "name": "Camera" },
                    { "id": "microphone", "name": "Microphone" },
                    { "id": "speaker", "name": "Speaker" }
                ]
            },
            {
                "id": "synthetic_input",
                "category": "local",
                "supported": true,
                "devices": [
                    { "id": "keyboard", "name": "Keyboard" },
                    { "id": "mouse", "name": "Mouse" }
                ]
            },
            {
                "id": "system",
                "category": "local",
                "supported": true,
                "devices": [
                    { "id": "printer", "name": "System printers" }
                ]
            },
            {
                "id": "network",
                "category": "network",
                "supported": false,
                "devices": []
            },
            {
                "id": "usb",
                "category": "usb",
                "supported": false,
                "devices": []
            },
            {
                "id": "*",
                "category": "other",
                "supported": true,
                "devices": []
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_local_network_usb() {
        let cat = device_interface_catalog();
        let ifaces = cat["interfaces"].as_array().expect("interfaces array");
        let categories: Vec<_> = ifaces
            .iter()
            .filter_map(|i| i.get("category").and_then(|c| c.as_str()))
            .collect();
        assert!(categories.contains(&"local"));
        assert!(categories.contains(&"network"));
        assert!(categories.contains(&"usb"));
    }
}
