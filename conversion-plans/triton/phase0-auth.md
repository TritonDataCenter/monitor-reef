<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Phase 0: Authentication Foundation

## Goal

Create the `triton-auth` library and update `cloudapi-client` to support authenticated CloudAPI requests using HTTP Signature authentication.

## Tasks

### Task 1: Create `libs/triton-auth` crate

Create a new library crate at `libs/triton-auth/` that provides HTTP Signature authentication for CloudAPI.

**Directory structure:**
```
libs/triton-auth/
├── Cargo.toml
└── src/
    ├── lib.rs             # Public API exports
    ├── signature.rs       # HTTP Signature generation
    ├── key_loader.rs      # SSH key loading from files
    ├── agent.rs           # SSH agent integration
    ├── fingerprint.rs     # MD5 fingerprint calculation
    └── error.rs           # Error types
```

**Cargo.toml:**
```toml
[package]
name = "triton-auth"
version = "0.1.0"
edition.workspace = true

[dependencies]
ssh-key = { version = "0.6", features = ["ed25519", "rsa", "ecdsa", "encryption"] }
ssh-agent-client-rs = "0.3"
base64 = "0.22"
md-5 = "0.10"
chrono = "0.4"
secrecy = "0.10"
thiserror = "2.0"
tokio = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

**Add to workspace `Cargo.toml` members:**
```toml
"libs/triton-auth",
```

### Task 2: Implement Core Types (`lib.rs`)

```rust
//! Triton HTTP Signature Authentication Library
//!
//! Provides SSH key-based HTTP Signature authentication for CloudAPI.

pub mod agent;
pub mod error;
pub mod fingerprint;
pub mod key_loader;
pub mod signature;

pub use error::AuthError;
pub use fingerprint::md5_fingerprint;
pub use key_loader::{KeyLoader, KeySource};
pub use signature::{HttpSigner, RequestSigner};

/// Authentication state for CloudAPI requests
#[derive(Clone)]
pub struct AuthState {
    /// Account login name (used for operations)
    pub account: String,
    /// RBAC sub-user login (optional)
    pub user: Option<String>,
    /// SSH key fingerprint (MD5 format: aa:bb:cc:...)
    pub key_id: String,
    /// How to load/access the signing key
    pub key_source: KeySource,
    /// RBAC roles to assume (optional)
    pub roles: Option<Vec<String>>,
}

impl AuthState {
    /// Create a new AuthState
    pub fn new(account: String, key_id: String, key_source: KeySource) -> Self {
        Self {
            account,
            user: None,
            key_id,
            key_source,
            roles: None,
        }
    }

    /// Set RBAC sub-user
    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Set RBAC roles
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = Some(roles);
        self
    }
}
```

### Task 3: Implement Key Source (`key_loader.rs`)

```rust
use crate::error::AuthError;
use secrecy::SecretString;
use ssh_key::{PrivateKey, PublicKey};
use std::path::PathBuf;

/// Source for loading SSH keys
#[derive(Clone)]
pub enum KeySource {
    /// Load key from SSH agent using fingerprint
    Agent { fingerprint: String },
    /// Load key from file path
    File {
        path: PathBuf,
        passphrase: Option<SecretString>,
    },
    /// Auto-detect: try agent first, then common file locations
    Auto { fingerprint: String },
}

/// Key loader for various sources
pub struct KeyLoader;

impl KeyLoader {
    /// Load a private key from the specified source
    pub async fn load_private_key(source: &KeySource) -> Result<PrivateKey, AuthError> {
        match source {
            KeySource::File { path, passphrase } => Self::load_from_file(path, passphrase.as_ref()),
            KeySource::Agent { fingerprint } | KeySource::Auto { fingerprint } => {
                // Try agent first
                match crate::agent::get_key_from_agent(fingerprint).await {
                    Ok(key) => Ok(key),
                    Err(_) if matches!(source, KeySource::Auto { .. }) => {
                        // Fall back to common file locations
                        Self::load_from_common_paths(fingerprint).await
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// Load from a specific file path
    fn load_from_file(
        path: &PathBuf,
        passphrase: Option<&SecretString>,
    ) -> Result<PrivateKey, AuthError> {
        let key_data = std::fs::read_to_string(path)
            .map_err(|e| AuthError::KeyLoadError(format!("Failed to read {}: {}", path.display(), e)))?;

        if let Some(pass) = passphrase {
            PrivateKey::from_openssh(key_data.as_bytes())
                .map_err(|e| AuthError::KeyLoadError(format!("Failed to parse encrypted key: {}", e)))
        } else {
            PrivateKey::from_openssh(key_data.as_bytes())
                .map_err(|e| AuthError::KeyLoadError(format!("Failed to parse key: {}", e)))
        }
    }

    /// Try loading from common SSH key locations
    async fn load_from_common_paths(fingerprint: &str) -> Result<PrivateKey, AuthError> {
        let home = dirs::home_dir()
            .ok_or_else(|| AuthError::KeyLoadError("Could not determine home directory".into()))?;

        let ssh_dir = home.join(".ssh");
        let key_files = ["id_ed25519", "id_ecdsa", "id_rsa"];

        for key_file in &key_files {
            let path = ssh_dir.join(key_file);
            if path.exists() {
                if let Ok(key) = Self::load_from_file(&path, None) {
                    // Check if fingerprint matches
                    let key_fp = crate::fingerprint::md5_fingerprint(&key.public_key());
                    if key_fp == fingerprint {
                        return Ok(key);
                    }
                }
            }
        }

        Err(AuthError::KeyNotFound(fingerprint.to_string()))
    }
}
```

### Task 4: Implement SSH Agent Integration (`agent.rs`)

```rust
use crate::error::AuthError;
use ssh_key::PrivateKey;

/// Get a key from the SSH agent matching the given fingerprint
pub async fn get_key_from_agent(fingerprint: &str) -> Result<PrivateKey, AuthError> {
    // Use ssh-agent-client-rs to connect to the agent
    // List keys and find matching fingerprint
    // Return the key or error

    use ssh_agent_client_rs::Client;

    let mut client = Client::connect()
        .await
        .map_err(|e| AuthError::AgentError(format!("Failed to connect to SSH agent: {}", e)))?;

    let identities = client
        .list_identities()
        .await
        .map_err(|e| AuthError::AgentError(format!("Failed to list agent identities: {}", e)))?;

    for identity in identities {
        let id_fp = crate::fingerprint::md5_fingerprint_bytes(&identity.pubkey_blob);
        if id_fp == fingerprint {
            // Note: ssh-agent-client-rs signs data using the agent
            // We return a marker that indicates agent-based signing
            return Err(AuthError::AgentSigningRequired(fingerprint.to_string()));
        }
    }

    Err(AuthError::KeyNotFound(fingerprint.to_string()))
}

/// Sign data using the SSH agent
pub async fn sign_with_agent(fingerprint: &str, data: &[u8]) -> Result<Vec<u8>, AuthError> {
    use ssh_agent_client_rs::Client;

    let mut client = Client::connect()
        .await
        .map_err(|e| AuthError::AgentError(format!("Failed to connect to SSH agent: {}", e)))?;

    let identities = client
        .list_identities()
        .await
        .map_err(|e| AuthError::AgentError(format!("Failed to list agent identities: {}", e)))?;

    for identity in identities {
        let id_fp = crate::fingerprint::md5_fingerprint_bytes(&identity.pubkey_blob);
        if id_fp == fingerprint {
            let signature = client
                .sign(&identity.pubkey_blob, data)
                .await
                .map_err(|e| AuthError::SigningError(format!("Agent signing failed: {}", e)))?;
            return Ok(signature);
        }
    }

    Err(AuthError::KeyNotFound(fingerprint.to_string()))
}
```

### Task 5: Implement MD5 Fingerprint (`fingerprint.rs`)

```rust
use md5::{Digest, Md5};
use ssh_key::PublicKey;

/// Calculate MD5 fingerprint of an SSH public key in colon-separated hex format
///
/// Returns format like "aa:bb:cc:dd:ee:ff:..."
pub fn md5_fingerprint(key: &PublicKey) -> String {
    md5_fingerprint_bytes(&key.to_bytes())
}

/// Calculate MD5 fingerprint from raw public key bytes
pub fn md5_fingerprint_bytes(key_bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(key_bytes);
    let result = hasher.finalize();

    result
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_fingerprint_format() {
        let test_bytes = b"test public key data";
        let fp = md5_fingerprint_bytes(test_bytes);

        // Should be 16 bytes = 32 hex chars + 15 colons = 47 chars
        assert_eq!(fp.len(), 47);
        assert!(fp.chars().filter(|c| *c == ':').count() == 15);
    }
}
```

### Task 6: Implement HTTP Signature Generation (`signature.rs`)

Reference: `target/node-smartdc-auth/lib/index.js` and `target/node-smartdc-auth/lib/keypair.js`

**Authorization Header Format (from node-smartdc-auth):**
```
Authorization: Signature keyId="/:account/keys/:md5_fingerprint",algorithm="rsa-sha256",signature=":base64_sig:"
```

**Key ID Format Variations:**
- Basic: `/:user/keys/:fingerprint`
- With subuser (SDC-style): `/:user/users/:subuser/keys/:fingerprint`
- With subuser (Manta-style): `/:user/:subuser/keys/:fingerprint`

**Algorithm Format:** `{keytype}-{hashtype}` (e.g., `rsa-sha256`, `ecdsa-sha256`, `dsa-sha1`)

**Supported Algorithms:**
- `rsa-sha256` (default for RSA)
- `rsa-sha1` (legacy)
- `dsa-sha1` (DSA keys)
- `ecdsa-sha256` (ECDSA-256)
- `ecdsa-sha384` (ECDSA-384)
- `ecdsa-sha512` (ECDSA-521)

```rust
use crate::error::AuthError;
use base64::Engine;
use chrono::Utc;

/// HTTP Signature signer
pub struct RequestSigner {
    account: String,
    subuser: Option<String>,
    fingerprint: String,  // MD5 hex format: aa:bb:cc:...
    algorithm: String,
}

impl RequestSigner {
    pub fn new(account: &str, fingerprint: &str, key_type: KeyType) -> Self {
        let algorithm = match key_type {
            KeyType::Rsa => "rsa-sha256",
            KeyType::Dsa => "dsa-sha1",
            KeyType::Ecdsa256 => "ecdsa-sha256",
            KeyType::Ecdsa384 => "ecdsa-sha384",
            KeyType::Ecdsa521 => "ecdsa-sha512",
            KeyType::Ed25519 => "ed25519-sha512",
        };
        Self {
            account: account.to_string(),
            subuser: None,
            fingerprint: fingerprint.to_string(),
            algorithm: algorithm.to_string(),
        }
    }

    pub fn with_subuser(mut self, subuser: impl Into<String>) -> Self {
        self.subuser = Some(subuser.into());
        self
    }

    /// Generate the signing string for the request (http-signature format)
    pub fn signing_string(&self, method: &str, path: &str, date: &str) -> String {
        format!(
            "date: {}\n(request-target): {} {}",
            date,
            method.to_lowercase(),
            path
        )
    }

    /// Generate Date header value (RFC 2822 format)
    pub fn date_header() -> String {
        Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string()
    }

    /// Generate the keyId for the Authorization header
    pub fn key_id_string(&self) -> String {
        match &self.subuser {
            Some(subuser) => format!("/{}/users/{}/keys/{}", self.account, subuser, self.fingerprint),
            None => format!("/{}/keys/{}", self.account, self.fingerprint),
        }
    }

    /// Generate the full Authorization header value given a base64 signature
    pub fn authorization_header(&self, signature_b64: &str) -> String {
        format!(
            "Signature keyId=\"{}\",algorithm=\"{}\",signature=\"{}\"",
            self.key_id_string(),
            self.algorithm,
            signature_b64
        )
    }
}

/// Key type for algorithm selection
#[derive(Debug, Clone, Copy)]
pub enum KeyType {
    Rsa,
    Dsa,
    Ecdsa256,
    Ecdsa384,
    Ecdsa521,
    Ed25519,
}

impl KeyType {
    pub fn from_private_key(key: &ssh_key::PrivateKey) -> Self {
        use ssh_key::Algorithm;
        match key.algorithm() {
            Algorithm::Rsa { .. } => Self::Rsa,
            Algorithm::Dsa => Self::Dsa,
            Algorithm::Ecdsa { curve } => {
                match curve.as_str() {
                    "nistp256" => Self::Ecdsa256,
                    "nistp384" => Self::Ecdsa384,
                    "nistp521" => Self::Ecdsa521,
                    _ => Self::Ecdsa256,
                }
            }
            Algorithm::Ed25519 => Self::Ed25519,
            _ => Self::Rsa, // fallback
        }
    }
}

/// Sign data with a private key
pub fn sign_with_key(
    key: &ssh_key::PrivateKey,
    data: &[u8],
) -> Result<String, AuthError> {
    use ssh_key::HashAlg;

    let hash_alg = match KeyType::from_private_key(key) {
        KeyType::Rsa | KeyType::Ecdsa256 => HashAlg::Sha256,
        KeyType::Ecdsa384 => HashAlg::Sha384,
        KeyType::Ecdsa521 | KeyType::Ed25519 => HashAlg::Sha512,
        KeyType::Dsa => HashAlg::Sha1,
    };

    let signature = key
        .sign("", hash_alg, data)
        .map_err(|e| AuthError::SigningError(format!("Failed to sign: {}", e)))?;

    let sig_bytes = signature.as_bytes();
    Ok(base64::engine::general_purpose::STANDARD.encode(sig_bytes))
}

/// Trait for signing HTTP requests
pub trait HttpSigner: Send + Sync {
    /// Sign data and return base64-encoded signature
    fn sign(&self, data: &[u8]) -> impl std::future::Future<Output = Result<String, AuthError>> + Send;

    /// Get the algorithm name
    fn algorithm(&self) -> &str;
}
```

### Task 7: Implement Error Types (`error.rs`)

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Failed to load key: {0}")]
    KeyLoadError(String),

    #[error("Key not found: {0}")]
    KeyNotFound(String),

    #[error("SSH agent error: {0}")]
    AgentError(String),

    #[error("Signing error: {0}")]
    SigningError(String),

    #[error("Agent signing required for key: {0}")]
    AgentSigningRequired(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),
}
```

### Task 8: Update `cloudapi-client` for Authentication

Modify `clients/internal/cloudapi-client/` to support authenticated requests.

**Add to `Cargo.toml`:**
```toml
[dependencies]
triton-auth = { path = "../../libs/triton-auth" }
```

**Create `src/auth.rs`:**
```rust
use progenitor_client::RequestBuilder;
use triton_auth::{AuthError, AuthState, RequestSigner};

/// Add authentication headers to a request
pub async fn add_auth_headers(
    state: &AuthState,
    request: &mut reqwest::Request,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let method = request.method().as_str();
    let path = request.url().path();

    let signer = RequestSigner::new(&state.account, &state.key_id);
    let date = RequestSigner::date_header();
    let signing_string = signer.signing_string(method, path, &date);

    // Sign the request
    let signature = match &state.key_source {
        triton_auth::KeySource::Agent { fingerprint } => {
            triton_auth::agent::sign_with_agent(fingerprint, signing_string.as_bytes()).await?;
            // Convert to base64
            base64::engine::general_purpose::STANDARD.encode(&signature)
        }
        triton_auth::KeySource::File { .. } | triton_auth::KeySource::Auto { .. } => {
            let key = triton_auth::KeyLoader::load_private_key(&state.key_source).await?;
            triton_auth::signature::sign_with_key(&key, signing_string.as_bytes())?
        }
    };

    // Add headers
    let headers = request.headers_mut();
    headers.insert("date", date.parse().unwrap());
    headers.insert(
        "authorization",
        signer.authorization_header(&signature).parse().unwrap(),
    );

    // Add RBAC roles if present
    if let Some(roles) = &state.roles {
        // Roles are added as query parameter: ?as-role=role1,role2
        let url = request.url_mut();
        let mut query = url.query().unwrap_or("").to_string();
        if !query.is_empty() {
            query.push('&');
        }
        query.push_str(&format!("as-role={}", roles.join(",")));
        url.set_query(Some(&query));
    }

    Ok(())
}
```

**Update `build.rs` to use `pre_hook_async`:**
```rust
fn main() {
    let spec = std::fs::read_to_string("../../../openapi-specs/generated/cloudapi-api.json")
        .expect("Failed to read OpenAPI spec");

    let file = progenitor::Generator::new(
        progenitor::GenerationSettings::default()
            .with_interface(progenitor::InterfaceStyle::Builder)
            .with_tag(progenitor::TagStyle::Merged)
            .with_inner_type(syn::parse_quote!(triton_auth::AuthState))
            .with_pre_hook_async(syn::parse_quote!(crate::auth::add_auth_headers))
    )
    .generate_text(&serde_json::from_str(&spec).unwrap())
    .unwrap();

    let out_dir = std::env::var("OUT_DIR").unwrap();
    std::fs::write(format!("{}/client.rs", out_dir), file).unwrap();
}
```

### Task 9: Create Authenticated Client Wrapper

Add to `clients/internal/cloudapi-client/src/lib.rs`:

```rust
pub mod auth;

/// Create a client with authentication
pub struct AuthenticatedClient {
    inner: TypedClient,
    auth_state: triton_auth::AuthState,
}

impl AuthenticatedClient {
    pub fn new(base_url: &str, auth_state: triton_auth::AuthState) -> Self {
        Self {
            inner: TypedClient::new_with_state(base_url, auth_state.clone()),
            auth_state,
        }
    }

    pub fn inner(&self) -> &TypedClient {
        &self.inner
    }

    pub fn auth_state(&self) -> &triton_auth::AuthState {
        &self.auth_state
    }
}
```

### Task 10: Write Integration Test

Create `libs/triton-auth/tests/integration_test.rs`:

**Test Vectors from node-smartdc-auth** (target/node-smartdc-auth/test/signers.test.js):

```rust
use triton_auth::{AuthState, KeySource, RequestSigner, KeyType};

/// Test key fingerprints from node-smartdc-auth test suite
/// RSA key (id_rsa): fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6 (MD5)
/// DSA key (id_dsa): 60:66:49:45:e1:91:6f:47:a5:e0:7c:28:0e:99:39:ff (MD5)

#[tokio::test]
async fn test_signature_generation() {
    let signer = RequestSigner::new(
        "testaccount",
        "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
        KeyType::Rsa,
    );

    let date = "Mon, 15 Dec 2025 10:30:00 GMT";
    let signing_string = signer.signing_string("GET", "/testaccount/machines", date);

    assert!(signing_string.contains("date: Mon, 15 Dec 2025"));
    assert!(signing_string.contains("(request-target): get /testaccount/machines"));
}

#[tokio::test]
async fn test_authorization_header_format() {
    let signer = RequestSigner::new(
        "testaccount",
        "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
        KeyType::Rsa,
    );
    let auth = signer.authorization_header("dGVzdHNpZ25hdHVyZQ==");

    assert!(auth.starts_with("Signature keyId=\"/testaccount/keys/"));
    assert!(auth.contains("algorithm=\"rsa-sha256\""));
    assert!(auth.contains("signature=\"dGVzdHNpZ25hdHVyZQ==\""));
}

#[tokio::test]
async fn test_subuser_key_id() {
    let signer = RequestSigner::new(
        "mainaccount",
        "fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6",
        KeyType::Rsa,
    ).with_subuser("subuser");

    let key_id = signer.key_id_string();
    assert_eq!(key_id, "/mainaccount/users/subuser/keys/fa:56:a1:6b:cc:04:97:fe:e2:98:54:c4:2e:0d:26:c6");
}

/// Known test vector from node-smartdc-auth (signers.test.js lines 24-31)
/// Data: "foobar"
/// Key: test/keys/id_rsa (1024-bit RSA)
/// Expected signature (rsa-sha256):
/// "KX1okEE5wWjgrDYM35z9sO49WRk/DeZy7QeSNCFdOsn45BO6rVOIH5vV7WD25/VWyGCiN86Pml/Eulhx3Xx4ZUEHHc18K0BAKU5CSu/jCRI0dEFt4q1bXCyM7aKFlAXpk7CJIM0Gx91CJEXcZFuUddngoqljyt9hu4dpMhrjVFA="
#[tokio::test]
async fn test_rsa_signature_against_known_vector() {
    // This test requires the actual test key from node-smartdc-auth/test/keys/id_rsa
    // Copy test keys to libs/triton-auth/tests/keys/ for testing
    let key_path = std::path::Path::new("tests/keys/id_rsa");
    if !key_path.exists() {
        eprintln!("Skipping test - test key not found. Copy from target/node-smartdc-auth/test/keys/");
        return;
    }

    let key = triton_auth::KeyLoader::load_from_file(key_path, None).unwrap();
    let data = b"foobar";
    let signature = triton_auth::signature::sign_with_key(&key, data).unwrap();

    // Expected signature from node-smartdc-auth test
    let expected = "KX1okEE5wWjgrDYM35z9sO49WRk/DeZy7QeSNCFdOsn45BO6rVOIH5vV7WD25/VWyGCiN86Pml/Eulhx3Xx4ZUEHHc18K0BAKU5CSu/jCRI0dEFt4q1bXCyM7aKFlAXpk7CJIM0Gx91CJEXcZFuUddngoqljyt9hu4dpMhrjVFA=";
    assert_eq!(signature, expected);
}
```

**Copy test keys for verification:**
```bash
mkdir -p libs/triton-auth/tests/keys
cp target/node-smartdc-auth/test/keys/id_rsa libs/triton-auth/tests/keys/
cp target/node-smartdc-auth/test/keys/id_rsa.pub libs/triton-auth/tests/keys/
cp target/node-smartdc-auth/test/keys/id_dsa libs/triton-auth/tests/keys/
cp target/node-smartdc-auth/test/keys/id_dsa.pub libs/triton-auth/tests/keys/
```

## Verification

After completing all tasks:

1. Run `cargo build -p triton-auth` - should compile
2. Run `cargo test -p triton-auth` - tests should pass
3. Run `cargo build -p cloudapi-client` - should compile with auth support
4. Run `cargo audit` - no new vulnerabilities

## Files Created/Modified

### New Files
- `libs/triton-auth/Cargo.toml`
- `libs/triton-auth/src/lib.rs`
- `libs/triton-auth/src/error.rs`
- `libs/triton-auth/src/key_loader.rs`
- `libs/triton-auth/src/agent.rs`
- `libs/triton-auth/src/fingerprint.rs`
- `libs/triton-auth/src/signature.rs`
- `libs/triton-auth/tests/integration_test.rs`
- `clients/internal/cloudapi-client/src/auth.rs`

### Modified Files
- `Cargo.toml` (workspace members)
- `clients/internal/cloudapi-client/Cargo.toml`
- `clients/internal/cloudapi-client/build.rs`
- `clients/internal/cloudapi-client/src/lib.rs`
