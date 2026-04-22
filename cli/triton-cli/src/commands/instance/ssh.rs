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
use cloudapi_api::Machine;
use serde_json::Value;

use crate::client::AnyClient;
use crate::dispatch;

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

/// Parse optional `USER@instance` syntax, returning (user, instance_id).
///
/// If the instance argument contains `@` with non-empty parts on both sides,
/// the portion before `@` is the SSH user and the portion after is the
/// instance name or ID. This matches the Node.js `triton instance ssh`
/// behavior where `triton inst ssh debian@myvm` connects as user `debian`.
fn parse_instance_arg(instance: &str) -> (Option<&str>, &str) {
    if let Some(at_pos) = instance.find('@') {
        let user = &instance[..at_pos];
        let id = &instance[at_pos + 1..];
        if !user.is_empty() && !id.is_empty() {
            return (Some(user), id);
        }
    }
    (None, instance)
}

/// Fetch a machine in canonical `cloudapi_api::Machine` form so the SSH
/// config resolution below doesn't depend on which client was used.
async fn fetch_machine(
    client: &AnyClient,
    account: &str,
    machine_id: uuid::Uuid,
) -> Result<Machine> {
    let machine: Machine = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();
        serde_json::from_value::<Machine>(serde_json::to_value(&resp)?)?
    });
    Ok(machine)
}

pub async fn run(
    mut args: SshArgs,
    client: &AnyClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<()> {
    // Support USER@instance syntax (e.g. "debian@myvm"). The user portion
    // overrides --user/-l and image default_user, matching Node.js behavior.
    let (at_user, instance_id) = parse_instance_arg(&args.instance);
    if let Some(user) = at_user {
        args.user = Some(user.to_string());
    }
    let machine_id = super::get::resolve_instance(instance_id, client).await?;
    let account = client.effective_account();

    // Get instance details
    let machine = fetch_machine(client, account, machine_id)
        .await
        .context("Failed to get instance")?;

    // Resolve SSH configuration
    let config = resolve_ssh_config(&args, &machine, client, cache).await?;

    // Build and execute SSH command
    execute_ssh(&args, &config)
}

/// Resolve full SSH configuration from instance tags, image tags, and command args
async fn resolve_ssh_config(
    args: &SshArgs,
    machine: &Machine,
    client: &AnyClient,
    cache: Option<&crate::cache::ImageCache>,
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
        fetch_image_default_user(machine.image, client, cache).await
    };

    // Determine proxy configuration (unless disabled)
    let proxy = if args.no_proxy {
        None
    } else {
        resolve_proxy_config(machine, client, cache).await?
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
    client: &AnyClient,
    cache: Option<&crate::cache::ImageCache>,
) -> Result<Option<ProxyConfig>> {
    // Check if instance has tritoncli.ssh.proxy tag
    let proxy_ref = match get_tag_string(&machine.tags, TAG_SSH_PROXY) {
        Some(p) => p,
        None => return Ok(None),
    };

    let account = client.effective_account();

    // Look up the proxy instance
    let proxy_id = super::get::resolve_instance(&proxy_ref, client)
        .await
        .with_context(|| format!("Failed to resolve SSH proxy instance '{}'", proxy_ref))?;

    let proxy_machine = fetch_machine(client, account, proxy_id)
        .await
        .with_context(|| format!("Failed to get SSH proxy instance '{}'", proxy_ref))?;

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
        fetch_image_default_user(proxy_machine.image, client, cache).await
    };

    Ok(Some(ProxyConfig {
        ip: proxy_ip,
        user: proxy_user,
    }))
}

/// Fetch an image by UUID and get its default user
async fn fetch_image_default_user(
    image_id: uuid::Uuid,
    client: &AnyClient,
    cache: Option<&crate::cache::ImageCache>,
) -> String {
    // Try cache first (uses longer GET_TTL)
    let cached_image = match cache {
        Some(c) => c.get_image(image_id).await,
        None => None,
    };
    if let Some(image) = cached_image {
        if let Some(ref tags) = image.tags
            && let Some(user) = get_tag_string(tags, TAG_DEFAULT_USER)
        {
            return user;
        }
        return "root".to_string();
    }

    let account = client.effective_account();

    // Cache miss — fetch from API, convert to a tags map.
    let tags_map: Option<serde_json::Map<String, Value>> = dispatch!(client, |c| {
        match c
            .inner()
            .get_image()
            .account(account)
            .dataset(image_id)
            .send()
            .await
        {
            Ok(resp) => {
                let value = serde_json::to_value(&resp.into_inner()).ok();
                value
                    .and_then(|v| v.get("tags").cloned())
                    .and_then(|t| serde_json::from_value(t).ok())
            }
            Err(e) => {
                tracing::debug!("Failed to fetch image for default user, using root: {}", e);
                None
            }
        }
    });

    if let Some(tags) = tags_map
        && let Some(user) = get_tag_string(&tags, TAG_DEFAULT_USER)
    {
        return user;
    }
    "root".to_string()
}

/// Extract a string value from tags.
///
/// The canonical `cloudapi_api::Machine.tags` aliases
/// `vmapi_api::Tags = HashMap<String, Value>`, whereas cache-loaded
/// `Image.tags` use `serde_json::Map<String, Value>`. We use a trait
/// bound so both key/value map types work.
fn get_tag_string<M>(tags: &M, key: &str) -> Option<String>
where
    M: TagsMap,
{
    tags.get_tag(key).and_then(|v| match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

trait TagsMap {
    fn get_tag(&self, key: &str) -> Option<&Value>;
}

impl TagsMap for std::collections::HashMap<String, Value> {
    fn get_tag(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
}

impl TagsMap for serde_json::Map<String, Value> {
    fn get_tag(&self, key: &str) -> Option<&Value> {
        self.get(key)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_instance_arg_with_user() {
        let (user, id) = parse_instance_arg("debian@migrate-test");
        assert_eq!(user, Some("debian"));
        assert_eq!(id, "migrate-test");
    }

    #[test]
    fn test_parse_instance_arg_plain_name() {
        let (user, id) = parse_instance_arg("migrate-test");
        assert_eq!(user, None);
        assert_eq!(id, "migrate-test");
    }

    #[test]
    fn test_parse_instance_arg_uuid() {
        let (user, id) = parse_instance_arg("28faa36c-2031-4632-a819-f7defa1299a3");
        assert_eq!(user, None);
        assert_eq!(id, "28faa36c-2031-4632-a819-f7defa1299a3");
    }

    #[test]
    fn test_parse_instance_arg_user_with_uuid() {
        let (user, id) = parse_instance_arg("root@28faa36c-2031-4632-a819-f7defa1299a3");
        assert_eq!(user, Some("root"));
        assert_eq!(id, "28faa36c-2031-4632-a819-f7defa1299a3");
    }

    #[test]
    fn test_parse_instance_arg_empty_user() {
        // "@instance" should not extract an empty user
        let (user, id) = parse_instance_arg("@instance");
        assert_eq!(user, None);
        assert_eq!(id, "@instance");
    }

    #[test]
    fn test_parse_instance_arg_empty_instance() {
        // "user@" should not extract an empty instance
        let (user, id) = parse_instance_arg("user@");
        assert_eq!(user, None);
        assert_eq!(id, "user@");
    }
}
