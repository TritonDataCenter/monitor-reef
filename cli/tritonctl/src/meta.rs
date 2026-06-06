// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl meta` — layered IMDS metadata for tenant-owned entities.
//!
//! A key/value surface (get/set/unset) over the `project` and
//! `instance` scopes only. The tenant and silo scopes are operator
//! territory (`tritonadm`); the server infers the tenant from the
//! bearer token, so we never name it here.

use anyhow::{Context, Result};
use clap::Subcommand;
use tritond_client::types::{MetaScope, SetMetaRequest};
use uuid::Uuid;

/// Which scope a tenant may address. The owning entity is named by its
/// UUID via `--id`; the tenant is inferred from the token.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum MetaScopeArg {
    Project,
    Instance,
}

impl From<MetaScopeArg> for MetaScope {
    fn from(s: MetaScopeArg) -> Self {
        match s {
            MetaScopeArg::Project => MetaScope::Project,
            MetaScopeArg::Instance => MetaScope::Instance,
        }
    }
}

#[derive(Subcommand)]
pub enum MetaCmd {
    /// List every metadata entry at one scope.
    List {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        /// UUID of the owning entity (project / instance).
        #[arg(long)]
        id: Uuid,
    },
    /// Read one metadata entry by key.
    Get {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        #[arg(long)]
        id: Uuid,
        /// Metadata key, e.g. `config/ntp-servers`, `guest/leader`,
        /// `user-data`. Slash-separated.
        #[arg(long)]
        key: String,
    },
    /// Upsert one metadata entry.
    Set {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        #[arg(long)]
        id: Uuid,
        #[arg(long)]
        key: String,
        /// Value as a JSON literal (e.g. `'"10.0.0.2"'`, `'42'`,
        /// `'{"a":1}'`). A bare string without quotes is accepted and
        /// stored as a JSON string.
        #[arg(long)]
        value: String,
        /// Override the default guest-visibility for this key.
        #[arg(long)]
        guest_visible: Option<bool>,
        /// Mark this key guest-writable. Only meaningful on `guest/*`
        /// keys at instance scope; the server rejects it elsewhere.
        #[arg(long)]
        guest_writable: bool,
    },
    /// Delete one metadata entry.
    Unset {
        #[arg(long, value_enum)]
        scope: MetaScopeArg,
        #[arg(long)]
        id: Uuid,
        #[arg(long)]
        key: String,
    },
    /// The full realized view for one instance: the precedence merge of
    /// project/instance metadata plus the computed system keys, each
    /// leaf tagged with the scope it came from.
    Realized {
        /// Instance UUID.
        #[arg(long)]
        instance: Uuid,
    },
}

pub async fn run(
    cli: &crate::Cli,
    cmd: &MetaCmd,
    format: triton_cli_core::OutputFormat,
) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        MetaCmd::List { scope, id } => {
            let entries = client
                .list_meta()
                .scope(MetaScope::from(*scope))
                .scope_id(*id)
                .send()
                .await
                .context("list meta")?
                .into_inner();
            if triton_cli_core::emit(format, &entries)? {
                return Ok(());
            }
            let mut t = triton_cli_core::Table::new(
                &["KEY", "VISIBLE", "WRITABLE", "BY", "VALUE"],
                cli.no_headers,
            );
            for entry in &entries {
                t.row([
                    entry.key.clone(),
                    yes_no(entry.guest_visible),
                    yes_no(entry.guest_writable),
                    entry.updated_by.clone(),
                    json_inline(&entry.value),
                ]);
            }
            t.print();
            Ok(())
        }
        MetaCmd::Get { scope, id, key } => {
            let entry = client
                .get_meta()
                .scope(MetaScope::from(*scope))
                .scope_id(*id)
                .key(key)
                .send()
                .await
                .context("get meta")?
                .into_inner();
            if triton_cli_core::emit(format, &entry)? {
                return Ok(());
            }
            println!("key:            {}", entry.key);
            println!("guest_visible:  {}", entry.guest_visible);
            println!("guest_writable: {}", entry.guest_writable);
            println!("updated_by:     {}", entry.updated_by);
            println!("updated_at:     {}", entry.updated_at);
            println!("value:          {}", json_inline(&entry.value));
            Ok(())
        }
        MetaCmd::Set {
            scope,
            id,
            key,
            value,
            guest_visible,
            guest_writable,
        } => {
            let body = SetMetaRequest {
                value: parse_value(value),
                guest_visible: *guest_visible,
                guest_writable: Some(*guest_writable),
            };
            let response = client
                .set_meta()
                .scope(MetaScope::from(*scope))
                .scope_id(*id)
                .key(key)
                .body(body)
                .send()
                .await
                .context("set meta")?
                .into_inner();
            if triton_cli_core::emit(format, &response)? {
                return Ok(());
            }
            println!(
                "set {} (generation {})",
                response.entry.key, response.generation
            );
            println!("  guest_visible:  {}", response.entry.guest_visible);
            println!("  guest_writable: {}", response.entry.guest_writable);
            Ok(())
        }
        MetaCmd::Unset { scope, id, key } => {
            client
                .delete_meta()
                .scope(MetaScope::from(*scope))
                .scope_id(*id)
                .key(key)
                .send()
                .await
                .context("delete meta")?;
            println!("deleted {key}.");
            Ok(())
        }
        MetaCmd::Realized { instance } => {
            let entries = client
                .get_instance_realized_meta()
                .instance_id(*instance)
                .send()
                .await
                .context("get realized meta")?
                .into_inner();
            if triton_cli_core::emit(format, &entries)? {
                return Ok(());
            }
            let mut t = triton_cli_core::Table::new(
                &["KEY", "FROM", "VISIBLE", "WRITABLE", "VALUE"],
                cli.no_headers,
            );
            for entry in &entries {
                t.row([
                    entry.key.clone(),
                    crate::wire(&entry.from),
                    yes_no(entry.value.guest_visible),
                    yes_no(entry.value.guest_writable),
                    json_inline(&entry.value.value),
                ]);
            }
            t.print();
            Ok(())
        }
    }
}

/// Parse a `--value` CLI string into a JSON value. A bare string that
/// isn't valid JSON is wrapped as a JSON string so a caller can pass
/// `--value foo` without quoting.
fn parse_value(raw: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => v,
        Err(_) => serde_json::Value::String(raw.to_string()),
    }
}

fn json_inline(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "<bad-json>".to_string())
}

fn yes_no(b: bool) -> String {
    if b { "yes" } else { "no" }.to_string()
}
