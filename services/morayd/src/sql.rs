// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `sql` RPC compatibility shim.
//!
//! Upstream moray exposes raw Postgres via the `sql` verb. FDB has no SQL
//! engine, so morayd recognises the small, fixed set of SQL shapes that
//! Triton services actually emit and re-implements each as an in-process
//! scan over `MorayStore`. Anything outside that set fails with
//! `NotImplemented` so callers never see silently-wrong results.
//!
//! Recognised shapes (real call sites in `~/workspace/triton_clean`):
//!
//! 1.  `SELECT count(*) FROM <bucket> WHERE login ~* '^<re>$'`
//!     — sdc-ufds duplicate-login check (`db/add.js`).
//! 2.  `SELECT count(*) FROM <bucket>`
//!     — sdc-ufds changelog cardinality (`ufds.js`).
//! 3.  `SELECT _id FROM <bucket> WHERE _id IS NOT NULL ORDER BY _id DESC LIMIT 1`
//!     — sdc-ufds `cn=latestchangenumber`.
//! 4.  `SELECT count(1) FROM <bucket> WHERE <col> = $1`
//!     — sdc-cnapi waitlist depth (`waitlist.js`).
//! 5.  `SELECT _id, uuid FROM <bucket> WHERE (subnet >> $1 OR subnet_start << $2) [AND fabric ...]`
//!     — sdc-napi network overlap (`network.js`).
//! 6.  `SELECT uuid FROM <bucket> WHERE subnet >> $1 AND vlan_id=$2 AND nic_tag=$3 [AND vnet_id=$4]`
//!     — sdc-napi containing-network for an IP (`network.js`).
//! 7.  Window-function gap-IP search over `napi_ips_<uuid>`
//!     — sdc-napi IP allocator (`ip/provision.js`).
//! 8.  `SELECT count(_id), execution FROM wf_jobs WHERE <range> GROUP BY execution`
//!     — node-workflow-moray-backend stats (`workflow-moray-backend.js`).
//! 9.  `SELECT count(_id) FROM wf_jobs WHERE task IS NULL AND vm_uuid IS NOT NULL`
//!     — sdc-workflow backfill (`wf-backfill.js`).
//!
//! Each row in the response stream is a flat JSON object whose keys
//! match the SELECT projection — what `pg.query()` would have emitted.

use std::net::Ipv4Addr;

use serde_json::{json, Value};

use crate::error::{MorayError, Result};
use crate::store::MorayStore;

/// Cap how many rows we'll pull through the in-process scan path. The
/// largest bucket we currently see in production (wf_jobs) sits well
/// under 10 k rows; 200 k is generous headroom that still bounds memory.
const SCAN_LIMIT: usize = 200_000;

/// Dispatch a `sql` call. Returns the per-row payload stream that the
/// RPC layer wraps in DATA frames.
pub async fn execute<S: MorayStore>(
    store: &S,
    stmt: &str,
    values: &[Value],
) -> Result<Vec<Value>> {
    let canon = canonicalize(stmt);

    if let Some((bucket, pattern)) = match_ufds_login_count(stmt) {
        return ufds_login_count(store, &bucket, &pattern).await;
    }
    if let Some(bucket) = match_max_id(&canon) {
        return max_id(store, &bucket).await;
    }
    if let Some((bucket, col)) = match_count_eq_param(&canon) {
        return count_eq(store, &bucket, &col, values.first()).await;
    }
    if let Some(bucket) = match_count_star_from(&canon) {
        return count_all(store, &bucket).await;
    }
    if let Some(case) = match_napi_overlap(&canon) {
        let bucket = extract_from_bucket(&canon)
            .ok_or_else(|| MorayError::Invocation("sql: unable to extract FROM bucket".into()))?;
        return napi_overlap(store, &bucket, case, values).await;
    }
    if let Some(case) = match_napi_containing(&canon) {
        let bucket = extract_from_bucket(&canon)
            .ok_or_else(|| MorayError::Invocation("sql: unable to extract FROM bucket".into()))?;
        return napi_containing(store, &bucket, case, values).await;
    }
    if let Some((bucket, col)) = match_napi_gap_ip(&canon) {
        return napi_gap_ip(store, &bucket, &col, values).await;
    }
    if match_wf_count_by_execution(&canon) {
        return wf_count_by_execution(store, &canon).await;
    }
    if match_wf_backfill_count(&canon) {
        return wf_backfill_count(store).await;
    }

    Err(MorayError::NotImplemented(format!(
        "sql: unrecognised statement: {stmt}"
    )))
}

// ---------------------------------------------------------------------------
// canonicalisation + tiny scanners

/// Lower-case, collapse runs of ASCII whitespace into single spaces, and
/// strip a trailing semicolon. Everything else is preserved so that
/// quoted literals (UFDS regex pattern) remain intact for downstream
/// extraction. We deliberately do not implement a real SQL parser.
fn canonicalize(stmt: &str) -> String {
    let mut out = String::with_capacity(stmt.len());
    let mut in_ws = false;
    for c in stmt.chars() {
        if c.is_ascii_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push(' ');
            }
            in_ws = true;
        } else {
            in_ws = false;
            for lc in c.to_lowercase() {
                out.push(lc);
            }
        }
    }
    while out.ends_with(';') || out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Pull the bucket identifier following `FROM` in the canonicalised
/// statement. Stops at the first whitespace or punctuation character.
fn extract_from_bucket(canon: &str) -> Option<String> {
    let idx = canon.find(" from ")?;
    let rest = canon[idx + 6..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// shape matchers

/// Match `select count(*) from <bucket> where login ~* '^<pat>$'`.
/// Operates on the *original* statement so we keep the regex literal
/// case-sensitive (the `~*` is case-insensitive on the postgres side,
/// but the regex body itself can include a literal we must echo back).
fn match_ufds_login_count(stmt: &str) -> Option<(String, String)> {
    let canon = canonicalize(stmt);
    if !canon.starts_with("select count(*) from ") {
        return None;
    }
    if !canon.contains(" where login ~* '^") {
        return None;
    }
    // Pull the bucket name from the canon copy.
    let after_from = &canon["select count(*) from ".len()..];
    let bucket_end = after_from
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let bucket = after_from[..bucket_end].to_string();

    // Pull the original-cased regex body between `^` and `$'`. Preserves
    // mixed case so login checks stay correct after lower-casing the
    // canonical form.
    let start = stmt.find("'^")? + 2;
    let rest = &stmt[start..];
    let end = rest.find("$'")?;
    Some((bucket, rest[..end].to_string()))
}

/// Match `select count(*) from <bucket>` with no WHERE clause.
fn match_count_star_from(canon: &str) -> Option<String> {
    if !canon.starts_with("select count(*) from ") {
        return None;
    }
    let rest = &canon["select count(*) from ".len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let bucket = rest[..end].to_string();
    // Reject if there's a WHERE clause — that's a different shape.
    if canon[("select count(*) from ".len() + end)..]
        .trim_start()
        .starts_with("where")
    {
        return None;
    }
    Some(bucket)
}

/// Match `select _id from <bucket> where _id is not null order by _id desc limit 1`.
fn match_max_id(canon: &str) -> Option<String> {
    if !canon.starts_with("select _id from ") {
        return None;
    }
    if !canon.contains("order by _id desc") {
        return None;
    }
    if !canon.contains("limit 1") {
        return None;
    }
    let rest = &canon["select _id from ".len()..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_string())
}

/// Match `select count(1) from <bucket> where <col>=$1` (the cnapi
/// waitlist call). Restricted to a single equality predicate against
/// `$1` so we don't accidentally fire on richer queries.
fn match_count_eq_param(canon: &str) -> Option<(String, String)> {
    if !canon.starts_with("select count(1)") {
        return None;
    }
    let from_idx = canon.find(" from ")?;
    let after_from = &canon[from_idx + 6..];
    let bucket_end = after_from
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let bucket = after_from[..bucket_end].to_string();
    let after_bucket = after_from[bucket_end..].trim_start();
    let where_rest = after_bucket.strip_prefix("where ")?;
    // Accept "<col>=$1" or "<col> = $1".
    let eq_idx = where_rest.find('=')?;
    let col = where_rest[..eq_idx].trim().to_string();
    if col.is_empty()
        || !col
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let after_eq = where_rest[eq_idx + 1..].trim();
    if !after_eq.starts_with("$1") {
        return None;
    }
    Some((bucket, col))
}

#[derive(Clone, Copy, Debug)]
enum NapiOverlapCase {
    Normal,
    Fabric,
}

/// `select _id, uuid from <bucket> where (subnet >> $1 or subnet_start << $2) and fabric != true`
/// or the fabric variant `... and fabric = true and vnet_id = $3`.
fn match_napi_overlap(canon: &str) -> Option<NapiOverlapCase> {
    if !canon.starts_with("select _id, uuid from ") {
        return None;
    }
    if !canon.contains("where (subnet >> $1 or subnet_start << $2)") {
        return None;
    }
    if canon.contains("fabric != true") {
        Some(NapiOverlapCase::Normal)
    } else if canon.contains("fabric = true") && canon.contains("vnet_id = $3") {
        Some(NapiOverlapCase::Fabric)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug)]
enum NapiContainingCase {
    Plain,
    WithVnet,
}

/// `select uuid from <bucket> where subnet >> $1 and vlan_id = $2 and nic_tag = $3 [and vnet_id = $4]`.
fn match_napi_containing(canon: &str) -> Option<NapiContainingCase> {
    if !canon.starts_with("select uuid from ") {
        return None;
    }
    if !canon.contains("where subnet >> $1 and vlan_id = $2 and nic_tag = $3") {
        return None;
    }
    if canon.contains("and vnet_id = $4") {
        Some(NapiContainingCase::WithVnet)
    } else {
        Some(NapiContainingCase::Plain)
    }
}

/// Match the gap-IP probe that NAPI uses on `napi_ips_<uuid>`. Two
/// flavours exist (string IPs vs numeric); both have a window function
/// shape we can recognise without a real parser.
fn match_napi_gap_ip(canon: &str) -> Option<(String, String)> {
    // The NAPI templates open with `select * from (select <ip+1> gap_start,`
    // and reference `lead(<ipcol>) over (order by <ipcol>) - <ipcol> - 1`.
    if !canon.starts_with("select * from (select ") {
        return None;
    }
    if !canon.contains("gap_start") || !canon.contains("gap_length") {
        return None;
    }
    // Bucket lives between `from %s` (already substituted) and the next
    // whitespace; we find it by taking the first identifier following
    // the inner FROM. Both templates put the napi_ips_<uuid> bucket
    // between `from ` and ` where `.
    let inner_from = canon.rfind(" from ")?;
    let after_from = canon[inner_from + 6..].trim_start();
    let bucket_end = after_from
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let bucket = after_from[..bucket_end].to_string();
    if !bucket.starts_with("napi_ips_") {
        return None;
    }
    // Decide the IP column. The string template uses `ipaddr`, the
    // numeric template uses `ip`. Look for the canonical lead() form.
    let col = if canon.contains("lead(ipaddr) over") {
        "ipaddr"
    } else if canon.contains("lead(ip) over") {
        "ip"
    } else {
        return None;
    };
    Some((bucket, col.to_string()))
}

/// Match the workflow stats query. We check the prefix and the
/// `group by execution` tail; the WHERE clause is parsed separately to
/// pick out the time-range bounds.
fn match_wf_count_by_execution(canon: &str) -> bool {
    canon.starts_with("select count(_id), execution from wf_jobs where")
        && canon.ends_with("group by execution")
}

/// Match the wf-backfill cardinality probe.
fn match_wf_backfill_count(canon: &str) -> bool {
    canon
        == "select count(_id) from wf_jobs where task is null and vm_uuid is not null"
}

// ---------------------------------------------------------------------------
// executors

async fn count_all<S: MorayStore>(store: &S, bucket: &str) -> Result<Vec<Value>> {
    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    Ok(vec![json!({ "count": rows.len() as u64 })])
}

async fn max_id<S: MorayStore>(store: &S, bucket: &str) -> Result<Vec<Value>> {
    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    let max = rows.iter().map(|m| m.id).max().unwrap_or(0);
    if max == 0 {
        // Match the upstream "no rows" behaviour: emit no DATA frames so
        // the caller's `record` listener never fires.
        Ok(Vec::new())
    } else {
        Ok(vec![json!({ "_id": max })])
    }
}

async fn count_eq<S: MorayStore>(
    store: &S,
    bucket: &str,
    col: &str,
    value: Option<&Value>,
) -> Result<Vec<Value>> {
    let needle = value
        .ok_or_else(|| MorayError::Invocation("sql: missing $1 for count predicate".into()))?;
    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    let n = rows
        .iter()
        .filter(|m| {
            m.value
                .as_object()
                .and_then(|o| o.get(col))
                .is_some_and(|v| values_equal(v, needle))
        })
        .count();
    Ok(vec![json!({ "count": n as u64 })])
}

/// Compare two JSON scalars with the same loose semantics that node-fast
/// would yield from the wire — strings to strings, numbers to numbers,
/// bools to bools. Anything else is `false`.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

async fn ufds_login_count<S: MorayStore>(
    store: &S,
    bucket: &str,
    pattern: &str,
) -> Result<Vec<Value>> {
    // The UFDS regex is always `^<lit>$` — anchored, no metacharacters.
    // We do a case-insensitive equality compare instead of pulling in a
    // real regex engine. If a future caller introduces a non-trivial
    // pattern we'll see the mismatch in their tests.
    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    let n = rows
        .iter()
        .filter(|m| {
            m.value
                .as_object()
                .and_then(|o| o.get("login"))
                .and_then(|v| v.as_str())
                .is_some_and(|login| login.eq_ignore_ascii_case(pattern))
        })
        .count();
    Ok(vec![json!({ "count": n as u64 })])
}

/// Pull a `[u8; 4]` IPv4 in network order out of either a string IP or
/// (best-effort) a stringified u32. Returns `None` for IPv6 or junk.
fn parse_v4(s: &str) -> Option<u32> {
    if let Ok(ip) = s.parse::<Ipv4Addr>() {
        return Some(u32::from(ip));
    }
    s.parse::<u32>().ok()
}

/// Parse an IPv4 CIDR `a.b.c.d/N`. Returns `(network_u32, prefix_bits)`.
fn parse_v4_cidr(s: &str) -> Option<(u32, u8)> {
    let (addr, bits) = s.split_once('/')?;
    let ip: Ipv4Addr = addr.parse().ok()?;
    let bits: u8 = bits.parse().ok()?;
    if bits > 32 {
        return None;
    }
    let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
    Some((u32::from(ip) & mask, bits))
}

/// Test whether `ip` lies inside the CIDR `(net, bits)`.
fn cidr_contains(net: u32, bits: u8, ip: u32) -> bool {
    let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
    (ip & mask) == (net & mask)
}

/// Field accessor that hops past `value:` if the row was dehydrated.
fn vget<'a>(meta: &'a crate::types::ObjectMeta, key: &str) -> Option<&'a Value> {
    meta.value.as_object().and_then(|o| o.get(key))
}

async fn napi_overlap<S: MorayStore>(
    store: &S,
    bucket: &str,
    case: NapiOverlapCase,
    values: &[Value],
) -> Result<Vec<Value>> {
    if values.len() < 2 {
        return Err(MorayError::Invocation(
            "sql: napi overlap expects 2+ args ($1=subnet_start, $2=subnet)".into(),
        ));
    }
    let arg1_start = values[0]
        .as_str()
        .ok_or_else(|| MorayError::Invocation("sql: $1 must be IP string".into()))?;
    let arg2_subnet = values[1]
        .as_str()
        .ok_or_else(|| MorayError::Invocation("sql: $2 must be CIDR string".into()))?;
    let new_start_ip = parse_v4(arg1_start)
        .ok_or_else(|| MorayError::Invocation("sql: $1 not a valid IPv4".into()))?;
    let (new_net, new_bits) = parse_v4_cidr(arg2_subnet)
        .ok_or_else(|| MorayError::Invocation("sql: $2 not a valid IPv4 CIDR".into()))?;

    let want_vnet: Option<i64> = match case {
        NapiOverlapCase::Fabric => Some(
            values
                .get(2)
                .and_then(|v| v.as_i64())
                .ok_or_else(|| MorayError::Invocation("sql: $3 must be vnet_id integer".into()))?,
        ),
        NapiOverlapCase::Normal => None,
    };

    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    let mut out = Vec::new();
    for meta in &rows {
        let fabric = vget(meta, "fabric").and_then(|v| v.as_bool()).unwrap_or(false);
        match case {
            NapiOverlapCase::Normal if fabric => continue,
            NapiOverlapCase::Fabric if !fabric => continue,
            _ => {}
        }
        if let Some(want) = want_vnet {
            let row_vnet = vget(meta, "vnet_id").and_then(|v| v.as_i64());
            if row_vnet != Some(want) {
                continue;
            }
        }
        let row_subnet = vget(meta, "subnet").and_then(|v| v.as_str());
        let row_start = vget(meta, "subnet_start").and_then(|v| v.as_str());
        let Some(row_subnet) = row_subnet else { continue };
        let Some(row_start) = row_start else { continue };
        let Some((row_net, row_bits)) = parse_v4_cidr(row_subnet) else {
            continue;
        };
        let Some(row_start_ip) = parse_v4(row_start) else {
            continue;
        };
        // pg's `subnet >> $1`: row's subnet contains the new start IP.
        let lhs = cidr_contains(row_net, row_bits, new_start_ip);
        // pg's `subnet_start << $2`: row's start IP is contained in the new subnet.
        let rhs = cidr_contains(new_net, new_bits, row_start_ip);
        if lhs || rhs {
            out.push(json!({
                "_id":  meta.id,
                "uuid": vget(meta, "uuid").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    Ok(out)
}

async fn napi_containing<S: MorayStore>(
    store: &S,
    bucket: &str,
    case: NapiContainingCase,
    values: &[Value],
) -> Result<Vec<Value>> {
    if values.len() < 3 {
        return Err(MorayError::Invocation(
            "sql: napi containing expects 3+ args ($1=ip, $2=vlan_id, $3=nic_tag)".into(),
        ));
    }
    let ip_str = values[0]
        .as_str()
        .ok_or_else(|| MorayError::Invocation("sql: $1 must be IP string".into()))?;
    let want_ip = parse_v4(ip_str)
        .ok_or_else(|| MorayError::Invocation("sql: $1 not a valid IPv4".into()))?;
    // node-fast may surface vlan_id as either Number or stringified.
    let want_vlan: i64 = match &values[1] {
        Value::Number(n) => n.as_i64().unwrap_or(-1),
        Value::String(s) => s.parse().unwrap_or(-1),
        _ => -1,
    };
    let want_tag = values[2]
        .as_str()
        .ok_or_else(|| MorayError::Invocation("sql: $3 must be nic_tag string".into()))?;
    let want_vnet: Option<i64> = match case {
        NapiContainingCase::WithVnet => Some(
            values
                .get(3)
                .and_then(|v| v.as_i64())
                .ok_or_else(|| MorayError::Invocation("sql: $4 must be vnet_id".into()))?,
        ),
        NapiContainingCase::Plain => None,
    };

    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    let mut out = Vec::new();
    for meta in &rows {
        if vget(meta, "vlan_id").and_then(|v| v.as_i64()) != Some(want_vlan) {
            continue;
        }
        if vget(meta, "nic_tag").and_then(|v| v.as_str()) != Some(want_tag) {
            continue;
        }
        if let Some(v) = want_vnet
            && vget(meta, "vnet_id").and_then(|v| v.as_i64()) != Some(v)
        {
            continue;
        }
        let Some(subnet) = vget(meta, "subnet").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some((net, bits)) = parse_v4_cidr(subnet) else {
            continue;
        };
        if cidr_contains(net, bits, want_ip) {
            out.push(json!({
                "uuid": vget(meta, "uuid").cloned().unwrap_or(Value::Null),
            }));
        }
    }
    Ok(out)
}

async fn napi_gap_ip<S: MorayStore>(
    store: &S,
    bucket: &str,
    col: &str,
    values: &[Value],
) -> Result<Vec<Value>> {
    if values.len() < 2 {
        return Err(MorayError::Invocation(
            "sql: napi gap-IP expects 2 args ($1=min, $2=max)".into(),
        ));
    }
    // For the string-IP variant, args arrive as IP-shaped strings; for
    // the numeric variant they are JSON numbers (or stringified u32s).
    let parse_bound = |v: &Value| -> Option<u32> {
        match v {
            Value::String(s) => parse_v4(s),
            Value::Number(n) => n.as_u64().map(|x| x as u32),
            _ => None,
        }
    };
    let min = parse_bound(&values[0])
        .ok_or_else(|| MorayError::Invocation("sql: $1 not a valid IP/u32".into()))?;
    let max = parse_bound(&values[1])
        .ok_or_else(|| MorayError::Invocation("sql: $2 not a valid IP/u32".into()))?;

    // Gather every used address in [min,max] and sort. Bucket size
    // is bounded by the network's /N — for a /16 we're at 65k rows
    // which is fine for an in-process scan.
    let rows = store.scan_objects(bucket, SCAN_LIMIT).await?;
    let mut used: Vec<u32> = Vec::with_capacity(rows.len());
    for meta in &rows {
        let Some(field) = vget(meta, col) else { continue };
        let v = match field {
            Value::String(s) => parse_v4(s),
            Value::Number(n) => n.as_u64().map(|x| x as u32),
            _ => None,
        };
        if let Some(ip) = v
            && ip >= min
            && ip <= max
        {
            used.push(ip);
        }
    }
    used.sort_unstable();
    used.dedup();

    // Walk `used` and pick the first gap. Mirrors the pg `lead(ip)` -
    // `ip` - 1 idiom: gap_start = used[i] + 1, gap_length = used[i+1] -
    // used[i] - 1.
    const MAX_GAP_LENGTH: u64 = 1024;
    for win in used.windows(2) {
        let here = win[0];
        let next = win[1];
        if next > here + 1 {
            let length = (next - here - 1) as u64;
            let length = length.min(MAX_GAP_LENGTH);
            let gap_start = here + 1;
            return Ok(vec![row_for_gap(col, gap_start, length)]);
        }
    }
    Ok(Vec::new())
}

fn row_for_gap(col: &str, gap_start: u32, gap_length: u64) -> Value {
    if col == "ipaddr" {
        json!({
            "gap_start":  Ipv4Addr::from(gap_start).to_string(),
            "gap_length": gap_length,
        })
    } else {
        json!({
            "gap_start":  gap_start as u64,
            "gap_length": gap_length,
        })
    }
}

async fn wf_count_by_execution<S: MorayStore>(
    store: &S,
    canon: &str,
) -> Result<Vec<Value>> {
    let where_clause = canon
        .strip_prefix("select count(_id), execution from wf_jobs where ")
        .and_then(|s| s.strip_suffix(" group by execution"))
        .ok_or_else(|| MorayError::Invocation("sql: malformed wf stats query".into()))?;

    // Parse the dual `created_at <op> N [and created_at <op> N]` form.
    let (lower, upper) = parse_wf_range(where_clause)?;
    let rows = store.scan_objects("wf_jobs", SCAN_LIMIT).await?;
    let mut buckets: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    for meta in &rows {
        let created = vget(meta, "created_at")
            .and_then(|v| v.as_i64())
            .or_else(|| {
                vget(meta, "created_at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or(0);
        if let Some(lo) = lower
            && created <= lo
        {
            continue;
        }
        if let Some(hi) = upper
            && created >= hi
        {
            continue;
        }
        let exec = vget(meta, "execution")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        *buckets.entry(exec).or_insert(0) += 1;
    }
    Ok(buckets
        .into_iter()
        .map(|(k, v)| json!({ "count": v, "execution": k }))
        .collect())
}

/// Parse `created_at > N` and/or `created_at < N` clauses joined by `and`.
/// Returns `(strictly_greater_than, strictly_less_than)` as ms timestamps.
fn parse_wf_range(s: &str) -> Result<(Option<i64>, Option<i64>)> {
    let mut lower = None;
    let mut upper = None;
    for chunk in s.split(" and ") {
        let chunk = chunk.trim();
        if let Some(rest) = chunk.strip_prefix("created_at > ") {
            lower = Some(rest.trim().parse::<i64>().map_err(|e| {
                MorayError::Invocation(format!("sql: bad wf lower bound: {e}"))
            })?);
        } else if let Some(rest) = chunk.strip_prefix("created_at < ") {
            upper = Some(rest.trim().parse::<i64>().map_err(|e| {
                MorayError::Invocation(format!("sql: bad wf upper bound: {e}"))
            })?);
        } else {
            return Err(MorayError::Invocation(format!(
                "sql: unsupported wf predicate: {chunk}"
            )));
        }
    }
    Ok((lower, upper))
}

async fn wf_backfill_count<S: MorayStore>(store: &S) -> Result<Vec<Value>> {
    let rows = store.scan_objects("wf_jobs", SCAN_LIMIT).await?;
    let n = rows
        .iter()
        .filter(|m| {
            let task_null = match vget(m, "task") {
                None | Some(Value::Null) => true,
                _ => false,
            };
            let vm_present = !matches!(vget(m, "vm_uuid"), None | Some(Value::Null));
            task_null && vm_present
        })
        .count();
    Ok(vec![json!({ "count": n as u64 })])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canon_collapses_whitespace_and_lowercases() {
        assert_eq!(
            canonicalize("SELECT  count(*)\nFROM\tx WHERE login = 'A';"),
            "select count(*) from x where login = 'a'"
        );
    }

    #[test]
    fn matches_count_star() {
        assert_eq!(
            match_count_star_from(&canonicalize("SELECT count(*) FROM ufds_changelog")),
            Some("ufds_changelog".into())
        );
    }

    #[test]
    fn rejects_count_star_with_where() {
        assert_eq!(
            match_count_star_from(&canonicalize("SELECT count(*) FROM x WHERE id = 1")),
            None
        );
    }

    #[test]
    fn matches_max_id() {
        assert_eq!(
            match_max_id(&canonicalize(
                "SELECT _id FROM ufds_changelog WHERE _id IS NOT NULL ORDER BY _id DESC LIMIT 1"
            )),
            Some("ufds_changelog".into())
        );
    }

    #[test]
    fn matches_count_eq_param() {
        assert_eq!(
            match_count_eq_param(&canonicalize(
                "SELECT count(1) FROM cnapi_waitlist_tickets WHERE server_uuid=$1"
            )),
            Some(("cnapi_waitlist_tickets".into(), "server_uuid".into()))
        );
    }

    #[test]
    fn matches_napi_overlap_normal_and_fabric() {
        let n = canonicalize(
            "SELECT _id, uuid FROM napi_networks WHERE (subnet >> $1 OR subnet_start << $2) AND fabric != true",
        );
        assert!(matches!(match_napi_overlap(&n), Some(NapiOverlapCase::Normal)));
        let f = canonicalize(
            "SELECT _id, uuid FROM napi_networks WHERE (subnet >> $1 OR subnet_start << $2) AND fabric = true AND vnet_id = $3",
        );
        assert!(matches!(match_napi_overlap(&f), Some(NapiOverlapCase::Fabric)));
    }

    #[test]
    fn matches_napi_containing() {
        let plain = canonicalize(
            "SELECT uuid FROM napi_networks WHERE subnet >> $1 AND vlan_id = $2 AND nic_tag = $3",
        );
        assert!(matches!(
            match_napi_containing(&plain),
            Some(NapiContainingCase::Plain)
        ));
        let v = canonicalize(
            "SELECT uuid FROM napi_networks WHERE subnet >> $1 AND vlan_id = $2 AND nic_tag = $3 AND vnet_id = $4",
        );
        assert!(matches!(
            match_napi_containing(&v),
            Some(NapiContainingCase::WithVnet)
        ));
    }

    #[test]
    fn cidr_contains_basic() {
        let (net, bits) = parse_v4_cidr("10.0.0.0/24").unwrap();
        assert!(cidr_contains(net, bits, parse_v4("10.0.0.5").unwrap()));
        assert!(!cidr_contains(net, bits, parse_v4("10.0.1.5").unwrap()));
    }

    #[test]
    fn matches_napi_gap_ip_string_form() {
        let s = canonicalize(
            "SELECT * FROM (SELECT ipaddr+1 gap_start, least(coalesce(lead(ipaddr) OVER (ORDER BY ipaddr) - ipaddr - 1, 0), 1024) gap_length FROM napi_ips_abc WHERE ipaddr >= $1 AND ipaddr <= $2) t WHERE gap_length > 0 LIMIT 1",
        );
        assert_eq!(
            match_napi_gap_ip(&s),
            Some(("napi_ips_abc".into(), "ipaddr".into()))
        );
    }

    #[test]
    fn matches_wf_stats_and_backfill() {
        let s = canonicalize(
            "SELECT count(_id), execution FROM wf_jobs WHERE created_at > 100 AND created_at < 200 GROUP BY execution",
        );
        assert!(match_wf_count_by_execution(&s));
        let b = canonicalize(
            "SELECT count(_id) FROM wf_jobs WHERE task IS NULL AND vm_uuid IS NOT NULL",
        );
        assert!(match_wf_backfill_count(&b));
    }

    #[test]
    fn ufds_login_pattern_extracted() {
        let stmt = "select count(*) from ufds_o_smartdc where login ~* '^Alice$'";
        let (b, p) = match_ufds_login_count(stmt).unwrap();
        assert_eq!(b, "ufds_o_smartdc");
        assert_eq!(p, "Alice");
    }
}
