// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Per-zone log tailer.
//!
//! Every `interval` (default 5s) the tailer:
//!
//! 1. Lists `/zones/*` to discover candidate zones (a zone exists
//!    iff its directory is there; we don't gate on lifecycle state
//!    because `console.log` survives after a stop and operators may
//!    want to read it post-mortem).
//! 2. For each zone × source (console / platform), reads the new
//!    tail of the corresponding file since the last tick. The last
//!    byte offset is persisted under
//!    `<state_dir>/<source>-<vm_uuid>.offset`.
//! 3. Posts a [`tritond_logs::LogBatch`] per (zone, source) to
//!    tritond's `/v1/agent/logs` endpoint.
//!
//! Robustness rules:
//!
//! * Per-tick read is capped at [`MAX_BYTES_PER_TICK`]; if the file
//!   grew more than that, we skip the gap and post a batch with
//!   `truncated_before = true` so the UI shows the discontinuity.
//! * If the file got smaller (rotated / truncated), the offset is
//!   reset to 0 and the next tick reads from the new start.
//! * Lines longer than [`MAX_LINE_BYTES`] are split into multiple
//!   `text` entries so one runaway line can't OOM the batch.
//! * I/O failures on one file never poison sibling zones -- each
//!   path is tried independently and failures are logged at `warn`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use tritond_client::Client;
use tritond_logs::{LogBatch, LogLine, LogSource};
use uuid::Uuid;

/// Default cadence. Same order of magnitude as a chatty SSH session;
/// keeps lag bounded without overwhelming tritond.
pub const DEFAULT_LOG_INTERVAL: Duration = Duration::from_secs(5);

/// Default zone root on SmartOS. Tests override via [`Config`].
pub const DEFAULT_ZONE_ROOT: &str = "/zones";

/// Default state directory where per-source byte offsets are
/// persisted across agent restarts.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/tritonagent/log-offsets";

/// Cap on bytes read per file per tick. At 5s interval this is
/// ~200 KiB/s sustained, plenty for normal workloads; bursts get
/// the `truncated_before` marker so operators see the gap.
pub const MAX_BYTES_PER_TICK: u64 = 1_024 * 1_024;

/// Cap on a single line's length before we split it. SmartOS bunyan
/// records can be quite long; this prevents one runaway log entry
/// from blowing the batch budget.
pub const MAX_LINE_BYTES: usize = 4 * 1024;

/// Sources the tailer follows.
const SOURCES: &[LogSource] = &[LogSource::Console, LogSource::Platform];

#[derive(Debug, Clone)]
pub struct Config {
    pub cn_uuid: Uuid,
    pub interval: Duration,
    pub zone_root: PathBuf,
    pub state_dir: PathBuf,
}

impl Config {
    pub fn new(cn_uuid: Uuid) -> Self {
        Self {
            cn_uuid,
            interval: DEFAULT_LOG_INTERVAL,
            zone_root: PathBuf::from(DEFAULT_ZONE_ROOT),
            state_dir: PathBuf::from(DEFAULT_STATE_DIR),
        }
    }
}

/// Spawn the tailer loop. Returns a [`LogTailerHandle`] callers can
/// `shutdown().await` to drain in-flight work cleanly.
pub fn spawn(client: Arc<Client>, cfg: Config) -> LogTailerHandle {
    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(run_loop(client, cfg, rx));
    LogTailerHandle {
        join: Some(join),
        shutdown: Some(tx),
    }
}

pub struct LogTailerHandle {
    join: Option<JoinHandle<()>>,
    shutdown: Option<oneshot::Sender<()>>,
}

impl LogTailerHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take()
            && let Err(e) = handle.await
        {
            warn!(error = %e, "log tailer join failed");
        }
    }
}

async fn run_loop(client: Arc<Client>, cfg: Config, mut shutdown: oneshot::Receiver<()>) {
    // Make sure the state dir exists so writes don't fail with ENOENT
    // every tick. Best-effort -- if creation fails we still try to
    // read (and just won't persist offsets).
    if let Err(e) = fs::create_dir_all(&cfg.state_dir).await
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        warn!(path = %cfg.state_dir.display(), error = %e, "log offset dir create failed");
    }

    let mut ticker = tokio::time::interval(cfg.interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if let Err(e) = tick_once(&client, &cfg).await {
                    warn!(error = %e, "log tailer tick failed");
                }
            }
            _ = &mut shutdown => {
                debug!("log tailer shutdown");
                return;
            }
        }
    }
}

async fn tick_once(client: &Client, cfg: &Config) -> anyhow::Result<()> {
    let zones = discover_zones(&cfg.zone_root).await?;
    if zones.is_empty() {
        return Ok(());
    }

    for zone in zones {
        for source in SOURCES {
            if let Err(e) = tail_one(client, cfg, zone, *source).await {
                warn!(
                    zone = %zone,
                    source = %source,
                    error = %e,
                    "tail one source failed; will retry next tick",
                );
            }
        }
    }
    Ok(())
}

async fn discover_zones(root: &Path) -> anyhow::Result<Vec<Uuid>> {
    let mut out = HashSet::new();
    let mut dir = match fs::read_dir(root).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    while let Some(entry) = dir.next_entry().await? {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Triton zones name their dir with the VM UUID. Anything
        // else (e.g. `global`, `joyent`, `cores`) is filtered out
        // here so we don't post non-VM zones into the log store.
        if let Ok(uuid) = Uuid::parse_str(name) {
            out.insert(uuid);
        }
    }
    let mut v: Vec<Uuid> = out.into_iter().collect();
    v.sort();
    Ok(v)
}

async fn tail_one(
    client: &Client,
    cfg: &Config,
    zone: Uuid,
    source: LogSource,
) -> anyhow::Result<()> {
    let log_path = cfg
        .zone_root
        .join(zone.to_string())
        .join("logs")
        .join(source.filename());
    let offset_path = cfg
        .state_dir
        .join(format!("{}-{}.offset", source.as_path(), zone));

    let prev_offset = read_offset(&offset_path).await.unwrap_or(0);

    let mut file = match fs::File::open(&log_path).await {
        Ok(f) => f,
        // No such file is the normal case for non-running zones or
        // sources that haven't produced output yet -- not an error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };

    let metadata = file.metadata().await?;
    let file_size = metadata.len();

    // Rotation: file shrank since last tick. Reset to start.
    let mut read_from = if file_size < prev_offset {
        0
    } else {
        prev_offset
    };
    let mut truncated_before = false;
    let available = file_size.saturating_sub(read_from);

    if available > MAX_BYTES_PER_TICK {
        read_from = file_size - MAX_BYTES_PER_TICK;
        truncated_before = true;
    }

    if file_size == read_from {
        // Nothing new since last tick.
        return Ok(());
    }

    file.seek(SeekFrom::Start(read_from)).await?;
    let to_read = (file_size - read_from) as usize;
    let mut buf = Vec::with_capacity(to_read);
    buf.resize(to_read, 0);
    let n = file.read(&mut buf).await?;
    buf.truncate(n);

    // Split on newlines; drop a trailing partial line (we'll pick
    // it up on the next tick when the writer finishes it).
    let new_offset = read_from + n as u64;
    let last_newline = buf.iter().rposition(|b| *b == b'\n');
    let usable_end = match last_newline {
        Some(idx) => idx + 1,
        None => 0,
    };
    let consumed_offset = read_from + usable_end as u64;
    let usable = &buf[..usable_end];

    let lines = parse_lines(usable);
    if lines.is_empty() && !truncated_before {
        // Persist the offset even when nothing was emitted, so the
        // next tick doesn't re-read this slice.
        let _ = write_offset(&offset_path, consumed_offset).await;
        return Ok(());
    }

    let batch = LogBatch {
        cn_id: cfg.cn_uuid,
        instance_id: zone,
        source,
        truncated_before,
        lines,
    };

    // Single-batch cap: split if the agent ran for a long time
    // without ingesting and discovered a huge new tail. We're
    // conservative: take the most recent MAX_LINES rather than the
    // oldest.
    let batch = clamp_batch(batch);

    client
        .agent_logs_ingest()
        .body(batch)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("agent_logs_ingest: {e}"))?;

    let _ = write_offset(&offset_path, consumed_offset).await;
    // Suppress unused if we ever stop emitting -- the offset write
    // is still useful for the next tick's bookkeeping.
    let _ = new_offset;
    Ok(())
}

fn parse_lines(bytes: &[u8]) -> Vec<LogLine> {
    let mut out: Vec<LogLine> = Vec::new();
    for raw in bytes.split(|b| *b == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(raw);
        let trimmed = text.trim_end_matches('\r');
        // Long-line guard: split into chunks of MAX_LINE_BYTES,
        // emit each as its own LogLine.
        if trimmed.len() <= MAX_LINE_BYTES {
            out.push(decode_one(trimmed));
        } else {
            let mut chunks = trimmed.as_bytes().chunks(MAX_LINE_BYTES).peekable();
            while let Some(chunk) = chunks.next() {
                let chunk_str = String::from_utf8_lossy(chunk).into_owned();
                let label = if chunks.peek().is_some() {
                    format!("{} \u{2026}", chunk_str)
                } else {
                    chunk_str
                };
                out.push(decode_one(&label));
            }
        }
    }
    out
}

/// Decode one line. If it looks like a SmartOS bunyan record (starts
/// with `{`), pluck `time`, `level`, and `msg` out. Otherwise stamp
/// `ingest_ts` and surface the raw text.
fn decode_one(s: &str) -> LogLine {
    let now = Utc::now();
    if s.starts_with('{')
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(s)
    {
        let ts = v
            .get("time")
            .and_then(|t| t.as_str())
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let level = v.get("level").and_then(level_str).map(str::to_string);
        let msg = v
            .get("msg")
            .and_then(|m| m.as_str())
            .unwrap_or(s)
            .to_string();
        return LogLine {
            ts,
            ingest_ts: now,
            level,
            text: msg,
        };
    }
    LogLine {
        ts: None,
        ingest_ts: now,
        level: None,
        text: s.to_string(),
    }
}

/// Translate a bunyan numeric level into the short string form the
/// UI displays. Bunyan levels are 10/20/30/40/50/60 = trace/debug/
/// info/warn/error/fatal.
fn level_str(v: &serde_json::Value) -> Option<&'static str> {
    match v.as_i64()? {
        0..=15 => Some("trace"),
        16..=25 => Some("debug"),
        26..=35 => Some("info"),
        36..=45 => Some("warn"),
        46..=55 => Some("error"),
        _ => Some("fatal"),
    }
}

fn clamp_batch(mut batch: LogBatch) -> LogBatch {
    let max = tritond_logs::LogBatch::MAX_LINES;
    if batch.lines.len() > max {
        let drop_n = batch.lines.len() - max;
        // We kept only the newest `max` lines, so by definition
        // older lines were dropped -- surface that to the UI.
        batch.truncated_before = true;
        batch.lines.drain(0..drop_n);
    }
    batch
}

async fn read_offset(path: &Path) -> Option<u64> {
    let s = fs::read_to_string(path).await.ok()?;
    s.trim().parse().ok()
}

async fn write_offset(path: &Path, offset: u64) -> std::io::Result<()> {
    fs::write(path, offset.to_string()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_lines() {
        let lines = parse_lines(b"hello\nworld\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, "world");
        assert!(lines[0].ts.is_none());
        assert!(lines[0].level.is_none());
    }

    #[test]
    fn parses_bunyan_record() {
        let bytes = b"{\"v\":0,\"name\":\"a\",\"hostname\":\"h\",\"pid\":1,\"level\":40,\"time\":\"2026-01-02T03:04:05.678Z\",\"msg\":\"slow query\"}\n";
        let lines = parse_lines(bytes);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "slow query");
        assert_eq!(lines[0].level.as_deref(), Some("warn"));
        assert!(lines[0].ts.is_some());
    }

    #[test]
    fn long_line_is_split_with_marker() {
        let mut huge = vec![b'A'; MAX_LINE_BYTES * 2];
        huge.push(b'\n');
        let lines = parse_lines(&huge);
        assert!(lines.len() >= 2);
        // First chunk gets the "…" continuation marker; last does not.
        assert!(lines[0].text.ends_with('\u{2026}'));
        assert!(!lines.last().unwrap().text.ends_with('\u{2026}'));
    }

    #[test]
    fn parse_lines_ignores_blank_separators() {
        // `parse_lines` is called with a slice already trimmed to the
        // last newline by `tail_one`; blank intermediate splits (from
        // `\n\n`) are dropped.
        let lines = parse_lines(b"a\n\nb\n");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn discovers_zone_dirs_by_uuid() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let zone = "abcdef01-2345-6789-abcd-ef0123456789";
        fs::create_dir_all(tmp.path().join(zone))
            .await
            .expect("create");
        fs::create_dir_all(tmp.path().join("global"))
            .await
            .expect("create-global");
        let zones = discover_zones(tmp.path()).await.expect("discover");
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].to_string(), zone);
    }
}
