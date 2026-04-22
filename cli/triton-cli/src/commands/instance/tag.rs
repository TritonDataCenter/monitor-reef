// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Instance tag subcommands

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use cloudapi_client::types::MachineState;
use serde_json::{Map, Value};

use crate::client::AnyClient;
use crate::dispatch;

#[derive(Subcommand, Clone)]
pub enum TagCommand {
    /// List tags on an instance
    #[command(visible_alias = "ls")]
    List(TagListArgs),

    /// Get a tag value
    Get(TagGetArgs),

    /// Set tag(s) on an instance
    Set(TagSetArgs),

    /// Delete tag(s) from an instance
    #[command(visible_alias = "rm")]
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
    pub async fn run(self, client: &AnyClient, use_json: bool) -> Result<()> {
        match self {
            Self::List(args) => list_tags(args, client, use_json).await,
            Self::Get(args) => get_tag(args, client).await,
            Self::Set(args) => set_tags(args, client).await,
            Self::Delete(args) => delete_tag(args, client).await,
            Self::ReplaceAll(args) => replace_all_tags(args, client).await,
        }
    }
}

/// Fetch the current tags map for `machine_id` as a canonical JSON object
/// (wire representation). The per-client `TagsRequest` type alias is
/// structurally a `HashMap<String, Value>` but the concrete generated type
/// differs, so we round-trip through `serde_json::Value` at the boundary.
async fn fetch_machine_tags(
    client: &AnyClient,
    account: &str,
    machine_id: uuid::Uuid,
) -> Result<HashMap<String, Value>> {
    let tags: HashMap<String, Value> = dispatch!(client, |c| {
        let resp = c
            .inner()
            .list_machine_tags()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();
        let value = serde_json::to_value(&resp)?;
        serde_json::from_value::<HashMap<String, Value>>(value)?
    });
    Ok(tags)
}

/// Fetch the machine state as the canonical `cloudapi_api::MachineState`.
async fn fetch_machine_state(
    client: &AnyClient,
    account: &str,
    machine_id: uuid::Uuid,
) -> Result<MachineState> {
    let state: MachineState = dispatch!(client, |c| {
        let resp = c
            .inner()
            .get_machine()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();
        let state_value = serde_json::to_value(&resp.state)?;
        serde_json::from_value::<MachineState>(state_value)?
    });
    Ok(state)
}

/// List tags on an instance
///
/// Output format matches node-triton:
/// - Without -j: pretty-printed JSON
/// - With -j: compact JSON
pub async fn list_tags(args: TagListArgs, client: &AnyClient, use_json: bool) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();
    let tags = fetch_machine_tags(client, account, machine_id).await?;

    // node-triton always outputs JSON for tag list
    // -j means compact JSON, otherwise pretty-print
    if use_json {
        println!("{}", serde_json::to_string(&tags)?);
    } else {
        println!("{}", crate::output::json::to_json_pretty(&tags)?);
    }

    Ok(())
}

/// Get a single tag value
///
/// Output format matches node-triton:
/// - Without -j: plain value (string representation)
/// - With -j: JSON-encoded value (e.g., "bar" for string, true for bool)
async fn get_tag(args: TagGetArgs, client: &AnyClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    // The per-tag endpoint returns a bare string; the full map gives the
    // typed value (bool / number / string). Fall back to the endpoint
    // response when the key isn't found in the list (unlikely but safe).
    let (single_value, tags): (String, HashMap<String, Value>) = dispatch!(client, |c| {
        let single = c
            .inner()
            .get_machine_tag()
            .account(account)
            .machine(machine_id)
            .tag(&args.key)
            .send()
            .await?
            .into_inner();
        let list = c
            .inner()
            .list_machine_tags()
            .account(account)
            .machine(machine_id)
            .send()
            .await?
            .into_inner();
        let list_value = serde_json::to_value(&list)?;
        let tags = serde_json::from_value::<HashMap<String, Value>>(list_value)?;
        (single, tags)
    });

    let value = tags
        .get(&args.key)
        .cloned()
        .unwrap_or_else(|| Value::String(single_value));

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
async fn load_tags_from_file(file_path: &std::path::Path) -> Result<Map<String, Value>> {
    let content = if file_path.as_os_str() == "-" {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        buffer
    } else {
        tokio::fs::read_to_string(file_path).await?
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
async fn set_tags(args: TagSetArgs, client: &AnyClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    // Collect tags from files first, then command line args (args win over files)
    let mut tag_map: Map<String, Value> = Map::new();

    // Load from files if provided
    if let Some(files) = &args.files {
        for file_path in files {
            let file_tags = load_tags_from_file(file_path).await?;
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

    let expected_tags: Map<String, Value> = tag_map.clone();
    // Both clients' `TagsRequest` type alias implements
    // `From<Map<String, Value>>`, and `.body(V)` is `V: TryInto<TagsRequest>`.
    // Passing the raw map lets each arm coerce to the per-client
    // `TagsRequest` without naming the type.
    let body = tag_map;

    // Capture current state before tag operation so --wait uses the correct target
    let pre_state = if args.wait {
        Some(fetch_machine_state(client, account, machine_id).await?)
    } else {
        None
    };

    dispatch!(client, |c| {
        c.inner()
            .add_machine_tags()
            .account(account)
            .machine(machine_id)
            .body(body)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    if let Some(target_state) = pre_state {
        super::wait::wait_for_state(machine_id, target_state, args.wait_timeout, client).await?;

        // After machine state settles, poll until all requested tags are visible
        wait_for_tags(machine_id, &expected_tags, args.wait_timeout, client).await?;
    }

    // Output the updated tags (matching node-triton behavior)
    if !args.quiet {
        let updated_tags = fetch_machine_tags(client, account, machine_id).await?;

        // -j means compact JSON, otherwise pretty-print
        if args.json {
            println!("{}", serde_json::to_string(&updated_tags)?);
        } else {
            println!("{}", crate::output::json::to_json_pretty(&updated_tags)?);
        }
    }

    Ok(())
}

/// Delete tag(s) from an instance
///
/// Output format matches node-triton:
/// - For each deleted tag: "Deleted tag NAME on instance INST"
/// - For --all: "Deleted all tags on instance INST"
async fn delete_tag(args: TagDeleteArgs, client: &AnyClient) -> Result<()> {
    // Validate args
    if args.all && !args.keys.is_empty() {
        return Err(anyhow::anyhow!("cannot specify both tag names and --all"));
    }
    if !args.all && args.keys.is_empty() {
        return Err(anyhow::anyhow!("must specify tag name(s) or --all"));
    }

    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    // Capture current state before tag operation so --wait uses the correct target
    let pre_state = if args.wait {
        Some(fetch_machine_state(client, account, machine_id).await?)
    } else {
        None
    };

    if args.all {
        // Delete all tags
        dispatch!(client, |c| {
            c.inner()
                .delete_machine_tags()
                .account(account)
                .machine(machine_id)
                .send()
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;

        if let Some(target_state) = pre_state {
            super::wait::wait_for_state(machine_id, target_state, args.wait_timeout, client)
                .await?;

            // Poll until all tags are actually gone
            let empty = Map::new();
            wait_for_tags_exact(machine_id, &empty, args.wait_timeout, client).await?;
        }

        println!("Deleted all tags on instance {}", args.instance);
    } else {
        // Delete individual tags (de-duplicate keys)
        let mut seen = std::collections::HashSet::new();
        let unique_keys: Vec<_> = args.keys.iter().filter(|k| seen.insert(*k)).collect();

        for key in &unique_keys {
            let key_str = key.as_str();
            dispatch!(client, |c| {
                c.inner()
                    .delete_machine_tag()
                    .account(account)
                    .machine(machine_id)
                    .tag(key_str)
                    .send()
                    .await?;
                Ok::<(), anyhow::Error>(())
            })?;

            println!("Deleted tag {} on instance {}", key, args.instance);
        }

        if let Some(target_state) = pre_state {
            super::wait::wait_for_state(machine_id, target_state, args.wait_timeout, client)
                .await?;

            // Poll until deleted tags are actually gone
            let deleted_keys: Vec<&str> = unique_keys.iter().map(|k| k.as_str()).collect();
            wait_for_tags_deleted(machine_id, &deleted_keys, args.wait_timeout, client).await?;
        }
    }

    Ok(())
}

/// Replace all tags on an instance
///
/// Output format matches node-triton: JSON with updated tags
async fn replace_all_tags(args: TagReplaceAllArgs, client: &AnyClient) -> Result<()> {
    let machine_id = super::get::resolve_instance(&args.instance, client).await?;
    let account = client.effective_account();

    // Collect tags from files first, then command line args (args win over files)
    let mut tag_map: Map<String, Value> = Map::new();

    // Load from files if provided
    if let Some(files) = &args.files {
        for file_path in files {
            let file_tags = load_tags_from_file(file_path).await?;
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

    let expected_tags: Map<String, Value> = tag_map.clone();
    // See set_tags() for the Map<String, Value> → TagsRequest rationale.
    let body = tag_map;

    // Capture current state before tag operation so --wait uses the correct target
    let pre_state = if args.wait {
        Some(fetch_machine_state(client, account, machine_id).await?)
    } else {
        None
    };

    dispatch!(client, |c| {
        c.inner()
            .replace_machine_tags()
            .account(account)
            .machine(machine_id)
            .body(body)
            .send()
            .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    if let Some(target_state) = pre_state {
        super::wait::wait_for_state(machine_id, target_state, args.wait_timeout, client).await?;

        // Poll until tags match exactly what was set (old tags gone, new tags present)
        wait_for_tags_exact(machine_id, &expected_tags, args.wait_timeout, client).await?;
    }

    // Output the updated tags (matching node-triton behavior)
    if !args.quiet {
        let updated_tags = fetch_machine_tags(client, account, machine_id).await?;

        // -j means compact JSON, otherwise pretty-print
        if args.json {
            println!("{}", serde_json::to_string(&updated_tags)?);
        } else {
            println!("{}", crate::output::json::to_json_pretty(&updated_tags)?);
        }
    }

    Ok(())
}

/// Poll until all expected tag key-value pairs are present on the machine
/// Poll until tags match exactly the expected set (no extras, no missing)
async fn wait_for_tags_exact(
    machine_id: uuid::Uuid,
    expected: &Map<String, Value>,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let tags = fetch_machine_tags(client, account, machine_id).await?;
        if tags.len() == expected.len() && expected.iter().all(|(k, v)| tags.get(k) == Some(v)) {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!(
                "Timeout waiting for tags to match expected set"
            ));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_tags_deleted(
    machine_id: uuid::Uuid,
    keys: &[&str],
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let tags = fetch_machine_tags(client, account, machine_id).await?;
        if keys.iter().all(|k| !tags.contains_key(*k)) {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for tags to be deleted"));
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_tags(
    machine_id: uuid::Uuid,
    expected: &Map<String, Value>,
    timeout_secs: u64,
    client: &AnyClient,
) -> Result<()> {
    use std::time::{Duration, Instant};
    use tokio::time::sleep;

    let account = client.effective_account();
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    loop {
        let tags = fetch_machine_tags(client, account, machine_id).await?;
        if expected.iter().all(|(k, v)| tags.get(k) == Some(v)) {
            return Ok(());
        }

        if start.elapsed() > timeout {
            return Err(anyhow::anyhow!("Timeout waiting for tags to appear"));
        }

        sleep(Duration::from_secs(2)).await;
    }
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

    /// Parse tags from a list of key=value strings into a Map,
    /// using the production parse_tag_value for type coercion.
    fn parse_tags_from_args(tags: &[String]) -> Result<Map<String, Value>> {
        let mut map: Map<String, Value> = Map::new();
        for tag in tags {
            let (key, value) = parse_tag(tag)?;
            map.insert(key, parse_tag_value(&value));
        }
        Ok(map)
    }

    // ===== parse_tag_value tests =====

    #[test]
    fn test_parse_tag_value_bool_true() {
        assert_eq!(parse_tag_value("true"), Value::Bool(true));
    }

    #[test]
    fn test_parse_tag_value_bool_false() {
        assert_eq!(parse_tag_value("false"), Value::Bool(false));
    }

    #[test]
    fn test_parse_tag_value_integer() {
        assert_eq!(
            parse_tag_value("42"),
            Value::Number(serde_json::Number::from_f64(42.0).unwrap())
        );
    }

    #[test]
    fn test_parse_tag_value_negative_number() {
        assert_eq!(
            parse_tag_value("-7"),
            Value::Number(serde_json::Number::from_f64(-7.0).unwrap())
        );
    }

    #[test]
    fn test_parse_tag_value_float() {
        let val = parse_tag_value("3.14");
        assert!(val.is_number());
    }

    #[test]
    fn test_parse_tag_value_string() {
        assert_eq!(parse_tag_value("hello"), Value::String("hello".to_string()));
    }

    #[test]
    fn test_parse_tag_value_empty_string() {
        assert_eq!(parse_tag_value(""), Value::String("".to_string()));
    }

    #[test]
    fn test_parse_tag_value_whitespace_trimming() {
        assert_eq!(parse_tag_value("  true  "), Value::Bool(true));
        assert_eq!(
            parse_tag_value("  42  "),
            Value::Number(serde_json::Number::from_f64(42.0).unwrap())
        );
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
        // count=42 is coerced to a number by parse_tag_value
        assert_eq!(
            *map.get("count").unwrap(),
            Value::Number(serde_json::Number::from_f64(42.0).unwrap())
        );
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
