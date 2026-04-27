// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Moray column-type validation for filter literals and object values.
//!
//! Moray indexed columns have typed schemas — `string`, `number`, `boolean`,
//! `date`, `ip`, `mac`, `uuid`, `subnet`, `numrange`, `daterange` — and the
//! server rejects filter clauses whose RHS isn't a legal literal for the
//! indexed column's type (e.g. `(mac=foo)` errors with `InvalidQueryError`).
//!
//! We implement lightweight parsers for each scalar type so we can refuse
//! bad input the same way the upstream server does.

use serde_json::Value;

/// Scalar index types Moray recognises, plus their array-of variants.
/// `type_str` is what appears in the bucket config, e.g. `"string"` or
/// `"[string]"`.
pub struct IndexType<'a> {
    pub raw: &'a str,
    /// None for plain scalar, Some for array-of variants like `[string]`.
    pub element: Option<&'a str>,
}

impl<'a> IndexType<'a> {
    pub fn parse(raw: &'a str) -> Option<IndexType<'a>> {
        if let Some(inner) = raw.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            Some(IndexType { raw, element: Some(inner) })
        } else {
            Some(IndexType { raw, element: None })
        }
    }

    pub fn scalar_type(&self) -> &'a str {
        self.element.unwrap_or(self.raw)
    }

    pub fn is_array(&self) -> bool {
        self.element.is_some()
    }
}

/// Validate that `literal` is a legal value for an indexed column of the
/// given type. Returns Err with a short reason string on failure.
pub fn validate_scalar_literal(scalar_ty: &str, literal: &str) -> Result<(), String> {
    match scalar_ty {
        "string" => Ok(()),
        "boolean" => match literal {
            "true" | "false" => Ok(()),
            _ => Err(format!("expected boolean, got {literal:?}")),
        },
        "number" => literal
            .parse::<f64>()
            .map(|_| ())
            .map_err(|_| format!("expected number, got {literal:?}")),
        "date" => parse_iso8601(literal)
            .map(|_| ())
            .map_err(|e| format!("expected date, got {literal:?}: {e}")),
        "ip" => parse_ip(literal)
            .map(|_| ())
            .map_err(|e| format!("expected ip, got {literal:?}: {e}")),
        "mac" => parse_mac(literal)
            .map(|_| ())
            .map_err(|e| format!("expected mac, got {literal:?}: {e}")),
        "uuid" => parse_uuid(literal)
            .map(|_| ())
            .map_err(|e| format!("expected uuid, got {literal:?}: {e}")),
        "subnet" => parse_subnet(literal)
            .map(|_| ())
            .map_err(|e| format!("expected subnet, got {literal:?}: {e}")),
        "numrange" => parse_range(literal, |s| {
            s.parse::<f64>().map(|_| ()).map_err(|_| "not a number".into())
        })
        .map_err(|e| format!("expected numrange, got {literal:?}: {e}")),
        "daterange" => parse_range(literal, |s| {
            parse_iso8601(s).map(|_| ()).map_err(|e| e.to_string())
        })
        .map_err(|e| format!("expected daterange, got {literal:?}: {e}")),
        other => Err(format!("unknown index type: {other}")),
    }
}

/// Array-typed columns accept both scalar and array values on the put
/// side. Scalars get coerced to a single-element array. For types with
/// a canonical string form (`ip`, `subnet`) each element is also
/// canonicalised so downstream equality checks don't miss `fd00::045`
/// vs `fd00::45`.
pub fn coerce_for_put(ty: &IndexType<'_>, v: Value) -> Value {
    let canonicalise = |item: Value| canonicalise_for_type(ty.scalar_type(), item);
    if !ty.is_array() {
        return canonicalise(v);
    }
    match v {
        Value::Array(items) => Value::Array(items.into_iter().map(canonicalise).collect()),
        Value::Null => Value::Null,
        scalar => Value::Array(vec![canonicalise(scalar)]),
    }
}

fn canonicalise_for_type(ty: &str, v: Value) -> Value {
    match (ty, v) {
        ("ip", Value::String(s)) => match s.parse::<std::net::IpAddr>() {
            Ok(ip) => Value::String(ip.to_string()),
            Err(_) => Value::String(s),
        },
        ("subnet", Value::String(s)) => match canonicalise_subnet(&s) {
            Some(out) => Value::String(out),
            None => Value::String(s),
        },
        (_, v) => v,
    }
}

fn canonicalise_subnet(s: &str) -> Option<String> {
    let (addr, prefix) = s.split_once('/')?;
    let ip: std::net::IpAddr = addr.parse().ok()?;
    let n: u8 = prefix.parse().ok()?;
    Some(format!("{ip}/{n}"))
}

// --- scalar-type parsers ---

fn parse_mac(s: &str) -> Result<(), &'static str> {
    // xx:xx:xx:xx:xx:xx (6 pairs of hex, colon-separated). Moray accepts
    // either single- or double-digit octets; we lock to double for
    // simplicity — the upstream regex is stricter.
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return Err("must be xx:xx:xx:xx:xx:xx");
    }
    for p in parts {
        if p.len() != 2 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("octet must be two hex digits");
        }
    }
    Ok(())
}

fn parse_uuid(s: &str) -> Result<(), &'static str> {
    // 8-4-4-4-12 hex.
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return Err("expected 5 segments");
    }
    let want = [8, 4, 4, 4, 12];
    for (p, w) in parts.iter().zip(want.iter()) {
        if p.len() != *w || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("bad segment");
        }
    }
    Ok(())
}

fn parse_ip(s: &str) -> Result<(), &'static str> {
    // IPv4 or IPv6. Rely on std::net::IpAddr.
    s.parse::<std::net::IpAddr>()
        .map(|_| ())
        .map_err(|_| "not an ip address")
}

fn parse_subnet(s: &str) -> Result<(), &'static str> {
    let Some((addr, prefix)) = s.split_once('/') else {
        return Err("missing /prefix");
    };
    parse_ip(addr)?;
    let n: u8 = prefix.parse().map_err(|_| "bad prefix")?;
    if n > 128 {
        return Err("prefix too large");
    }
    Ok(())
}

/// ISO 8601 / RFC 3339 subset. Accepts `YYYY-MM-DD`, with optional
/// `Thh:mm:ss` and timezone (`Z`, `+hh:mm`, etc.). We just run
/// `chrono::DateTime::parse_from_rfc3339` plus the date-only form.
fn parse_iso8601(s: &str) -> Result<(), &'static str> {
    // Upstream rejects the space-separated form (e.g. `"2018-05-06 12:00:00Z"`)
    // even though chrono occasionally accepts it. We match that: the
    // date/time separator must be `T`.
    if s.len() >= 11 && s.as_bytes().get(10) == Some(&b' ') {
        return Err("date/time separator must be 'T'");
    }
    if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
        return Ok(());
    }
    if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() {
        return Ok(());
    }
    if chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").is_ok() {
        return Ok(());
    }
    Err("not an ISO 8601 date")
}

/// Parse a Moray range literal: `[low,high]`, `(low,high)`, `[low,high)`,
/// `(low,high]`, optionally omitting either endpoint (`[,5]`, `(5,]`).
/// The element-level check is the caller's.
fn parse_range<F>(s: &str, check: F) -> Result<(), String>
where
    F: Fn(&str) -> Result<(), String>,
{
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return Err("range too short".into());
    }
    let open = bytes[0];
    let close = bytes[bytes.len() - 1];
    if !matches!(open, b'[' | b'(') {
        return Err("expected '[' or '('".into());
    }
    if !matches!(close, b']' | b')') {
        return Err("expected ']' or ')'".into());
    }
    let inner = &s[1..s.len() - 1];
    let Some((lo, hi)) = inner.split_once(',') else {
        return Err("missing comma".into());
    };
    let lo_empty = lo.trim().is_empty();
    let hi_empty = hi.trim().is_empty();
    if !lo_empty {
        check(lo.trim())?;
    }
    if !hi_empty {
        check(hi.trim())?;
    }
    Ok(())
}

/// True when `literal` is an unbounded range — both endpoints empty
/// (e.g. `[,]`, `(,)`). Moray accepts this on scalar-column
/// `:within:=` (it matches every row) but rejects it on range-column
/// `:within:=` / `:contains:=` / `:overlaps:=`.
pub fn is_unbounded_range(literal: &str) -> bool {
    let bytes = literal.as_bytes();
    if bytes.len() < 3 {
        return false;
    }
    let open = matches!(bytes[0], b'[' | b'(');
    let close = matches!(bytes[bytes.len() - 1], b']' | b')');
    if !open || !close {
        return false;
    }
    let inner = &literal[1..literal.len() - 1];
    let Some((lo, hi)) = inner.split_once(',') else {
        return false;
    };
    lo.trim().is_empty() && hi.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macs() {
        assert!(validate_scalar_literal("mac", "04:31:f4:15:28:ec").is_ok());
        assert!(validate_scalar_literal("mac", "foo").is_err());
        assert!(validate_scalar_literal("mac", "04:31:f4:15:28").is_err());
    }

    #[test]
    fn uuids() {
        assert!(
            validate_scalar_literal("uuid", "12345678-1234-1234-1234-123456789abc").is_ok()
        );
        assert!(validate_scalar_literal("uuid", "bogus").is_err());
    }

    #[test]
    fn ips() {
        assert!(validate_scalar_literal("ip", "192.168.1.1").is_ok());
        assert!(validate_scalar_literal("ip", "::1").is_ok());
        assert!(validate_scalar_literal("ip", "no").is_err());
    }

    #[test]
    fn subnets() {
        assert!(validate_scalar_literal("subnet", "10.0.0.0/24").is_ok());
        assert!(validate_scalar_literal("subnet", "fe80::/10").is_ok());
        assert!(validate_scalar_literal("subnet", "10.0.0.0").is_err());
    }

    #[test]
    fn dates() {
        assert!(validate_scalar_literal("date", "2026-04-20").is_ok());
        assert!(validate_scalar_literal("date", "2026-04-20T12:34:56Z").is_ok());
        assert!(validate_scalar_literal("date", "5").is_err());
    }

    #[test]
    fn ranges() {
        assert!(validate_scalar_literal("numrange", "[1,5]").is_ok());
        assert!(validate_scalar_literal("numrange", "(1,5)").is_ok());
        assert!(validate_scalar_literal("numrange", "[,5]").is_ok());
        assert!(validate_scalar_literal("numrange", "[a,b]").is_err());
        assert!(validate_scalar_literal("numrange", "bogus").is_err());
    }

    #[test]
    fn coerce() {
        let ty = IndexType::parse("[string]").unwrap();
        assert_eq!(
            coerce_for_put(&ty, serde_json::json!("foo")),
            serde_json::json!(["foo"])
        );
        let already = serde_json::json!(["a", "b"]);
        assert_eq!(coerce_for_put(&ty, already.clone()), already);
        let scalar_ty = IndexType::parse("string").unwrap();
        assert_eq!(
            coerce_for_put(&scalar_ty, serde_json::json!("foo")),
            serde_json::json!("foo")
        );
    }
}
