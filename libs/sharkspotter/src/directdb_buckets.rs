/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2026 Edgecast Cloud LLC.
 *
 * AI-Generated Code
 */

//! Direct database discovery for buckets-postgres clones.
//!
//! This module connects directly to a read-only
//! rebalancer-buckets-postgres PostgreSQL instance (provisioned
//! by `pgclone.sh clone-buckets`) and scans for objects stored
//! on a target shark.
//!
//! Unlike moray's `directdb.rs` which queries a single `manta`
//! table, buckets-postgres organises data into per-vnode schemas:
//!
//! ```text
//! manta_bucket_{vnode}.manta_bucket_object
//! ```
//!
//! The `sharks` column is a `text[]` where each element is
//! formatted as `"datacenter:manta_storage_id"`.

use crossbeam_channel as crossbeam;
use futures::{pin_mut, TryStreamExt};
use serde_json::{json, Value};
use slog::{debug, error, info, trace, warn, Logger};
use std::io::{Error, ErrorKind};
use tokio_postgres::NoTls;

use crate::config::Config;
use crate::SharkspotterMessage;

/// Discover which vnodes exist on this buckets-postgres
/// instance by querying `information_schema.schemata`.
///
/// Returns a sorted `Vec<u32>` of vnode numbers.
///
/// # Algorithmic cost
///
/// O(1) single query, result set bounded by the number of
/// vnodes (at most 1024 in practice).
async fn discover_vnodes(
    client: &tokio_postgres::Client,
    log: &Logger,
) -> Result<Vec<u32>, Error> {
    debug!(log, "Discovering vnodes from information_schema");

    let rows = client
        .query(
            "SELECT schema_name FROM information_schema.schemata \
             WHERE schema_name LIKE 'manta_bucket_%' \
             ORDER BY schema_name",
            &[],
        )
        .await
        .map_err(|e| {
            error!(log, "vnode discovery query failed: {}", e);
            Error::new(
                ErrorKind::Other,
                format!("vnode discovery: {}", e),
            )
        })?;

    let mut vnodes = Vec::with_capacity(rows.len());
    for row in &rows {
        let schema_name: &str = row.get(0);
        // "manta_bucket_42" -> "42"
        if let Some(suffix) = schema_name.strip_prefix(
            "manta_bucket_",
        ) {
            match suffix.parse::<u32>() {
                Ok(vnode) => vnodes.push(vnode),
                Err(_) => {
                    warn!(
                        log,
                        "Ignoring non-numeric schema: {}",
                        schema_name
                    );
                }
            }
        }
    }

    vnodes.sort_unstable();
    debug!(log, "Discovered {} vnodes", vnodes.len());
    Ok(vnodes)
}

/// Parse a single element of the postgres `sharks text[]`
/// column.
///
/// Format: `"datacenter:manta_storage_id"`
///
/// Returns `(datacenter, manta_storage_id)` or an error if
/// the format is unexpected.
fn parse_shark_text(s: &str) -> Result<(&str, &str), String> {
    match s.find(':') {
        Some(idx) => Ok((&s[..idx], &s[idx + 1..])),
        None => Err(format!(
            "Invalid shark text format (no colon): '{}'",
            s
        )),
    }
}

/// Build a moray-compatible `serde_json::Value` from a
/// buckets-postgres row, suitable for the evacuate pipeline.
///
/// This mirrors `remap_mdapi_to_moray` in `mdapi_discovery.rs`
/// so that the downstream metadata update code can route the
/// object to the correct backend (mdapi) using `bucket_id`.
fn row_to_manta_value(
    row: &tokio_postgres::Row,
    vnode: u32,
) -> Result<Value, Error> {
    // UUID columns are read as text to avoid needing
    // tokio-postgres "with-uuid-0_7" feature.
    let id: &str = row.get("id");
    let owner: &str = row.get("owner");
    let bucket_id: &str = row.get("bucket_id");
    let name: &str = row.get("name");
    let content_length: i64 = row.get("content_length");
    let content_type: &str = row.get("content_type");

    // content_md5 is bytea — base64-encode it.
    let content_md5_bytes: &[u8] = row.get("content_md5");
    let content_md5 = base64_encode(content_md5_bytes);

    // sharks is text[] — convert to JSON array of objects.
    let sharks_text: Vec<String> = row.get("sharks");
    let sharks_json: Vec<Value> = sharks_text
        .iter()
        .filter_map(|s| {
            parse_shark_text(s).ok().map(|(dc, sid)| {
                json!({
                    "datacenter": dc,
                    "manta_storage_id": sid
                })
            })
        })
        .collect();

    Ok(json!({
        "objectId": id,
        "etag": id,
        "contentLength": content_length,
        "contentMD5": content_md5,
        "contentType": content_type,
        "name": name,
        "key": name,
        "owner": owner,
        "creator": owner,
        "sharks": sharks_json,
        "bucket_id": bucket_id,
        "headers": {},
        "dirname": "",
        "roles": [],
        "mtime": 0,
        "vnode": vnode,
        "type": "object"
    }))
}

/// Simple base64 encoder (RFC 4648 standard alphabet).
///
/// We avoid pulling in the `base64` crate for this single use.
fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrs\
          tuvwxyz0123456789+/";

    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 {
            chunk[1] as u32
        } else {
            0
        };
        let b2 = if chunk.len() > 2 {
            chunk[2] as u32
        } else {
            0
        };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Scan a single vnode for objects on target sharks.
///
/// Streams rows from the vnode's `manta_bucket_object` table,
/// filters on the sharks column, and sends matches to the
/// crossbeam channel.
///
/// # Algorithmic cost
///
/// O(N) where N is the number of objects in the vnode.
/// Memory is O(1) — rows are streamed, not buffered.
async fn scan_vnode(
    client: &tokio_postgres::Client,
    vnode: u32,
    filter_sharks: &[String],
    obj_tx: &crossbeam::Sender<SharkspotterMessage>,
    shard: u32,
    log: &Logger,
) -> Result<u64, Error> {
    let query = format!(
        "SELECT id, owner, bucket_id, name, content_length, \
         content_md5, content_type, sharks \
         FROM manta_bucket_{}.manta_bucket_object",
        vnode
    );

    debug!(log, "Scanning vnode {}", vnode);

    let rows = client
        .query_raw(query.as_str(), vec![])
        .await
        .map_err(|e| {
            error!(
                log,
                "Query error for vnode {}: {}", vnode, e
            );
            Error::new(
                ErrorKind::Other,
                format!("query vnode {}: {}", vnode, e),
            )
        })?;

    pin_mut!(rows);

    let mut count: u64 = 0;
    while let Some(row) = rows.try_next().await.map_err(|e| {
        Error::new(
            ErrorKind::Other,
            format!("row fetch vnode {}: {}", vnode, e),
        )
    })? {
        let sharks_text: Vec<String> = row.get("sharks");

        // Check if any shark in this object matches a target.
        let matching_shark = sharks_text.iter().find_map(|s| {
            parse_shark_text(s).ok().and_then(|(_, sid)| {
                if filter_sharks.contains(
                    &sid.to_string(),
                ) {
                    Some(sid.to_string())
                } else {
                    None
                }
            })
        });

        if let Some(shark) = matching_shark {
            let manta_value = row_to_manta_value(&row, vnode)?;
            let id: &str = row.get("id");

            trace!(
                log,
                "Match in vnode {}: {} on {}", vnode, id, shark
            );

            let msg = SharkspotterMessage {
                manta_value,
                etag: id.to_string(),
                shark,
                shard,
            };

            if let Err(e) = obj_tx.send(msg) {
                warn!(log, "Tx channel disconnected: {}", e);
                return Err(Error::new(ErrorKind::BrokenPipe, e));
            }

            count += 1;
        }
    }

    debug!(
        log,
        "Vnode {} complete: {} matching objects", vnode, count
    );
    Ok(count)
}

/// Main entry point: scan a buckets-postgres clone for objects
/// on target sharks.
///
/// Connects to `{shard}.rebalancer-buckets-postgres.{domain}`,
/// discovers vnodes, and scans each one.
///
/// # Arguments
///
/// * `shard` -- shard number (used for DNS lookup and in
///   SharkspotterMessage)
/// * `conf` -- sharkspotter configuration
/// * `log` -- structured logger
/// * `obj_tx` -- channel for discovered objects
pub async fn get_buckets_objects_from_shard(
    shard: u32,
    conf: Config,
    log: Logger,
    obj_tx: crossbeam::Sender<SharkspotterMessage>,
) -> Result<(), Error> {
    let host = format!(
        "{}.rebalancer-buckets-postgres.{}",
        shard, conf.domain
    );

    info!(log, "Connecting to {}", host);

    let (client, connection) = tokio_postgres::Config::new()
        .host(host.as_str())
        .user("postgres")
        .dbname("buckets_metadata")
        .keepalives_idle(std::time::Duration::from_secs(30))
        .connect(NoTls)
        .await
        .map_err(|e| {
            error!(log, "Failed to connect to {}: {}", &host, e);
            Error::new(
                ErrorKind::Other,
                format!("connect to {}: {}", host, e),
            )
        })?;

    let task_host = host.clone();
    let task_log = log.clone();
    tokio::spawn(async move {
        connection.await.map_err(|e| {
            error!(
                task_log,
                "Lost connection to {}: {}", task_host, e
            );
            Error::new(
                ErrorKind::Other,
                format!("connection to {}: {}", task_host, e),
            )
        })?;
        Ok::<(), Error>(())
    });

    let vnodes = discover_vnodes(&client, &log).await?;
    info!(
        log,
        "Scanning {} vnodes on {}", vnodes.len(), host
    );

    let mut total: u64 = 0;
    for vnode in &vnodes {
        match scan_vnode(
            &client,
            *vnode,
            &conf.sharks,
            &obj_tx,
            shard,
            &log,
        )
        .await
        {
            Ok(count) => total += count,
            Err(e) => {
                if e.kind() == ErrorKind::BrokenPipe {
                    return Err(e);
                }
                error!(
                    log,
                    "Error scanning vnode {}: {}", vnode, e
                );
            }
        }
    }

    info!(
        log,
        "Scan complete for {}: {} objects across {} vnodes",
        host,
        total,
        vnodes.len()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shark_text_valid() {
        let (dc, sid) =
            parse_shark_text("us-east-1:1.stor.example.com")
                .unwrap();
        assert_eq!(dc, "us-east-1");
        assert_eq!(sid, "1.stor.example.com");
    }

    #[test]
    fn parse_shark_text_coal() {
        let (dc, sid) =
            parse_shark_text("coal:1.stor.coal.joyent.us")
                .unwrap();
        assert_eq!(dc, "coal");
        assert_eq!(sid, "1.stor.coal.joyent.us");
    }

    #[test]
    fn parse_shark_text_no_colon() {
        assert!(parse_shark_text("no-colon-here").is_err());
    }

    #[test]
    fn parse_shark_text_empty() {
        assert!(parse_shark_text("").is_err());
    }

    #[test]
    fn parse_shark_text_colon_at_start() {
        let (dc, sid) = parse_shark_text(":storage").unwrap();
        assert_eq!(dc, "");
        assert_eq!(sid, "storage");
    }

    #[test]
    fn base64_encode_basic() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn base64_encode_md5_like() {
        // 16-byte MD5 digest: 16 / 3 = 5 rem 1, so 24-char
        // base64 with == padding on the last group.
        let md5 = [
            0x2f, 0xd3, 0x6b, 0x21, 0x95, 0xd3, 0x63, 0x7e,
            0xc0, 0xc9, 0x56, 0xcd, 0xf7, 0xe8, 0x19, 0xef,
        ];
        let encoded = base64_encode(&md5);
        assert_eq!(encoded.len(), 24);
        assert!(encoded.ends_with("=="));
    }
}
