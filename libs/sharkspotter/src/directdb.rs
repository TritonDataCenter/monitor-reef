/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 * Copyright 2026 Edgecast Cloud LLC.
 */

use crossbeam_channel as crossbeam;
use futures::{TryStreamExt, pin_mut};
use serde_json::{self, Value};
use slog::{Logger, debug, error, trace, warn};
use std::io::{Error, ErrorKind};
use std::sync::{Arc, Mutex};
use tokio_postgres::{NoTls, Row};

use crate::config::Config;
use crate::{
    SharkspotterMessage, get_sharks_from_manta_obj, object_id_from_manta_obj,
};

// Unfortunately the Manta records in the moray database are slightly
// different from what we get back from the moray service (both for the
// `findobjects` and `sql` endpoints.  So if we are going direct to the database
// we need to use a different struct to represent the record (DB schema).
// Fortunately we don't need every field, only _value and _etag.  Note that
// there are some differences in production manta schema versus the latest
// manta schema.  Specifically production has a 4 byte int for _id and it
// also includes the _idx column.
//
// moray=> SELECT table_name, column_name, data_type FROM information_schema.columns WHERE table_name = 'manta';
// table_name | column_name | data_type
// ------------+-------------+-----------
//  manta      | _id         | bigint
//  manta      | _txn_snap   | integer
//  manta      | _key        | text
//  manta      | _value      | text
//  manta      | _etag       | character
//  manta      | _mtime      | bigint
//  manta      | _vnode      | bigint
//  manta      | dirname     | text
//  manta      | name        | text
//  manta      | owner       | text
//  manta      | objectid    | text
//  manta      | type        | text

pub async fn get_objects_from_shard(
    shard: u32,
    conf: Config,
    log: Logger,
    obj_tx: crossbeam::Sender<SharkspotterMessage>,
) -> Result<(), Error> {
    let shard_host_name =
        format!("{}.rebalancer-postgres.{}", shard, conf.domain);

    debug!(log, "Connecting to {}", shard_host_name);
    // Connect to this shard's reblancer-postgres moray database.
    let (client, connection) = tokio_postgres::Config::new()
        .host(shard_host_name.as_str())
        .user("postgres")
        .dbname("moray")
        .keepalives_idle(std::time::Duration::from_secs(30))
        .connect(NoTls)
        .await
        .map_err(|e| {
            error!(log, "failed to connect to {}: {}", &shard_host_name, e);
            Error::other(e)
        })?;

    let task_host_name = shard_host_name.clone();
    let task_log = log.clone();

    // Store connection errors so they can be checked at the end
    let conn_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let conn_error_clone = conn_error.clone();

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            let msg =
                format!("could not communicate with {}: {}", task_host_name, e);
            error!(task_log, "{}", msg);
            if let Ok(mut err) = conn_error_clone.lock() {
                *err = Some(msg);
            }
        }
    });

    let params: [&str; 0] = [];
    let rows = client
        .query_raw("SELECT * from manta where type='object'", params)
        .await
        .map_err(|e| {
            error!(log, "query error for {}: {}", &shard_host_name, e);
            Error::other(e)
        })?;

    pin_mut!(rows);
    // Iterate over the rows in the stream.  For each one determine if it
    // matches the shark we are looking for.
    while let Some(row) = rows.try_next().await.map_err(Error::other)? {
        let val_str: &str = row.get("_value");
        let value: Value =
            serde_json::from_str(val_str).map_err(Error::other)?;
        check_value_for_match(
            &value,
            &row,
            &conf.sharks,
            shard,
            &obj_tx,
            &log,
        )?;
    }

    // Check if the connection task encountered an error
    if let Ok(err) = conn_error.lock()
        && let Some(msg) = err.as_ref()
    {
        return Err(Error::other(msg.clone()));
    }

    Ok(())
}

fn check_value_for_match(
    value: &Value,
    row: &Row,
    filter_sharks: &[String],
    shard: u32,
    obj_tx: &crossbeam_channel::Sender<SharkspotterMessage>,
    log: &Logger,
) -> Result<(), Error> {
    let obj_id = object_id_from_manta_obj(value).map_err(Error::other)?;
    let sharks = get_sharks_from_manta_obj(value, log)?;

    trace!(log, "sharkspotter checking {}", obj_id);
    sharks
        .iter()
        .filter(|s| filter_sharks.contains(&s.manta_storage_id))
        .try_for_each(|s| {
            send_matching_object(row, &s.manta_storage_id, shard, obj_tx, log)
        })
}

fn send_matching_object(
    row: &Row,
    shark_name: &str,
    shard: u32,
    obj_tx: &crossbeam_channel::Sender<SharkspotterMessage>,
    log: &Logger,
) -> Result<(), Error> {
    trace!(log, "Found matching record: {:#?}", &row);

    // Extract _value and _etag directly from the row
    let manta_value_str: &str = row.get("_value");
    let etag: &str = row.get("_etag");

    let manta_value: Value =
        serde_json::from_str(manta_value_str).map_err(Error::other)?;

    trace!(log, "Sending value: {:#?}", manta_value);

    let msg = SharkspotterMessage {
        manta_value,
        etag: etag.to_string(),
        shark: shark_name.to_string(),
        shard,
    };

    if let Err(e) = obj_tx.send(msg) {
        warn!(log, "Tx channel disconnected: {}", e);
        return Err(Error::new(ErrorKind::BrokenPipe, e));
    }
    Ok(())
}
