// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonctl ssh-key` — manage your own (caller-scoped) SSH keys.
//!
//! Mirrors `tritonadm auth ssh-key`: every operation is bound to the
//! caller's `User` scope, inferred from the bearer token. There is no
//! tenant/silo/project selector — the server resolves the owner.

use anyhow::{Context, Result};
use clap::Subcommand;
use triton_cli_core::{OutputFormat, Table, emit};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum SshKeyCmd {
    /// List your SSH keys.
    List,
    /// Show one SSH key by UUID.
    Show { id: Uuid },
    /// Register a new SSH key owned by you.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        /// OpenSSH-formatted public key string.
        #[arg(long, conflicts_with = "public_key_file")]
        public_key: Option<String>,
        /// Path to a file containing the OpenSSH public key.
        #[arg(long, conflicts_with = "public_key")]
        public_key_file: Option<String>,
    },
    /// Delete an SSH key by UUID.
    Delete { id: Uuid },
}

pub async fn run(cli: &crate::Cli, cmd: &SshKeyCmd, format: OutputFormat) -> Result<()> {
    let client = crate::connect(cli).await?;
    match cmd {
        SshKeyCmd::List => {
            let keys = client
                .list_my_ssh_keys()
                .send()
                .await
                .context("list ssh keys")?
                .into_inner();
            if emit(format, &keys)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "FINGERPRINT", "SCOPE"], cli.no_headers);
            for k in &keys {
                t.row([
                    k.id.to_string(),
                    k.name.clone(),
                    k.fingerprint.clone(),
                    crate::wire(&k.scope),
                ]);
            }
            t.print();
            Ok(())
        }
        SshKeyCmd::Show { id } => {
            let key = client
                .get_ssh_key()
                .key_id(*id)
                .send()
                .await
                .context("get ssh key")?
                .into_inner();
            if emit(format, &key)? {
                return Ok(());
            }
            print_ssh_key(&key);
            Ok(())
        }
        SshKeyCmd::Create {
            name,
            description,
            public_key,
            public_key_file,
        } => {
            let public_key = resolve_public_key(public_key.clone(), public_key_file.clone())?;
            let key = client
                .create_my_ssh_key()
                .body(tritond_client::types::NewSshKey {
                    name: name.clone(),
                    description: description.clone(),
                    public_key,
                })
                .send()
                .await
                .context("create ssh key")?
                .into_inner();
            if emit(format, &key)? {
                return Ok(());
            }
            println!("Registered ssh key {}", key.id);
            print_ssh_key(&key);
            Ok(())
        }
        SshKeyCmd::Delete { id } => {
            client
                .delete_ssh_key()
                .key_id(*id)
                .send()
                .await
                .context("delete ssh key")?;
            println!("SSH key {id} deleted.");
            Ok(())
        }
    }
}

/// Resolve `--public-key` / `--public-key-file` into the openssh string
/// the API edge expects. clap's `conflicts_with` guarantees the two are
/// never both set.
fn resolve_public_key(
    public_key: Option<String>,
    public_key_file: Option<String>,
) -> Result<String> {
    match (public_key, public_key_file) {
        (Some(s), _) => Ok(s),
        (None, Some(path)) => {
            std::fs::read_to_string(&path).with_context(|| format!("read public key from {path}"))
        }
        (None, None) => anyhow::bail!("--public-key or --public-key-file is required"),
    }
}

fn print_ssh_key(key: &tritond_client::types::SshKey) {
    println!("id:          {}", key.id);
    println!("name:        {}", key.name);
    println!("scope:       {}", crate::wire(&key.scope));
    println!("fingerprint: {}", key.fingerprint);
    println!("(use -o json for the full record)");
}
