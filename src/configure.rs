//! Helpers for applying the proxy's Configure event.
//!
//! The proxy sends an agent's `config { }` block on every connection: once per
//! pooled connection at startup and again on every reconnect. Applying it must
//! therefore be idempotent, must not throw away settings the block does not
//! mention, and must never be logged in full (it can carry secrets).

use serde_json::{Map, Value};

/// Returns `value` with every object's keys in sorted order, so two configs
/// that differ only in key order serialize (and hash) identically.
pub fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), canonicalize(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

/// A stable 64-bit hash (FNV-1a) of the canonical JSON form of `value`.
///
/// Stable across processes and releases, so it can be logged and compared.
pub fn config_hash(value: &Value) -> String {
    let canonical = canonicalize(value).to_string();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in canonical.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Overlays `overlay` onto `base`: objects merge key by key, any other value
/// replaces what `base` had. Keys `overlay` does not mention keep their value.
///
/// The proxy turns a KDL node with a single argument into a scalar, so a
/// one-item list arrives as a string; where `base` holds a list, a scalar is
/// taken as a one-item list.
pub fn merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base @ Value::Array(_), value) if !value.is_array() && !value.is_null() => {
            *base = Value::Array(vec![value.clone()]);
        }
        (base, value) => *base = value.clone(),
    }
}

/// Dotted paths of the settings that differ between `old` and `new`, for
/// logging which keys a configure changed without logging their values.
pub fn changed_keys(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    diff(old, new, String::new(), &mut out);
    out
}

fn diff(old: &Value, new: &Value, prefix: String, out: &mut Vec<String>) {
    match (old, new) {
        (Value::Object(a), Value::Object(b)) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match (a.get(k), b.get(k)) {
                    (Some(x), Some(y)) => diff(x, y, path, out),
                    _ => out.push(path),
                }
            }
        }
        (a, b) if a != b => out.push(if prefix.is_empty() {
            "<root>".to_string()
        } else {
            prefix
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hash_ignores_key_order() {
        let a = json!({"a": 1, "b": {"x": true, "y": [1, 2]}});
        let b = json!({"b": {"y": [1, 2], "x": true}, "a": 1});
        assert_eq!(config_hash(&a), config_hash(&b));
        assert_ne!(config_hash(&a), config_hash(&json!({"a": 2})));
    }

    #[test]
    fn merge_keeps_absent_keys_and_wraps_scalars_into_lists() {
        let mut base = json!({"a": 1, "list": ["x", "y"], "nested": {"k": 1, "j": 2}});
        merge(&mut base, &json!({"list": "z", "nested": {"k": 5}}));
        assert_eq!(
            base,
            json!({"a": 1, "list": ["z"], "nested": {"k": 5, "j": 2}})
        );
    }

    #[test]
    fn changed_keys_names_paths_only() {
        let old = json!({"a": 1, "n": {"secret": "s1", "same": 1}});
        let new = json!({"a": 1, "n": {"secret": "s2", "same": 1}});
        assert_eq!(changed_keys(&old, &new), vec!["n.secret".to_string()]);
    }
}
