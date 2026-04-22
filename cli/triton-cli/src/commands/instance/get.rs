// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance get and IP commands

use anyhow::Result;
use clap::Args;

use crate::client::AnyClient;
use crate::dispatch;
use crate::output::json;

#[derive(Args, Clone)]
pub struct GetArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct IpArgs {
    /// Instance ID or name
    pub instance: String,
}

pub async fn run(args: GetArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let account = client.effective_account();
    let machine_uuid = resolve_instance(&args.instance, client).await?;

    // The Progenitor-generated `Machine` type differs per client crate,
    // so we serialize inside the dispatch arm and render the resulting
    // `serde_json::Value` uniformly outside.
    let machine_json: serde_json::Value = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_machine()
            .account(account)
            .machine(machine_uuid)
            .send()
            .await?
            .into_inner();
        serde_json::to_value(&resp)?
    });

    if use_json {
        json::print_json(&machine_json)?;
    } else {
        json::print_json_pretty(&machine_json)?;
    }

    Ok(())
}

pub async fn ip(args: IpArgs, client: &AnyClient) -> Result<()> {
    let account = client.effective_account();
    let machine_uuid = resolve_instance(&args.instance, client).await?;

    // Only the `primary_ip` field escapes the dispatch arm, as a plain
    // `Option<String>`, so both arms converge on a std type.
    let primary_ip: Option<String> = dispatch!(client, |c| {
        c.inner()
            .get_machine()
            .account(account)
            .machine(machine_uuid)
            .send()
            .await?
            .into_inner()
            .primary_ip
    });

    if let Some(ip) = primary_ip {
        println!("{}", ip);
    } else {
        return Err(anyhow::anyhow!("Instance has no primary IP"));
    }

    Ok(())
}

/// Resolve instance name or short ID via a cloudapi-direct
/// `TypedClient`. Kept for call sites in the `image` / `create` modules
/// that haven't been fully ported to [`AnyClient`] yet; those paths are
/// SSH-profile only by design in this slice.
pub async fn resolve_instance_ssh(
    id_or_name: &str,
    client: &cloudapi_client::TypedClient,
) -> Result<uuid::Uuid> {
    // Delegate through the AnyClient implementation; wrap the TypedClient
    // in a CloudApi variant on the fly. This is cheap (just a reference
    // dance in the match arms) and avoids duplicating the lookup logic.
    //
    // Safety: the temporary borrow is dropped at the end of this fn.
    // SAFETY NOTE: We can't own a TypedClient by move (no Clone), so we
    // re-implement the minimal logic inline here instead.
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        return Ok(uuid);
    }

    let account = client.effective_account();
    let is_short_uuid = id_or_name.len() >= 8
        && id_or_name
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-');
    if is_short_uuid {
        let resp = client
            .inner()
            .list_machines()
            .account(account)
            .send()
            .await?;
        let machines = resp.into_inner();
        let matches: Vec<_> = machines
            .iter()
            .filter(|m| m.id.to_string().starts_with(id_or_name))
            .collect();
        match matches.len() {
            1 => return Ok(matches[0].id),
            n if n > 1 => {
                let ids: Vec<String> = matches
                    .iter()
                    .map(|m| m.id.to_string()[..8].to_string())
                    .collect();
                return Err(anyhow::anyhow!(
                    "Ambiguous short ID '{}' matches {} instances: {}",
                    id_or_name,
                    n,
                    ids.join(", ")
                ));
            }
            _ => {}
        }
    }

    let resp = client
        .inner()
        .list_machines()
        .account(account)
        .name(id_or_name)
        .send()
        .await?;
    if let Some(m) = resp.into_inner().first() {
        return Ok(m.id);
    }

    Err(crate::errors::ResourceNotFoundError(format!("Instance not found: {}", id_or_name)).into())
}

/// Resolve instance name or short ID to full UUID.
///
/// Variant-agnostic: fetches the minimum information needed from each
/// arm (a `Vec<(Uuid, String)>` of id+name pairs) so the downstream
/// matching logic works against `std` types regardless of which client
/// the caller is holding.
pub async fn resolve_instance(id_or_name: &str, client: &AnyClient) -> Result<uuid::Uuid> {
    // First try as UUID
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        // NOTE: We accept the parsed ID without verifying it exists server-side.
        // node-triton calls getMachine(uuid) to verify, but the action itself will
        // hit the server and fail with a clear error if the UUID doesn't exist.
        return Ok(uuid);
    }

    let account = client.effective_account();

    // Try short ID match (at least 8 hex characters) — requires fetching all machines
    let is_short_uuid = id_or_name.len() >= 8
        && id_or_name
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '-');
    if is_short_uuid {
        let machines: Vec<(uuid::Uuid, String)> = dispatch!(client, |c| {
            let resp = c
                .inner()
                .list_machines()
                .account(account)
                .send()
                .await?
                .into_inner();
            resp.into_iter().map(|m| (m.id, m.name)).collect()
        });
        let matches: Vec<_> = machines
            .iter()
            .filter(|(id, _)| id.to_string().starts_with(id_or_name))
            .collect();
        match matches.len() {
            1 => return Ok(matches[0].0),
            n if n > 1 => {
                let ids: Vec<String> = matches
                    .iter()
                    .map(|(id, _)| id.to_string()[..8].to_string())
                    .collect();
                return Err(anyhow::anyhow!(
                    "Ambiguous short ID '{}' matches {} instances: {}",
                    id_or_name,
                    n,
                    ids.join(", ")
                ));
            }
            _ => {} // No matches, fall through to name lookup
        }
    }

    // Try exact name match using server-side filter
    let first_match: Option<uuid::Uuid> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_machines()
            .account(account)
            .name(id_or_name)
            .send()
            .await?
            .into_inner();
        resp.first().map(|m| m.id)
    });
    if let Some(id) = first_match {
        return Ok(id);
    }

    Err(crate::errors::ResourceNotFoundError(format!("Instance not found: {}", id_or_name)).into())
}
