<!-- This Source Code Form is subject to the terms of the Mozilla Public
     License, v. 2.0. If a copy of the MPL was not distributed with this
     file, You can obtain one at https://mozilla.org/MPL/2.0/.

     Copyright 2026 Edgecast Cloud LLC. -->

# Design: triton-common Shared Primitives Crate

## Status

**Proposed** - Not yet implemented

## Problem Statement

As we convert multiple Triton services from Node.js to Rust, each API crate
currently defines its own primitive types (UUIDs, MAC addresses, timestamps,
pagination). This leads to:

1. **Code duplication** - Same validation logic repeated across crates
2. **Inconsistent handling** - Different services may handle edge cases differently
3. **Weak typing** - `String` used where validated types would catch errors at compile time
4. **No shared vocabulary** - Harder to compose services or share code

### Current State Example

```rust
// apis/vmapi-api/src/types/vm.rs
pub struct Vm {
    pub uuid: String,              // Should be validated UUID
    pub owner_uuid: String,        // Should be validated UUID
    pub server_uuid: Option<String>, // Should be validated UUID
    // ...
}

// apis/vmapi-api/src/types/network.rs
pub struct Nic {
    pub mac: String,               // Should be validated MAC address
    // ...
}
```

## Proposed Solution

Create a `triton-common` crate containing shared primitives with validation,
serde support, and JsonSchema derivation.

### Crate Location

```
triton-common/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── uuid.rs          # UUID wrapper types
    ├── mac_address.rs   # MAC address type
    ├── timestamp.rs     # Timestamp handling
    ├── pagination.rs    # Pagination query/response types
    └── error.rs         # Common error patterns
```

## Detailed Design

### 1. UUID Types

```rust
// triton-common/src/uuid.rs

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// A validated UUID string.
///
/// This type ensures the string is a valid UUID format at parse time,
/// providing compile-time safety when working with identifiers.
///
/// # Serialization
/// Serializes as a lowercase hyphenated string: "550e8400-e29b-41d4-a716-446655440000"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct Uuid(uuid::Uuid);

impl Uuid {
    /// Create a new random UUID (v4)
    pub fn new_v4() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    /// Get the inner uuid::Uuid
    pub fn as_uuid(&self) -> &uuid::Uuid {
        &self.0
    }

    /// Convert to hyphenated lowercase string
    pub fn to_string(&self) -> String {
        self.0.hyphenated().to_string()
    }
}

impl FromStr for Uuid {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(uuid::Uuid::parse_str(s)?))
    }
}

impl TryFrom<String> for Uuid {
    type Error = uuid::Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Uuid> for String {
    fn from(u: Uuid) -> String {
        u.to_string()
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.hyphenated())
    }
}

/// Type alias for VM UUIDs (documentation clarity)
pub type VmUuid = Uuid;

/// Type alias for owner/account UUIDs
pub type OwnerUuid = Uuid;

/// Type alias for server UUIDs
pub type ServerUuid = Uuid;

/// Type alias for image UUIDs
pub type ImageUuid = Uuid;

/// Type alias for network UUIDs
pub type NetworkUuid = Uuid;

/// Type alias for job UUIDs
pub type JobUuid = Uuid;
```

**Rationale:**
- Wrapping `uuid::Uuid` provides serde and JsonSchema integration
- Type aliases provide documentation without runtime overhead
- `TryFrom<String>` enables automatic validation during deserialization

### 2. MAC Address Type

```rust
// triton-common/src/mac_address.rs

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MacAddressError {
    #[error("invalid MAC address format: {0}")]
    InvalidFormat(String),
    #[error("invalid MAC address length")]
    InvalidLength,
}

/// A validated MAC address.
///
/// Accepts multiple input formats:
/// - Colon-separated: "aa:bb:cc:dd:ee:ff"
/// - Hyphen-separated: "aa-bb-cc-dd-ee-ff"
/// - No separator: "aabbccddeeff"
///
/// Always serializes as colon-separated lowercase: "aa:bb:cc:dd:ee:ff"
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct MacAddress([u8; 6]);

impl MacAddress {
    /// Create from raw bytes
    pub fn from_bytes(bytes: [u8; 6]) -> Self {
        Self(bytes)
    }

    /// Get the raw bytes
    pub fn as_bytes(&self) -> &[u8; 6] {
        &self.0
    }

    /// Format without separators (for URL paths)
    pub fn to_string_no_sep(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

impl FromStr for MacAddress {
    type Err = MacAddressError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Remove common separators
        let clean: String = s
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .collect();

        if clean.len() != 12 {
            return Err(MacAddressError::InvalidLength);
        }

        let mut bytes = [0u8; 6];
        for (i, chunk) in clean.as_bytes().chunks(2).enumerate() {
            let hex_str = std::str::from_utf8(chunk)
                .map_err(|_| MacAddressError::InvalidFormat(s.to_string()))?;
            bytes[i] = u8::from_str_radix(hex_str, 16)
                .map_err(|_| MacAddressError::InvalidFormat(s.to_string()))?;
        }

        Ok(Self(bytes))
    }
}

impl TryFrom<String> for MacAddress {
    type Error = MacAddressError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<MacAddress> for String {
    fn from(m: MacAddress) -> String {
        m.to_string()
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}
```

**Rationale:**
- Triton APIs accept MAC addresses in multiple formats but should normalize output
- CloudAPI URLs use no-separator format (`/nics/aabbccddeeff`)
- Internal APIs use colon-separated format

### 3. Pagination Types

```rust
// triton-common/src/pagination.rs

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Standard pagination query parameters
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PaginationQuery {
    /// Maximum number of results to return
    #[serde(default)]
    pub limit: Option<u64>,

    /// Number of results to skip
    #[serde(default)]
    pub offset: Option<u64>,
}

impl PaginationQuery {
    /// Get limit with a default maximum
    pub fn limit_or(&self, default: u64, max: u64) -> u64 {
        self.limit.unwrap_or(default).min(max)
    }

    /// Get offset with default of 0
    pub fn offset_or(&self, default: u64) -> u64 {
        self.offset.unwrap_or(default)
    }
}

/// Pagination metadata for responses
#[derive(Debug, Serialize, JsonSchema)]
pub struct PaginationMeta {
    /// Total count of matching items (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count: Option<u64>,

    /// Number of items returned in this response
    pub count: u64,

    /// Offset used for this query
    pub offset: u64,

    /// Limit used for this query
    pub limit: u64,
}
```

### 4. Timestamp Handling

```rust
// triton-common/src/timestamp.rs

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// ISO 8601 timestamp in UTC
///
/// Serializes as: "2025-01-15T10:30:00.000Z"
pub type Timestamp = DateTime<Utc>;

/// Helper for creating timestamps
pub fn now() -> Timestamp {
    Utc::now()
}

/// Optional timestamp with custom JsonSchema
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OptionalTimestamp(#[serde(default)] pub Option<Timestamp>);

impl From<Option<Timestamp>> for OptionalTimestamp {
    fn from(t: Option<Timestamp>) -> Self {
        Self(t)
    }
}

impl From<Timestamp> for OptionalTimestamp {
    fn from(t: Timestamp) -> Self {
        Self(Some(t))
    }
}
```

### 5. Common Error Types

```rust
// triton-common/src/error.rs

use schemars::JsonSchema;
use serde::Serialize;

/// Standard error response body
///
/// Matches the common Triton error format.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorResponse {
    /// Error code (e.g., "ResourceNotFound", "InvalidArgument")
    pub code: String,

    /// Human-readable error message
    pub message: String,

    /// Additional error details (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ErrorDetail>>,
}

/// Detailed error information
#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorDetail {
    /// Field that caused the error
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,

    /// Error code for this specific error
    pub code: String,

    /// Error message
    pub message: String,
}

impl ErrorResponse {
    pub fn not_found(resource: &str, id: &str) -> Self {
        Self {
            code: "ResourceNotFound".to_string(),
            message: format!("{} {} not found", resource, id),
            errors: None,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self {
            code: "InvalidArgument".to_string(),
            message: message.into(),
            errors: None,
        }
    }

    pub fn validation_failed(errors: Vec<ErrorDetail>) -> Self {
        Self {
            code: "ValidationFailed".to_string(),
            message: "Request validation failed".to_string(),
            errors: Some(errors),
        }
    }
}
```

## Cargo.toml

```toml
[package]
name = "triton-common"
version = "0.1.0"
edition = "2021"
description = "Shared primitives for Triton Rust services"
license = "MPL-2.0"

[dependencies]
chrono = { version = "0.4", features = ["serde"] }
schemars = "0.8"
serde = { version = "1", features = ["derive"] }
thiserror = "1"
uuid = { version = "1", features = ["v4", "serde"] }
```

## Migration Strategy

### Phase 1: Create Crate (Non-Breaking)

1. Create `triton-common` crate with types
2. Add to workspace
3. Write comprehensive tests
4. Document all types

### Phase 2: Gradual Adoption (Per-Service)

For each API crate, optionally adopt `triton-common` types:

```rust
// Before
pub struct Vm {
    pub uuid: String,
    pub owner_uuid: String,
}

// After
use triton_common::{Uuid, OwnerUuid};

pub struct Vm {
    pub uuid: Uuid,
    pub owner_uuid: OwnerUuid,
}
```

**Note:** This is a breaking change for the generated client types. Adopt when
creating new services, not as a retrofit to existing services unless there's
a compelling reason.

### Phase 3: New Services Use By Default

Update skill documentation to recommend `triton-common` for new conversions.

## Alternatives Considered

### 1. Keep Using Strings

**Pros:**
- No new dependencies
- Simple mental model

**Cons:**
- No compile-time validation
- Easy to mix up UUID types
- Duplicate validation code

### 2. Use External Crates Directly

**Pros:**
- No maintenance burden

**Cons:**
- Different crates have different serde/JsonSchema behavior
- No Triton-specific conventions (MAC address formats, error types)
- Harder to ensure consistency

### 3. Generate Types from JSON Schema

**Pros:**
- Single source of truth

**Cons:**
- Complex tooling
- Less idiomatic Rust
- Harder to add Rust-specific methods

## Open Questions

1. **Should type aliases be newtypes?**
   - `type VmUuid = Uuid` vs `struct VmUuid(Uuid)`
   - Newtypes prevent mixing up UUID types but add conversion boilerplate

2. **Should we include IP address types?**
   - `std::net::IpAddr` exists but doesn't have JsonSchema
   - May need wrapper for OpenAPI compatibility

3. **What about CIDR notation?**
   - Network APIs use CIDR (`10.0.0.0/24`)
   - Could add `CidrBlock` type

## References

- [uuid crate](https://docs.rs/uuid)
- [chrono crate](https://docs.rs/chrono)
- [schemars crate](https://docs.rs/schemars)
- Existing Triton API documentation for error formats
