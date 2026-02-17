// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Volume management commands

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;

use cloudapi_client::types::VolumeState;

use crate::output::enum_to_display;
use crate::output::table::{TableBuilder, TableFormatArgs};
use crate::output::{self, json};

#[derive(Args, Clone)]
pub struct VolumeListArgs {
    /// Filter by name
    #[arg(long)]
    pub name: Option<String>,

    /// Filter by state (creating, ready, failed, deleting)
    #[arg(long)]
    pub state: Option<VolumeState>,

    /// Filter by size in MiB
    #[arg(long)]
    pub size: Option<u64>,

    /// Filter by type (e.g., tritonnfs)
    #[arg(long = "type")]
    pub volume_type: Option<String>,

    #[command(flatten)]
    pub table: TableFormatArgs,

    /// Filters in key=value format (e.g., name=mydata, state=ready)
    ///
    /// Supported filter keys: name, size, state, type
    #[arg(trailing_var_arg = true)]
    pub filters: Vec<String>,
}

#[derive(Args, Clone)]
pub struct VolumeSizesArgs {
    #[command(flatten)]
    pub table: TableFormatArgs,
}

#[derive(Subcommand, Clone)]
pub enum VolumeCommand {
    /// List volumes
    #[command(visible_alias = "ls")]
    List(VolumeListArgs),
    /// Get volume details
    Get(VolumeGetArgs),
    /// Create volume
    Create(VolumeCreateArgs),
    /// Delete volume(s)
    #[command(visible_alias = "rm")]
    Delete(VolumeDeleteArgs),
    /// List available volume sizes
    Sizes(VolumeSizesArgs),
}

#[derive(Args, Clone)]
pub struct VolumeGetArgs {
    /// Volume ID or name
    pub volume: String,
}

#[derive(Args, Clone)]
pub struct VolumeCreateArgs {
    /// Volume name (optional, generated server-side if not provided)
    #[arg(long, short = 'n')]
    pub name: Option<String>,

    /// Volume size in gibibytes (e.g., "20G") or megabytes (e.g., 10240)
    #[arg(long, short = 's')]
    pub size: Option<String>,

    /// Volume type (default: tritonnfs)
    #[arg(long, short = 't', default_value = "tritonnfs")]
    pub r#type: String,

    /// Network ID, name, or short ID (uses default fabric network if not specified)
    #[arg(long, short = 'N')]
    pub network: Option<String>,

    /// Tags in key=value format (can be specified multiple times)
    #[arg(long = "tag")]
    pub tags: Option<Vec<String>>,

    /// Affinity rules for server selection (can be specified multiple times)
    #[arg(long, short = 'a')]
    pub affinity: Option<Vec<String>>,

    /// Wait for creation to complete (use multiple times for spinner)
    #[arg(long, short = 'w', action = clap::ArgAction::Count)]
    pub wait: u8,

    /// Timeout in seconds when waiting
    #[arg(long = "wait-timeout")]
    pub wait_timeout: Option<u64>,
}

#[derive(Args, Clone)]
pub struct VolumeDeleteArgs {
    /// Volume ID(s) or name(s)
    pub volumes: Vec<String>,
    /// Skip confirmation
    #[arg(long, short)]
    pub force: bool,
    /// Wait for deletion
    #[arg(long, short)]
    pub wait: bool,
    /// Wait timeout in seconds
    #[arg(long, default_value = "300")]
    pub wait_timeout: u64,
}

impl VolumeCommand {
    /// Returns true if this is a variadic command with no arguments (a no-op).
    pub fn is_empty_variadic(&self) -> bool {
        match self {
            Self::Delete(args) => args.volumes.is_empty(),
            _ => false,
        }
    }

    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_volumes(args, client, use_json).await,
            Self::Get(args) => get_volume(args, client, use_json).await,
            Self::Create(args) => create_volume(args, client, use_json).await,
            Self::Delete(args) => delete_volumes(args, client).await,
            Self::Sizes(args) => list_volume_sizes(args, client, use_json).await,
        }
    }
}

/// Valid filter keys for positional key=value arguments
const VALID_FILTERS: &[&str] = &["name", "size", "state", "type"];

/// Check if a filter key is valid
fn is_valid_filter(key: &str) -> bool {
    VALID_FILTERS.contains(&key)
}

/// Deserialize a serde enum from its wire-format string value.
fn parse_serde_enum<T: serde::de::DeserializeOwned>(
    value: &str,
) -> std::result::Result<T, serde_json::Error> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
}

/// Apply positional key=value filters to the VolumeListArgs, merging with any
/// existing --flag values. Positional filters override flags if both are set.
fn apply_positional_filters(args: &mut VolumeListArgs) -> Result<()> {
    for filter in std::mem::take(&mut args.filters) {
        let (key, value) = filter
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid filter '{}': must be key=value", filter))?;

        if !is_valid_filter(key) {
            anyhow::bail!(
                "Unknown filter '{}'. Valid filters: {}",
                key,
                VALID_FILTERS.join(", ")
            );
        }

        match key {
            "name" => args.name = Some(value.to_string()),
            "state" => {
                args.state = Some(parse_serde_enum(value).map_err(|_| {
                    anyhow::anyhow!(
                        "Invalid state value '{}': expected creating, ready, failed, deleting",
                        value
                    )
                })?);
            }
            "size" => {
                args.size = Some(value.parse().map_err(|_| {
                    anyhow::anyhow!("Invalid size value '{}': expected a number in MiB", value)
                })?);
            }
            "type" => args.volume_type = Some(value.to_string()),
            _ => unreachable!(),
        }
    }
    Ok(())
}

async fn list_volumes(
    mut args: VolumeListArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    apply_positional_filters(&mut args)?;
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_volumes()
        .account(account)
        .send()
        .await?;

    let all_volumes = response.into_inner();

    // Apply client-side filters
    let volumes: Vec<_> = all_volumes
        .into_iter()
        .filter(|vol| {
            if let Some(ref name) = args.name
                && vol.name != *name
            {
                return false;
            }
            if let Some(state) = args.state
                && vol.state != state
            {
                return false;
            }
            if let Some(size) = args.size
                && vol.size != size
            {
                return false;
            }
            if let Some(ref vtype) = args.volume_type
                && vol.type_ != *vtype
            {
                return false;
            }
            true
        })
        .collect();

    if use_json {
        json::print_json_stream(&volumes)?;
    } else {
        // node-triton column order: SHORTID, NAME, SIZE, TYPE, STATE, AGE
        let mut tbl = TableBuilder::new(&["SHORTID", "NAME", "SIZE", "TYPE", "STATE", "AGE"])
            .with_long_headers(&["ID", "CREATED"]);
        for vol in &volumes {
            tbl.add_row(vec![
                vol.id.to_string()[..8].to_string(),
                vol.name.clone(),
                format!("{} MiB", vol.size),
                vol.type_.clone(),
                enum_to_display(&vol.state),
                output::format_age(&vol.created.to_string()),
                vol.id.to_string(),
                vol.created.to_string(),
            ]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

async fn get_volume(args: VolumeGetArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;
    let volume_id = resolve_volume(&args.volume, client).await?;

    let response = client
        .inner()
        .get_volume()
        .account(account)
        .id(volume_id)
        .send()
        .await?;

    let volume = response.into_inner();

    if use_json {
        json::print_json(&volume)?;
    } else {
        json::print_json_pretty(&volume)?;
    }

    Ok(())
}

/// Parse volume size from string, supporting GiB format ("20G") or plain MB
///
/// Valid formats:
/// - "42G" or "42g" - 42 gibibytes (converted to MiB)
/// - "1024" - 1024 mebibytes
///
/// Invalid formats (will return error):
/// - "foo" - non-numeric
/// - "0" or "0G" - zero size
/// - "-42" or "-42G" - negative size
/// - "042" or "042G" - leading zeros (octal-like)
/// - "42Gasdf" - trailing garbage after suffix
fn parse_volume_size(size_str: &str) -> Result<u64> {
    // Empty string is invalid
    if size_str.is_empty() {
        return Err(anyhow::anyhow!("Invalid size format: empty string"));
    }

    // Check for GiB format (e.g., "20G")
    if let Some(gib_str) = size_str
        .strip_suffix('G')
        .or_else(|| size_str.strip_suffix('g'))
    {
        // Check for leading zeros (octal-like, e.g., "042G")
        if gib_str.len() > 1 && gib_str.starts_with('0') {
            return Err(anyhow::anyhow!(
                "Invalid size format: leading zeros not allowed: {}",
                size_str
            ));
        }

        let gib: u64 = gib_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid size format: {}", size_str))?;

        if gib == 0 {
            return Err(anyhow::anyhow!("Size must be greater than 0"));
        }

        // 1 GiB = 1024 MiB
        Ok(gib * 1024)
    } else {
        // Check for leading zeros (octal-like, e.g., "042")
        if size_str.len() > 1 && size_str.starts_with('0') {
            return Err(anyhow::anyhow!(
                "Invalid size format: leading zeros not allowed: {}",
                size_str
            ));
        }

        // Plain MB format
        let mib: u64 = size_str
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid size format: {}", size_str))?;

        if mib == 0 {
            return Err(anyhow::anyhow!("Size must be greater than 0"));
        }

        Ok(mib)
    }
}

/// Parse tags from key=value format into a serde_json Map
fn parse_tags(tag_list: &[String]) -> Result<serde_json::Map<String, serde_json::Value>> {
    let mut tags = serde_json::Map::new();
    for tag in tag_list {
        if let Some((key, value)) = tag.split_once('=') {
            // Try to parse as bool or number, otherwise use string
            let json_value = if value == "true" {
                serde_json::Value::Bool(true)
            } else if value == "false" {
                serde_json::Value::Bool(false)
            } else if let Ok(num) = value.parse::<i64>() {
                serde_json::Value::Number(num.into())
            } else if let Ok(num) = value.parse::<f64>() {
                serde_json::json!(num)
            } else {
                serde_json::Value::String(value.to_string())
            };
            tags.insert(key.to_string(), json_value);
        } else {
            return Err(anyhow::anyhow!(
                "Invalid tag format '{}', expected key=value",
                tag
            ));
        }
    }
    Ok(tags)
}

async fn create_volume(args: VolumeCreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let account = &client.auth_config().account;

    // Warn about affinity if specified (not currently supported by API)
    if args.affinity.is_some() {
        eprintln!("Warning: --affinity option is not currently supported by the API");
    }

    // Parse size
    let size = if let Some(size_str) = &args.size {
        parse_volume_size(size_str)?
    } else {
        // Use smallest available size (default behavior per node-triton)
        let sizes_response = client
            .inner()
            .list_volume_sizes()
            .account(account)
            .send()
            .await?;
        let sizes = sizes_response.into_inner();
        sizes.iter().map(|s| s.size).min().unwrap_or(10240) // Fallback: 10 GiB in MiB
    };

    // Handle network - resolve name/shortid to UUID if provided
    let networks = if let Some(net) = &args.network {
        let network_id = crate::commands::network::resolve_network(net, client).await?;
        Some(vec![network_id])
    } else {
        None
    };

    // Parse tags
    let tags = args.tags.as_ref().map(|t| parse_tags(t)).transpose()?;

    let request = cloudapi_client::types::CreateVolumeRequest {
        name: args.name.clone(),
        type_: Some(args.r#type.clone()),
        size,
        networks,
        tags,
    };

    let response = client
        .inner()
        .create_volume()
        .account(account)
        .body(request)
        .send()
        .await?;
    let volume = response.into_inner();

    let should_wait = args.wait > 0;
    let wait_timeout = args.wait_timeout.unwrap_or(300); // Default 5 minutes

    if should_wait {
        println!(
            "Creating volume {} ({})...",
            volume.name,
            &volume.id.to_string()[..8]
        );

        let final_volume =
            wait_for_volume_ready(&volume.id.to_string(), client, wait_timeout).await?;

        if use_json {
            json::print_json(&final_volume)?;
        } else if final_volume.state == VolumeState::Ready {
            println!(
                "Created volume {} ({}) - {} MiB",
                final_volume.name,
                &final_volume.id.to_string()[..8],
                final_volume.size
            );
        } else {
            return Err(anyhow::anyhow!(
                "Failed to create volume {} ({})",
                final_volume.name,
                final_volume.id
            ));
        }
    } else {
        println!(
            "Creating volume {} ({}) - {} MiB",
            volume.name,
            &volume.id.to_string()[..8],
            volume.size
        );

        if use_json {
            json::print_json(&volume)?;
        }
    }

    Ok(())
}

async fn wait_for_volume_ready(
    volume_id: &str,
    client: &TypedClient,
    timeout_secs: u64,
) -> Result<cloudapi_client::types::Volume> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let response = client
            .inner()
            .get_volume()
            .account(account)
            .id(volume_id)
            .send()
            .await?;

        let volume = response.into_inner();

        if volume.state == VolumeState::Ready || volume.state == VolumeState::Failed {
            return Ok(volume);
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for volume to become ready"
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn delete_volumes(args: VolumeDeleteArgs, client: &TypedClient) -> Result<()> {
    let account = &client.auth_config().account;

    for volume_name in &args.volumes {
        let volume_id = resolve_volume(volume_name, client).await?;

        if !args.force {
            use dialoguer::Confirm;
            if !Confirm::new()
                .with_prompt(format!("Delete volume '{}'?", volume_name))
                .default(false)
                .interact()?
            {
                continue;
            }
        }

        client
            .inner()
            .delete_volume()
            .account(account)
            .id(volume_id)
            .send()
            .await?;

        println!("Deleting volume {}", volume_name);

        if args.wait {
            let volume_id_str = volume_id.to_string();
            wait_for_volume_deletion(&volume_id_str, client, args.wait_timeout).await?;
            println!("Volume {} deleted", volume_name);
        }
    }

    Ok(())
}

async fn list_volume_sizes(
    args: VolumeSizesArgs,
    client: &TypedClient,
    use_json: bool,
) -> Result<()> {
    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_volume_sizes()
        .account(account)
        .send()
        .await?;

    let sizes = response.into_inner();

    if use_json {
        json::print_json(&sizes)?;
    } else {
        let mut tbl = TableBuilder::new(&["SIZE"]);
        for size in &sizes {
            tbl.add_row(vec![format!("{} MiB", size.size)]);
        }
        tbl.print(&args.table);
    }

    Ok(())
}

/// Resolve volume name or short ID to full UUID
pub async fn resolve_volume(id_or_name: &str, client: &TypedClient) -> Result<uuid::Uuid> {
    if let Ok(uuid) = uuid::Uuid::parse_str(id_or_name) {
        return Ok(uuid);
    }

    let account = &client.auth_config().account;
    let response = client
        .inner()
        .list_volumes()
        .account(account)
        .send()
        .await?;

    let volumes = response.into_inner();

    // Try short ID match first (at least 8 characters)
    if id_or_name.len() >= 8 {
        for vol in &volumes {
            if vol.id.to_string().starts_with(id_or_name) {
                return Ok(vol.id);
            }
        }
    }

    // Try exact name match
    for vol in &volumes {
        if vol.name == id_or_name {
            return Ok(vol.id);
        }
    }

    Err(anyhow::anyhow!("Volume not found: {}", id_or_name))
}

async fn wait_for_volume_deletion(
    volume_id: &str,
    client: &TypedClient,
    timeout_secs: u64,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = &client.auth_config().account;
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let result = client
            .inner()
            .get_volume()
            .account(account)
            .id(volume_id)
            .send()
            .await;

        match result {
            Ok(response) => {
                let volume = response.into_inner();
                if volume.state == VolumeState::Failed {
                    return Err(anyhow::anyhow!("Volume deletion failed"));
                }
            }
            Err(_) => {
                // Volume not found means it's deleted
                return Ok(());
            }
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for volume deletion"));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== parse_volume_size tests =====
    // Ported from node-triton test/unit/parseVolumeSize.test.js

    #[test]
    fn test_parse_volume_size_gib() {
        // "42G" should equal 42 * 1024 = 43008 MiB
        assert_eq!(parse_volume_size("42G").unwrap(), 42 * 1024);
    }

    #[test]
    fn test_parse_volume_size_gib_100() {
        assert_eq!(parse_volume_size("100G").unwrap(), 100 * 1024);
    }

    #[test]
    fn test_parse_volume_size_plain_mib() {
        // Plain number interpreted as MB
        assert_eq!(parse_volume_size("1024").unwrap(), 1024);
    }

    // Invalid sizes - should all return errors

    #[test]
    fn test_parse_volume_size_invalid_foo() {
        assert!(parse_volume_size("foo").is_err());
    }

    #[test]
    fn test_parse_volume_size_invalid_zero_g() {
        let result = parse_volume_size("0G");
        // 0G is technically parseable but should be rejected for size being 0
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_volume_size_invalid_empty() {
        assert!(parse_volume_size("").is_err());
    }

    #[test]
    fn test_parse_volume_size_invalid_mixed() {
        assert!(parse_volume_size("42Gasdf").is_err());
        assert!(parse_volume_size("42gasdf").is_err());
        assert!(parse_volume_size("42asdf").is_err());
    }

    #[test]
    fn test_parse_volume_size_invalid_prefix() {
        assert!(parse_volume_size("asdf42G").is_err());
        assert!(parse_volume_size("asdf42g").is_err());
        assert!(parse_volume_size("asdf42").is_err());
    }

    #[test]
    fn test_parse_volume_size_invalid_leading_zero() {
        // Leading zeros should be rejected (octal interpretation issue)
        assert!(parse_volume_size("042g").is_err());
        assert!(parse_volume_size("042G").is_err());
        assert!(parse_volume_size("042").is_err());
    }

    // ===== parse_tags tests =====

    #[test]
    fn test_parse_tags_simple() {
        let tags = vec!["foo=bar".to_string()];
        let result = parse_tags(&tags).unwrap();
        assert_eq!(result.get("foo").unwrap(), "bar");
    }

    #[test]
    fn test_parse_tags_boolean() {
        let tags = vec!["enabled=true".to_string(), "disabled=false".to_string()];
        let result = parse_tags(&tags).unwrap();
        assert_eq!(result.get("enabled").unwrap(), true);
        assert_eq!(result.get("disabled").unwrap(), false);
    }

    #[test]
    fn test_parse_tags_numeric() {
        let tags = vec!["count=42".to_string()];
        let result = parse_tags(&tags).unwrap();
        assert_eq!(result.get("count").unwrap(), 42);
    }

    #[test]
    fn test_parse_tags_float() {
        let tags = vec!["pi=3.14".to_string()];
        let result = parse_tags(&tags).unwrap();
        // Check it's a number (float comparison is tricky)
        assert!(result.get("pi").unwrap().is_f64());
    }

    #[test]
    fn test_parse_tags_string_with_number_like_value() {
        // Values that look like numbers but shouldn't be converted
        let tags = vec!["version=1.0.0".to_string()];
        let result = parse_tags(&tags).unwrap();
        // This should remain a string because it can't be parsed as a number
        assert!(result.get("version").unwrap().is_string());
    }

    #[test]
    fn test_parse_tags_empty() {
        let tags: Vec<String> = vec![];
        let result = parse_tags(&tags).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_tags_rejects_invalid_format() {
        let tags = vec!["valid=tag".to_string(), "invalid-no-equals".to_string()];
        let result = parse_tags(&tags);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid-no-equals"));
    }
}
