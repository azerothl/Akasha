//! Deep merge: fill missing keys from an example document while preserving user values.

use serde_json::Value as JsonValue;
use serde_yaml::Value as YamlValue;

/// Merge `example` into `user`: keys present in `user` win (recursively); missing keys are taken from `example`.
/// For YAML arrays: if `user` has a non-empty sequence at a node, it replaces; empty sequence uses `example`.
pub fn merge_yaml_fill_missing(example: YamlValue, user: YamlValue) -> YamlValue {
    match (example, user) {
        (ex, YamlValue::Null) => ex,
        (YamlValue::Mapping(ex_map), YamlValue::Mapping(mut us_map)) => {
            for (k, v_ex) in ex_map {
                match us_map.get(&k) {
                    None => {
                        us_map.insert(k, v_ex);
                    }
                    Some(v_us) => {
                        let merged = merge_yaml_fill_missing(v_ex, v_us.clone());
                        us_map.insert(k, merged);
                    }
                }
            }
            YamlValue::Mapping(us_map)
        }
        (YamlValue::Sequence(ex_seq), YamlValue::Sequence(us_seq)) => {
            if us_seq.is_empty() {
                YamlValue::Sequence(ex_seq)
            } else {
                YamlValue::Sequence(us_seq)
            }
        }
        (_, us) => us,
    }
}

/// Same semantics for JSON (agent profile, etc.).
pub fn merge_json_fill_missing(example: JsonValue, user: JsonValue) -> JsonValue {
    match (example, user) {
        (ex, JsonValue::Null) => ex,
        (JsonValue::Object(ex_map), JsonValue::Object(mut us_map)) => {
            for (k, v_ex) in ex_map {
                match us_map.get(&k) {
                    None => {
                        us_map.insert(k, v_ex);
                    }
                    Some(v_us) => {
                        let merged = merge_json_fill_missing(v_ex, v_us.clone());
                        us_map.insert(k, merged);
                    }
                }
            }
            JsonValue::Object(us_map)
        }
        (JsonValue::Array(ex_arr), JsonValue::Array(us_arr)) => {
            if us_arr.is_empty() {
                JsonValue::Array(ex_arr)
            } else {
                JsonValue::Array(us_arr)
            }
        }
        (_, us) => us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_fills_missing_top_level() {
        let ex: serde_yaml::Value = serde_yaml::from_str("a: 1\nb: 2\n").unwrap();
        let us: serde_yaml::Value = serde_yaml::from_str("a: 9\n").unwrap();
        let m = merge_yaml_fill_missing(ex, us);
        let s = serde_yaml::to_string(&m).unwrap();
        assert!(s.contains("a: 9") && s.contains("b: 2"));
    }

    #[test]
    fn yaml_nested_merge() {
        let ex: serde_yaml::Value = serde_yaml::from_str("outer:\n  x: 1\n  y: 2\n").unwrap();
        let us: serde_yaml::Value = serde_yaml::from_str("outer:\n  x: 99\n").unwrap();
        let m = merge_yaml_fill_missing(ex, us);
        let s = serde_yaml::to_string(&m).unwrap();
        assert!(s.contains("x: 99") && s.contains("y: 2"));
    }

    #[test]
    fn json_merge() {
        let ex = serde_json::json!({"name":"A","rules":[],"extra":1});
        let us = serde_json::json!({"name":"B"});
        let m = merge_json_fill_missing(ex, us);
        assert_eq!(m["name"], "B");
        assert_eq!(m["rules"], serde_json::json!([]));
        assert_eq!(m["extra"], 1);
    }
}
