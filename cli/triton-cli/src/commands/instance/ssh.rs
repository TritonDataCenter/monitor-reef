// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance SSH command

use std::process::Command;

use anyhow::Result;
use clap::Args;
use cloudapi_client::TypedClient;

#[derive(Args, Clone)]
pub struct SshArgs {
    /// Instance ID or name
    pub instance: String,

    /// SSH user (default: root)
    #[arg(long, short = 'l', default_value = "root")]
    pub user: String,

    /// SSH identity file
    #[arg(long, short = 'i')]
    pub identity: Option<String>,

    /// Additional SSH options
    #[arg(long, short = 'o')]
    pub ssh_option: Option<Vec<String>>,

    /// Command to run on instance
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

pub async fn run(args: SshArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Get instance to find IP
    let response = client
        .inner()
        .get_machine()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let machine = response.into_inner();

    let ip = machine
        .primary_ip
        .ok_or_else(|| anyhow::anyhow!("Instance has no primary IP"))?;

    // Build SSH command
    let mut ssh_cmd = Command::new("ssh");

    // Add user@host
    ssh_cmd.arg(format!("{}@{}", args.user, ip));

    // Add identity file if specified
    if let Some(identity) = &args.identity {
        ssh_cmd.arg("-i").arg(identity);
    }

    // Add SSH options
    if let Some(opts) = &args.ssh_option {
        for opt in opts {
            ssh_cmd.arg("-o").arg(opt);
        }
    }

    // Add remote command if specified
    if !args.command.is_empty() {
        ssh_cmd.args(&args.command);
    }

    // Execute SSH
    let status = ssh_cmd.status()?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
