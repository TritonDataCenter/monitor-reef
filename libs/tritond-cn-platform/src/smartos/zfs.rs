// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Thin wrapper around `/sbin/zfs` and `/sbin/zpool`.
//!
//! The node-zfs module (`deps/node-zfs`) hands parsed tab-separated output
//! back to JavaScript callers. We reproduce the wire format by running the
//! same commands ourselves and converting the columns into `serde_json`
//! objects. Tests override the binary paths to point at shell scripts that
//! emit captured output.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use thiserror::Error;

/// Default columns the legacy `zpool.list` returns.
///
/// Matches `zpool.listFields_` in node-zfs. Kept verbatim so downstream
/// callers (and any operator scripts grepping JSON fields) see the exact
/// same keys.
pub const ZPOOL_LIST_FIELDS: &[&str] = &[
    "name",
    "size",
    "allocated",
    "free",
    "cap",
    "health",
    "altroot",
];

/// Default columns the legacy `zfs.list` returns.
pub const ZFS_LIST_FIELDS: &[&str] = &["name", "used", "avail", "refer", "type", "mountpoint"];

/// Default binary paths on SmartOS compute nodes.
pub const DEFAULT_ZFS_BIN: &str = "/sbin/zfs";
pub const DEFAULT_ZPOOL_BIN: &str = "/sbin/zpool";

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ZfsError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} exited with status {status}: {stderr}")]
    NonZeroExit {
        path: PathBuf,
        status: ExitStatus,
        stderr: String,
    },
    #[error("unexpected output format: {0}")]
    BadOutput(String),
}

/// Dataset type filter for `list_datasets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DatasetType {
    Filesystem,
    Volume,
    Snapshot,
    All,
}

impl DatasetType {
    fn as_arg(self) -> &'static str {
        match self {
            DatasetType::Filesystem => "filesystem",
            DatasetType::Volume => "volume",
            DatasetType::Snapshot => "snapshot",
            DatasetType::All => "all",
        }
    }
}

/// Options for `list_datasets`.
#[derive(Debug, Clone)]
pub struct ListDatasetsOptions {
    pub dataset: Option<String>,
    pub kind: DatasetType,
    pub recursive: bool,
}

impl Default for ListDatasetsOptions {
    fn default() -> Self {
        Self {
            dataset: None,
            kind: DatasetType::All,
            recursive: false,
        }
    }
}

/// Health view of a single pool, parsed from `zpool status -v`.
///
/// illumos `zpool status` has no machine-readable flag we can rely on across
/// platform versions (`-j`/JSON only landed in newer OpenZFS), so we parse the
/// human tree. The shape is deliberately flat -- each device carries a `depth`
/// -- so the admin UI can render the vdev tree without re-parsing.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PoolStatus {
    pub name: String,
    pub state: String,
    /// `status:` advisory (why the pool is unhealthy), if present.
    pub status_line: Option<String>,
    /// `action:` advisory (what the operator should do), if present.
    pub action_line: Option<String>,
    pub scan: Option<PoolScan>,
    pub devices: Vec<PoolDevice>,
    /// `errors:` line, e.g. "No known data errors".
    pub errors: Option<String>,
}

/// Parsed `scan:` line(s) from `zpool status`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PoolScan {
    /// Full scan text, continuation lines joined with spaces.
    pub summary: String,
    /// First word: `scrub` | `resilver` | `trim` | `none` | `other`.
    pub kind: String,
    pub in_progress: bool,
    /// Percent complete (0-100) for an in-progress scan, when reported.
    pub percent_done: Option<f64>,
}

/// One row of the `config:` vdev tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PoolDevice {
    /// Tree depth: 0 = pool, 1 = top-level vdev (or leaf in a stripe).
    pub depth: usize,
    pub name: String,
    pub state: String,
    pub read_errors: u64,
    pub write_errors: u64,
    pub cksum_errors: u64,
    /// Trailing annotation such as "(resilvering)".
    pub note: Option<String>,
}

/// One row of `zpool iostat -Hp -v <pool>` (since-boot snapshot).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PoolIostatRow {
    /// Tree depth derived from the name's leading indentation. Best-effort:
    /// flat (all 0) on platforms whose `-H` output drops the indentation.
    pub depth: usize,
    pub name: String,
    pub alloc_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub read_ops: Option<u64>,
    pub write_ops: Option<u64>,
    pub read_bw: Option<u64>,
    pub write_bw: Option<u64>,
}

/// Snapshot summary for a pool, capped so per-VM image snapshots can't bloat
/// the heartbeat status payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SnapshotSummary {
    pub count: usize,
    pub total_used_bytes: u64,
    /// The `top_n` snapshots by `used` bytes.
    pub largest: Vec<SnapshotEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SnapshotEntry {
    pub name: String,
    pub used_bytes: u64,
}

/// Zfs/zpool command runner. Holds binary paths so tests can swap in mocks.
#[derive(Debug, Clone)]
pub struct ZfsTool {
    pub zfs_bin: PathBuf,
    pub zpool_bin: PathBuf,
}

impl Default for ZfsTool {
    fn default() -> Self {
        Self {
            zfs_bin: PathBuf::from(DEFAULT_ZFS_BIN),
            zpool_bin: PathBuf::from(DEFAULT_ZPOOL_BIN),
        }
    }
}

impl ZfsTool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_bins(zfs_bin: impl Into<PathBuf>, zpool_bin: impl Into<PathBuf>) -> Self {
        Self {
            zfs_bin: zfs_bin.into(),
            zpool_bin: zpool_bin.into(),
        }
    }

    /// `zpool list -Hp -o <fields>` -> Vec of row objects keyed by field.
    pub async fn list_pools(
        &self,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, ZfsError> {
        let fields = ZPOOL_LIST_FIELDS.join(",");
        let args = vec!["list", "-H", "-p", "-o", &fields];
        let stdout = run_cmd(&self.zpool_bin, &args).await?;
        Ok(parse_rows(&stdout, ZPOOL_LIST_FIELDS))
    }

    /// `zfs list -Hp -o <fields> -t <type> [-r] [<dataset>]`.
    pub async fn list_datasets(
        &self,
        options: &ListDatasetsOptions,
    ) -> Result<Vec<serde_json::Map<String, serde_json::Value>>, ZfsError> {
        let fields = ZFS_LIST_FIELDS.join(",");
        let mut args: Vec<&str> = vec![
            "list",
            "-H",
            "-p",
            "-o",
            &fields,
            "-t",
            options.kind.as_arg(),
        ];
        if options.recursive {
            args.push("-r");
        }
        if let Some(dataset) = options.dataset.as_deref() {
            args.push(dataset);
        }
        let stdout = run_cmd(&self.zfs_bin, &args).await?;
        Ok(parse_rows(&stdout, ZFS_LIST_FIELDS))
    }

    /// `zpool status -v` parsed into a typed per-pool health view.
    ///
    /// Read-only servicing surface: pool health, scrub/resilver progress, and
    /// per-device read/write/cksum error counts. Best-effort parser (see
    /// [`PoolStatus`]); unknown sections are skipped, not fatal.
    pub async fn pool_status_all(&self) -> Result<Vec<PoolStatus>, ZfsError> {
        let stdout = run_cmd(&self.zpool_bin, &["status", "-v"]).await?;
        Ok(parse_pool_status_all(&stdout))
    }

    /// `zpool iostat -Hp -v <pool>` -> per-device since-boot stats.
    pub async fn pool_iostat(&self, pool: &str) -> Result<Vec<PoolIostatRow>, ZfsError> {
        let stdout = run_cmd(&self.zpool_bin, &["iostat", "-Hp", "-v", pool]).await?;
        Ok(parse_iostat(&stdout))
    }

    /// Summarize every snapshot in the pool, keeping only the `top_n` largest
    /// by `used` bytes (plus a total + count).
    pub async fn snapshot_summary(&self, top_n: usize) -> Result<SnapshotSummary, ZfsError> {
        let options = ListDatasetsOptions {
            dataset: None,
            kind: DatasetType::Snapshot,
            recursive: false,
        };
        let rows = self.list_datasets(&options).await?;
        Ok(summarize_snapshots(&rows, top_n))
    }

    /// `zfs create <dataset>`.
    pub async fn create_dataset(&self, dataset: &str) -> Result<(), ZfsError> {
        self.run_mutation(&["create", dataset]).await
    }

    /// `zfs destroy <dataset>`.
    ///
    /// Non-recursive to match the legacy `zfs.destroy` (which is distinct
    /// from `destroyAll`). Callers that need recursion can pre-walk the
    /// dataset tree.
    pub async fn destroy_dataset(&self, dataset: &str) -> Result<(), ZfsError> {
        self.run_mutation(&["destroy", dataset]).await
    }

    /// `zfs rename <dataset> <new_name>`.
    pub async fn rename_dataset(&self, dataset: &str, new_name: &str) -> Result<(), ZfsError> {
        self.run_mutation(&["rename", dataset, new_name]).await
    }

    /// `zfs snapshot <name>`.
    pub async fn snapshot_dataset(&self, snapshot: &str) -> Result<(), ZfsError> {
        self.run_mutation(&["snapshot", snapshot]).await
    }

    /// `zfs rollback -r <name>`.
    ///
    /// Legacy `zfs.rollback` always passes `-r`, so recent snapshots are
    /// destroyed as part of the rollback. That's the behavior callers expect.
    pub async fn rollback_dataset(&self, snapshot: &str) -> Result<(), ZfsError> {
        self.run_mutation(&["rollback", "-r", snapshot]).await
    }

    /// `zfs clone <snapshot> <dataset>`.
    pub async fn clone_dataset(&self, snapshot: &str, dataset: &str) -> Result<(), ZfsError> {
        self.run_mutation(&["clone", snapshot, dataset]).await
    }

    /// `zfs set key=val <dataset>` per property.
    ///
    /// Legacy iteratively runs one `zfs set` per property rather than
    /// combining them, and we preserve that so errors still report the
    /// offending property by line.
    pub async fn set_properties(
        &self,
        dataset: &str,
        properties: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), ZfsError> {
        for (key, value) in properties {
            let value_str = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Null => "-".to_string(),
                _ => {
                    return Err(ZfsError::BadOutput(format!(
                        "property {key}: unsupported value type {value}"
                    )));
                }
            };
            let pair = format!("{key}={value_str}");
            self.run_mutation(&["set", &pair, dataset]).await?;
        }
        Ok(())
    }

    /// Run a zfs mutation that yields no interesting stdout. Surfaces stderr
    /// verbatim on non-zero exit.
    async fn run_mutation(&self, args: &[&str]) -> Result<(), ZfsError> {
        let output = tokio::process::Command::new(&self.zfs_bin)
            .args(args)
            .output()
            .await
            .map_err(|source| ZfsError::Spawn {
                path: self.zfs_bin.clone(),
                source,
            })?;
        if !output.status.success() {
            return Err(ZfsError::NonZeroExit {
                path: self.zfs_bin.clone(),
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(())
    }

    /// `zfs get -Hp -o name,property,value <props>` across every dataset.
    ///
    /// Same wire format as [`get_properties`] but with no dataset filter,
    /// so a single `zfs get` call produces the full pool-wide view the
    /// disk-usage sampler needs. The legacy `getDiskUsage` implementation
    /// calls `zfs.get(null, [...], true, ...)` the same way.
    pub async fn get_all_properties(
        &self,
        properties: &[&str],
    ) -> Result<serde_json::Map<String, serde_json::Value>, ZfsError> {
        self.get_properties(None, properties).await
    }

    /// `zfs get -Hp -o name,property,value <props> [<dataset>]`.
    ///
    /// Returns `{ dataset: { property: value } }` to match the legacy
    /// `parsePropertyList` shape.
    pub async fn get_properties(
        &self,
        dataset: Option<&str>,
        properties: &[&str],
    ) -> Result<serde_json::Map<String, serde_json::Value>, ZfsError> {
        // Empty properties defaults to "all" so consumers don't have to
        // special-case it.
        let props_arg: String = if properties.is_empty() {
            "all".to_string()
        } else {
            properties.join(",")
        };
        let mut args: Vec<&str> = vec!["get", "-Hp", "-o", "name,property,value", &props_arg];
        if let Some(d) = dataset {
            args.push(d);
        }
        let stdout = run_cmd(&self.zfs_bin, &args).await?;
        parse_property_list(&stdout)
    }
}

async fn run_cmd(bin: &Path, args: &[&str]) -> Result<String, ZfsError> {
    let output = tokio::process::Command::new(bin)
        .args(args)
        .output()
        .await
        .map_err(|source| ZfsError::Spawn {
            path: bin.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(ZfsError::NonZeroExit {
            path: bin.to_path_buf(),
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parse tab-separated rows into a Vec of `{column: value}` maps.
///
/// Pulls the legacy behavior verbatim: values stay as strings even when they
/// look numeric, because downstream code already special-cases what needs
/// `parseInt`. Missing columns (when a row is short) get an empty string, and
/// extras are dropped silently -- both mirror node-zfs.
fn parse_rows(text: &str, fields: &[&str]) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let mut obj = serde_json::Map::with_capacity(fields.len());
        for field in fields {
            let val = cols.next().unwrap_or("");
            obj.insert(
                (*field).to_string(),
                serde_json::Value::String(val.to_string()),
            );
        }
        rows.push(obj);
    }
    rows
}

/// Parse `name\tproperty\tvalue` lines into a nested map.
fn parse_property_list(text: &str) -> Result<serde_json::Map<String, serde_json::Value>, ZfsError> {
    let mut out: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let name = parts
            .next()
            .ok_or_else(|| ZfsError::BadOutput(format!("missing name column: {line}")))?;
        let prop = parts
            .next()
            .ok_or_else(|| ZfsError::BadOutput(format!("missing property column: {line}")))?;
        let value = parts
            .next()
            .ok_or_else(|| ZfsError::BadOutput(format!("missing value column: {line}")))?;

        let entry = out
            .entry(name.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        match entry {
            serde_json::Value::Object(map) => {
                map.insert(
                    prop.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
            // `or_insert_with` just wrote an Object; the unreachable branch
            // exists only because Value::Object destructuring is fallible.
            _ => unreachable!("entry was inserted as Value::Object above"),
        }
    }
    Ok(out)
}

/// Recognized `zpool status` header keys. Restricting to this set keeps device
/// rows (which never carry a leading "key:") from being mistaken for headers.
const STATUS_KEYS: &[&str] = &[
    "pool", "state", "status", "action", "scan", "config", "errors", "see",
];

/// Split `zpool status -v` into one [`PoolStatus`] per pool.
fn parse_pool_status_all(text: &str) -> Vec<PoolStatus> {
    let mut pools = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("pool:") {
            if let Some(ps) = parse_one_pool(&block) {
                pools.push(ps);
            }
            block = vec![line];
        } else if !block.is_empty() {
            block.push(line);
        }
    }
    if let Some(ps) = parse_one_pool(&block) {
        pools.push(ps);
    }
    pools
}

fn split_status_key(trimmed: &str) -> Option<(&str, &str)> {
    let colon = trimmed.find(':')?;
    let key = &trimmed[..colon];
    if !STATUS_KEYS.contains(&key) {
        return None;
    }
    Some((key, trimmed[colon + 1..].trim_start()))
}

fn append_status_value(slot: &mut Option<String>, text: &str) {
    match slot {
        Some(existing) => {
            existing.push(' ');
            existing.push_str(text);
        }
        None => *slot = Some(text.to_string()),
    }
}

fn parse_one_pool(block: &[&str]) -> Option<PoolStatus> {
    enum Cont {
        None,
        Status,
        Action,
        Scan,
    }

    let mut name: Option<String> = None;
    let mut state = String::new();
    let mut status_line: Option<String> = None;
    let mut action_line: Option<String> = None;
    let mut scan_text: Option<String> = None;
    let mut errors: Option<String> = None;
    let mut devices: Vec<PoolDevice> = Vec::new();

    let mut cont = Cont::None;
    let mut in_config = false;
    let mut config_seen_header = false;
    let mut config_base_indent: Option<usize> = None;

    for line in block {
        let trimmed = line.trim_start();

        if let Some((key, value)) = split_status_key(trimmed) {
            in_config = false;
            cont = Cont::None;
            match key {
                "pool" => name = Some(value.to_string()),
                "state" => state = value.to_string(),
                "status" => {
                    status_line = Some(value.to_string());
                    cont = Cont::Status;
                }
                "action" => {
                    action_line = Some(value.to_string());
                    cont = Cont::Action;
                }
                "scan" => {
                    scan_text = Some(value.to_string());
                    cont = Cont::Scan;
                }
                "errors" => errors = Some(value.to_string()),
                "config" => {
                    in_config = true;
                    config_seen_header = false;
                }
                _ => {}
            }
            continue;
        }

        if in_config {
            if trimmed.is_empty() {
                // A blank line ends the config section, but only once device
                // rows have started (the line right after `config:` is blank).
                if config_seen_header && !devices.is_empty() {
                    in_config = false;
                }
                continue;
            }
            if !config_seen_header && trimmed.starts_with("NAME") {
                config_seen_header = true;
                continue;
            }
            if let Some(dev) = parse_device_row(line, &mut config_base_indent) {
                devices.push(dev);
            }
            continue;
        }

        if !trimmed.is_empty() {
            match cont {
                Cont::Status => append_status_value(&mut status_line, trimmed),
                Cont::Action => append_status_value(&mut action_line, trimmed),
                Cont::Scan => append_status_value(&mut scan_text, trimmed),
                Cont::None => {}
            }
        }
    }

    Some(PoolStatus {
        name: name?,
        state,
        status_line,
        action_line,
        scan: scan_text.map(|s| parse_scan(&s)),
        devices,
        errors,
    })
}

fn parse_device_row(line: &str, base_indent: &mut Option<usize>) -> Option<PoolDevice> {
    let indent = line.len() - line.trim_start().len();
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let name = tokens.first().copied()?.to_string();

    let base = *base_indent.get_or_insert(indent);
    let depth = indent.saturating_sub(base) / 2;

    let count = |i: usize| tokens.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let note = if tokens.len() > 5 {
        Some(tokens[5..].join(" "))
    } else {
        None
    };

    Some(PoolDevice {
        depth,
        name,
        state: tokens.get(1).map(|s| s.to_string()).unwrap_or_default(),
        read_errors: count(2),
        write_errors: count(3),
        cksum_errors: count(4),
        note,
    })
}

fn parse_scan(summary: &str) -> PoolScan {
    let first = summary
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let kind = match first.as_str() {
        "scrub" | "trim" | "none" => first,
        "resilver" | "resilvered" => "resilver".to_string(),
        _ => "other".to_string(),
    };
    let in_progress = summary.contains("in progress");
    let percent_done = summary
        .split_whitespace()
        .find_map(|tok| tok.strip_suffix('%').and_then(|p| p.parse::<f64>().ok()));

    PoolScan {
        summary: summary.to_string(),
        kind,
        in_progress,
        percent_done,
    }
}

/// Parse `zpool iostat -Hp -v` rows (tab-separated). The name column keeps the
/// leading indentation that encodes vdev nesting.
fn parse_iostat(text: &str) -> Vec<PoolIostatRow> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let Some(name_raw) = cols.first() else {
            continue;
        };
        let name = name_raw.trim();
        if name.is_empty() {
            continue;
        }
        let depth = (name_raw.len() - name_raw.trim_start().len()) / 2;
        let col = |i: usize| cols.get(i).and_then(|s| s.trim().parse::<u64>().ok());
        rows.push(PoolIostatRow {
            depth,
            name: name.to_string(),
            alloc_bytes: col(1),
            free_bytes: col(2),
            read_ops: col(3),
            write_ops: col(4),
            read_bw: col(5),
            write_bw: col(6),
        });
    }
    rows
}

fn summarize_snapshots(
    rows: &[serde_json::Map<String, serde_json::Value>],
    top_n: usize,
) -> SnapshotSummary {
    let mut entries: Vec<SnapshotEntry> = Vec::with_capacity(rows.len());
    let mut total_used_bytes: u64 = 0;
    for row in rows {
        let Some(name) = row.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let used = row
            .get("used")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        total_used_bytes = total_used_bytes.saturating_add(used);
        entries.push(SnapshotEntry {
            name: name.to_string(),
            used_bytes: used,
        });
    }
    let count = entries.len();
    entries.sort_by_key(|e| std::cmp::Reverse(e.used_bytes));
    entries.truncate(top_n);
    SnapshotSummary {
        count,
        total_used_bytes,
        largest: entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rows_round_trip() {
        let text = "zones\t8000000\t500000\t7500000\t6\tONLINE\t-\n\
                    zones/images\t1000000\t100000\t900000\t10\tONLINE\t-\n";
        let rows = parse_rows(text, ZPOOL_LIST_FIELDS);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "zones");
        assert_eq!(rows[0]["size"], "8000000");
        assert_eq!(rows[0]["health"], "ONLINE");
        assert_eq!(rows[1]["name"], "zones/images");
    }

    #[test]
    fn parse_rows_tolerates_short_rows() {
        // Real zpool output never truncates columns, but if it does we
        // should return an empty string rather than panic.
        let text = "zones\t8000000\n";
        let rows = parse_rows(text, ZPOOL_LIST_FIELDS);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "zones");
        assert_eq!(rows[0]["health"], "");
    }

    #[test]
    fn parse_property_list_nests_by_dataset() {
        let text = "zones\tused\t8000000\n\
                    zones\tavailable\t500000\n\
                    zones/images\tused\t100000\n";
        let out = parse_property_list(text).expect("parse");
        assert_eq!(out["zones"]["used"], "8000000");
        assert_eq!(out["zones"]["available"], "500000");
        assert_eq!(out["zones/images"]["used"], "100000");
    }

    #[test]
    fn parse_property_list_rejects_malformed_rows() {
        let text = "zones\tused\n"; // missing value column
        let err = parse_property_list(text).expect_err("should fail");
        assert!(matches!(err, ZfsError::BadOutput(_)));
    }

    #[test]
    fn parse_pool_status_healthy_mirror() {
        let text = [
            "  pool: zones",
            " state: ONLINE",
            "  scan: scrub repaired 0B in 02:13:45 with 0 errors on Sun May 18 2026",
            "config:",
            "",
            "\tNAME        STATE     READ WRITE CKSUM",
            "\tzones       ONLINE       0     0     0",
            "\t  mirror-0  ONLINE       0     0     0",
            "\t    c1t0d0  ONLINE       0     0     0",
            "\t    c1t1d0  ONLINE       0     0     0",
            "",
            "errors: No known data errors",
        ]
        .join("\n");

        let pools = parse_pool_status_all(&text);
        assert_eq!(pools.len(), 1);
        let p = &pools[0];
        assert_eq!(p.name, "zones");
        assert_eq!(p.state, "ONLINE");
        assert_eq!(p.errors.as_deref(), Some("No known data errors"));
        let scan = p.scan.as_ref().expect("scan present");
        assert_eq!(scan.kind, "scrub");
        assert!(!scan.in_progress);
        assert_eq!(p.devices.len(), 4);
        assert_eq!((p.devices[0].name.as_str(), p.devices[0].depth), ("zones", 0));
        assert_eq!((p.devices[1].name.as_str(), p.devices[1].depth), ("mirror-0", 1));
        assert_eq!((p.devices[2].name.as_str(), p.devices[2].depth), ("c1t0d0", 2));
        assert!(p
            .devices
            .iter()
            .all(|d| d.read_errors == 0 && d.write_errors == 0 && d.cksum_errors == 0));
    }

    #[test]
    fn parse_pool_status_degraded_resilver() {
        let text = [
            "  pool: zones",
            " state: DEGRADED",
            "status: One or more devices is currently being resilvered.",
            "\tThe pool will continue to function in a degraded state.",
            "action: Wait for the resilver to complete.",
            "  scan: resilver in progress since Mon May 19 10:00:00 2026",
            "\t1.20T scanned at 100M/s, 1.10T issued at 90M/s, 2.50T total",
            "\t0B resilvered, 44.00% done, 01:30:00 to go",
            "config:",
            "",
            "\tNAME          STATE     READ WRITE CKSUM",
            "\tzones         DEGRADED     0     0     2",
            "\t  mirror-0    DEGRADED     0     0     2",
            "\t    c1t0d0    ONLINE       0     0     0",
            "\t    c1t2d0    ONLINE       0     0     0  (resilvering)",
            "",
            "errors: No known data errors",
        ]
        .join("\n");

        let pools = parse_pool_status_all(&text);
        assert_eq!(pools.len(), 1);
        let p = &pools[0];
        assert_eq!(p.state, "DEGRADED");
        assert!(p.status_line.as_deref().unwrap_or("").contains("resilvered"));
        let scan = p.scan.as_ref().expect("scan present");
        assert_eq!(scan.kind, "resilver");
        assert!(scan.in_progress);
        assert_eq!(scan.percent_done, Some(44.0));
        assert_eq!(p.devices[0].cksum_errors, 2);
        assert!(p
            .devices
            .iter()
            .any(|d| d.note.as_deref() == Some("(resilvering)")));
    }

    #[test]
    fn parse_pool_status_multiple_pools() {
        let text = [
            "  pool: zones",
            " state: ONLINE",
            "  scan: none requested",
            "config:",
            "",
            "\tNAME      STATE     READ WRITE CKSUM",
            "\tzones     ONLINE       0     0     0",
            "\t  c1t0d0  ONLINE       0     0     0",
            "",
            "errors: No known data errors",
            "",
            "  pool: tank",
            " state: ONLINE",
            "  scan: none requested",
            "config:",
            "",
            "\tNAME      STATE     READ WRITE CKSUM",
            "\ttank      ONLINE       0     0     0",
            "\t  c2t0d0  ONLINE       0     0     0",
            "",
            "errors: No known data errors",
        ]
        .join("\n");

        let pools = parse_pool_status_all(&text);
        assert_eq!(pools.len(), 2);
        assert_eq!(pools[0].name, "zones");
        assert_eq!(pools[1].name, "tank");
        assert_eq!(pools[0].scan.as_ref().expect("scan").kind, "none");
    }

    #[test]
    fn parse_iostat_tree() {
        let text = [
            "zones\t1234567\t7654321\t10\t5\t1048576\t524288",
            "  mirror-0\t1234567\t7654321\t10\t5\t1048576\t524288",
            "    c1t0d0\t617283\t3827160\t5\t2\t524288\t262144",
            "    c1t1d0\t617284\t3827161\t5\t3\t524288\t262144",
        ]
        .join("\n");

        let rows = parse_iostat(&text);
        assert_eq!(rows.len(), 4);
        assert_eq!((rows[0].name.as_str(), rows[0].depth), ("zones", 0));
        assert_eq!(rows[0].alloc_bytes, Some(1234567));
        assert_eq!((rows[1].name.as_str(), rows[1].depth), ("mirror-0", 1));
        assert_eq!((rows[2].name.as_str(), rows[2].depth), ("c1t0d0", 2));
        assert_eq!(rows[2].read_ops, Some(5));
    }

    #[test]
    fn parse_iostat_dashes_become_none() {
        let rows = parse_iostat("spare-0\t-\t-\t-\t-\t-\t-\n");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].alloc_bytes, None);
    }

    #[test]
    fn summarize_snapshots_caps_and_totals() {
        let mk = |name: &str, used: &str| {
            let mut m = serde_json::Map::new();
            m.insert("name".to_string(), serde_json::Value::String(name.to_string()));
            m.insert("used".to_string(), serde_json::Value::String(used.to_string()));
            m
        };
        let rows = vec![
            mk("zones/a@s1", "100"),
            mk("zones/b@s2", "300"),
            mk("zones/c@s3", "200"),
        ];

        let summary = summarize_snapshots(&rows, 2);
        assert_eq!(summary.count, 3);
        assert_eq!(summary.total_used_bytes, 600);
        assert_eq!(summary.largest.len(), 2);
        assert_eq!(summary.largest[0].name, "zones/b@s2");
        assert_eq!(summary.largest[0].used_bytes, 300);
        assert_eq!(summary.largest[1].used_bytes, 200);
    }
}
