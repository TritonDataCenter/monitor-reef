// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared IMGAPI helpers used by both `tritonadm image install` and the
//! `tritonadm post-setup <svc>` flows.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Context, Result};
use uuid::Uuid;

use super::errors::{action_is_404, is_404};

/// Maximum origin chain depth. `triton-origin-*` typically has a single
/// ancestor (the pkgsrc base image) and never chains deeper than two or
/// three levels, so 16 is a generous guard against a pathological manifest
/// or a cycle.
const MAX_ORIGIN_DEPTH: usize = 16;

/// Ensure an image's origin chain is present in the local IMGAPI.
///
/// IMGAPI refuses to activate an image whose origin it has never seen, so
/// before importing a service image (e.g. `triton-api`) we have to make
/// sure the `triton-origin-*` ancestor is local first. Walks the chain
/// iteratively: for each unseen origin UUID we fetch the manifest, import
/// it if absent, then enqueue *its* origin for the next iteration.
///
/// Cross-channel fallback: service images are often published to
/// `experimental` while their origin only exists on the default channel.
/// When `channel` is `Some`, we try that channel first, then fall back to
/// channel-less lookup on 404. `sdc-imgadm import -S <updates>` is the
/// documented manual workaround if both fail.
pub(crate) async fn ensure_origin_imported(
    client: &imgapi_client::Client,
    typed_client: &imgapi_client::TypedClient,
    origin: Option<Uuid>,
    updates_url: &str,
    channel: Option<&str>,
) -> Result<()> {
    let mut queue: Vec<Uuid> = origin.into_iter().collect();
    let mut seen: HashSet<Uuid> = HashSet::new();

    while let Some(uuid) = queue.pop() {
        if !seen.insert(uuid) {
            continue;
        }
        if seen.len() > MAX_ORIGIN_DEPTH {
            anyhow::bail!(
                "origin chain depth exceeds {MAX_ORIGIN_DEPTH} while resolving {uuid} \
                 — manifest loop or pathological chain?"
            );
        }

        // Fetch the manifest: either from the local IMGAPI (already
        // imported) or, if absent, import it first and then fetch. Only a
        // genuine 404 means "not local"; transport / 5xx errors mean the
        // local IMGAPI is broken and we'd just fail confusingly mid-import.
        let manifest = match client.get_image().uuid(uuid).send().await {
            Ok(resp) => resp.into_inner(),
            Err(e) if is_404(&e) => {
                eprintln!("Origin image {uuid} not found locally, importing from {updates_url}...");
                import_remote_with_channel_fallback(typed_client, uuid, updates_url, channel)
                    .await?;
                wait_for_image_active(client, uuid).await?;
                eprintln!("Origin image {uuid} imported.");
                client
                    .get_image()
                    .uuid(uuid)
                    .send()
                    .await
                    .with_context(|| {
                        format!("failed to fetch manifest for freshly-imported {uuid}")
                    })?
                    .into_inner()
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to look up origin image {uuid} in local IMGAPI")
                });
            }
        };

        if let Some(next) = manifest.origin {
            queue.push(next);
        }
    }

    Ok(())
}

/// Call `import_remote_image`, retrying without a channel query parameter
/// if the channel-scoped lookup fails. Origin images like
/// `triton-origin-*` are published to the stable channel and aren't
/// re-published when a derived image lands on `experimental`, so the
/// channel-scoped call 404s in that case.
async fn import_remote_with_channel_fallback(
    typed_client: &imgapi_client::TypedClient,
    uuid: Uuid,
    updates_url: &str,
    channel: Option<&str>,
) -> Result<()> {
    if let Some(ch) = channel {
        let url_with_channel = format!("{updates_url}?channel={ch}");
        match typed_client
            .import_remote_image(&uuid, &url_with_channel, true)
            .await
        {
            Ok(_) => return Ok(()),
            // Only fall back to the default channel on a true "not found".
            // 5xx / auth / TLS errors are not channel-mismatch problems —
            // retrying without a channel just produces a confusing chained
            // error and a misleading remediation hint.
            // arch-lint: allow(no-error-swallowing) reason="404 falls through to default-channel retry below; non-404s are propagated by the next arm"
            Err(e) if action_is_404(&e) => {
                eprintln!(
                    "Origin {uuid} not available on channel {ch}; \
                     retrying against the updates server's default channel..."
                );
            }
            Err(e) => {
                return Err(anyhow::anyhow!(e)).with_context(|| {
                    format!(
                        "failed to import origin image {uuid} from {updates_url} \
                         (channel={ch})"
                    )
                });
            }
        }
    }

    typed_client
        .import_remote_image(&uuid, updates_url, true)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "failed to import origin image {uuid} from {updates_url} \
                 (channel={channel:?} and default channel both failed): {e}. \
                 Manual workaround: sdc-imgadm import -S {updates_url} {uuid}"
            )
        })?;
    Ok(())
}

/// Poll local IMGAPI until the image reaches "active" state. The
/// import-remote action is async: the image may 404 at first, then appear
/// as "unactivated", then become "active".
///
/// 404 is the expected "not yet imported" signal. Other errors (5xx,
/// transport, auth) mean the local IMGAPI itself is broken; we tolerate a
/// brief blip but bail out rather than print dots for 4 minutes and then
/// claim a "timeout" — that masks the real failure.
async fn wait_for_image_active(client: &imgapi_client::Client, uuid: Uuid) -> Result<()> {
    const NON_404_TOLERANCE: usize = 3;
    let mut consecutive_non_404 = 0usize;
    for _ in 0..120 {
        match client.get_image().uuid(uuid).send().await {
            Ok(resp) => {
                consecutive_non_404 = 0;
                if resp.into_inner().state == imgapi_client::types::ImageState::Active {
                    eprintln!();
                    return Ok(());
                }
            }
            Err(e) if is_404(&e) => {
                consecutive_non_404 = 0;
            }
            Err(e) => {
                consecutive_non_404 += 1;
                if consecutive_non_404 >= NON_404_TOLERANCE {
                    eprintln!();
                    return Err(e).with_context(|| {
                        format!(
                            "local IMGAPI returned non-404 errors {NON_404_TOLERANCE} times \
                             in a row while waiting for {uuid} to become active"
                        )
                    });
                }
            }
        }
        eprint!(".");
        std::io::stderr().flush().ok();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::bail!("timed out waiting for image {uuid} to become active (4 minutes)")
}
