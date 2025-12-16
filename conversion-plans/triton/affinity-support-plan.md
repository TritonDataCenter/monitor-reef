# Triton CLI Affinity Rules Support Plan

## Background

The `triton instance create` command currently has an `--affinity` flag that accepts affinity rules, but the rules are not passed to the CloudAPI. This document outlines the plan to complete this feature.

**Source:** `cli/triton-cli/src/commands/instance/create.rs:94-95`
```rust
// Note: affinity is handled through locality, which we don't support in this CLI yet
// TODO: Support locality/affinity hints
```

## CloudAPI Context

### Historical Context

CloudAPI has two mechanisms for instance placement hints:

1. **`locality`** (Deprecated in v8.3.0): Object-based format
   ```json
   {
     "strict": false,
     "near": ["uuid1", "uuid2"],
     "far": ["uuid3"]
   }
   ```

2. **`affinity`** (Added in v8.3.0, preferred): Array of rule strings
   ```
   ["instance==silent_bob", "role!=database", "instance!=~foo*"]
   ```

### Affinity Rule Format

Rules follow the pattern: `<key><operator><value>`

**Keys:**
- `instance` or `container`: Match against instance names or UUIDs
- `<tagName>`: Match against tag values (e.g., `role`, `env`)

**Operators:**
- `==`: Must be on same server (strict)
- `!=`: Must be on different server (strict)
- `==~`: Should be on same server (soft/hint)
- `!=~`: Should be on different server (soft/hint)

**Values:**
- Exact string: `instance==myvm`
- Glob pattern: `instance!=foo*`
- Regex: `instance!=/^foo/`

### Examples

```bash
# Run on same node as instance "silent_bob"
triton instance create -a instance==silent_bob ...

# Run on different node from instances tagged role=database
triton instance create -a 'role!=database' ...

# Run on different node from instances starting with "foo" (soft rule)
triton instance create -a 'instance!=~foo*' ...
```

## Current State Analysis

### API Layer (`apis/cloudapi-api/src/types/machine.rs`)

The `CreateMachineRequest` struct has `locality` but NOT `affinity`:

```rust
pub struct CreateMachineRequest {
    // ...
    /// Locality hints
    #[serde(default)]
    pub locality: Option<serde_json::Value>,
    // ...
}
```

**Issue:** The API definition is missing the `affinity` field that was added in CloudAPI v8.3.0.

### CLI Layer (`cli/triton-cli/src/commands/instance/create.rs`)

The CLI accepts `--affinity` flags:

```rust
/// Affinity rules
#[arg(long)]
pub affinity: Option<Vec<String>>,
```

But the flag is never used - the affinity rules are collected but not passed to the API.

### Generated Client

The Progenitor-generated client reflects the API, so it also lacks an `affinity` field on `CreateMachineRequest`.

## Implementation Plan

### Phase 1: Update API Types

**File:** `apis/cloudapi-api/src/types/machine.rs`

Add `affinity` field to `CreateMachineRequest`:

```rust
/// Request to create a machine
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateMachineRequest {
    /// Machine alias/name
    #[serde(default)]
    pub name: Option<String>,
    /// Image UUID
    pub image: Uuid,
    /// Package name or UUID
    pub package: String,
    /// Networks (array of UUIDs)
    #[serde(default)]
    pub networks: Option<Vec<Uuid>>,
    /// Affinity rules (preferred, added in CloudAPI v8.3.0)
    #[serde(default)]
    pub affinity: Option<Vec<String>>,
    /// Locality hints (deprecated, use affinity instead)
    #[serde(default)]
    pub locality: Option<serde_json::Value>,
    /// Metadata
    #[serde(default)]
    pub metadata: Option<Metadata>,
    /// Tags
    #[serde(default)]
    pub tags: Option<Tags>,
    /// Firewall enabled
    #[serde(default)]
    pub firewall_enabled: Option<bool>,
    /// Deletion protection enabled
    #[serde(default)]
    pub deletion_protection: Option<bool>,
}
```

### Phase 2: Regenerate OpenAPI Spec and Client

```bash
make openapi-generate
make build
```

This will:
1. Update `openapi-specs/generated/cloudapi-api.json` with the new `affinity` field
2. Regenerate the Progenitor client with the `affinity` method on the builder

### Phase 3: Update CLI Implementation

**File:** `cli/triton-cli/src/commands/instance/create.rs`

Update the `run` function to pass affinity rules:

```rust
pub async fn run(args: CreateArgs, client: &TypedClient, use_json: bool) -> Result<()> {
    // ... existing code ...

    // Build create request using the builder pattern
    let mut request = cloudapi_client::types::CreateMachineRequest::builder()
        .image(image_id)
        .package(package_id);

    if let Some(name) = &args.name {
        request = request.name(name.clone());
    }
    if args.firewall {
        request = request.firewall_enabled(true);
    }
    if args.deletion_protection {
        request = request.deletion_protection(true);
    }

    // Handle networks
    if let Some(networks) = &args.network {
        let network_ids: Vec<String> = networks
            .iter()
            .flat_map(|n| n.split(','))
            .map(|s| s.trim().to_string())
            .collect();
        request = request.networks(network_ids);
    }

    // Handle affinity rules
    if let Some(affinity) = &args.affinity {
        request = request.affinity(affinity.clone());
    }

    // ... rest of existing code ...
}
```

### Phase 4: Remove TODO Comment

Remove the comment at lines 94-95:
```rust
// Note: affinity is handled through locality, which we don't support in this CLI yet
// TODO: Support locality/affinity hints
```

### Phase 5: Testing

#### Unit Tests
Add parsing/validation tests if we decide to validate affinity rules client-side.

#### Integration Tests
Test against a real CloudAPI to verify:
1. Affinity rules are passed correctly in the request body
2. Server accepts the rules and uses them for placement

#### Manual Testing
```bash
# Test strict same-server placement
triton instance create -a 'instance==existing-vm' image@version package

# Test soft different-server placement
triton instance create -a 'instance!=~existing-vm' image@version package

# Test tag-based placement
triton instance create -a 'role!=database' image@version package

# Test multiple rules
triton instance create -a 'instance!=db-*' -a 'role==webserver' image@version package
```

## Optional Enhancements

### Client-Side Validation

Consider adding validation for affinity rule syntax before sending to server:

```rust
fn validate_affinity_rule(rule: &str) -> Result<()> {
    // Parse: <key><op><value>
    // Key: instance|container|<tag-name>
    // Op: ==|!=|==~|!=~
    // Value: string|glob|/regex/

    let re = Regex::new(r"^(\w+)(==~?|!=~?)(.+)$")?;
    if !re.is_match(rule) {
        return Err(anyhow!("Invalid affinity rule format: {}", rule));
    }
    Ok(())
}
```

This would provide better error messages than waiting for server rejection.

### Help Text Enhancement

Update the `--affinity` help text to be more descriptive:

```rust
/// Affinity rules for instance placement.
/// Format: <key><op><value> where:
///   key: 'instance', 'container', or a tag name
///   op: '==' (must), '!=' (must not), '==~' (prefer), '!=~' (prefer not)
///   value: exact string, glob (*), or regex (/pattern/)
/// Examples: 'instance==myvm', 'role!=database', 'instance!=~foo*'
#[arg(long, short = 'a')]
pub affinity: Option<Vec<String>>,
```

## Files to Modify

1. `apis/cloudapi-api/src/types/machine.rs` - Add `affinity` field
2. `openapi-specs/generated/cloudapi-api.json` - Regenerated automatically
3. `cli/triton-cli/src/commands/instance/create.rs` - Wire up affinity flag
4. `conversion-plans/triton/validation-report-2025-12-16.md` - Update TODO status

## Estimated Effort

- Phase 1-2: ~15 minutes (API update + regeneration)
- Phase 3-4: ~15 minutes (CLI wiring)
- Phase 5: ~30 minutes (testing)
- Optional enhancements: ~30-60 minutes each

Total: ~1 hour for core implementation, +1-2 hours for optional enhancements

## References

- [CloudAPI CreateMachine Documentation](https://apidocs.tritondatacenter.com/cloudapi/#CreateMachine)
- [Triton Docker Placement Documentation](https://apidocs.tritondatacenter.com/docker/features/placement)
- [node-triton do_create.js](target/node-triton/lib/do_instance/do_create.js)
- [sdc-cloudapi docs/index.md](target/sdc-cloudapi/docs/index.md) - Lines 5157-5227
