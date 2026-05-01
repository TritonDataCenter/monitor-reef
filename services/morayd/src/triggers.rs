// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Named built-in triggers.
//!
//! Moray lets clients register arbitrary JavaScript `pre` / `post`
//! trigger bodies in the bucket config. morayd can't eval JS, so
//! instead we fingerprint each trigger string by its leading
//! identifier (`function NAME(...) { ... }`) and dispatch to a Rust
//! implementation.
//!
//! Today only two real Triton services use triggers in production, and
//! both are pure value-mutation. We implement them here:
//!
//! - [`timestamps`] — from sdc-papi's `packages` bucket. Stamps
//!   `created_at` (if absent) and `updated_at` with the current Unix
//!   time-in-ms on every write.
//! - [`fixTypes`] — from sdc-ufds's main directory bucket. Walks the
//!   object, and for every field whose bucket-schema type is `boolean`
//!   or `number` / `number[]`, coerces the LDAP-wire string values to
//!   the real scalar type. Only fires on `x-ufds-operation: add` or
//!   `modify` (those are the mutation paths that receive freshly parsed
//!   LDAP data; other operations already have typed values).
//!
//! The registry is **closed**: if a bucket's pre/post carries a
//! function we don't recognise, we hard-fail. Silent-skip would let a
//! service team register a new trigger that we'd then never run,
//! masking divergence between morayd and upstream moray. Failing loud
//! at both bucket-create time (`validate.rs`) and at put time (here)
//! forces every new trigger through a code change to this registry.

use serde_json::{Map, Value};

use crate::error::MorayError;
use crate::types::Bucket;

/// Inputs the trigger can read or mutate. Mirrors what upstream Moray
/// exposes as `req.*` inside the trigger body.
pub struct TriggerContext<'a> {
    /// The bucket's `options.index` map — the schema driving type
    /// coercion in `fixTypes`.
    pub schema: &'a Map<String, Value>,
    /// Request-scoped headers (`opts.headers` on the wire). `fixTypes`
    /// keys off `x-ufds-operation` to decide whether to coerce.
    pub headers: &'a Map<String, Value>,
}

type TriggerFn = fn(&mut Value, &TriggerContext<'_>);

/// Look up a trigger by its function-name identifier. Returns `None`
/// for unknown names — callers must treat `None` as a hard error,
/// **not** a no-op.
pub fn resolve(name: &str) -> Option<TriggerFn> {
    match name {
        "timestamps" => Some(timestamps),
        "fixTypes" => Some(fix_types),
        _ => None,
    }
}

/// Cheap registry-membership check for `validate.rs` so it doesn't
/// have to materialise the function pointer.
pub fn is_known(name: &str) -> bool {
    resolve(name).is_some()
}

/// Extract the first identifier from a function-string body
/// (`"function NAME(...) { ... }"`). Returns `None` for anonymous
/// functions, empty strings, or bodies we can't parse.
pub fn identifier_of(body: &str) -> Option<&str> {
    let trimmed = body.trim_start();
    let rest = trimmed.strip_prefix("function")?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(&rest[..end])
}

/// Scan a bucket's config for recognised triggers, returning their
/// resolved names in the order they'd fire (pre then post).
pub fn collect_from_bucket(b: &Bucket) -> Vec<String> {
    let mut out = Vec::new();
    for side in [&b.options.pre, &b.options.post] {
        for entry in side {
            let Value::String(body) = entry else { continue };
            let Some(name) = identifier_of(body) else { continue };
            if resolve(name).is_some() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Run every trigger registered on this bucket against the incoming
/// value, mutating in place. Returns `NotFunction` if any entry has no
/// parseable function identifier or names a function not in the
/// registry — the caller surfaces this to node-moray as a
/// `NotFunctionError` so the bad write fails immediately rather than
/// silently bypassing the trigger.
pub fn apply<'a>(
    bucket: &Bucket,
    value: &mut Value,
    headers: &Map<String, Value>,
) -> Result<(), MorayError> {
    let ctx = TriggerContext {
        schema: &bucket.options.index,
        headers,
    };
    for (side_name, side) in [
        ("pre", &bucket.options.pre),
        ("post", &bucket.options.post),
    ] {
        for entry in side {
            // The structural shape (string-bodied function) is enforced
            // at bucket-create / update time in `validate.rs`. Anything
            // else here is a corrupt-config invariant violation.
            let Value::String(body) = entry else {
                return Err(MorayError::NotFunction(format!(
                    "bucket '{}' {} trigger entry is not a function string",
                    bucket.name, side_name
                )));
            };
            let Some(name) = identifier_of(body) else {
                return Err(MorayError::NotFunction(format!(
                    "bucket '{}' {} trigger has no parseable function identifier; \
                     anonymous triggers are not supported (only named entries from \
                     the morayd registry can run server-side)",
                    bucket.name, side_name
                )));
            };
            let Some(f) = resolve(name) else {
                return Err(MorayError::NotFunction(format!(
                    "bucket '{}' {} trigger '{}' is not in morayd's trigger registry; \
                     add a Rust implementation in src/triggers.rs and re-deploy",
                    bucket.name, side_name, name
                )));
            };
            f(value, &ctx);
        }
    }
    Ok(())
}

// --- concrete triggers ---

/// `timestamps` — sdc-papi/lib/backend.js.
///
/// ```js
/// function timestamps(req, callback) {
///     var date = new Date().getTime();
///     if (!req.value.created_at) req.value.created_at = date;
///     req.value.updated_at = date;
///     return callback();
/// }
/// ```
fn timestamps(value: &mut Value, _ctx: &TriggerContext<'_>) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default();
    // `if (!req.value.created_at)` — treat missing OR any falsy-ish
    // value the same as missing, matching JS's truthiness coercion.
    let has_created_at = obj
        .get("created_at")
        .is_some_and(|v| !matches!(v, Value::Null | Value::Bool(false)));
    if !has_created_at {
        obj.insert("created_at".into(), Value::from(now_ms));
    }
    obj.insert("updated_at".into(), Value::from(now_ms));
}

/// `fixTypes` — sdc-ufds/lib/db/pre.js. Walks the object and, for each
/// schema entry whose `type` is `boolean`, `number`, or `number[]`,
/// coerces the LDAP-wire string values (always arrays per LDAP RFC) to
/// the real scalar type. Only fires on `add` / `modify` ops — other
/// ops already carry typed values.
fn fix_types(value: &mut Value, ctx: &TriggerContext<'_>) {
    let op = ctx
        .headers
        .get("x-ufds-operation")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if op != "add" && op != "modify" {
        return;
    }
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    // Collect keys up front so we can mutate the map while iterating.
    let keys: Vec<String> = obj.keys().cloned().collect();
    for k in keys {
        let Some(schema_entry) = ctx.schema.get(&k) else {
            continue;
        };
        let Some(ty) = schema_entry.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(vs) = obj.get(&k).and_then(|v| v.as_array()) else {
            continue;
        };
        let coerced: Option<Vec<Value>> = match ty {
            "boolean" => Some(
                vs.iter()
                    .map(|s| {
                        let s = s.as_str().unwrap_or("");
                        // `/true/i.test(s)` — case-insensitive "true"
                        // substring match. Anything else → false.
                        Value::from(s.to_ascii_lowercase().contains("true"))
                    })
                    .collect(),
            ),
            "number" | "number[]" => Some(
                vs.iter()
                    .map(|s| {
                        let raw = s.as_str().unwrap_or("");
                        // node-moray uses parseInt(s, 10) — takes the
                        // leading integer prefix, NaN for empty / non-
                        // numeric. We map NaN to `null` since JSON has
                        // no NaN representation.
                        let leading: String = raw
                            .chars()
                            .take_while(|c| c.is_ascii_digit() || *c == '-' || *c == '+')
                            .collect();
                        leading
                            .parse::<i64>()
                            .map(Value::from)
                            .unwrap_or(Value::Null)
                    })
                    .collect(),
            ),
            _ => None,
        };
        if let Some(new_values) = coerced {
            obj.insert(k, Value::Array(new_values));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx_with<'a>(
        schema: &'a Map<String, Value>,
        headers: &'a Map<String, Value>,
    ) -> TriggerContext<'a> {
        TriggerContext { schema, headers }
    }

    #[test]
    fn identifier_parses_function_names() {
        assert_eq!(
            identifier_of("function timestamps(req, cb) { return cb(); }"),
            Some("timestamps")
        );
        assert_eq!(identifier_of("  function fixTypes(r){}"), Some("fixTypes"));
        assert_eq!(identifier_of("function  () { }"), None);
        assert_eq!(identifier_of("fixTypes"), None);
    }

    #[test]
    fn timestamps_stamps_both_on_create() {
        let mut v = json!({"name": "small"});
        let schema = Map::new();
        let headers = Map::new();
        timestamps(&mut v, &ctx_with(&schema, &headers));
        assert!(v["created_at"].as_u64().is_some());
        assert!(v["updated_at"].as_u64().is_some());
    }

    #[test]
    fn timestamps_preserves_existing_created_at() {
        let mut v = json!({"created_at": 1234u64, "name": "small"});
        let schema = Map::new();
        let headers = Map::new();
        timestamps(&mut v, &ctx_with(&schema, &headers));
        assert_eq!(v["created_at"].as_u64(), Some(1234));
        assert!(v["updated_at"].as_u64().unwrap() > 1234);
    }

    #[test]
    fn fix_types_coerces_on_add() {
        let schema: Map<String, Value> = [
            ("disabled".to_string(), json!({"type": "boolean"})),
            ("age".to_string(), json!({"type": "number"})),
        ]
        .into_iter()
        .collect();
        let headers: Map<String, Value> =
            [("x-ufds-operation".to_string(), json!("add"))]
                .into_iter()
                .collect();
        let mut v = json!({"disabled": ["true"], "age": ["42"], "name": ["alice"]});
        fix_types(&mut v, &ctx_with(&schema, &headers));
        assert_eq!(v["disabled"], json!([true]));
        assert_eq!(v["age"], json!([42]));
        // unchanged because no schema entry
        assert_eq!(v["name"], json!(["alice"]));
    }

    fn bucket_with_pre(pre: Vec<Value>) -> Bucket {
        use crate::types::BucketConfig;
        Bucket {
            name: "t".into(),
            id: uuid::Uuid::nil(),
            options: BucketConfig {
                index: Map::new(),
                pre,
                post: Vec::new(),
                options: Map::new(),
            },
            mtime: chrono::Utc::now(),
            reindex_active: None,
            reindex_just_finished: false,
        }
    }

    #[test]
    fn apply_rejects_unknown_named_trigger() {
        let bucket = bucket_with_pre(vec![json!(
            "function makeCoffee(req, cb) { cb(); }"
        )]);
        let mut v = json!({"x": 1});
        let headers = Map::new();
        let err = apply(&bucket, &mut v, &headers).unwrap_err();
        assert!(matches!(err, MorayError::NotFunction(_)), "got {err:?}");
    }

    #[test]
    fn apply_rejects_anonymous_trigger() {
        let bucket = bucket_with_pre(vec![json!("function (req, cb) { cb(); }")]);
        let mut v = json!({"x": 1});
        let headers = Map::new();
        let err = apply(&bucket, &mut v, &headers).unwrap_err();
        match err {
            MorayError::NotFunction(msg) => assert!(msg.contains("anonymous"), "{msg}"),
            other => panic!("expected NotFunction, got {other:?}"),
        }
    }

    #[test]
    fn apply_runs_known_triggers() {
        let bucket = bucket_with_pre(vec![json!(
            "function timestamps(req, cb) { cb(); }"
        )]);
        let mut v = json!({"name": "p"});
        let headers = Map::new();
        apply(&bucket, &mut v, &headers).unwrap();
        assert!(v["updated_at"].as_u64().is_some());
    }

    #[test]
    fn fix_types_skips_other_ops() {
        let schema: Map<String, Value> =
            [("disabled".to_string(), json!({"type": "boolean"}))]
                .into_iter()
                .collect();
        let headers: Map<String, Value> =
            [("x-ufds-operation".to_string(), json!("search"))]
                .into_iter()
                .collect();
        let mut v = json!({"disabled": ["true"]});
        fix_types(&mut v, &ctx_with(&schema, &headers));
        // untouched — still strings
        assert_eq!(v["disabled"], json!(["true"]));
    }
}
