// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! RPC dispatch: map a decoded fast-protocol `FastMessage` to one or more
//! response messages. Each handler returns the `data` payloads; the caller
//! wraps them with status=Data and appends the required End message.

use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::error::MorayError;
use crate::fast::{FastData, FastMessage, FastStatus};
use crate::filter::Filter;
use crate::store::{MorayStore, PutOpts};
use crate::triggers;
use crate::types::{Bucket, BucketConfig, ObjectMeta};
use crate::typeval::{self, IndexType};
use crate::validate;

const DEFAULT_FIND_LIMIT: usize = 1000;
const MAX_FIND_LIMIT: usize = 100_000;

/// Convert a handler's `Result<Vec<Value>>` into a set of fast-protocol
/// responses (DATA frames + the final END), or an ERROR frame on failure.
pub async fn dispatch<S: MorayStore>(
    store: Arc<S>,
    msg: &FastMessage,
) -> Vec<FastMessage> {
    let rpc = msg.data.m.name.as_str();
    match handle(&store, rpc, &msg.data).await {
        Ok(payloads) => {
            let mut out: Vec<FastMessage> = payloads
                .into_iter()
                .map(|p| FastMessage::data(msg.id, rpc, p))
                .collect();
            out.push(FastMessage::end(msg.id, rpc));
            out
        }
        Err(e) => {
            warn!(rpc = %rpc, err = %e, "rpc failed");
            vec![FastMessage::error(msg.id, rpc, e.to_wire())]
        }
    }
}

async fn handle<S: MorayStore>(
    store: &Arc<S>,
    rpc: &str,
    data: &FastData,
) -> Result<Vec<Value>, MorayError> {
    let args = data.d.as_array().cloned().unwrap_or_default();
    debug!(rpc, nargs = args.len(), "dispatch");

    // Each verb has a fixed rpcargs arity. If the request is short,
    // upstream emits `"X expects N arguments"` at `InvocationError`
    // grade — do the same here before attempting any per-verb parsing.
    let expected_arity: Option<usize> = match rpc {
        "listBuckets" | "getTokens" | "ping" | "version" => Some(1),
        "getBucket" | "delBucket" | "deleteBucket" | "batch" => Some(2),
        "createBucket" | "updateBucket" | "getObject" | "delObject"
        | "deleteObject" | "findObjects" | "reindexObjects" | "deleteMany"
        | "sql" => Some(3),
        "putObject" | "updateObjects" => Some(4),
        _ => None,
    };
    if let Some(expected) = expected_arity
        && args.len() < expected
    {
        let s = if expected == 1 { "" } else { "s" };
        return Err(MorayError::Invocation(format!(
            "{rpc} expects {expected} argument{s}"
        )));
    }

    match rpc {
        // `ping` uses rpcCommonNoData on the client side — the client
        // asserts that exactly zero DATA frames come back. Options are
        // still validated: `null`, arrays, and non-boolean `deep`
        // produce `InvocationError`.
        "ping" => {
            let opts = expect_options_object(&args, 0, "ping")?;
            // The options *object* check (via expect_options_object)
            // emits the generic "a valid options object" message. But
            // upstream adds an `": options should be object"` suffix for
            // null / non-object options, which matters for invalid-fast.
            // Rather than thread two messages through, we detect the
            // specific failure here and emit the suffixed form.
            if let Some(v) = opts.get("deep")
                && !matches!(v, Value::Bool(_))
            {
                return Err(MorayError::Invocation(
                    "ping expects \"options\" (args[0]) to be \
                     a valid options object: options.deep should be boolean"
                        .into(),
                ));
            }
            Ok(Vec::new())
        }
        // `version` must return one DATA row with a numeric `version`
        // field. Options object is validated for type.
        "version" => {
            let _ = expect_options_object(&args, 0, "version")?;
            Ok(vec![json!({"version": 2})])
        }

        "createBucket" => {
            let name = bucket_name_arg(&args, 0, "createBucket")?;
            validate::bucket_name(&name)?;
            validate::bucket_config(args.get(1).unwrap_or(&Value::Null))?;
            let _ = expect_options_object(&args, 2, "createBucket")?;
            let cfg = parse_bucket_config(args.get(1))?;
            let _ = store.create_bucket(&name, cfg).await?;
            Ok(Vec::new())
        }
        "updateBucket" => {
            // Wire: [bucket, cfg, opts]
            let name = bucket_name_arg(&args, 0, "updateBucket")?;
            validate::bucket_name(&name)?;
            expect_rpc_object(&args, 1, "updateBucket", "config")?;
            validate::bucket_config(args.get(1).unwrap_or(&Value::Null))?;
            let opts = expect_options_object(&args, 2, "updateBucket")?;
            // updateBucket's options take one extra flag: `no_reindex:
            // boolean` — a hint to skip rebuilding the secondary index.
            if let Some(v) = opts.get("no_reindex")
                && !matches!(v, Value::Bool(_))
            {
                return Err(MorayError::Invocation(
                    "updateBucket expects \"options\" (args[2]) to be \
                     a valid options object: options.no_reindex should be boolean"
                        .into(),
                ));
            }
            let cfg = parse_bucket_config(args.get(1))?;
            let _ = store.update_bucket(&name, cfg).await?;
            Ok(Vec::new())
        }
        // getBucket's wire shape is `[opts, name]`, unlike every other verb.
        "getBucket" => {
            let _ = expect_options_object(&args, 0, "getBucket")?;
            let name = expect_nonempty_string(&args, 1, "getBucket", "bucket")?;
            let b = store.get_bucket(&name).await?;
            Ok(vec![bucket_wire(&b)])
        }
        "delBucket" | "deleteBucket" => {
            let name = bucket_name_arg(&args, 0, "delBucket")?;
            let _ = expect_options_object(&args, 1, "delBucket")?;
            store.delete_bucket(&name).await?;
            Ok(Vec::new())
        }
        "listBuckets" => {
            let _ = expect_options_object(&args, 0, "listBuckets")?;
            let list = store.list_buckets().await?;
            Ok(list.iter().map(bucket_wire).collect())
        }

        // putObject(bucket, key, value, opts?)
        "putObject" => {
            let bucket = expect_nonempty_string(&args, 0, "putObject", "bucket")?;
            let key = expect_nonempty_string(&args, 1, "putObject", "key")?;
            let _ = expect_object(&args, 2, "putObject", "value")?;
            let _ = expect_options_object(&args, 3, "putObject")?;
            let value = args.get(2).cloned().unwrap_or(Value::Null);
            let opts = parse_put_opts(args.get(3));
            let value = prepare_value_for_put(
                store.as_ref(),
                &bucket,
                value,
                &opts.headers,
            )
            .await?;
            let unique_cols =
                collect_unique_claims(store.as_ref(), &bucket, &value).await?;
            let meta = store
                .put_object(&bucket, &key, value, opts, unique_cols)
                .await?;
            Ok(vec![json!({
                "etag": meta.etag,
                "_id":  meta.id,
                "_mtime": meta.mtime.timestamp_millis(),
            })])
        }

        "getObject" => {
            let bucket = expect_nonempty_string(&args, 0, "getObject", "bucket")?;
            let key = expect_nonempty_string(&args, 1, "getObject", "key")?;
            let opts = expect_options_object(&args, 2, "getObject")?;
            let meta = store.get_object(&bucket, &key).await?;
            // getObject also supports the `_handledOptions` metadata-
            // record prefix: when the client sets
            // `internalOpts.sendHandledOptions`, it expects the first
            // frame to advertise which options we honoured. Otherwise
            // node-moray raises `UnhandledOptionsError`.
            let wants_metadata = opts
                .get("internalOpts")
                .and_then(|v| v.get("sendHandledOptions"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut out = Vec::new();
            if wants_metadata {
                out.push(json!({"_handledOptions": ["requireOnlineReindexing"]}));
            }
            out.push(object_wire(&bucket, &meta));
            Ok(out)
        }

        "delObject" | "deleteObject" => {
            let bucket = expect_nonempty_string(&args, 0, "delObject", "bucket")?;
            let key = expect_nonempty_string(&args, 1, "delObject", "key")?;
            let _ = expect_options_object(&args, 2, "delObject")?;
            let opts = parse_put_opts(args.get(2));
            store.delete_object(&bucket, &key, opts.etag).await?;
            Ok(Vec::new())
        }

        // findObjects(bucket, filter, opts?)
        "findObjects" => {
            let bucket = expect_nonempty_string(&args, 0, "findObjects", "bucket")?;
            let filter_str = expect_nonempty_string(&args, 1, "findObjects", "filter")?;
            let opts_map = expect_options_object(&args, 2, "findObjects")?;
            validate_find_options(&opts_map)?;
            let opts = Value::Object(opts_map);

            let mut out: Vec<Value> = Vec::new();

            // node-moray's client sends `internalOpts.sendHandledOptions`
            // when it passes options like `requireIndexes` that the
            // server must acknowledge. The ack is a first DATA record
            // carrying `_handledOptions`, listing which options we
            // honoured. If we don't send it, the client raises
            // `UnhandledOptionsError`.
            let wants_metadata = opts
                .get("internalOpts")
                .and_then(|v| v.get("sendHandledOptions"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if wants_metadata {
                out.push(json!({
                    "_handledOptions": ["requireIndexes", "requireOnlineReindexing"],
                }));
            }

            let (rows, total) =
                find_impl(store.as_ref(), &bucket, &filter_str, &opts).await?;
            out.extend(rows.into_iter().map(|m| {
                let mut r = object_wire(&bucket, &m);
                // Moray stamps `_count` on every streamed row with the
                // total row count matched by the filter — the client's
                // findObjects pattern uses it as a sanity check.
                r["_count"] = json!(total as u64);
                r
            }));
            Ok(out)
        }

        // updateObjects(bucket, fields, filter, opts?)
        "updateObjects" => {
            let bucket = expect_nonempty_string(&args, 0, "updateObjects", "bucket")?;
            let fields_map = expect_object(&args, 1, "updateObjects", "fields")?;
            if fields_map.is_empty() {
                return Err(MorayError::EmptyFieldUpdate);
            }
            let filter_str = expect_nonempty_string(&args, 2, "updateObjects", "filter")?;
            let _ = expect_options_object(&args, 3, "updateObjects")?;
            let mut opts = args.get(3).cloned().unwrap_or(Value::Null);
            // MORAY-406: reject updateObjects calls whose new fields carry
            // null values — upstream returns `NotNullableError`.
            for (k, v) in fields_map.iter() {
                if v.is_null() {
                    return Err(MorayError::NotNullable { field: k.clone() });
                }
            }
            // Reject updates targeting a column whose index is still
            // being reindexed.
            let bucket_meta = store.get_bucket(&bucket).await?;
            if let Some(state) = bucket_meta.reindex_active.as_ref() {
                for k in fields_map.keys() {
                    if state.columns.iter().any(|c| c == k) {
                        return Err(MorayError::NotIndexed {
                            bucket: bucket.clone(),
                            filter: filter_str.clone(),
                            reindexing: vec![k.clone()],
                            unindexed: Vec::new(),
                        });
                    }
                }
            }
            // updateObjects is a write-path verb: it must hit an indexed,
            // non-reindexing column. Force the filter-layer's strict
            // checks regardless of the caller's `requireOnlineReindexing`
            // setting.
            if let Value::Object(ref mut o) = opts {
                o.insert("requireOnlineReindexing".into(), json!(true));
                o.insert("requireIndexes".into(), json!(true));
            }

            let (rows, _) = find_impl(store.as_ref(), &bucket, &filter_str, &opts).await?;
            let mut updated = 0usize;
            // Track the latest etag emitted by any put — this is what
            // upstream returns as `meta.etag` so the client can chain a
            // conditional get/put against it.
            let mut last_etag: Option<String> = None;
            let headers = opts
                .as_object()
                .and_then(|o| o.get("headers"))
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            for row in rows {
                let mut new_value = row.value.clone();
                if let Some(obj) = new_value.as_object_mut() {
                    for (k, v) in fields_map.iter() {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                let new_value = prepare_value_for_put(
                    store.as_ref(),
                    &bucket,
                    new_value,
                    &headers,
                )
                .await?;
                let uniq =
                    collect_unique_claims(store.as_ref(), &bucket, &new_value).await?;
                let mut put_opts = PutOpts::default();
                put_opts.headers = headers.clone();
                let meta = store
                    .put_object(&bucket, &row.key, new_value, put_opts, uniq)
                    .await?;
                last_etag = Some(meta.etag);
                updated += 1;
            }
            Ok(vec![json!({
                "count": updated,
                "etag": last_etag,
            })])
        }

        // deleteMany(bucket, filter, opts?)
        "deleteMany" => {
            let bucket = expect_nonempty_string(&args, 0, "deleteMany", "bucket")?;
            let filter_str = expect_nonempty_string(&args, 1, "deleteMany", "filter")?;
            let _ = expect_options_object(&args, 2, "deleteMany")?;
            let mut opts = args.get(2).cloned().unwrap_or(Value::Null);
            if let Value::Object(ref mut o) = opts {
                o.insert("requireOnlineReindexing".into(), json!(true));
                o.insert("requireIndexes".into(), json!(true));
            }

            let (rows, _) = find_impl(store.as_ref(), &bucket, &filter_str, &opts).await?;
            let mut removed = 0usize;
            for row in rows {
                store.delete_object(&bucket, &row.key, None).await?;
                removed += 1;
            }
            Ok(vec![json!({"count": removed})])
        }

        // reindexObjects(bucket, count, opts?) — progress through the
        // bucket's reindex_active rows.
        //
        // Response shape tracks three states:
        //   * reindex_active is Some → `{processed: n, remaining: m}`.
        //     Every call decrements the remaining count.
        //   * One call after the reindex finished → also emits
        //     `{processed: 0, remaining: 0}` (upstream's "just-
        //     finished" sentinel). The flag clears on that call.
        //   * Otherwise → `{processed: 0}` with no `remaining` field.
        "reindexObjects" => {
            let bucket = expect_nonempty_string(&args, 0, "reindexObjects", "bucket")?;
            let count = expect_nonnegative_integer(&args, 1, "reindexObjects", "count")?;
            let _ = expect_options_object(&args, 2, "reindexObjects")?;

            let bucket_meta = store.get_bucket(&bucket).await?;
            if bucket_meta.reindex_active.is_some() {
                let (processed, remaining) =
                    store.reindex_step(&bucket, count).await?;
                return Ok(vec![json!({"processed": processed, "remaining": remaining})]);
            }
            if bucket_meta.reindex_just_finished {
                store.clear_reindex_just_finished(&bucket).await?;
                return Ok(vec![json!({"processed": 0, "remaining": 0})]);
            }
            Ok(vec![json!({"processed": 0})])
        }

        // batch(requests, opts?) — run each sub-op and collect results.
        // For now every sub-op gets its own FDB transaction; proper
        // all-or-nothing requires lifting into a single `db.run` — tracked
        // as follow-on work.
        "batch" => {
            let requests = match args.first() {
                Some(Value::Array(a)) => a.clone(),
                _ => {
                    return Err(MorayError::Invocation(
                        "batch expects \"requests\" (args[0]) to be an array".into(),
                    ))
                }
            };
            let _ = expect_options_object(&args, 1, "batch")?;

            // Upstream validates each sub-request against a schema before
            // any work happens. Match its error-message wording exactly
            // so node-moray's invalid-fast test suite is happy.
            for (idx, req) in requests.iter().enumerate() {
                if req.get("bucket").is_none() {
                    return Err(MorayError::Invocation(format!(
                        "batch expects \"requests\" (args[0]) to be an array of \
                         valid request objects: requests[{idx}] should have \
                         required property 'bucket'"
                    )));
                }
                if let Some(op) = req.get("operation").and_then(|v| v.as_str())
                    && !matches!(op, "put" | "delete" | "update" | "deleteMany")
                {
                    return Err(MorayError::Invocation(format!(
                        "batch expects \"requests\" (args[0]) to be an array of \
                         valid request objects: requests[{idx}].operation should \
                         be equal to one of the allowed values"
                    )));
                }
            }
            // Build a list of BatchOps. Pre-compute everything we can
            // outside the storage transaction (trigger application,
            // unique-column extraction, filter scans for update /
            // deleteMany) so the storage call only has to perform the
            // writes — atomically. The store's `apply_batch` either
            // commits all ops or none. That guarantee is what stops
            // sdc-napi's commitBatch retry loop dead: if any op of the
            // batch fails, no earlier op leaves a partial commit
            // behind to make the retry's cached etag stale.
            let mut ops: Vec<crate::store::BatchOp> = Vec::with_capacity(requests.len());
            // Mirror the response shape upstream produces. Per-op:
            // {bucket,key,etag,_id} for puts; {bucket,key} for
            // deletes; {bucket,count} for update/deleteMany.
            let mut wire_results: Vec<Value> = Vec::with_capacity(requests.len());

            for req in requests {
                let op = req
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("put");
                let bucket = req
                    .get("bucket")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| MorayError::InvalidArg("batch: missing bucket".into()))?
                    .to_string();
                match op {
                    "put" => {
                        let key = req
                            .get("key")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                MorayError::InvalidArg("batch.put: missing key".into())
                            })?
                            .to_string();
                        let value = req.get("value").cloned().unwrap_or(Value::Null);
                        let opts = parse_put_opts(req.get("options"));
                        let value = prepare_value_for_put(
                            store.as_ref(),
                            &bucket,
                            value,
                            &opts.headers,
                        )
                        .await?;
                        let uniq =
                            collect_unique_claims(store.as_ref(), &bucket, &value).await?;
                        wire_results.push(json!({
                            "bucket": bucket,
                            "key":    key,
                            // etag/_id replaced from the store's
                            // BatchOpResult after apply_batch returns.
                        }));
                        ops.push(crate::store::BatchOp::Put {
                            bucket,
                            key,
                            value,
                            opts,
                            unique_cols: uniq,
                        });
                    }
                    "delete" => {
                        let key = req
                            .get("key")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                MorayError::InvalidArg("batch.delete: missing key".into())
                            })?
                            .to_string();
                        wire_results.push(json!({
                            "bucket": bucket,
                            "key":    key,
                        }));
                        ops.push(crate::store::BatchOp::Delete {
                            bucket,
                            key,
                            expected_etag: None,
                        });
                    }
                    // sdc-napi (and a few other Triton services) routinely
                    // submit batches whose middle op is `update` — e.g.
                    // "set primary_flag=false on sibling nics". Earlier
                    // morayd returned NotImplemented for `update` /
                    // `deleteMany` and let the previous `put` commit
                    // anyway, leaving a partial state that pinned the
                    // retry's cached etag and caused an infinite
                    // EtagConflict loop. We now apply every op in a
                    // single FDB transaction so partial commits are
                    // impossible.
                    "update" => {
                        let filter_str = req
                            .get("filter")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                MorayError::InvalidArg(
                                    "batch.update: missing filter".into(),
                                )
                            })?
                            .to_string();
                        let fields_map = req
                            .get("fields")
                            .and_then(|v| v.as_object())
                            .cloned()
                            .ok_or_else(|| {
                                MorayError::InvalidArg(
                                    "batch.update: missing fields object".into(),
                                )
                            })?;
                        if fields_map.is_empty() {
                            return Err(MorayError::EmptyFieldUpdate);
                        }
                        for (k, v) in fields_map.iter() {
                            if v.is_null() {
                                return Err(MorayError::NotNullable {
                                    field: k.clone(),
                                });
                            }
                        }
                        let bucket_meta = store.get_bucket(&bucket).await?;
                        if let Some(state) = bucket_meta.reindex_active.as_ref() {
                            for k in fields_map.keys() {
                                if state.columns.iter().any(|c| c == k) {
                                    return Err(MorayError::NotIndexed {
                                        bucket: bucket.clone(),
                                        filter: filter_str.clone(),
                                        reindexing: vec![k.clone()],
                                        unindexed: Vec::new(),
                                    });
                                }
                            }
                        }
                        let scan_opts = json!({
                            "requireOnlineReindexing": true,
                            "requireIndexes": true,
                        });
                        let (rows, _) = find_impl(
                            store.as_ref(),
                            &bucket,
                            &filter_str,
                            &scan_opts,
                        )
                        .await?;
                        let headers = req
                            .get("options")
                            .and_then(|v| v.get("headers"))
                            .and_then(|v| v.as_object())
                            .cloned()
                            .unwrap_or_default();

                        let mut prepared: Vec<(String, Value, Vec<(String, Vec<String>)>)> =
                            Vec::with_capacity(rows.len());
                        for row in rows {
                            let mut new_value = row.value.clone();
                            if let Some(obj) = new_value.as_object_mut() {
                                for (k, v) in fields_map.iter() {
                                    obj.insert(k.clone(), v.clone());
                                }
                            }
                            let new_value = prepare_value_for_put(
                                store.as_ref(),
                                &bucket,
                                new_value,
                                &headers,
                            )
                            .await?;
                            let uniq = collect_unique_claims(
                                store.as_ref(),
                                &bucket,
                                &new_value,
                            )
                            .await?;
                            prepared.push((row.key, new_value, uniq));
                        }
                        wire_results.push(json!({
                            "bucket": bucket,
                            "count":  prepared.len(),
                        }));
                        ops.push(crate::store::BatchOp::UpdateRows {
                            bucket,
                            rows: prepared,
                        });
                    }
                    "deleteMany" => {
                        let filter_str = req
                            .get("filter")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                MorayError::InvalidArg(
                                    "batch.deleteMany: missing filter".into(),
                                )
                            })?
                            .to_string();
                        let scan_opts = json!({
                            "requireOnlineReindexing": true,
                            "requireIndexes": true,
                        });
                        let (rows, _) = find_impl(
                            store.as_ref(),
                            &bucket,
                            &filter_str,
                            &scan_opts,
                        )
                        .await?;
                        let keys: Vec<String> = rows.into_iter().map(|r| r.key).collect();
                        wire_results.push(json!({
                            "bucket": bucket,
                            "count":  keys.len(),
                        }));
                        ops.push(crate::store::BatchOp::DeleteKeys { bucket, keys });
                    }
                    other => {
                        return Err(MorayError::InvalidArg(format!(
                            "batch: unknown operation {other}"
                        )))
                    }
                }
            }

            // Atomic commit (or atomic abort): one FDB transaction.
            let store_results = store.apply_batch(ops).await?;
            // Stitch the store's per-op result back into our wire
            // shape (etag/_id for Put ops). We zip on index — both
            // Vecs are produced in the same order.
            for (slot, r) in wire_results.iter_mut().zip(store_results.iter()) {
                if let Some(obj) = slot.as_object_mut() {
                    if let Some(e) = &r.etag {
                        obj.insert("etag".into(), Value::String(e.clone()));
                    }
                    if let Some(id) = r.id {
                        obj.insert("_id".into(), json!(id));
                    }
                }
            }
            Ok(vec![json!({"etags": wire_results})])
        }

        // `getTokens` is a cluster-membership RPC the client uses to
        // discover the set of shards backing a bucket. FDB is a single
        // logical database; we return an empty token list so callers
        // proceed without sharding assumptions.
        "getTokens" => {
            let _ = expect_options_object(&args, 0, "getTokens")?;
            Ok(vec![json!({"tokens": []})])
        }

        // `sql` runs through a pattern dispatcher in `crate::sql` that
        // recognises the fixed set of statement shapes Triton actually
        // emits in production. Argument validation still mirrors what
        // upstream moray does so test-suite probes get the same errors
        // before any execution happens.
        "sql" => {
            let stmt = expect_nonempty_string(&args, 0, "sql", "statement")?;
            let values_arr: Vec<Value> = match args.get(1) {
                Some(Value::Array(arr)) => arr.clone(),
                _ => {
                    return Err(MorayError::Invocation(
                        "sql expects \"values\" (args[1]) to be an array".into(),
                    ))
                }
            };
            let opts = expect_options_object(&args, 2, "sql")?;
            if let Some(req_id) = opts.get("req_id")
                && let Some(s) = req_id.as_str()
                && crate::typeval::validate_scalar_literal("uuid", s).is_err()
            {
                return Err(MorayError::Invocation(
                    "sql expects \"options\" (args[2]) to be \
                     a valid options object: options.req_id should match \
                     format \"uuid\""
                        .into(),
                ));
            }
            if let Some(timeout) = opts.get("timeout") {
                let ok = match timeout {
                    Value::Number(n) => n.as_i64().is_some_and(|i| i >= 0),
                    _ => false,
                };
                if !ok {
                    return Err(MorayError::Invocation(
                        "sql expects \"options\" (args[2]) to be \
                         a valid options object: options.timeout should be >= 0"
                            .into(),
                    ));
                }
            }
            crate::sql::execute(store.as_ref(), &stmt, &values_arr).await
        }

        other => Err(MorayError::UnsupportedRpc(other.into())),
    }
}

/// Core findObjects logic shared by findObjects/updateObjects/deleteMany.
///
/// Enforces the Moray `requireIndexes` invariant by checking that every
/// attribute in the filter is either a synthetic field (`_id`, `_key`,
/// `_etag`, `_mtime`) or appears in the bucket's `index` schema. If any
/// attribute is unindexed, returns `NotIndexedError` — matching what
/// node-moray expects from a legit Moray server.
async fn find_impl<S: MorayStore>(
    store: &S,
    bucket: &str,
    filter_str: &str,
    opts: &Value,
) -> Result<(Vec<ObjectMeta>, usize), MorayError> {
    let filter = Filter::parse(filter_str)
        .map_err(|e| MorayError::InvalidQuery(format!("filter: {e}")))?;

    let bucket_meta = store.get_bucket(bucket).await?;
    let index_keys: Vec<&String> = bucket_meta.options.index.keys().collect();

    // Canonicalise filter literals for typed columns so equality works
    // regardless of how the client wrote the value (e.g. `fd00::045`
    // vs `fd00::45`, or `10.0.0.0/16` vs `10.0.0.0/16`).
    let schema_snapshot = bucket_meta.options.index.clone();
    let filter = filter.map_literals(&|attr: &str, literal: &str| {
        let Some(cfg) = schema_snapshot.get(attr) else {
            return literal.to_string();
        };
        let Some(ty_str) = cfg.get("type").and_then(|v| v.as_str()) else {
            return literal.to_string();
        };
        let Some(ty) = IndexType::parse(ty_str) else {
            return literal.to_string();
        };
        match ty.scalar_type() {
            "ip" => match literal.parse::<std::net::IpAddr>() {
                Ok(ip) => ip.to_string(),
                Err(_) => literal.to_string(),
            },
            "subnet" => canonical_subnet(literal),
            _ => literal.to_string(),
        }
    });

    // Substring queries on array-typed columns aren't supported — Moray
    // rejects `(arrayField=prefix*)` because a prefix-in-array test can't
    // be pushed into a BTREE-indexed scan.
    for attr in filter.attrs() {
        if attr.starts_with('_') {
            continue;
        }
        let Some(idx_cfg) = bucket_meta.options.index.get(attr) else {
            continue;
        };
        let Some(ty_str) = idx_cfg.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(ty) = IndexType::parse(ty_str) else {
            continue;
        };
        if ty.is_array() && filter_uses_substring(&filter, attr) {
            return Err(MorayError::NotIndexed {
                bucket: bucket.into(),
                filter: filter_str.to_string(),
                reindexing: Vec::new(),
                unindexed: vec![attr.to_string()],
            });
        }
    }

    // For every filter literal whose attribute is an indexed typed column,
    // reject if the literal doesn't parse as that type. This matches the
    // upstream server's strict-validation behaviour (e.g. `(mac=foo)`
    // fails with InvalidQueryError rather than silently matching nothing).
    //
    // `:within:=` has special rules: the literal is a CIDR subnet for IP
    // columns, a range for numrange/daterange, and is not defined at all
    // for other column types.
    for (attr, literal) in filter.literals() {
        if attr.starts_with('_') {
            continue;
        }
        let Some(idx_cfg) = bucket_meta.options.index.get(attr) else {
            continue;
        };
        let Some(ty_str) = idx_cfg.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(ty) = IndexType::parse(ty_str) else {
            continue;
        };
        let (uses_within, uses_contains, uses_overlaps) =
            filter_attr_range_ops(&filter, attr);
        if uses_within {
            validate_within_literal(attr, ty.scalar_type(), literal)?;
        }
        if uses_contains {
            validate_contains_literal(attr, ty.scalar_type(), literal)?;
        }
        if uses_overlaps {
            validate_overlaps_literal(attr, ty.scalar_type(), literal)?;
        }
        if !uses_within
            && !uses_contains
            && !uses_overlaps
            && let Err(reason) =
                typeval::validate_scalar_literal(ty.scalar_type(), literal)
        {
            return Err(MorayError::InvalidQuery(format!(
                "filter attribute {attr}: {reason}"
            )));
        }
    }
    let require_indexes = opts
        .get("requireIndexes")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let require_online_reindexing = opts
        .get("requireOnlineReindexing")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let reindexing_cols: Vec<&String> = bucket_meta
        .reindex_active
        .as_ref()
        .map(|s| s.columns.iter().collect())
        .unwrap_or_default();

    let attrs: Vec<&str> = filter.attrs();
    let unindexed: Vec<String> = attrs
        .iter()
        .filter(|a| {
            !a.starts_with('_')
                && !index_keys.iter().any(|k| k.as_str() == **a)
        })
        .map(|a| (*a).to_string())
        .collect();

    // `requireOnlineReindexing: true` forces the filter to hit columns
    // whose indexes are already up-to-date. Reindexing columns fail the
    // check even though they appear in the schema's index map.
    if require_online_reindexing {
        let reindexing_in_filter: Vec<String> = attrs
            .iter()
            .filter(|a| {
                !a.starts_with('_')
                    && reindexing_cols.iter().any(|c| c.as_str() == **a)
            })
            .map(|a| (*a).to_string())
            .collect();
        if !reindexing_in_filter.is_empty() {
            return Err(MorayError::NotIndexed {
                bucket: bucket.into(),
                filter: filter_str.to_string(),
                reindexing: reindexing_in_filter,
                unindexed: Vec::new(),
            });
        }
    }

    // Moray never does a full-table scan: a findObjects whose filter
    // doesn't touch *any* indexed attribute errors out regardless of
    // `requireIndexes`. With `requireIndexes = true` we additionally
    // require *every* attr to be indexed. Synthetic attrs (`_id`, etc.)
    // don't count toward indexed coverage on the LHS but don't need to.
    let has_indexed_attr = attrs.iter().any(|a| {
        a.starts_with('_') || index_keys.iter().any(|k| k.as_str() == *a)
    });
    if !has_indexed_attr {
        return Err(MorayError::NotIndexed {
            bucket: bucket.into(),
            filter: filter_str.to_string(),
            reindexing: Vec::new(),
            unindexed: unindexed.clone(),
        });
    }
    if require_indexes {
        // With requireIndexes: true, any column that's either outright
        // unindexed or still reindexing fails the check. We split them
        // into two lists so the error message matches upstream's
        // "Reindexing fields: [...]. Unindexed fields: [...]" shape.
        let reindexing: Vec<String> = attrs
            .iter()
            .filter(|a| {
                !a.starts_with('_')
                    && reindexing_cols.iter().any(|c| c.as_str() == **a)
            })
            .map(|a| (*a).to_string())
            .collect();
        if !unindexed.is_empty() || !reindexing.is_empty() {
            return Err(MorayError::NotIndexed {
                bucket: bucket.into(),
                filter: filter_str.to_string(),
                reindexing,
                unindexed: unindexed.clone(),
            });
        }
    }

    // node-moray (and upstream Moray) accept `limit` / `offset` as
    // either JSON numbers or strings of ascii digits. We coerce both.
    let coerce_usize = |v: &Value| -> Option<usize> {
        match v {
            Value::Number(n) => n.as_u64().map(|u| u as usize),
            Value::String(s) => s.parse::<usize>().ok(),
            _ => None,
        }
    };
    let no_limit = opts
        .get("noLimit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = if no_limit {
        MAX_FIND_LIMIT
    } else {
        // Triton callers (sdc-papi, sdc-vmapi, others) idiomatically
        // pass `limit: params.limit || 0` — relying on upstream moray
        // treating an explicit `0` as "no caller-supplied limit, use
        // the server default". A strict reading would return zero rows
        // for limit=0; we match upstream semantics so PAPI's
        // `/packages` listing (and the rest of the fleet) doesn't
        // silently return empty after the cutover.
        let raw = opts.get("limit").and_then(coerce_usize);
        match raw {
            None | Some(0) => DEFAULT_FIND_LIMIT,
            Some(n) => n.min(MAX_FIND_LIMIT),
        }
    };
    let offset = opts.get("offset").and_then(coerce_usize).unwrap_or(0);

    // Scan oversized so we can filter before applying limit. We cap at
    // MAX_FIND_LIMIT + offset to bound memory for pathological callers.
    let scan_cap = (limit + offset).min(MAX_FIND_LIMIT);
    // Without secondary indexes we have to over-scan and filter in memory;
    // this is O(bucket size). Acceptable for v1 — follow-on work adds an
    // indexed subspace to do push-down scans.
    let raw = store.scan_objects(bucket, MAX_FIND_LIMIT).await?;
    let mut hits: Vec<ObjectMeta> = raw
        .into_iter()
        .filter(|meta| filter.eval(&meta.value, &synthetic_view(meta)))
        .collect();

    // Sort: Moray applies `opts.sort = {attribute, order}`; we support
    // top-level fields and the `_id` synthetic attribute.
    // opts.sort is either `{attribute, order}` or an array of them. We
    // treat the array form as a series of tiebreakers applied in order.
    if let Some(sort_spec) = opts.get("sort") {
        let specs = match sort_spec {
            Value::Array(items) => items.clone(),
            Value::Object(_) => vec![sort_spec.clone()],
            _ => Vec::new(),
        };
        let parsed: Vec<(String, bool)> = specs
            .iter()
            .filter_map(|s| {
                let o = s.as_object()?;
                let attr = o.get("attribute")?.as_str()?.to_string();
                let desc = o
                    .get("order")
                    .and_then(|v| v.as_str())
                    .map(|s| s.eq_ignore_ascii_case("desc"))
                    .unwrap_or(false);
                Some((attr, desc))
            })
            .collect();
        if !parsed.is_empty() {
            hits.sort_by(|a, b| {
                for (attr, desc) in &parsed {
                    let av = attr_for_sort(a, attr);
                    let bv = attr_for_sort(b, attr);
                    let ord = compare_values(&av, &bv);
                    let ord = if *desc { ord.reverse() } else { ord };
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
                std::cmp::Ordering::Equal
            });
        }
    }

    let total = hits.len();
    let hits: Vec<ObjectMeta> = hits.into_iter().skip(offset).take(limit).collect();
    let _ = scan_cap;
    Ok((hits, total))
}

/// For each `unique: true` indexed column on this bucket, extract every
/// value from the incoming object that needs to claim a uniqueness slot.
/// For scalar-typed unique columns that's a single value; for array-typed
/// unique columns every array element is a separate claim.
///
/// Returns an empty Vec if the bucket has no unique columns.
async fn collect_unique_claims<S: MorayStore>(
    store: &S,
    bucket: &str,
    value: &Value,
) -> Result<Vec<(String, Vec<String>)>, MorayError> {
    let Ok(meta) = store.get_bucket(bucket).await else {
        return Ok(Vec::new());
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Ok(Vec::new()),
    };
    let mut out = Vec::new();
    for (col, cfg) in meta.options.index.iter() {
        let is_unique = cfg
            .get("unique")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !is_unique {
            continue;
        }
        let Some(v) = obj.get(col) else { continue };
        let values: Vec<String> = match v {
            Value::Null => continue,
            Value::String(s) => vec![s.clone()],
            Value::Number(n) => vec![n.to_string()],
            Value::Bool(b) => vec![b.to_string()],
            Value::Array(items) => items
                .iter()
                .filter_map(|it| match it {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect(),
            _ => continue,
        };
        if !values.is_empty() {
            out.push((col.clone(), values));
        }
    }
    Ok(out)
}

/// Top-level "ready the incoming value for the store" pipeline. Runs,
/// in order:
///
/// 1. Named built-in triggers registered on the bucket (e.g. sdc-papi's
///    `timestamps`, sdc-ufds's `fixTypes`).
/// 2. Schema-driven type coercion — scalar→array wrap plus IP / subnet
///    canonicalisation — via [`coerce_value_for_schema`].
///
/// Triggers run first because `fixTypes` rewrites types that
/// `coerce_for_put` later normalises (e.g. bool arrays, canonical IPs).
async fn prepare_value_for_put<S: MorayStore>(
    store: &S,
    bucket: &str,
    mut value: Value,
    headers: &serde_json::Map<String, Value>,
) -> Result<Value, MorayError> {
    if let Ok(meta) = store.get_bucket(bucket).await {
        // Hard-fail on unrecognised triggers — see triggers.rs. The
        // bucket-create/update path also rejects unknown triggers, so
        // hitting this is either a stale bucket on disk or a deploy
        // skew between fleet members. Either way the caller should
        // see the error rather than have their put silently miss the
        // mutation step.
        triggers::apply(&meta, &mut value, headers)?;
    }
    coerce_value_for_schema(store, bucket, value).await
}

/// For each column indexed with an array type (`[string]`, `[number]`,
/// etc.), wrap scalar values in a single-element array. Mirrors upstream
/// Moray's put-time type coercion.
async fn coerce_value_for_schema<S: MorayStore>(
    store: &S,
    bucket: &str,
    mut value: Value,
) -> Result<Value, MorayError> {
    let Ok(meta) = store.get_bucket(bucket).await else {
        // If the bucket doesn't exist, pass the value through — the store
        // will return the proper BucketNotFoundError at put time.
        return Ok(value);
    };
    let Some(obj) = value.as_object_mut() else {
        return Ok(value);
    };
    for (col, cfg) in meta.options.index.iter() {
        let Some(ty_str) = cfg.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(ty) = IndexType::parse(ty_str) else {
            continue;
        };
        if let Some(existing) = obj.remove(col) {
            obj.insert(col.clone(), typeval::coerce_for_put(&ty, existing));
        }
    }
    Ok(value)
}

/// Is there a `Substring` clause (the `foo=pre*` / `*mid*` / `*suf`
/// pattern form) anywhere in the filter that targets `attr`?
fn filter_uses_substring(filter: &Filter, target: &str) -> bool {
    match filter {
        Filter::Substring { attr, .. } => attr == target,
        Filter::And(xs) | Filter::Or(xs) => {
            xs.iter().any(|x| filter_uses_substring(x, target))
        }
        Filter::Not(x) => filter_uses_substring(x, target),
        _ => false,
    }
}

/// Scan the filter tree for `:within:=` / `:contains:=` / `:overlaps:=`
/// references against `target`. Each operator has distinct literal-shape
/// rules so the three flags are tracked independently.
fn filter_attr_range_ops(filter: &Filter, target: &str) -> (bool, bool, bool) {
    let mut within = false;
    let mut contains = false;
    let mut overlaps = false;
    fn walk(
        filter: &Filter,
        target: &str,
        within: &mut bool,
        contains: &mut bool,
        overlaps: &mut bool,
    ) {
        match filter {
            Filter::Within { attr, .. } if attr == target => *within = true,
            Filter::Contains { attr, .. } if attr == target => *contains = true,
            Filter::Overlaps { attr, .. } if attr == target => *overlaps = true,
            Filter::And(xs) | Filter::Or(xs) => {
                for x in xs {
                    walk(x, target, within, contains, overlaps);
                }
            }
            Filter::Not(x) => walk(x, target, within, contains, overlaps),
            _ => {}
        }
    }
    walk(filter, target, &mut within, &mut contains, &mut overlaps);
    (within, contains, overlaps)
}

/// `:within:=` operator type rules.
///
/// - `ip` column: literal must be a CIDR subnet.
/// - `subnet` column: not supported (use `:contains:=` instead).
/// - `number`/`date` column (scalar): literal must be a range literal —
///   a plain point is rejected, because saying "5 is within 5" is
///   degenerate and upstream rejects it.
/// - `numrange`/`daterange` column: literal may be a point (range
///   contains element) or a range (subrange check).
/// - All other types: unsupported.
fn validate_within_literal(attr: &str, ty: &str, literal: &str) -> Result<(), MorayError> {
    let err = |msg: String| MorayError::InvalidQuery(format!("filter attribute {attr}: {msg}"));
    match ty {
        "ip" => typeval::validate_scalar_literal("subnet", literal)
            .map_err(|_| err("value must be a subnet".into())),
        "number" => {
            if literal.parse::<f64>().is_ok() {
                return Err(err(
                    ":within:= on a scalar number requires a range literal".into(),
                ));
            }
            typeval::validate_scalar_literal("numrange", literal)
                .map_err(err)
        }
        "date" => {
            if literal.parse::<f64>().is_ok()
                || typeval::validate_scalar_literal("date", literal).is_ok()
            {
                return Err(err(
                    ":within:= on a scalar date requires a range literal".into(),
                ));
            }
            typeval::validate_scalar_literal("daterange", literal).map_err(err)
        }
        "numrange" | "daterange" => {
            // `:within:=` on a range column: literal must be a point of
            // the base type. Unbounded range literals like `[,]` are
            // rejected (the "is this point within a range" question is
            // ill-defined against an empty/universal filter).
            if typeval::is_unbounded_range(literal) {
                return Err(err(":within:= literal must be bounded".into()));
            }
            let base = if ty == "numrange" { "number" } else { "date" };
            typeval::validate_scalar_literal(base, literal)
                .or_else(|_| typeval::validate_scalar_literal(ty, literal))
                .map_err(err)
        }
        other => Err(err(format!(":within:= is not supported for type {other}"))),
    }
}

/// `:contains:=` operator type rules (the mirror of `:within:=`):
///
/// - `subnet` column: literal must be an IP address.
/// - `numrange` / `daterange`: literal must be a scalar point of the
///   element type, never a range.
/// - Scalar types (`number`, `date`, `ip`): unsupported — a scalar can't
///   contain anything.
/// - All other types: unsupported.
/// `:overlaps:=` operator type rules — only valid on range columns
/// (`numrange`, `daterange`). For those, the literal must parse as a
/// range or point; unbounded `[,]` is accepted (it trivially overlaps).
fn validate_overlaps_literal(
    attr: &str,
    ty: &str,
    literal: &str,
) -> Result<(), MorayError> {
    let err = |msg: String| MorayError::InvalidQuery(format!("filter attribute {attr}: {msg}"));
    match ty {
        "numrange" => typeval::validate_scalar_literal("numrange", literal)
            .or_else(|_| literal.parse::<f64>().map(|_| ()).map_err(|e| e.to_string()))
            .map_err(err),
        "daterange" => typeval::validate_scalar_literal("daterange", literal)
            .or_else(|_| typeval::validate_scalar_literal("date", literal))
            .map_err(err),
        other => Err(err(format!(":overlaps:= is not supported for type {other}"))),
    }
}

fn validate_contains_literal(
    attr: &str,
    ty: &str,
    literal: &str,
) -> Result<(), MorayError> {
    let err = |msg: String| MorayError::InvalidQuery(format!("filter attribute {attr}: {msg}"));
    match ty {
        "subnet" => typeval::validate_scalar_literal("ip", literal)
            .map_err(|_| err("value must be an ip".into())),
        "numrange" => {
            if literal.parse::<f64>().is_err() {
                return Err(err(":contains:= on numrange requires a number".into()));
            }
            Ok(())
        }
        "daterange" => {
            // `:contains:=` on a daterange wants a point; reject range
            // literals and malformed dates. Upstream rejects space-
            // separated forms like `"1758-05-06 00:00:00.000Z"` that
            // aren't valid RFC 3339.
            if literal.starts_with('[') || literal.starts_with('(') {
                return Err(err(":contains:= on daterange requires a date".into()));
            }
            typeval::validate_scalar_literal("date", literal)
                .map_err(|_| err(":contains:= on daterange requires a date".into()))
        }
        other => Err(err(format!(":contains:= is not supported for type {other}"))),
    }
}

/// Canonicalise an IP-subnet literal to the form chrono/std::net emits
/// from a round-trip parse — e.g. `fe80::0/64` becomes `fe80::/64`.
fn canonical_subnet(s: &str) -> String {
    let Some((addr, prefix)) = s.split_once('/') else {
        return s.to_string();
    };
    let Ok(ip) = addr.parse::<std::net::IpAddr>() else {
        return s.to_string();
    };
    let Ok(prefix_n) = prefix.parse::<u8>() else {
        return s.to_string();
    };
    format!("{ip}/{prefix_n}")
}

fn synthetic_view(m: &ObjectMeta) -> Value {
    json!({
        "_id": m.id,
        "_key": m.key,
        "_etag": m.etag,
        "_mtime": m.mtime.timestamp_millis(),
    })
}

fn attr_for_sort(m: &ObjectMeta, attr: &str) -> Value {
    if attr == "_id" {
        return json!(m.id);
    }
    if attr == "_etag" {
        return json!(m.etag);
    }
    if attr == "_mtime" {
        return json!(m.mtime.timestamp_millis());
    }
    m.value.get(attr).cloned().unwrap_or(Value::Null)
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Number(an), Value::Number(bn)) => an
            .as_f64()
            .and_then(|af| bn.as_f64().map(|bf| af.partial_cmp(&bf)))
            .flatten()
            .unwrap_or(Ordering::Equal),
        (Value::String(as_), Value::String(bs)) => as_.cmp(bs),
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Less,
        (_, Value::Null) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

fn object_wire(bucket: &str, m: &ObjectMeta) -> Value {
    json!({
        "bucket": bucket,
        "key": m.key,
        "value": m.value,
        "_etag": m.etag,
        "_id":   m.id,
        "_mtime": m.mtime.timestamp_millis(),
    })
}

/// node-moray parseBucketConfig expects index/pre/post/options as
/// JSON-encoded strings (Moray/Postgres legacy — those columns were TEXT).
fn bucket_wire(b: &Bucket) -> Value {
    let stringify = |v: &Value| serde_json::to_string(v).unwrap_or_else(|_| "null".into());
    let index = Value::Object(b.options.index.clone());
    let pre = Value::Array(b.options.pre.clone());
    let post = Value::Array(b.options.post.clone());
    let options = Value::Object(b.options.options.clone());
    let mut out = json!({
        "name":    b.name,
        "index":   stringify(&index),
        "pre":     stringify(&pre),
        "post":    stringify(&post),
        "options": stringify(&options),
        "mtime":   b.mtime.timestamp_millis(),
    });
    if let Some(state) = &b.reindex_active {
        // reindex_active ships as a JSON-encoded string keyed by bucket
        // version — matches upstream where that column was Postgres TEXT.
        let payload = json!({
            b.options
                .options
                .get("version")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .to_string(): state.columns,
        });
        out["reindex_active"] = Value::String(stringify(&payload));
    }
    out
}

/// Fetch a string bucket-name argument at a specific index. Returns the
/// raw string; empty-string handling is delegated to
/// [`validate::bucket_name`] which emits the full
/// `"nonempty string: bucket should NOT be shorter than 1 characters"`
/// message upstream expects for that specific failure.
fn bucket_name_arg(args: &[Value], i: usize, rpc: &str) -> Result<String, MorayError> {
    match args.get(i) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(MorayError::Invocation(format!(
            "{rpc} expects \"bucket\" (args[{i}]) to be a nonempty string"
        ))),
    }
}

fn parse_bucket_config(v: Option<&Value>) -> Result<BucketConfig, MorayError> {
    let mut cfg: BucketConfig = match v {
        Some(v) if !v.is_null() => serde_json::from_value(v.clone()).map_err(|e| {
            MorayError::InvalidArg(format!("bucket config: {e}"))
        })?,
        _ => BucketConfig::default(),
    };
    // Moray always materialises a version on the stored bucket: if the
    // caller didn't supply one, it defaults to 0. We mirror that so
    // roundtrips through `getBucket` report `options.version: 0`.
    cfg.options
        .entry("version".to_string())
        .or_insert_with(|| serde_json::json!(0));
    Ok(cfg)
}

/// Validate findObjects' `opts.sort` shape. Accepts either one sort spec
/// (`{attribute, order?}`) or an array of them for multi-attribute
/// sorting.
fn validate_find_options(
    opts: &serde_json::Map<String, Value>,
) -> Result<(), MorayError> {
    let Some(sort) = opts.get("sort") else {
        return Ok(());
    };
    let specs: Vec<&serde_json::Map<String, Value>> = match sort {
        Value::Object(o) => vec![o],
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let Some(o) = it.as_object() else {
                    return Err(MorayError::Invocation(
                        "findObjects expects \"options\" (args[2]) to be \
                         a valid options object: options.sort[*] should be object"
                            .into(),
                    ));
                };
                out.push(o);
            }
            out
        }
        _ => {
            return Err(MorayError::Invocation(
                "findObjects expects \"options\" (args[2]) to be \
                 a valid options object: options.sort should be"
                    .into(),
            ))
        }
    };
    for sort_obj in specs {
        match sort_obj.get("attribute") {
            Some(Value::String(_)) => {}
            _ => {
                return Err(MorayError::Invocation(
                    "findObjects expects \"options\" (args[2]) to be \
                     a valid options object: options.sort.attribute should be string"
                        .into(),
                ))
            }
        }
        if let Some(order) = sort_obj.get("order") {
            match order {
                Value::String(s)
                    if s.eq_ignore_ascii_case("ASC")
                        || s.eq_ignore_ascii_case("DESC") => {}
                _ => {
                    return Err(MorayError::Invocation(
                        "options.sort.order should be equal to \
                         one of the allowed values (\"ASC\", \"DESC\")"
                            .into(),
                    ))
                }
            }
        }
    }
    Ok(())
}

fn parse_put_opts(v: Option<&Value>) -> PutOpts {
    let Some(o) = v.and_then(|v| v.as_object()) else {
        return PutOpts::default();
    };
    let headers = o
        .get("headers")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    PutOpts {
        // Wire forms for `etag`:
        //   key absent     → no guard (unconditional upsert).
        //   Value::Null    → `Some("null")` — "must not exist" guard.
        //                    node-moray calls this MANTA-980 "null etag".
        //   Value::String  → literal etag to compare against.
        etag: match o.get("etag") {
            None => None,
            Some(Value::Null) => Some("null".into()),
            Some(Value::String(s)) => Some(s.clone()),
            Some(_) => None,
        },
        headers,
    }
}

fn string_arg(args: &[Value], i: usize) -> Result<String, MorayError> {
    args.get(i)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| MorayError::InvalidArg(format!("missing string arg #{i}")))
}

/// `InvocationError`-grade check: arg at index `i` must be a non-empty
/// string. Error message matches upstream Moray's ajv schema output.
fn expect_nonempty_string(
    args: &[Value],
    i: usize,
    rpc: &str,
    name: &str,
) -> Result<String, MorayError> {
    match args.get(i) {
        Some(Value::String(s)) if !s.is_empty() => Ok(s.clone()),
        _ => Err(MorayError::Invocation(format!(
            "{rpc} expects \"{name}\" (args[{i}]) to be a nonempty string"
        ))),
    }
}

/// `InvocationError`: arg at index `i` must be a JSON object, phrased
/// with the verb's own wording (e.g. `updateBucket expects "config"
/// (args[1]) to be an object`). Upstream's error message doesn't include
/// the generic schema prefix for this case.
fn expect_rpc_object(
    args: &[Value],
    i: usize,
    rpc: &str,
    name: &str,
) -> Result<(), MorayError> {
    match args.get(i) {
        Some(Value::Object(_)) => Ok(()),
        _ => Err(MorayError::Invocation(format!(
            "{rpc} expects \"{name}\" (args[{i}]) to be an object"
        ))),
    }
}

/// `InvocationError`-grade check: arg at index `i` must be a JSON object.
fn expect_object(
    args: &[Value],
    i: usize,
    rpc: &str,
    name: &str,
) -> Result<serde_json::Map<String, Value>, MorayError> {
    match args.get(i) {
        Some(Value::Object(o)) => Ok(o.clone()),
        _ => Err(MorayError::Invocation(format!(
            "{rpc} expects \"{name}\" (args[{i}]) to be an object"
        ))),
    }
}

/// Like `expect_object` but tolerates a missing trailing argument
/// (defaulting to an empty object). An explicit `null` or a non-object
/// like an array is rejected with upstream's exact message.
fn expect_options_object(
    args: &[Value],
    i: usize,
    rpc: &str,
) -> Result<serde_json::Map<String, Value>, MorayError> {
    match args.get(i) {
        None => Ok(serde_json::Map::new()),
        Some(Value::Object(o)) => Ok(o.clone()),
        _ => Err(MorayError::Invocation(format!(
            "{rpc} expects \"options\" (args[{i}]) to be \
             a valid options object: options should be object"
        ))),
    }
}

fn expect_nonnegative_integer(
    args: &[Value],
    i: usize,
    rpc: &str,
    name: &str,
) -> Result<u64, MorayError> {
    match args.get(i) {
        Some(Value::Number(n)) if n.is_u64() => Ok(n.as_u64().unwrap_or(0)),
        _ => Err(MorayError::Invocation(format!(
            "{rpc} expects \"{name}\" (args[{i}]) to be a nonnegative integer"
        ))),
    }
}

// Silence unused warning for FastStatus when only Data frames go out —
// FastMessage::data/error constants already use it internally.
#[allow(dead_code)]
fn _force_status_use(_: FastStatus) {}
