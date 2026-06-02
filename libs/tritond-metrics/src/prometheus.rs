// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Prometheus text-format exposition for `Sample`s.
//!
//! Sized for the per-tenant + admin scrape paths: takes a slice of
//! `Sample`s (e.g. the latest sample per `(schema, identity)` pair
//! returned by [`crate::MetricsStore::latest_for_schema`]) and writes
//! exposition format 0.0.4 to a [`std::fmt::Write`] sink.
//!
//! The schema name is mapped to the metric name verbatim with `.`
//! replaced by `_` so `triton.cpu_per_zone` becomes
//! `triton_cpu_per_zone_user_ns` (etc). Identity fields become labels:
//! `cn`, `tenant`, `project`, `instance`, `series`. Per-mode CPU
//! counters split into one Prom metric per `series` value (matching
//! the cmon agent's `cpu_user_usage` / `cpu_sys_usage` /
//! `cpu_wait_time` exposition).

use std::fmt::Write;

use crate::sample::Sample;
use crate::schema::schemas;

/// Emit `# HELP` + `# TYPE` headers and one line per sample.
///
/// Returns `Err` only if the underlying `Write` does -- callers
/// formatting into a `String` can `.expect("string format never
/// fails")` after passing a `&mut String`. The signature matches
/// `std::fmt::Write` so a `tokio::io::AsyncWrite` adapter can be
/// added later without changing the call sites.
pub fn write_text<W: Write>(out: &mut W, samples: &[Sample]) -> std::fmt::Result {
    // Group by (schema, series) so we only emit the HELP/TYPE
    // preamble once per Prom metric name.
    use std::collections::BTreeMap;
    let mut grouped: BTreeMap<(String, String), Vec<&Sample>> = BTreeMap::new();
    for s in samples {
        let series = s.identity.series.clone().unwrap_or_default();
        grouped
            .entry((s.schema.0.clone(), series))
            .or_default()
            .push(s);
    }

    for ((schema, series), bucket) in grouped {
        let metric_name = prom_metric_name(&schema, &series);
        let (help, kind) = describe(&schema, &series);
        writeln!(out, "# HELP {metric_name} {help}")?;
        writeln!(out, "# TYPE {metric_name} {kind}")?;
        for s in bucket {
            write_sample_line(out, &metric_name, s)?;
        }
    }
    Ok(())
}

fn write_sample_line<W: Write>(out: &mut W, name: &str, s: &Sample) -> std::fmt::Result {
    let value = s.datum.as_f64();
    let ts_ms = s.timestamp.timestamp_millis();
    write!(out, "{name}{{cn=\"{}\"", s.identity.cn_id)?;
    if let Some(t) = s.identity.tenant_id {
        write!(out, ",tenant=\"{t}\"")?;
    }
    if let Some(p) = s.identity.project_id {
        write!(out, ",project=\"{p}\"")?;
    }
    if let Some(i) = s.identity.instance_id {
        write!(out, ",instance=\"{i}\"")?;
    }
    if let Some(dev) = s.identity.device.as_deref() {
        write!(out, ",device=\"{}\"", escape_label(dev))?;
    }
    if let Some(series) = s.identity.series.as_deref() {
        write!(out, ",series=\"{}\"", escape_label(series))?;
    }
    writeln!(out, "}} {value} {ts_ms}")
}

fn prom_metric_name(schema: &str, series: &str) -> String {
    let base = schema.replace('.', "_");
    if series.is_empty() {
        base
    } else {
        format!("{base}_{}_ns", series.replace(['.', '-'], "_"))
    }
}

fn describe(schema: &str, series: &str) -> (&'static str, &'static str) {
    if schema == schemas::CPU_PER_ZONE || schema == schemas::CPU_PER_CN {
        let help = match series {
            "user" => "Cumulative CPU time spent in user mode, nanoseconds.",
            "system" => "Cumulative CPU time spent in system mode, nanoseconds.",
            "iowait" => "Cumulative CPU wait-runqueue time, nanoseconds.",
            _ => "Cumulative CPU time, nanoseconds.",
        };
        (help, "counter")
    } else {
        ("Triton metrics sample.", "untyped")
    }
}

fn escape_label(s: &str) -> String {
    // Prom label values: backslash, double quote, and newline get
    // escaped. This is intentionally minimal -- our schemas only emit
    // ASCII alphanumerics + dashes today.
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::{Datum, SampleIdentity};
    use crate::schema::SchemaName;
    use chrono::TimeZone;
    use uuid::Uuid;

    #[test]
    fn emits_help_type_and_value() {
        let cn = Uuid::nil();
        let inst = Uuid::nil();
        let ts = chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let s = Sample {
            schema: SchemaName::from("triton.cpu_per_zone"),
            identity: SampleIdentity {
                cn_id: cn,
                tenant_id: None,
                project_id: None,
                instance_id: Some(inst),
                series: Some("user".to_string()),
                device: None,
            },
            timestamp: ts,
            datum: Datum::CumulativeU64 { value: 12345 },
        };
        let mut out = String::new();
        write_text(&mut out, &[s]).unwrap();
        assert!(out.contains("# TYPE triton_cpu_per_zone_user_ns counter"));
        assert!(out.contains("triton_cpu_per_zone_user_ns{cn="));
        assert!(out.contains("12345"));
    }
}
