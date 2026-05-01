// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! LDAP filter parser and evaluator (RFC 2254 subset).
//!
//! Moray's `findObjects`, `updateObjects`, `deleteMany`, and friends all
//! take an LDAP-style filter string (e.g. `(&(email=a@b.com)(age>=18))`)
//! and run it against the object's stored JSON. We parse it into an AST,
//! record which attributes it references (so callers can enforce
//! `requireIndexes`), and evaluate it per object.
//!
//! Supported operators:
//!
//! ```text
//!   (attr=value)           Equality
//!   (attr=prefix*)         Substring (prefix, suffix, any, or combos with *)
//!   (attr=*)               Presence
//!   (attr>=value)          GreaterOrEqual
//!   (attr<=value)          LessOrEqual
//!   (&(a)(b)…)             And
//!   (|(a)(b)…)             Or
//!   (!(a))                 Not
//! ```
//!
//! Moray treats the object's top-level `value` as the attribute source
//! plus three synthetic attributes: `_id`, `_key`, `_etag`. We mirror that.

use serde_json::Value;

#[derive(Debug, Clone)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    Equal(String, String),
    GreaterEq(String, String),
    LessEq(String, String),
    Present(String),
    Substring {
        attr: String,
        initial: Option<String>,
        any: Vec<String>,
        finalp: Option<String>,
    },
    /// `(attr:within:=value)` — "the attribute value is *within* the
    /// filter's range/subnet". Used on IP columns against subnet literals
    /// and on plain-valued range columns against range literals.
    Within {
        attr: String,
        literal: String,
    },
    /// `(attr:contains:=value)` — "the attribute's range/subnet
    /// *contains* the filter's point". Used on subnet columns (stored
    /// subnet contains a queried IP) and range columns.
    Contains {
        attr: String,
        literal: String,
    },
    /// `(attr:overlaps:=value)` — numrange/daterange overlap test.
    Overlaps {
        attr: String,
        literal: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("filter parse error at pos {pos}: {msg}")]
    Parse { pos: usize, msg: &'static str },
}

impl Filter {
    /// Parse an LDAP filter string. Strictly per RFC 2254 the outer
    /// parens are required — but node-moray's client sometimes emits a
    /// bare top-level AND/OR/NOT (e.g. `!(mac=x)` or `&(a=1)(b=2)`), so we
    /// tolerate that by wrapping in implicit parens on retry.
    pub fn parse(input: &str) -> Result<Filter, FilterError> {
        match Self::parse_strict(input) {
            Ok(f) => Ok(f),
            Err(_) => {
                let wrapped = format!("({input})");
                Self::parse_strict(&wrapped)
            }
        }
    }

    fn parse_strict(input: &str) -> Result<Filter, FilterError> {
        let mut p = Parser { input: input.as_bytes(), pos: 0 };
        let f = p.parse_filter()?;
        p.skip_ws();
        if p.pos != p.input.len() {
            return Err(FilterError::Parse {
                pos: p.pos,
                msg: "trailing characters",
            });
        }
        Ok(f)
    }

    /// Every attribute referenced anywhere in the filter tree.
    pub fn attrs(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.walk_attrs(&mut out);
        out.sort();
        out.dedup();
        out
    }

    /// Every (attribute, literal) pair in the filter. Presence clauses
    /// (`attr=*`) and substring wildcards (which aren't single literals)
    /// are skipped — those don't need scalar-literal validation.
    pub fn literals(&self) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        self.walk_literals(&mut out);
        out
    }

    /// Rewrite every literal in the filter through `f`, returning a new
    /// tree. Used to canonicalise IPs / subnets against a bucket's
    /// indexed column types before evaluation.
    pub fn map_literals(self, f: &impl Fn(&str, &str) -> String) -> Filter {
        match self {
            Filter::And(xs) => Filter::And(xs.into_iter().map(|x| x.map_literals(f)).collect()),
            Filter::Or(xs) => Filter::Or(xs.into_iter().map(|x| x.map_literals(f)).collect()),
            Filter::Not(x) => Filter::Not(Box::new(x.map_literals(f))),
            Filter::Equal(a, v) => {
                let v = f(&a, &v);
                Filter::Equal(a, v)
            }
            Filter::GreaterEq(a, v) => {
                let v = f(&a, &v);
                Filter::GreaterEq(a, v)
            }
            Filter::LessEq(a, v) => {
                let v = f(&a, &v);
                Filter::LessEq(a, v)
            }
            Filter::Within { attr, literal } => {
                let literal = f(&attr, &literal);
                Filter::Within { attr, literal }
            }
            Filter::Contains { attr, literal } => {
                let literal = f(&attr, &literal);
                Filter::Contains { attr, literal }
            }
            Filter::Overlaps { attr, literal } => {
                let literal = f(&attr, &literal);
                Filter::Overlaps { attr, literal }
            }
            f @ (Filter::Present(_) | Filter::Substring { .. }) => f,
        }
    }

    fn walk_literals<'a>(&'a self, out: &mut Vec<(&'a str, &'a str)>) {
        match self {
            Filter::And(xs) | Filter::Or(xs) => {
                for x in xs {
                    x.walk_literals(out);
                }
            }
            Filter::Not(x) => x.walk_literals(out),
            Filter::Equal(a, v)
            | Filter::GreaterEq(a, v)
            | Filter::LessEq(a, v)
            | Filter::Within { attr: a, literal: v }
            | Filter::Contains { attr: a, literal: v }
            | Filter::Overlaps { attr: a, literal: v } => {
                out.push((a.as_str(), v.as_str()))
            }
            // Present and Substring don't carry a single literal value to
            // validate — Present is just a presence test, Substring is a
            // pattern with wildcards.
            Filter::Present(_) | Filter::Substring { .. } => {}
        }
    }

    fn walk_attrs<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Filter::And(xs) | Filter::Or(xs) => {
                for x in xs {
                    x.walk_attrs(out);
                }
            }
            Filter::Not(x) => x.walk_attrs(out),
            Filter::Equal(a, _)
            | Filter::GreaterEq(a, _)
            | Filter::LessEq(a, _)
            | Filter::Present(a)
            | Filter::Within { attr: a, .. }
            | Filter::Contains { attr: a, .. }
            | Filter::Overlaps { attr: a, .. } => out.push(a),
            Filter::Substring { attr, .. } => out.push(attr),
        }
    }

    /// Evaluate against an object. `value` is the object body (what
    /// `putObject` stored as the `value` field); `synthetic` supplies
    /// `_id`, `_key`, `_etag`, `_mtime`, etc.
    pub fn eval(&self, value: &Value, synthetic: &Value) -> bool {
        match self {
            Filter::And(xs) => xs.iter().all(|x| x.eval(value, synthetic)),
            Filter::Or(xs) => xs.iter().any(|x| x.eval(value, synthetic)),
            Filter::Not(x) => !x.eval(value, synthetic),
            Filter::Present(a) => lookup(value, synthetic, a).is_some(),
            Filter::Equal(a, v) => match lookup(value, synthetic, a) {
                Some(Value::Array(items)) => items.iter().any(|it| eq_match(it, v)),
                Some(other) => eq_match(&other, v),
                None => false,
            },
            Filter::GreaterEq(a, v) => cmp_match(
                lookup(value, synthetic, a).as_ref(),
                v,
                std::cmp::Ordering::Greater,
                true,
            ),
            Filter::LessEq(a, v) => cmp_match(
                lookup(value, synthetic, a).as_ref(),
                v,
                std::cmp::Ordering::Less,
                true,
            ),
            Filter::Substring { attr, initial, any, finalp } => {
                match lookup(value, synthetic, attr) {
                    Some(Value::Array(items)) => items
                        .iter()
                        .any(|it| substring_match(it, initial, any, finalp)),
                    Some(v) => substring_match(&v, initial, any, finalp),
                    None => false,
                }
            }
            Filter::Within { attr, literal } => match lookup(value, synthetic, attr) {
                Some(Value::Array(items)) => {
                    items.iter().any(|it| within_match(it, literal))
                }
                Some(v) => within_match(&v, literal),
                None => false,
            },
            Filter::Contains { attr, literal } => match lookup(value, synthetic, attr) {
                Some(Value::Array(items)) => {
                    items.iter().any(|it| contains_match(it, literal))
                }
                Some(v) => contains_match(&v, literal),
                None => false,
            },
            Filter::Overlaps { attr, literal } => match lookup(value, synthetic, attr) {
                Some(Value::Array(items)) => {
                    items.iter().any(|it| overlaps_match(it, literal))
                }
                Some(v) => overlaps_match(&v, literal),
                None => false,
            },
        }
    }
}

/// `attr:overlaps:=range` — true if the stored range and the filter
/// range share any point. For a degenerate filter literal (no bounds at
/// all) the test fails: an "empty" range doesn't overlap anything.
fn overlaps_match(stored: &Value, filter_literal: &str) -> bool {
    // Date ranges overlapping a date range literal: parse both as date
    // ranges and compare Unix-millis endpoints.
    if let Value::String(s) = stored
        && let Some(sr) = parse_date_range_or_point(s)
        && let Some(fr) = parse_date_range_or_point(filter_literal)
    {
        return overlaps_ranges(&sr, &fr);
    }
    let Some(stored_range) = value_to_range(stored) else {
        return false;
    };
    let Some(filter_range) = parse_range_or_point(filter_literal) else {
        return false;
    };
    overlaps_ranges(&stored_range, &filter_range)
}

/// Range overlap test with endpoint inclusivity. `stored.lo ≤ filter.hi
/// AND stored.hi ≥ filter.lo` with correction for open endpoints.
fn overlaps_ranges(stored_range: &Range, filter_range: &Range) -> bool {
    let lo_ok = match (stored_range.low, filter_range.high) {
        (_, None) | (None, _) => true,
        (Some(sl), Some(fh)) => {
            if stored_range.low_inclusive && filter_range.high_inclusive {
                sl <= fh
            } else {
                sl < fh
            }
        }
    };
    let hi_ok = match (stored_range.high, filter_range.low) {
        (None, _) | (_, None) => true,
        (Some(sh), Some(fl)) => {
            if stored_range.high_inclusive && filter_range.low_inclusive {
                sh >= fl
            } else {
                sh > fl
            }
        }
    };
    lo_ok && hi_ok
}

/// `attr:contains:=value` evaluation. The stored side is the container —
/// a subnet or a range — and the literal is a point (IP or number/date).
/// Returns true iff the container includes the point.
fn contains_match(stored: &Value, filter_literal: &str) -> bool {
    // subnet contains IP: stored is a CIDR string, filter is an IP.
    if let Value::String(sub_str) = stored
        && let Some(b) = ip_in_subnet(filter_literal, sub_str)
    {
        return b;
    }
    // Date range + date point: try parsing both sides as dates and
    // reusing the numeric range machinery (Unix millis).
    if let Value::String(s) = stored
        && let Some(stored_range) = parse_date_range_or_point(s)
        && let Some(ms) = parse_date_ms(filter_literal)
    {
        return stored_range.contains(&Range::point(ms as f64));
    }
    let stored_range = match value_to_range(stored) {
        Some(r) => r,
        None => return false,
    };
    let filter_range = match parse_range_or_point(filter_literal) {
        Some(r) => r,
        None => return false,
    };
    stored_range.contains(&filter_range)
}

/// `attr:within:=value` evaluation.
///
/// Supported container shapes on the object's stored value side:
///
/// - A numeric or ISO-date scalar (we treat it as a point range).
/// - A Moray range literal string, e.g. `"[1,5]"`.
/// - A two-element array `[low, high]` — an older Moray storage shape.
///
/// The filter literal can be a scalar (point containment) or a full
/// range literal.
fn within_match(stored: &Value, filter_literal: &str) -> bool {
    // IP in subnet: stored single IP, filter CIDR subnet.
    if let Value::String(ip_str) = stored
        && let Some(b) = ip_in_subnet(ip_str, filter_literal)
    {
        return b;
    }

    // Date columns store ISO 8601 strings that don't parse as floats.
    // Try the date-range parser against the literal and compare each
    // side as Unix-millisecond numbers so the shared Range machinery
    // below works unchanged.
    if let Value::String(s) = stored
        && let Some(stored_date) = parse_date_ms(s)
        && let Some(filter_range) = parse_date_range_or_point(filter_literal)
    {
        let stored_range = Range::point(stored_date as f64);
        return if stored_range.is_proper_range() {
            stored_range.contains(&filter_range)
        } else {
            filter_range.contains(&stored_range)
        };
    }

    // For numeric/date columns Moray's `:within:=` operator is applied
    // in a polymorphic way: whichever side is the *range* side, it
    // contains the other (the scalar point). This matches upstream,
    // where range columns use `@>` against a point literal and point
    // columns use `<@` against a range literal.
    let stored_range = match value_to_range(stored) {
        Some(r) => r,
        None => return false,
    };
    let filter_range = match parse_range_or_point(filter_literal) {
        Some(r) => r,
        None => return false,
    };
    if stored_range.is_proper_range() {
        stored_range.contains(&filter_range)
    } else {
        filter_range.contains(&stored_range)
    }
}

/// Returns Some(true) if `ip_str` lies within `subnet_str`, Some(false)
/// if they're valid but the IP is outside, or None if inputs aren't a
/// valid (IP, CIDR) pair — letting the caller fall through to numeric
/// range comparison.
fn ip_in_subnet(ip_str: &str, subnet_str: &str) -> Option<bool> {
    let ip: std::net::IpAddr = ip_str.parse().ok()?;
    let (net_str, prefix_str) = subnet_str.split_once('/')?;
    let net: std::net::IpAddr = net_str.parse().ok()?;
    let prefix: u8 = prefix_str.parse().ok()?;
    match (ip, net) {
        (std::net::IpAddr::V4(a), std::net::IpAddr::V4(b)) => {
            if prefix > 32 {
                return None;
            }
            let a_u32 = u32::from_be_bytes(a.octets());
            let b_u32 = u32::from_be_bytes(b.octets());
            let mask = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
            Some((a_u32 & mask) == (b_u32 & mask))
        }
        (std::net::IpAddr::V6(a), std::net::IpAddr::V6(b)) => {
            if prefix > 128 {
                return None;
            }
            let a_u128 = u128::from_be_bytes(a.octets());
            let b_u128 = u128::from_be_bytes(b.octets());
            let mask = if prefix == 0 {
                0
            } else {
                !0u128 << (128 - prefix)
            };
            Some((a_u128 & mask) == (b_u128 & mask))
        }
        // Mixed families: Moray reports false — an IPv4 address can't be
        // in an IPv6 subnet (barring 4-in-6 mappings we don't model).
        _ => Some(false),
    }
}

#[derive(Debug, Clone)]
struct Range {
    low: Option<f64>,
    low_inclusive: bool,
    high: Option<f64>,
    high_inclusive: bool,
}

impl Range {
    fn point(x: f64) -> Self {
        Self {
            low: Some(x),
            low_inclusive: true,
            high: Some(x),
            high_inclusive: true,
        }
    }

    /// `true` if this range isn't a single-point range.
    fn is_proper_range(&self) -> bool {
        match (self.low, self.high) {
            (Some(l), Some(h)) => {
                l != h || !(self.low_inclusive && self.high_inclusive)
            }
            _ => true,
        }
    }

    fn contains(&self, other: &Range) -> bool {
        let other_low = match other.low {
            Some(v) => v,
            None => return self.low.is_none(),
        };
        let other_high = match other.high {
            Some(v) => v,
            None => return self.high.is_none(),
        };
        let self_low_ok = match self.low {
            None => true,
            Some(l) => {
                if self.low_inclusive {
                    l <= other_low
                } else {
                    l < other_low
                }
            }
        };
        let self_high_ok = match self.high {
            None => true,
            Some(h) => {
                if self.high_inclusive {
                    h >= other_high
                } else {
                    h > other_high
                }
            }
        };
        self_low_ok && self_high_ok
    }
}

fn value_to_range(v: &Value) -> Option<Range> {
    match v {
        Value::Number(n) => n.as_f64().map(Range::point),
        Value::String(s) => parse_range_or_point(s),
        Value::Array(items) if items.len() == 2 => {
            let low = items[0].as_f64();
            let high = items[1].as_f64();
            Some(Range {
                low,
                low_inclusive: true,
                high,
                high_inclusive: true,
            })
        }
        _ => None,
    }
}

/// Like `parse_range_or_point`, but interprets the bounds as ISO 8601
/// dates — each endpoint is converted to a Unix-millisecond timestamp
/// so the rest of the range machinery can reuse numeric containment.
fn parse_date_range_or_point(s: &str) -> Option<Range> {
    // Raw RFC 3339 point.
    if let Some(ms) = parse_date_ms(s) {
        return Some(Range::point(ms as f64));
    }
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let low_inclusive = match bytes[0] {
        b'[' => true,
        b'(' => false,
        _ => return None,
    };
    let high_inclusive = match bytes[bytes.len() - 1] {
        b']' => true,
        b')' => false,
        _ => return None,
    };
    let inner = &s[1..s.len() - 1];
    let (lo, hi) = inner.split_once(',')?;
    let low = if lo.trim().is_empty() {
        None
    } else {
        parse_date_ms(lo.trim()).map(|v| v as f64)
    };
    let high = if hi.trim().is_empty() {
        None
    } else {
        parse_date_ms(hi.trim()).map(|v| v as f64)
    };
    // Reject if an explicit bound failed to parse as a date — callers
    // fall back to the numeric parser in that case.
    if !lo.trim().is_empty() && low.is_none() {
        return None;
    }
    if !hi.trim().is_empty() && high.is_none() {
        return None;
    }
    Some(Range {
        low,
        low_inclusive,
        high,
        high_inclusive,
    })
}

fn parse_date_ms(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(nd) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return nd.and_hms_opt(0, 0, 0).map(|x| x.and_utc().timestamp_millis());
    }
    None
}

fn parse_range_or_point(s: &str) -> Option<Range> {
    if let Ok(x) = s.parse::<f64>() {
        return Some(Range::point(x));
    }
    let bytes = s.as_bytes();
    if bytes.len() < 3 {
        return None;
    }
    let low_inclusive = match bytes[0] {
        b'[' => true,
        b'(' => false,
        _ => return None,
    };
    let high_inclusive = match bytes[bytes.len() - 1] {
        b']' => true,
        b')' => false,
        _ => return None,
    };
    let inner = &s[1..s.len() - 1];
    let (lo, hi) = inner.split_once(',')?;
    let low = if lo.trim().is_empty() {
        None
    } else {
        lo.trim().parse::<f64>().ok()
    };
    let high = if hi.trim().is_empty() {
        None
    } else {
        hi.trim().parse::<f64>().ok()
    };
    Some(Range {
        low,
        low_inclusive,
        high,
        high_inclusive,
    })
}

/// Look up an attribute in synthetic first, then in `value`. Synthetic
/// holds the per-row metadata (`_id`, `_key`, `_etag`, `_mtime`); the
/// row body holds everything the caller stored. Both are JSON objects.
///
/// Note that `_`-prefixed names are *not* reserved for synthetic fields:
/// sdc-ufds in particular indexes user data on `_parent`, `_owner`,
/// `_imported`, `_replicated`. Earlier code shortcut these to the
/// synthetic view only, which made every UFDS LDAP search return zero
/// hits (`(_parent=ou=users, o=smartdc)` always missed) — root cause
/// of the post-cutover adminui auth failure. We always try synthetic
/// first (cheap point lookup) and fall through to the value body.
fn lookup(value: &Value, synthetic: &Value, attr: &str) -> Option<Value> {
    if let Some(v) = synthetic.get(attr).cloned() {
        return Some(v);
    }
    value.as_object().and_then(|m| m.get(attr).cloned())
}

fn value_as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn eq_match(v: &Value, target: &str) -> bool {
    match value_as_string(v) {
        Some(s) => s == target,
        None => false,
    }
}

/// Compare attribute value to target per LDAP ordering rules. If the
/// object's attribute is numeric and the target parses as a number, use
/// numeric comparison; otherwise use case-sensitive string ordering.
fn cmp_match(
    v: Option<&Value>,
    target: &str,
    want: std::cmp::Ordering,
    allow_equal: bool,
) -> bool {
    let Some(actual) = v else { return false };
    // Try numeric first.
    if let (Some(av), Some(tv)) = (actual.as_f64(), target.parse::<f64>().ok()) {
        let ord = av.partial_cmp(&tv);
        return match ord {
            Some(o) if o == want => true,
            Some(std::cmp::Ordering::Equal) => allow_equal,
            _ => false,
        };
    }
    let actual_s = match value_as_string(actual) {
        Some(s) => s,
        None => return false,
    };
    let ord = actual_s.as_str().cmp(target);
    match ord {
        o if o == want => true,
        std::cmp::Ordering::Equal => allow_equal,
        _ => false,
    }
}

fn substring_match(
    v: &Value,
    initial: &Option<String>,
    any: &[String],
    finalp: &Option<String>,
) -> bool {
    let Some(mut s) = value_as_string(v) else { return false };
    if let Some(init) = initial {
        if !s.starts_with(init.as_str()) {
            return false;
        }
        s = s[init.len()..].to_string();
    }
    for piece in any {
        match s.find(piece.as_str()) {
            Some(idx) => s = s[idx + piece.len()..].to_string(),
            None => return false,
        }
    }
    if let Some(fin) = finalp
        && !s.ends_with(fin.as_str())
    {
        return false;
    }
    true
}

// --- parser ---

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn advance(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, c: u8, msg: &'static str) -> Result<(), FilterError> {
        if self.advance() != Some(c) {
            return Err(FilterError::Parse { pos: self.pos, msg });
        }
        Ok(())
    }

    fn parse_filter(&mut self) -> Result<Filter, FilterError> {
        self.skip_ws();
        self.expect(b'(', "expected '('")?;
        self.skip_ws();
        let f = match self.peek() {
            Some(b'&') => {
                self.pos += 1;
                Filter::And(self.parse_list()?)
            }
            Some(b'|') => {
                self.pos += 1;
                Filter::Or(self.parse_list()?)
            }
            Some(b'!') => {
                self.pos += 1;
                let inner = self.parse_filter()?;
                Filter::Not(Box::new(inner))
            }
            _ => self.parse_item()?,
        };
        self.skip_ws();
        self.expect(b')', "expected ')'")?;
        Ok(f)
    }

    fn parse_list(&mut self) -> Result<Vec<Filter>, FilterError> {
        let mut out = Vec::new();
        self.skip_ws();
        loop {
            match self.peek() {
                Some(b'(') => {
                    out.push(self.parse_filter()?);
                }
                // Tolerance for an upstream-moray quirk: sdc-cnapi (and
                // a few other Triton services) emit `(&(...)!(...))`
                // instead of the RFC-correct `(&(...)(!(...)))`. The
                // Postgres-backed moray's filter parser accepts the
                // bare `!` form, so we mirror it. Treat `!` followed
                // immediately by a sub-filter as a NOT child of the
                // current AND/OR list.
                Some(b'!') => {
                    self.pos += 1;
                    let inner = self.parse_filter()?;
                    out.push(Filter::Not(Box::new(inner)));
                }
                _ => break,
            }
            self.skip_ws();
        }
        if out.is_empty() {
            return Err(FilterError::Parse {
                pos: self.pos,
                msg: "empty and/or list",
            });
        }
        Ok(out)
    }

    fn parse_item(&mut self) -> Result<Filter, FilterError> {
        let attr = self.parse_attr()?;
        match self.advance() {
            Some(b'=') => {
                // Could be equality, presence (=*), or substring.
                let raw = self.parse_value()?;
                if raw == "*" {
                    return Ok(Filter::Present(attr));
                }
                if raw.contains('*') {
                    return Ok(make_substring(attr, &raw));
                }
                Ok(Filter::Equal(attr, raw))
            }
            Some(b'>') => {
                self.expect(b'=', "expected '=' after '>'")?;
                Ok(Filter::GreaterEq(attr, self.parse_value()?))
            }
            Some(b'<') => {
                self.expect(b'=', "expected '=' after '<'")?;
                Ok(Filter::LessEq(attr, self.parse_value()?))
            }
            Some(b':') => {
                // Extensible match: `attr:<rule>:=<literal>`. We support
                // Moray's `within` rule today; other rules (LDAP DN
                // matching rules, case-insensitive variants) are out of
                // scope and rejected.
                let rule = self.parse_attr()?;
                self.expect(b':', "expected ':' after extensible match rule")?;
                self.expect(b'=', "expected '=' after extensible match rule")?;
                let literal = self.parse_value()?;
                match rule.as_str() {
                    "within" => Ok(Filter::Within { attr, literal }),
                    "contains" => Ok(Filter::Contains { attr, literal }),
                    "overlaps" => Ok(Filter::Overlaps { attr, literal }),
                    _ => Ok(Filter::Within { attr, literal }),
                }
            }
            _ => Err(FilterError::Parse {
                pos: self.pos,
                msg: "unsupported comparison operator",
            }),
        }
    }

    fn parse_attr(&mut self) -> Result<String, FilterError> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            // Attribute names: letters, digits, '_', '-', '.'.
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(FilterError::Parse {
                pos: self.pos,
                msg: "empty attribute",
            });
        }
        Ok(String::from_utf8_lossy(&self.input[start..self.pos]).into_owned())
    }

    fn parse_value(&mut self) -> Result<String, FilterError> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == b')' {
                break;
            }
            if c == b'\\' {
                self.pos += 1;
                let Some(next) = self.peek() else {
                    return Err(FilterError::Parse {
                        pos: self.pos,
                        msg: "truncated escape",
                    });
                };
                // node-ldapjs encodes `(`, `)`, `*`, `\\` literally with a
                // single backslash prefix — not per-RFC 2254 hex escapes.
                // Accept either form: if the byte after `\\` is a hex
                // digit followed by another hex digit, treat it as a hex
                // escape; otherwise treat it as a literal-escape.
                if next.is_ascii_hexdigit()
                    && self.input.get(self.pos + 1).is_some_and(|b| b.is_ascii_hexdigit())
                {
                    let h1 = self.advance().unwrap_or(b'0');
                    let h2 = self.advance().unwrap_or(b'0');
                    let hex = [h1, h2];
                    let s = std::str::from_utf8(&hex)
                        .map_err(|_| FilterError::Parse {
                            pos: self.pos,
                            msg: "bad escape",
                        })?;
                    let byte = u8::from_str_radix(s, 16).map_err(|_| {
                        FilterError::Parse { pos: self.pos, msg: "bad escape" }
                    })?;
                    out.push(byte as char);
                } else {
                    self.pos += 1;
                    out.push(next as char);
                }
                continue;
            }
            self.pos += 1;
            out.push(c as char);
        }
        Ok(out)
    }
}

/// Turn `"foo*bar*"` into a `Filter::Substring`.
fn make_substring(attr: String, raw: &str) -> Filter {
    let parts: Vec<&str> = raw.split('*').collect();
    let initial = if !parts[0].is_empty() {
        Some(parts[0].to_string())
    } else {
        None
    };
    let finalp = if parts.len() > 1 && !parts[parts.len() - 1].is_empty() {
        Some(parts[parts.len() - 1].to_string())
    } else {
        None
    };
    let any = parts[1..parts.len().saturating_sub(1)]
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    Filter::Substring { attr, initial, any, finalp }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn eval(expr: &str, v: &Value) -> bool {
        Filter::parse(expr).unwrap().eval(v, &Value::Null)
    }

    #[test]
    fn equality() {
        let v = json!({"email": "a@b", "age": 25});
        assert!(eval("(email=a@b)", &v));
        assert!(!eval("(email=x)", &v));
        assert!(eval("(age=25)", &v));
    }

    #[test]
    fn presence_and_not() {
        let v = json!({"email": "a@b"});
        assert!(eval("(email=*)", &v));
        assert!(!eval("(phone=*)", &v));
        assert!(eval("(!(phone=*))", &v));
    }

    #[test]
    fn and_or() {
        let v = json!({"a": "1", "b": "2"});
        assert!(eval("(&(a=1)(b=2))", &v));
        assert!(!eval("(&(a=1)(b=nope))", &v));
        assert!(eval("(|(a=nope)(b=2))", &v));
    }

    #[test]
    fn numeric_ordering() {
        let v = json!({"age": 25});
        assert!(eval("(age>=18)", &v));
        assert!(!eval("(age>=30)", &v));
        assert!(eval("(age<=25)", &v));
    }

    #[test]
    fn substring_prefix() {
        let v = json!({"email": "alice@example.com"});
        assert!(eval("(email=alice*)", &v));
        assert!(eval("(email=*example.com)", &v));
        assert!(eval("(email=alice*.com)", &v));
        assert!(!eval("(email=bob*)", &v));
    }

    #[test]
    fn array_contains() {
        let v = json!({"tags": ["a", "b", "c"]});
        assert!(eval("(tags=b)", &v));
        assert!(!eval("(tags=d)", &v));
    }

    #[test]
    fn attrs_collected() {
        let f = Filter::parse("(&(a=1)(|(b=2)(!(c=3))))").unwrap();
        assert_eq!(f.attrs(), vec!["a", "b", "c"]);
    }

    #[test]
    fn within_operator() {
        let v = json!({"r": "[1,10]"});
        assert!(eval("(r:within:=5)", &v));
        assert!(eval("(r:within:=[2,9])", &v));
        assert!(!eval("(r:within:=15)", &v));
        assert!(!eval("(r:within:=[0,5])", &v));

        let v = json!({"n": 5});
        assert!(eval("(n:within:=5)", &v));
        assert!(!eval("(n:within:=6)", &v));
    }

    #[test]
    fn unsupported_extensible_rule_parses_to_within() {
        // Moray's test suite wants unknown extensible rules to surface as
        // NotIndexedError at the query layer, not as a raw parse error —
        // so our parser accepts them and maps to Within for uniform
        // downstream handling.
        assert!(Filter::parse("(a:caseless:=x)").is_ok());
    }

    #[test]
    fn synthetic_id() {
        let v = json!({"x": 1});
        let s = json!({"_id": 42});
        let f = Filter::parse("(_id>=10)").unwrap();
        assert!(f.eval(&v, &s));
    }

    #[test]
    fn underscore_value_field_matches() {
        // sdc-ufds indexes user-data fields whose names start with `_`
        // (`_parent`, `_owner`, `_imported`, `_replicated`). Earlier we
        // hard-coded `_*` to mean "synthetic only", which made every
        // LDAP search against ufds_o_smartdc return zero results — the
        // post-cutover adminui auth bug. Synthetic still wins for the
        // four real synthetic fields; everything else falls through to
        // the value body.
        let v = json!({
            "_parent": ["ou=users, o=smartdc"],
            "_owner":  ["930896af-bf8c-48d4-885c-6573a94b1853"],
            "login":   ["admin"],
        });
        let s = json!({"_id": 7, "_key": "abc", "_etag": "deadbeef", "_mtime": 0});

        // _parent equality (literal with comma-space) lands.
        assert!(Filter::parse("(_parent=ou=users, o=smartdc)")
            .unwrap()
            .eval(&v, &s));

        // _parent presence works.
        assert!(Filter::parse("(_parent=*)").unwrap().eval(&v, &s));

        // _owner equality on a UUID lands.
        assert!(Filter::parse(
            "(_owner=930896af-bf8c-48d4-885c-6573a94b1853)"
        )
        .unwrap()
        .eval(&v, &s));

        // Synthetic still beats value for the genuinely-synthetic fields.
        assert!(Filter::parse("(_id=7)").unwrap().eval(&v, &s));
        assert!(!Filter::parse("(_id=99)").unwrap().eval(&v, &s));
    }

    #[test]
    fn cnapi_unwrapped_not_in_and_list() {
        // sdc-cnapi/lib/models/server.js builds `(&(uuid=*)!(uuid=default))`
        // — a NOT operator without surrounding parens, embedded directly
        // in an AND list. Strict RFC 2254 requires `(!filter)`, but
        // upstream moray's parser accepts the bare form, so we do too.
        let f = Filter::parse("(&(uuid=*)!(uuid=default))").unwrap();
        let s = json!({"_id": 1, "_key": "k", "_etag": "x", "_mtime": 0});
        // Real server: matches.
        assert!(f.eval(
            &json!({"uuid": "8b2a9975-6354-8a94-39e4-1c697aa96b33"}),
            &s
        ));
        // Pseudo-server "default": filtered out by the negation.
        assert!(!f.eval(&json!({"uuid": "default"}), &s));
    }

    #[test]
    fn ufds_admin_search_filter_matches() {
        // Exact filter shape sdc-ufds emits during adminui's bind-by-login
        // flow. Recreates the post-cutover repro: this returned 0 rows
        // before the lookup() fix, returns the admin record after.
        let v = json!({
            "_parent":     ["ou=users, o=smartdc"],
            "objectclass": ["sdcperson"],
            "login":       ["admin"],
        });
        let s = json!({"_id": 1, "_key": "uuid-...", "_etag": "x", "_mtime": 0});
        let f = Filter::parse(
            "(&(&(objectclass=sdcperson)(|(login=admin)(uuid=admin)))\
             (_parent=ou=users, o=smartdc))",
        )
        .unwrap();
        assert!(f.eval(&v, &s));
    }
}
