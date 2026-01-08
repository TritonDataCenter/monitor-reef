// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance SSH command
//!
//! This module provides SSH connectivity to instances with support for:
//! - Automatic default user detection from image tags
//! - SSH proxy/bastion host support via instance tags
//! - Custom SSH options and identity files

use std::process::Command;

use anyhow::{Context, Result};
use clap::Args;
use cloudapi_client::TypedClient;
use cloudapi_client::types::Machine;
use serde_json::Value;

/// Tag constants for SSH configuration on instances
const TAG_SSH_IP: &str = "tritoncli.ssh.ip";
const TAG_SSH_PORT: &str = "tritoncli.ssh.port";
const TAG_SSH_PROXY: &str = "tritoncli.ssh.proxy";
const TAG_SSH_PROXY_USER: &str = "tritoncli.ssh.proxyuser";

/// Tag for default SSH user on images
const TAG_DEFAULT_USER: &str = "default_user";

#[derive(Args, Clone)]
pub struct SshArgs {
    /// Instance ID or name
    pub instance: String,

    /// SSH user (default: auto-detect from image, or root)
    #[arg(long, short = 'l')]
    pub user: Option<String>,

    /// SSH identity file
    #[arg(long, short = 'i')]
    pub identity: Option<String>,

    /// Additional SSH options
    #[arg(long, short = 'o')]
    pub ssh_option: Option<Vec<String>>,

    /// Disable SSH proxy support (ignore tritoncli.ssh.proxy tag)
    #[arg(long)]
    pub no_proxy: bool,

    /// Don't disable SSH ControlMaster (mux). By default, SSH connection
    /// multiplexing is disabled due to known issues with stdout/stderr.
    #[arg(long)]
    pub no_disable_mux: bool,

    /// Command to run on instance
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

/// SSH connection configuration resolved from instance and image
struct SshConfig {
    /// Target IP address
    ip: String,
    /// Target SSH port
    port: u16,
    /// SSH user
    user: String,
    /// Optional proxy configuration
    proxy: Option<ProxyConfig>,
}

/// Proxy/bastion host configuration
struct ProxyConfig {
    /// Proxy IP address
    ip: String,
    /// Proxy SSH user
    user: String,
}

pub async fn run(args: SshArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Get instance details
    let response = client
        .inner()
        .get_machine()
        .account(account)
        .machine(machine_id)
        .send()
        .await
        .context("Failed to get instance")?;

    let machine = response.into_inner();

    // Resolve SSH configuration
    let config = resolve_ssh_config(&args, &machine, client).await?;

    // Build and execute SSH command
    execute_ssh(&args, &config)
}

/// Resolve full SSH configuration from instance tags, image tags, and command args
async fn resolve_ssh_config(
    args: &SshArgs,
    machine: &Machine,
    client: &TypedClient,
) -> Result<SshConfig> {
    // Determine target IP - check tritoncli.ssh.ip tag first, then primary_ip
    let ip = if let Some(tag_ip) = get_tag_string(&machine.tags, TAG_SSH_IP) {
        // Validate the tag IP is in the instance's IP list
        if machine.ips.contains(&tag_ip) {
            tag_ip
        } else {
            // Tag IP not valid, fall back to primary
            machine
                .primary_ip
                .clone()
                .ok_or_else(|| anyhow::anyhow!("Instance has no primary IP"))?
        }
    } else {
        machine
            .primary_ip
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Instance has no primary IP"))?
    };

    // Determine SSH port - check tritoncli.ssh.port tag
    let port = get_tag_string(&machine.tags, TAG_SSH_PORT)
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(22);

    // Determine SSH user - priority: command-line > image default_user tag > root
    let user = if let Some(ref user) = args.user {
        user.clone()
    } else {
        // Try to get default_user from image
        fetch_image_default_user(machine.image, client).await
    };

    // Determine proxy configuration (unless disabled)
    let proxy = if args.no_proxy {
        None
    } else {
        resolve_proxy_config(machine, client).await?
    };

    Ok(SshConfig {
        ip,
        port,
        user,
        proxy,
    })
}

/// Resolve SSH proxy configuration from instance tags
async fn resolve_proxy_config(
    machine: &Machine,
    client: &TypedClient,
) -> Result<Option<ProxyConfig>> {
    // Check if instance has tritoncli.ssh.proxy tag
    let proxy_ref = match get_tag_string(&machine.tags, TAG_SSH_PROXY) {
        Some(p) => p,
        None => return Ok(None),
    };

    let account = &client.auth_config().account;

    // Look up the proxy instance
    let proxy_id = super::get::resolve_instance(&proxy_ref, client)
        .await
        .with_context(|| format!("Failed to resolve SSH proxy instance '{}'", proxy_ref))?;

    let proxy_response = client
        .inner()
        .get_machine()
        .account(account)
        .machine(proxy_id)
        .send()
        .await
        .with_context(|| format!("Failed to get SSH proxy instance '{}'", proxy_ref))?;

    let proxy_machine = proxy_response.into_inner();

    // Get proxy IP - check tritoncli.ssh.ip tag first, then primary_ip
    let proxy_ip = if let Some(tag_ip) = get_tag_string(&proxy_machine.tags, TAG_SSH_IP) {
        if proxy_machine.ips.contains(&tag_ip) {
            tag_ip
        } else {
            proxy_machine
                .primary_ip
                .ok_or_else(|| anyhow::anyhow!("Proxy instance has no primary IP"))?
        }
    } else {
        proxy_machine
            .primary_ip
            .ok_or_else(|| anyhow::anyhow!("Proxy instance has no primary IP"))?
    };

    // Get proxy user - check tritoncli.ssh.proxyuser tag on target, then proxy's image default_user
    let proxy_user = if let Some(proxy_user) = get_tag_string(&machine.tags, TAG_SSH_PROXY_USER) {
        proxy_user
    } else {
        // Try to get default_user from proxy's image
        fetch_image_default_user(proxy_machine.image, client).await
    };

    Ok(Some(ProxyConfig {
        ip: proxy_ip,
        user: proxy_user,
    }))
}

/// Fetch an image by UUID and get its default user
async fn fetch_image_default_user(image_id: uuid::Uuid, client: &TypedClient) -> String {
    let account = &client.auth_config().account;

    // Try to fetch the image
    let image_result = client
        .inner()
        .get_image()
        .account(account)
        .dataset(image_id)
        .send()
        .await;

    match image_result {
        Ok(response) => {
            let image = response.into_inner();
            // Check the image's tags for default_user
            if let Some(ref tags) = image.tags
                && let Some(user) = get_tag_string(tags, TAG_DEFAULT_USER)
            {
                return user;
            }
            "root".to_string()
        }
        Err(_) => {
            // If we can't fetch the image, just default to root
            "root".to_string()
        }
    }
}

/// Extract a string value from tags (uses serde_json::Map from generated types)
fn get_tag_string(tags: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    tags.get(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        // Also handle if someone stored it as a number
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Build and execute the SSH command
fn execute_ssh(args: &SshArgs, config: &SshConfig) -> Result<()> {
    let mut ssh_cmd = Command::new("ssh");

    // By default, disable ControlMaster (mux) to work around stdout/stderr issues
    // See: https://github.com/TritonDataCenter/node-triton/issues/52
    if !args.no_disable_mux {
        // We need both options to effectively disable mux:
        // - ControlMaster=no prevents new mux sessions
        // - ControlPath to non-existent file prevents using existing mux
        let null_control_path = if cfg!(windows) {
            "NUL".to_string()
        } else {
            // Use a path that should never exist
            format!(
                "{}/.triton/tmp/nullSshControlPath",
                std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
            )
        };
        ssh_cmd
            .arg("-o")
            .arg("ControlMaster=no")
            .arg("-o")
            .arg(format!("ControlPath={}", null_control_path));
    }

    // Add ProxyJump if we have a proxy configured
    if let Some(ref proxy) = config.proxy {
        ssh_cmd
            .arg("-o")
            .arg(format!("ProxyJump={}@{}", proxy.user, proxy.ip));
    }

    // Add port if non-standard
    if config.port != 22 {
        ssh_cmd.arg("-p").arg(config.port.to_string());
    }

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

    // Add user@host
    ssh_cmd.arg(format!("{}@{}", config.user, config.ip));

    // Add remote command if specified
    if !args.command.is_empty() {
        ssh_cmd.args(&args.command);
    }

    // Execute SSH
    let status = ssh_cmd.status().context("Failed to execute ssh")?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}
