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

    /// `zpool list -Hp -o <fields>` → Vec of row objects keyed by field.
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
/// extras are dropped silently — both mirror node-zfs.
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
}
