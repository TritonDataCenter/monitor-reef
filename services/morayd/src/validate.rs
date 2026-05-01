// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Bucket name and config validation. Mirrors what upstream Moray enforces
//! (see `moray/lib/buckets.js` + `moray/lib/schema.js`) so the moray-test-
//! suite's `invalid-buckets.test.js` sees the same error names/messages.

use serde_json::Value;

use crate::error::MorayError;

/// Types Moray accepts as index column types. Each may also appear
/// wrapped in brackets: `[string]`, `[number]`, etc., for array-valued
/// columns.
const INDEX_SCALAR_TYPES: &[&str] = &[
    "string",
    "number",
    "boolean",
    "date",
    "ip",
    "mac",
    "uuid",
    "subnet",
    "numrange",
    "daterange",
];

/// Reserved bucket names Moray refuses.
const RESERVED_NAMES: &[&str] = &["moray", "search", "buckets_config"];

/// Maximum bucket name length. Matches upstream schema.
const MAX_BUCKET_NAME_LEN: usize = 63;

/// Validate a bucket name per Moray's rules:
///
/// - Non-empty (else `InvocationError`).
/// - Starts with an ASCII letter.
/// - Contains only `[A-Za-z0-9_]` (no hyphens).
/// - Length ≤ 63.
/// - Not one of the reserved names.
pub fn bucket_name(name: &str) -> Result<(), MorayError> {
    if name.is_empty() {
        return Err(MorayError::Invocation(
            "createBucket expects \"bucket\" (args[0]) to be a nonempty string: \
             bucket should NOT be shorter than 1 characters"
                .into(),
        ));
    }
    if name.len() > MAX_BUCKET_NAME_LEN
        || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || RESERVED_NAMES.contains(&name)
    {
        return Err(MorayError::InvalidBucketName(name.into()));
    }
    Ok(())
}

/// Validate the shape of a bucket config *before* we deserialize it. We
/// look at the raw JSON so we can report the exact type-level error Moray
/// does (the upstream messages come from ajv's JSON-schema failures;
/// ours match the text byte-for-byte where feasible).
pub fn bucket_config(raw: &Value) -> Result<(), MorayError> {
    let obj = match raw {
        Value::Null => return Ok(()),
        Value::Object(o) => o,
        _ => {
            return Err(MorayError::InvalidBucketConfig(
                "bucket should be object".into(),
            ))
        }
    };

    if let Some(pre) = obj.get("pre") {
        validate_triggers(pre, "pre")?;
    }
    if let Some(post) = obj.get("post") {
        validate_triggers(post, "post")?;
    }
    if let Some(index) = obj.get("index") {
        validate_index(index)?;
    }
    if let Some(options) = obj.get("options") {
        validate_options(options)?;
    }
    Ok(())
}

fn validate_options(v: &Value) -> Result<(), MorayError> {
    let obj = match v {
        Value::Null => return Ok(()),
        Value::Object(o) => o,
        _ => {
            return Err(MorayError::InvalidBucketConfig(
                "bucket.options should be object".into(),
            ))
        }
    };
    if obj.contains_key("trackModification") {
        return Err(MorayError::InvalidBucketConfig(
            "bucket.options.trackModification is no longer supported".into(),
        ));
    }
    if let Some(ver) = obj.get("version") {
        match ver {
            Value::Number(n) => {
                if !n.is_i64() && !n.is_u64() {
                    return Err(MorayError::InvalidBucketConfig(
                        "bucket.options.version should be integer".into(),
                    ));
                }
                let i = n.as_i64().unwrap_or(0);
                if i < 0 {
                    return Err(MorayError::InvalidBucketConfig(
                        "bucket.options.version should be >= 0".into(),
                    ));
                }
            }
            Value::Null => {}
            _ => {
                return Err(MorayError::InvalidBucketConfig(
                    "bucket.options.version should be integer".into(),
                ))
            }
        }
    }
    Ok(())
}

fn validate_triggers(v: &Value, field: &str) -> Result<(), MorayError> {
    let arr = match v {
        Value::Null => return Ok(()),
        Value::Array(a) => a,
        _ => {
            return Err(MorayError::InvalidBucketConfig(format!(
                "bucket.{field} should be array"
            )))
        }
    };
    for entry in arr {
        let s = match entry {
            Value::String(s) => s,
            _ => {
                return Err(MorayError::NotFunction(
                    "trigger not function must be [Function]".into(),
                ))
            }
        };
        // Moray's eval-based parseFunctor decides "function or not" with
        // a two-stage fallback. We mirror it so the error messages match
        // upstream byte-for-byte:
        //
        //   - If the string starts with `function`, it's accepted.
        //   - If it parses as some *other* JS expression (a literal —
        //     string, number, object, array), the error is the
        //     field-specific `"<field> must be [Function]"`.
        //   - If eval would *throw* (bare identifier like `hello`), the
        //     error is the generic `"trigger not function must be
        //     [Function]"`.
        let trimmed = s.trim_start();
        if trimmed.starts_with("function") {
            // Beyond shape: morayd has a closed registry of named
            // triggers, so reject anything we wouldn't actually run.
            // This is morayd-specific (upstream moray would just eval
            // the body) — failing at registration time gives the
            // service a clear signal at first contact instead of
            // silently dropping triggers on every put.
            ensure_trigger_in_registry(s, field)?;
            continue;
        }
        let looks_like_js_value = trimmed
            .chars()
            .next()
            .is_some_and(|c| matches!(c, '"' | '\'' | '[' | '{' | '0'..='9' | '-'));
        if looks_like_js_value {
            return Err(MorayError::NotFunction(format!(
                "{field} must be [Function]"
            )));
        }
        return Err(MorayError::NotFunction(
            "trigger not function must be [Function]".into(),
        ));
    }
    Ok(())
}

/// Reject any trigger function-string whose identifier isn't in
/// `triggers::resolve`. Anonymous bodies (`function (req, cb) {…}`) and
/// unknown names both fail here.
fn ensure_trigger_in_registry(body: &str, field: &str) -> Result<(), MorayError> {
    match crate::triggers::identifier_of(body) {
        Some(name) if crate::triggers::is_known(name) => Ok(()),
        Some(name) => Err(MorayError::NotFunction(format!(
            "bucket.{field}: trigger '{name}' is not in morayd's named-trigger \
             registry; add a Rust implementation in src/triggers.rs and re-deploy"
        ))),
        None => Err(MorayError::NotFunction(format!(
            "bucket.{field}: anonymous triggers are not supported by morayd; \
             only named functions whose identifier resolves in the trigger registry"
        ))),
    }
}

fn validate_index(v: &Value) -> Result<(), MorayError> {
    let obj = match v {
        Value::Object(o) => o,
        _ => {
            return Err(MorayError::InvalidBucketConfig(
                "bucket.index should be object".into(),
            ))
        }
    };
    for (name, val) in obj.iter() {
        let entry = match val {
            Value::Object(e) => e,
            _ => {
                return Err(MorayError::InvalidBucketConfig(format!(
                    "bucket.index['{name}'] should be object"
                )))
            }
        };
        let ty = match entry.get("type") {
            Some(Value::String(s)) => s.as_str(),
            Some(_) => {
                return Err(MorayError::InvalidBucketConfig(format!(
                    "bucket.index['{name}'].type should be string"
                )))
            }
            None => {
                return Err(MorayError::InvalidBucketConfig(format!(
                    "bucket.index['{name}'] should have required property 'type'"
                )))
            }
        };
        if !is_valid_index_type(ty) {
            return Err(MorayError::InvalidBucketConfig(format!(
                "bucket.index['{name}'].type should be equal to one of the \
                 allowed values"
            )));
        }
        // `unique` must be boolean if present.
        if let Some(u) = entry.get("unique")
            && !matches!(u, Value::Bool(_))
        {
            return Err(MorayError::InvalidBucketConfig(format!(
                "bucket.index['{name}'].unique should be boolean"
            )));
        }
        // Reject unknown keys per upstream's strict schema.
        for k in entry.keys() {
            if !matches!(k.as_str(), "type" | "unique") {
                return Err(MorayError::InvalidBucketConfig(format!(
                    "bucket.index['{name}'] should NOT have additional properties"
                )));
            }
        }
    }
    Ok(())
}

fn is_valid_index_type(ty: &str) -> bool {
    if INDEX_SCALAR_TYPES.contains(&ty) {
        return true;
    }
    // Array variant: "[string]", "[number]", etc.
    if let Some(inner) = ty.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
        && INDEX_SCALAR_TYPES.contains(&inner)
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_names() {
        bucket_name("users").unwrap();
        bucket_name("Foo_Bar123").unwrap();
    }

    #[test]
    fn empty_name_is_invocation_error() {
        assert!(matches!(
            bucket_name(""),
            Err(MorayError::Invocation(_))
        ));
    }

    #[test]
    fn bad_names() {
        for n in &["moray", "search", "buckets_config", "1foo", "a-b", "_x"] {
            assert!(matches!(
                bucket_name(n),
                Err(MorayError::InvalidBucketName(_))
            ), "{n}");
        }
        let long = "a".repeat(64);
        assert!(matches!(
            bucket_name(&long),
            Err(MorayError::InvalidBucketName(_))
        ));
    }

    #[test]
    fn bucket_config_bad_pre() {
        assert!(matches!(
            bucket_config(&json!({"pre": "hello"})),
            Err(MorayError::InvalidBucketConfig(_))
        ));
        assert!(matches!(
            bucket_config(&json!({"pre": ["hello"]})),
            Err(MorayError::NotFunction(_))
        ));
        // Named function that's in the registry — accepted.
        bucket_config(&json!({
            "pre": ["function timestamps(req,cb){cb()}"],
        }))
        .unwrap();
    }

    #[test]
    fn bucket_config_rejects_anonymous_trigger() {
        // Anonymous body (no identifier) — must reject.
        let err = bucket_config(&json!({"pre": ["function(r,cb){cb()}"]})).unwrap_err();
        match err {
            MorayError::NotFunction(msg) => {
                assert!(msg.contains("anonymous"), "got: {msg}");
            }
            other => panic!("expected NotFunction, got {other:?}"),
        }
    }

    #[test]
    fn bucket_config_rejects_unknown_trigger() {
        // Named function that's NOT in morayd's registry — must reject
        // at registration time so we never silently drop the trigger
        // on subsequent puts.
        let err = bucket_config(&json!({
            "post": ["function makeCoffee(req,cb){cb()}"],
        }))
        .unwrap_err();
        match err {
            MorayError::NotFunction(msg) => {
                assert!(msg.contains("makeCoffee"), "got: {msg}");
                assert!(msg.contains("registry"), "got: {msg}");
            }
            other => panic!("expected NotFunction, got {other:?}"),
        }
    }

    #[test]
    fn bucket_config_accepts_known_triggers() {
        bucket_config(&json!({
            "pre":  ["function fixTypes(req,cb){cb()}"],
            "post": ["function timestamps(req,cb){cb()}"],
        }))
        .unwrap();
    }

    #[test]
    fn bucket_config_bad_index() {
        assert!(matches!(
            bucket_config(&json!({"index": 5})),
            Err(MorayError::InvalidBucketConfig(_))
        ));
        assert!(matches!(
            bucket_config(&json!({"index": {"foo": null}})),
            Err(MorayError::InvalidBucketConfig(_))
        ));
        assert!(matches!(
            bucket_config(&json!({"index": {"foo": {}}})),
            Err(MorayError::InvalidBucketConfig(_))
        ));
        assert!(matches!(
            bucket_config(&json!({"index": {"foo": {"type": "bogus"}}})),
            Err(MorayError::InvalidBucketConfig(_))
        ));
        bucket_config(&json!({"index": {"foo": {"type": "string"}}})).unwrap();
        bucket_config(&json!({"index": {"foo": {"type": "[string]"}}})).unwrap();
    }
}
