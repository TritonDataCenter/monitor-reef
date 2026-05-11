// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Storage trait + in-memory ring buffer for log lines.
//!
//! The trait is shaped for the UI's tail-read pattern: "give me the
//! N most recent lines for `(instance, source)`, optionally before
//! sequence X for pagination". `seq` is a monotonic per-stream
//! counter the store assigns at insert time -- the agent never sees
//! it, so the wire format stays simple. Pagination is by `before_seq`
//! rather than wall-clock to survive clock skew between CN ticks.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::types::{LogBatch, LogLine, LogSource};

/// Capacity per `(instance, source)` ring. Sized so a chatty VM at
/// ~30 lines/sec can keep ~5 minutes of context without paging out;
/// production deploys swap in a ClickHouse-backed sink for longer
/// retention.
const DEFAULT_RING_CAPACITY: usize = 10_000;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LogStoreError {
    #[error("log storage unavailable: {0}")]
    Unavailable(String),
    #[error("invalid tail query: {0}")]
    InvalidQuery(String),
}

#[async_trait::async_trait]
pub trait LogStore: Send + Sync {
    /// Append a batch of lines. Implementations stamp a per-stream
    /// monotonic `seq` on each line as it's inserted.
    async fn insert(&self, batch: LogBatch) -> Result<(), LogStoreError>;

    /// Return the most recent `q.lines` lines for `(instance, source)`,
    /// optionally bounded above by `q.before_seq` for pagination.
    async fn tail(&self, q: &LogTailQuery) -> Result<LogTailResult, LogStoreError>;
}

#[derive(Debug, Clone)]
pub struct LogTailQuery {
    pub instance_id: Uuid,
    pub source: LogSource,
    /// Maximum lines to return. Hard-capped at 5000 server-side.
    pub lines: usize,
    /// Return only lines whose `seq < before_seq` (older). Used by
    /// the UI's "load older" pagination.
    pub before_seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LogTailResult {
    pub instance_id: Uuid,
    pub source: LogSource,
    pub lines: Vec<StoredLogLine>,
    /// `true` when the ring buffer dropped older lines before the
    /// oldest line returned (i.e. there's more history available
    /// in a follow-up backend like ClickHouse, when wired).
    pub older_dropped: bool,
}

/// A line as the store serves it back: the agent's [`LogLine`] plus
/// the per-stream `seq` the store assigned at insert time.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StoredLogLine {
    pub seq: u64,
    #[serde(flatten)]
    pub line: LogLine,
}

/// In-memory ring keyed by `(instance, source)`. Dev / test default.
pub struct RingBufferLogStore {
    capacity: usize,
    inner: Mutex<Inner>,
}

struct Inner {
    rings: HashMap<(Uuid, LogSource), Ring>,
    next_seq: u64,
}

struct Ring {
    buf: VecDeque<StoredLogLine>,
    /// Total number of lines ever pushed into this ring. Used to
    /// detect that older lines have been evicted.
    pushed: u64,
}

impl RingBufferLogStore {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_RING_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            inner: Mutex::new(Inner {
                rings: HashMap::new(),
                next_seq: 1,
            }),
        }
    }
}

impl Default for RingBufferLogStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl LogStore for RingBufferLogStore {
    async fn insert(&self, batch: LogBatch) -> Result<(), LogStoreError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| LogStoreError::Unavailable("log ring mutex poisoned".into()))?;

        let key = (batch.instance_id, batch.source);
        let ring = guard.rings.entry(key).or_insert_with(|| Ring {
            buf: VecDeque::with_capacity(self.capacity.min(1024)),
            pushed: 0,
        });

        if batch.truncated_before && !ring.buf.is_empty() {
            // Surface the gap as an in-band synthetic line so the UI
            // can show "... bytes skipped ..." without a separate
            // sideband.
            let seq = guard.next_seq;
            guard.next_seq = guard.next_seq.saturating_add(1);
            let ring = guard.rings.get_mut(&key).expect("ring just inserted above");
            ring.buf.push_back(StoredLogLine {
                seq,
                line: LogLine {
                    ts: None,
                    ingest_ts: chrono::Utc::now(),
                    level: Some("warn".to_string()),
                    text: "... bytes skipped (log grew faster than ingest cap) ...".into(),
                },
            });
            ring.pushed += 1;
            while ring.buf.len() > self.capacity {
                ring.buf.pop_front();
            }
        }

        for line in batch.lines {
            let seq = guard.next_seq;
            guard.next_seq = guard.next_seq.saturating_add(1);
            let ring = guard.rings.get_mut(&key).expect("ring just inserted above");
            ring.buf.push_back(StoredLogLine { seq, line });
            ring.pushed += 1;
            while ring.buf.len() > self.capacity {
                ring.buf.pop_front();
            }
        }

        Ok(())
    }

    async fn tail(&self, q: &LogTailQuery) -> Result<LogTailResult, LogStoreError> {
        if q.lines == 0 {
            return Err(LogStoreError::InvalidQuery("lines must be > 0".into()));
        }
        let max = q.lines.min(5_000);

        let guard = self
            .inner
            .lock()
            .map_err(|_| LogStoreError::Unavailable("log ring mutex poisoned".into()))?;

        let Some(ring) = guard.rings.get(&(q.instance_id, q.source)) else {
            return Ok(LogTailResult {
                instance_id: q.instance_id,
                source: q.source,
                lines: Vec::new(),
                older_dropped: false,
            });
        };

        // Walk newest → oldest, collecting up to `max` lines that
        // satisfy `seq < before_seq`. Reversing back to chronological
        // before returning keeps the UI's append-at-bottom code path
        // simple.
        let mut out: Vec<StoredLogLine> = Vec::with_capacity(max);
        for entry in ring.buf.iter().rev() {
            if let Some(before) = q.before_seq
                && entry.seq >= before
            {
                continue;
            }
            out.push(entry.clone());
            if out.len() >= max {
                break;
            }
        }
        out.reverse();

        // If the oldest line in the returned slice has a `seq`
        // greater than `pushed - buf.len() + 1`, that means older
        // entries were evicted to make room. Surface that to the UI.
        let oldest_kept_seq = ring.buf.front().map(|e| e.seq).unwrap_or(0);
        let older_dropped = match out.first() {
            Some(first) => first.seq > oldest_kept_seq || ring.pushed > ring.buf.len() as u64,
            None => false,
        };

        Ok(LogTailResult {
            instance_id: q.instance_id,
            source: q.source,
            lines: out,
            older_dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LogLine;
    use chrono::Utc;

    fn line(text: &str) -> LogLine {
        LogLine {
            ts: None,
            ingest_ts: Utc::now(),
            level: None,
            text: text.to_string(),
        }
    }

    fn batch(inst: Uuid, source: LogSource, lines: Vec<LogLine>) -> LogBatch {
        LogBatch {
            cn_id: Uuid::nil(),
            instance_id: inst,
            source,
            truncated_before: false,
            lines,
        }
    }

    #[tokio::test]
    async fn tail_returns_chronological_slice() {
        let store = RingBufferLogStore::new();
        let inst = Uuid::nil();
        store
            .insert(batch(
                inst,
                LogSource::Console,
                vec![line("a"), line("b"), line("c")],
            ))
            .await
            .expect("insert");

        let r = store
            .tail(&LogTailQuery {
                instance_id: inst,
                source: LogSource::Console,
                lines: 10,
                before_seq: None,
            })
            .await
            .expect("tail");
        let texts: Vec<&str> = r.lines.iter().map(|s| s.line.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
        assert!(!r.older_dropped);
    }

    #[tokio::test]
    async fn ring_caps_and_reports_drop() {
        let store = RingBufferLogStore::with_capacity(3);
        let inst = Uuid::nil();
        store
            .insert(batch(
                inst,
                LogSource::Console,
                vec![line("a"), line("b"), line("c"), line("d"), line("e")],
            ))
            .await
            .expect("insert");

        let r = store
            .tail(&LogTailQuery {
                instance_id: inst,
                source: LogSource::Console,
                lines: 10,
                before_seq: None,
            })
            .await
            .expect("tail");
        let texts: Vec<&str> = r.lines.iter().map(|s| s.line.text.as_str()).collect();
        assert_eq!(texts, vec!["c", "d", "e"]);
        assert!(r.older_dropped, "ring evicted a,b -- consumer should know");
    }

    #[tokio::test]
    async fn before_seq_pages_older() {
        let store = RingBufferLogStore::new();
        let inst = Uuid::nil();
        store
            .insert(batch(
                inst,
                LogSource::Console,
                vec![line("a"), line("b"), line("c"), line("d"), line("e")],
            ))
            .await
            .expect("insert");

        // First fetch: newest 2 ("d", "e")
        let r1 = store
            .tail(&LogTailQuery {
                instance_id: inst,
                source: LogSource::Console,
                lines: 2,
                before_seq: None,
            })
            .await
            .expect("tail1");
        let t1: Vec<&str> = r1.lines.iter().map(|s| s.line.text.as_str()).collect();
        assert_eq!(t1, vec!["d", "e"]);

        // Page back from the oldest seq we got.
        let oldest = r1.lines[0].seq;
        let r2 = store
            .tail(&LogTailQuery {
                instance_id: inst,
                source: LogSource::Console,
                lines: 2,
                before_seq: Some(oldest),
            })
            .await
            .expect("tail2");
        let t2: Vec<&str> = r2.lines.iter().map(|s| s.line.text.as_str()).collect();
        assert_eq!(t2, vec!["b", "c"]);
    }

    #[tokio::test]
    async fn truncated_inserts_gap_marker() {
        let store = RingBufferLogStore::new();
        let inst = Uuid::nil();
        store
            .insert(batch(inst, LogSource::Console, vec![line("a")]))
            .await
            .expect("insert");
        store
            .insert(LogBatch {
                cn_id: Uuid::nil(),
                instance_id: inst,
                source: LogSource::Console,
                truncated_before: true,
                lines: vec![line("z")],
            })
            .await
            .expect("insert2");
        let r = store
            .tail(&LogTailQuery {
                instance_id: inst,
                source: LogSource::Console,
                lines: 10,
                before_seq: None,
            })
            .await
            .expect("tail");
        let texts: Vec<&str> = r.lines.iter().map(|s| s.line.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "a",
                "... bytes skipped (log grew faster than ingest cap) ...",
                "z"
            ]
        );
    }
}
