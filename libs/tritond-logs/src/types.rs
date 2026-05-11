// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Wire types posted by the agent and returned by tritond's tail-read
//! endpoint.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Source file the line came from. Stable on the wire -- new sources
/// (e.g. SDC's `metadata.log`) get new variants rather than
/// re-purposing existing ones. `#[non_exhaustive]` so adding a
/// variant is non-breaking for clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    /// `/zones/<uuid>/logs/console.log` -- guest console output.
    Console,
    /// `/zones/<uuid>/logs/platform.log` -- SmartOS platform log.
    Platform,
}

impl LogSource {
    /// Filename within `/zones/<uuid>/logs/` that produces this
    /// source. Used by the agent's log tailer and by docs.
    pub fn filename(&self) -> &'static str {
        match self {
            LogSource::Console => "console.log",
            LogSource::Platform => "platform.log",
        }
    }

    /// URL-safe identifier, used in `GET .../logs/{source}` path
    /// parameters. Mirrors the serde wire form.
    pub fn as_path(&self) -> &'static str {
        match self {
            LogSource::Console => "console",
            LogSource::Platform => "platform",
        }
    }
}

impl fmt::Display for LogSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_path())
    }
}

impl FromStr for LogSource {
    type Err = UnknownLogSource;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "console" => Ok(LogSource::Console),
            "platform" => Ok(LogSource::Platform),
            _ => Err(UnknownLogSource(s.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown log source: {0} (expected: console, platform)")]
pub struct UnknownLogSource(pub String);

/// One line from a log file, as the agent decodes it.
///
/// Timestamps are best-effort: the platform log uses syslog-style
/// `bunyan` records with a parseable `time` field; the console log
/// usually does not. When timestamp parsing fails the agent stamps
/// `ingest_ts` instead so the UI can still order lines monotonically.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogLine {
    /// Best-effort wall-clock timestamp from the line itself. `None`
    /// means the line had no parseable timestamp -- consumers should
    /// fall through to `ingest_ts` for ordering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<DateTime<Utc>>,
    /// When the agent observed the line. Always present; used as a
    /// monotonic ordering key when `ts` is `None`.
    pub ingest_ts: DateTime<Utc>,
    /// Best-effort severity. `info` / `warn` / `error` / `debug`
    /// when the agent could detect it (e.g. from bunyan `level`).
    /// `None` for raw text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// The line itself, with the trailing newline stripped. May be
    /// empty (blank line in the source).
    pub text: String,
}

/// Wire shape for the agent's ingest call. Posted in small batches
/// (one batch per source per tick); tritond rejects batches larger
/// than [`LogBatch::MAX_LINES`] to bound memory.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogBatch {
    /// CN that produced the batch.
    pub cn_id: Uuid,
    /// VM the lines came from.
    pub instance_id: Uuid,
    /// Which file on the zone these lines came from.
    pub source: LogSource,
    /// Whether the agent had to skip bytes between the previous
    /// batch and the first line here (e.g. because the file grew
    /// faster than the cap-per-tick limit). When `true` the UI shows
    /// a "... N bytes skipped ..." marker so operators know the
    /// stream isn't lossless.
    #[serde(default)]
    pub truncated_before: bool,
    /// The lines themselves, in chronological (file) order.
    pub lines: Vec<LogLine>,
}

impl LogBatch {
    /// Conservative cap. Each line is bounded at ~4 KiB by the
    /// tailer, so 2_000 lines * 4 KiB = ~8 MiB worst case per
    /// batch -- enough headroom for a chatty VM mid-burst without
    /// risking OOM at tritond.
    pub const MAX_LINES: usize = 2_000;
}
