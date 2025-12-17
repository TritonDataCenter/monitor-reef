// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Instance tag subcommands

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::TypedClient;
use cloudapi_client::types::TagsRequest;
use serde_json::{Map, Value};

#[derive(Subcommand, Clone)]
pub enum TagCommand {
    /// List tags on an instance
    #[command(alias = "ls")]
    List(TagListArgs),

    /// Get a tag value
    Get(TagGetArgs),

    /// Set tag(s) on an instance
    Set(TagSetArgs),

    /// Delete tag(s) from an instance
    #[command(alias = "rm")]
    Delete(TagDeleteArgs),

    /// Replace all tags on an instance
    #[command(name = "replace-all")]
    ReplaceAll(TagReplaceAllArgs),
}

#[derive(Args, Clone)]
pub struct TagListArgs {
    /// Instance ID or name
    pub instance: String,
}

#[derive(Args, Clone)]
pub struct TagGetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tag key
    pub key: String,

    /// JSON output (quoted string/value)
    #[arg(short = 'j', long = "json")]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct TagSetArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tags to set (key=value, multiple allowed)
    #[arg(required_unless_present = "files")]
    pub tags: Vec<String>,

    /// Read tags from file (JSON object or key=value pairs). Can be used multiple times.
    #[arg(short = 'f', long = "file", action = clap::ArgAction::Append)]
    pub files: Option<Vec<PathBuf>>,

    /// Wait for tag update to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "120")]
    pub wait_timeout: u64,

    /// Suppress output after setting tags
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// JSON output (compact, single line)
    #[arg(short = 'j', long = "json")]
    pub json: bool,
}

#[derive(Args, Clone)]
pub struct TagDeleteArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tag key(s) to delete
    pub keys: Vec<String>,

    /// Delete all tags on the instance
    #[arg(short = 'a', long = "all")]
    pub all: bool,

    /// Wait for tag update to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "120")]
    pub wait_timeout: u64,
}

#[derive(Args, Clone)]
pub struct TagReplaceAllArgs {
    /// Instance ID or name
    pub instance: String,

    /// Tags to set (key=value, multiple allowed)
    #[arg(required_unless_present = "files")]
    pub tags: Vec<String>,

    /// Read tags from file (JSON object or key=value pairs). Can be used multiple times.
    #[arg(short = 'f', long = "file", action = clap::ArgAction::Append)]
    pub files: Option<Vec<PathBuf>>,

    /// Wait for tag update to complete
    #[arg(long, short)]
    pub wait: bool,

    /// Wait timeout in seconds
    #[arg(long, default_value = "120")]
    pub wait_timeout: u64,

    /// Suppress output after replacing tags
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// JSON output (compact, single line)
    #[arg(short = 'j', long = "json")]
    pub json: bool,
}

impl TagCommand {
    pub async fn run(self, client: &TypedClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_tags(args, client, use_json).await,
            Self::Get(args) => get_tag(args, client).await,
            Self::Set(args) => set_tags(args, client).await,
            Self::Delete(args) => delete_tag(args, client).await,
            Self::ReplaceAll(args) => replace_all_tags(args, client).await,
        }
    }
}

/// List tags on an instance
///
/// Output format matches node-triton:
/// - Without -j: pretty-printed JSON
/// - With -j: compact JSON
pub async fn list_tags(args: TagListArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .list_machine_tags()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let tags = response.into_inner();

    // node-triton always outputs JSON for tag list
    // -j means compact JSON, otherwise pretty-print
    if use_json {
        println!("{}", serde_json::to_string(&tags)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&tags)?);
    }

    Ok(())
}

/// Get a single tag value
///
/// Output format matches node-triton:
/// - Without -j: plain value (string representation)
/// - With -j: JSON-encoded value (e.g., "bar" for string, true for bool)
async fn get_tag(args: TagGetArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    let response = client
        .inner()
        .get_machine_tag()
        .account(account)
        .machine(&machine_id)
        .tag(&args.key)
        .send()
        .await?;

    // The API returns a string, but for tags it could be a typed value
    // We need to get all tags to know the actual type
    let tags_response = client
        .inner()
        .list_machine_tags()
        .account(account)
        .machine(&machine_id)
        .send()
        .await?;

    let tags = tags_response.into_inner();
    let value = tags.get(&args.key).cloned().unwrap_or_else(|| {
        // Fallback to the direct response if not found in tags
        Value::String(response.into_inner())
    });

    if args.json {
        // Output as JSON (e.g., "bar" for strings, true for bools)
        println!("{}", serde_json::to_string(&value)?);
    } else {
        // Output plain value
        match &value {
            Value::String(s) => println!("{}", s),
            Value::Bool(b) => println!("{}", b),
            Value::Number(n) => println!("{}", n),
            _ => println!("{}", value),
        }
    }

    Ok(())
}

/// Parse a tag value string into the appropriate JSON type.
/// Matches node-triton behavior: "true"/"false" -> bool, numeric strings -> number
fn parse_tag_value(value: &str) -> Value {
    let trimmed = value.trim();
    if trimmed == "true" {
        Value::Bool(true)
    } else if trimmed == "false" {
        Value::Bool(false)
    } else if let Ok(num) = trimmed.parse::<f64>() {
        // Use Number type for numeric values
        if let Some(n) = serde_json::Number::from_f64(num) {
            Value::Number(n)
        } else {
            Value::String(value.to_string())
        }
    } else {
        Value::String(value.to_string())
    }
}

/// Load tags from a file (JSON object or key=value pairs)
fn load_tags_from_file(file_path: &std::path::Path) -> Result<Map<String, Value>> {
    let content = if file_path.as_os_str() == "-" {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        std::fs::read_to_string(file_path)?
    };

    let trimmed = content.trim();

    // If content starts with '{', parse as JSON object
    if trimmed.starts_with('{') {
        let obj: Map<String, Value> = serde_json::from_str(trimmed)?;
        Ok(obj)
    } else {
        // Parse as key=value pairs (one per line)
        let mut map: Map<String, Value> = Map::new();
        for line in trimmed.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                map.insert(key.trim().to_string(), parse_tag_value(value));
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid tag format '{}', expected key=value",
                    line
                ));
            }
        }
        Ok(map)
    }
}

/// Set tags and output the resulting tags as JSON
async fn set_tags(args: TagSetArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Collect tags from files first, then command line args (args win over files)
    let mut tag_map: Map<String, Value> = Map::new();

    // Load from files if provided
    if let Some(files) = &args.files {
        for file_path in files {
            let file_tags = load_tags_from_file(file_path)?;
            for (key, value) in file_tags {
                tag_map.insert(key, value);
            }
        }
    }

    // Parse command line args (overwrite file values)
    for tag in &args.tags {
        if let Some((key, value)) = tag.split_once('=') {
            tag_map.insert(key.to_string(), parse_tag_value(value));
        } else {
            return Err(anyhow::anyhow!(
                "Invalid tag format '{}', expected key=value",
                tag
            ));
        }
    }

    if tag_map.is_empty() {
        return Err(anyhow::anyhow!("No tags specified"));
    }

    let request = TagsRequest::from(tag_map);

    client
        .inner()
        .add_machine_tags()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    if args.wait {
        super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client).await?;
    }

    // Output the updated tags (matching node-triton behavior)
    if !args.quiet {
        // Fetch all tags to show the complete set
        let response = client
            .inner()
            .list_machine_tags()
            .account(account)
            .machine(&machine_id)
            .send()
            .await?;

        let updated_tags = response.into_inner();

        // -j means compact JSON, otherwise pretty-print
        if args.json {
            println!("{}", serde_json::to_string(&updated_tags)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&updated_tags)?);
        }
    }

    Ok(())
}

/// Delete tag(s) from an instance
///
/// Output format matches node-triton:
/// - For each deleted tag: "Deleted tag NAME on instance INST"
/// - For --all: "Deleted all tags on instance INST"
async fn delete_tag(args: TagDeleteArgs, client: &TypedClient) -> Result<()> {
    // Validate args
    if args.all && !args.keys.is_empty() {
        return Err(anyhow::anyhow!("cannot specify both tag names and --all"));
    }
    if !args.all && args.keys.is_empty() {
        return Err(anyhow::anyhow!("must specify tag name(s) or --all"));
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    if args.all {
        // Delete all tags
        client
            .inner()
            .delete_machine_tags()
            .account(account)
            .machine(&machine_id)
            .send()
            .await?;

        if args.wait {
            super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client).await?;
        }

        println!("Deleted all tags on instance {}", args.instance);
    } else {
        // Delete individual tags (de-duplicate keys)
        let mut seen = std::collections::HashSet::new();
        let unique_keys: Vec<_> = args.keys.iter().filter(|k| seen.insert(*k)).collect();

        for key in unique_keys {
            client
                .inner()
                .delete_machine_tag()
                .account(account)
                .machine(&machine_id)
                .tag(key)
                .send()
                .await?;

            if args.wait {
                super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client)
                    .await?;
            }

            println!("Deleted tag {} on instance {}", key, args.instance);
        }
    }

    Ok(())
}

/// Replace all tags on an instance
///
/// Output format matches node-triton: JSON with updated tags
async fn replace_all_tags(args: TagReplaceAllArgs, client: &TypedClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = &client.auth_config().account;

    // Collect tags from files first, then command line args (args win over files)
    let mut tag_map: Map<String, Value> = Map::new();

    // Load from files if provided
    if let Some(files) = &args.files {
        for file_path in files {
            let file_tags = load_tags_from_file(file_path)?;
            for (key, value) in file_tags {
                tag_map.insert(key, value);
            }
        }
    }

    // Parse command line args (overwrite file values)
    for tag in &args.tags {
        if let Some((key, value)) = tag.split_once('=') {
            tag_map.insert(key.to_string(), parse_tag_value(value));
        } else {
            return Err(anyhow::anyhow!(
                "Invalid tag format '{}', expected key=value",
                tag
            ));
        }
    }

    if tag_map.is_empty() {
        return Err(anyhow::anyhow!("no tags were provided"));
    }

    let request = TagsRequest::from(tag_map);

    client
        .inner()
        .replace_machine_tags()
        .account(account)
        .machine(&machine_id)
        .body(request)
        .send()
        .await?;

    if args.wait {
        super::wait::wait_for_state(&machine_id, "running", args.wait_timeout, client).await?;
    }

    // Output the updated tags (matching node-triton behavior)
    if !args.quiet {
        // Fetch all tags to show the complete set
        let response = client
            .inner()
            .list_machine_tags()
            .account(account)
            .machine(&machine_id)
            .send()
            .await?;

        let updated_tags = response.into_inner();

        // -j means compact JSON, otherwise pretty-print
        if args.json {
            println!("{}", serde_json::to_string(&updated_tags)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&updated_tags)?);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse tag key=value string into (key, value) tuple
    fn parse_tag(s: &str) -> Result<(String, String)> {
        if let Some((key, value)) = s.split_once('=') {
            Ok((key.to_string(), value.to_string()))
        } else {
            Err(anyhow::anyhow!(
                "Invalid tag format '{}', expected key=value",
                s
            ))
        }
    }

    /// Parse tags from a list of key=value strings into a Map
    fn parse_tags_from_args(tags: &[String]) -> Result<Map<String, Value>> {
        let mut map: Map<String, Value> = Map::new();
        for tag in tags {
            let (key, value) = parse_tag(tag)?;
            map.insert(key, Value::String(value));
        }
        Ok(map)
    }

    // ===== parse_tag tests =====

    #[test]
    fn test_parse_tag_simple() {
        let (key, value) = parse_tag("foo=bar").unwrap();
        assert_eq!(key, "foo");
        assert_eq!(value, "bar");
    }

    #[test]
    fn test_parse_tag_with_equals_in_value() {
        let (key, value) = parse_tag("equation=a=b").unwrap();
        assert_eq!(key, "equation");
        assert_eq!(value, "a=b");
    }

    #[test]
    fn test_parse_tag_empty_value() {
        let (key, value) = parse_tag("empty=").unwrap();
        assert_eq!(key, "empty");
        assert_eq!(value, "");
    }

    #[test]
    fn test_parse_tag_no_equals() {
        let result = parse_tag("notag");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tag_numeric_value() {
        let (key, value) = parse_tag("count=42").unwrap();
        assert_eq!(key, "count");
        assert_eq!(value, "42");
    }

    #[test]
    fn test_parse_tag_boolean_value() {
        let (key, value) = parse_tag("enabled=true").unwrap();
        assert_eq!(key, "enabled");
        assert_eq!(value, "true");
    }

    // ===== parse_tags_from_args tests =====

    #[test]
    fn test_parse_tags_from_args_single() {
        let tags = vec!["foo=bar".to_string()];
        let map = parse_tags_from_args(&tags).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("foo").unwrap(), "bar");
    }

    #[test]
    fn test_parse_tags_from_args_multiple() {
        let tags = vec![
            "foo=bar".to_string(),
            "baz=qux".to_string(),
            "count=42".to_string(),
        ];
        let map = parse_tags_from_args(&tags).unwrap();
        assert_eq!(map.len(), 3);
        assert_eq!(map.get("foo").unwrap(), "bar");
        assert_eq!(map.get("baz").unwrap(), "qux");
        assert_eq!(map.get("count").unwrap(), "42");
    }

    #[test]
    fn test_parse_tags_from_args_empty() {
        let tags: Vec<String> = vec![];
        let map = parse_tags_from_args(&tags).unwrap();
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn test_parse_tags_from_args_invalid() {
        let tags = vec!["valid=tag".to_string(), "invalid".to_string()];
        let result = parse_tags_from_args(&tags);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tags_from_args_duplicate_key() {
        // Later values should overwrite earlier ones
        let tags = vec!["key=first".to_string(), "key=second".to_string()];
        let map = parse_tags_from_args(&tags).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("key").unwrap(), "second");
    }
}
