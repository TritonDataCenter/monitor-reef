# triton-auth

HTTP Signature authentication library for Triton CloudAPI, providing SSH key-based request signing compatible with [node-smartdc-auth](https://github.com/TritonDataCenter/node-smartdc-auth).

## Overview

This library implements the HTTP Signature authentication scheme used by Triton CloudAPI:

```
Authorization: Signature keyId="/:account/keys/:fingerprint",algorithm="rsa-sha256",signature=":base64:"
```

The signature is computed over:
```
date: <RFC2822 date>
(request-target): <method lowercase> <path>
```

## Features

### Key Format Support

| Format | Header | Status | Notes |
|--------|--------|--------|-------|
| OpenSSH | `-----BEGIN OPENSSH PRIVATE KEY-----` | ✅ Supported | Native ssh-key crate support |
| PKCS#1 RSA | `-----BEGIN RSA PRIVATE KEY-----` | ✅ Supported | Via legacy_pem module |
| SEC1 ECDSA | `-----BEGIN EC PRIVATE KEY-----` | ✅ Supported | P-256 and P-384 curves |
| DSA | `-----BEGIN DSA PRIVATE KEY-----` | ✅ Supported | Via legacy_pem module |
| PKCS#8 | `-----BEGIN PRIVATE KEY-----` | ✅ Supported | RSA and ECDSA |
| Encrypted PKCS#1 | `Proc-Type: 4,ENCRYPTED` | ❌ Not supported | Convert to OpenSSH format |
| Encrypted OpenSSH | `-----BEGIN OPENSSH PRIVATE KEY-----` | ✅ Supported | With passphrase |

### Algorithm Support

| Algorithm | HTTP Signature String | Status | Notes |
|-----------|----------------------|--------|-------|
| RSA-SHA256 | `rsa-sha256` | ✅ Supported | Default for RSA keys |
| RSA-SHA1 | `rsa-sha1` | ❌ Not supported | Use RSA-SHA256 instead |
| DSA-SHA1 | `dsa-sha1` | ✅ Supported | |
| ECDSA-SHA256 | `ecdsa-sha256` | ✅ Supported | P-256 curve |
| ECDSA-SHA384 | `ecdsa-sha384` | ✅ Supported | P-384 curve |
| ECDSA-SHA512 | `ecdsa-sha512` | ✅ Supported | P-521 curve |
| Ed25519-SHA512 | `ed25519-sha512` | ✅ Supported | OpenSSH format only |

### Key Sources

| Source | Status | Notes |
|--------|--------|-------|
| File (explicit path) | ✅ Supported | Any supported format |
| SSH Agent | ✅ Supported | Via ssh-agent-client-rs |
| Auto-detect (~/.ssh/) | ✅ Supported | Searches common key names |

### Fingerprint Support

| Format | Status | Notes |
|--------|--------|-------|
| MD5 (colon-separated) | ✅ Supported | `aa:bb:cc:...` - CloudAPI format |
| MD5 (plain hex) | ✅ Supported | `aabbcc...` |
| MD5 (with prefix) | ✅ Supported | `MD5:aa:bb:cc:...` |
| SHA256 | ⚠️ Partial | Parsing only, not generation |

## Comparison with node-smartdc-auth

### Functional Parity

| Feature | node-smartdc-auth | triton-auth | Notes |
|---------|-------------------|-------------|-------|
| RSA signing | ✅ SHA1 + SHA256 | ⚠️ SHA256 only | SHA1 deprecated |
| DSA signing | ✅ | ✅ | |
| ECDSA signing | ✅ | ✅ | |
| Ed25519 signing | ✅ | ✅ | OpenSSH format |
| SSH agent signing | ✅ | ✅ | |
| File-based signing | ✅ | ✅ | |
| Encrypted key support | ✅ | ⚠️ OpenSSH only | PKCS#1 encrypted not supported |
| PKCS#1/SEC1 format | ✅ | ✅ | |
| MD5 fingerprints | ✅ | ✅ | |
| Subuser support | ✅ | ✅ | RBAC |
| KeyRing abstraction | ✅ | ❌ | Different architecture |
| Custom signer callback | ✅ | ❌ | Different architecture |

### API Differences

**node-smartdc-auth** uses a callback-based API:
```javascript
var sign = auth.cliSigner({ keyId: fp, user: 'foo' });
sign('data', function(err, sigData) {
    // sigData.signature, sigData.algorithm, sigData.keyId
});
```

**triton-auth** uses a direct API:
```rust
let key = KeyLoader::load_legacy_from_file(&path, None)?;
let sig_bytes = key.sign(data)?;
let signature = encode_signature(&sig_bytes);
```

## Test Coverage

Tests are organized to mirror the original node-smartdc-auth test structure:

| Original Test File | Rust Test File | Tests | Coverage |
|--------------------|----------------|-------|----------|
| `signers.test.js` | `tests/signers_test.rs` | 10 | Signature generation, request signing |
| `fs-keys.test.js` | `tests/fs_keys_test.rs` | 14 | File loading, fingerprints, formats |
| `agent-keys.test.js` | `tests/agent_keys_test.rs` | 8 | SSH agent integration |
| (unit tests) | `src/*.rs` | 17 | Internal module tests |

**Total: 49 tests**

### Test Vector Compatibility

The critical `test_basic_signer_rsa` test verifies exact signature compatibility with node-smartdc-auth:

```rust
// Known signature for "foobar" with id_rsa using RSA-SHA256
// From signers.test.js line 25-27
const SIG_RSA_SHA256: &str = "KX1okEE5wWjgrDYM35z9sO49WRk/DeZy7QeSNCFdOsn45BO6rVOIH5v...";

#[test]
fn test_basic_signer_rsa() {
    let key = KeyLoader::load_legacy_from_file(&key_path, None)?;
    let sig_bytes = key.sign(b"foobar")?;
    let signature = encode_signature(&sig_bytes);
    assert_eq!(signature, SIG_RSA_SHA256); // Exact match!
}
```

### Ported Tests

| Test Category | node-smartdc-auth | triton-auth | Status |
|---------------|-------------------|-------------|--------|
| Basic RSA signing | ✅ | ✅ | Exact signature match |
| Basic DSA signing | ✅ | ✅ | Format verified |
| Basic ECDSA signing | ✅ | ✅ | Format verified |
| RSA-SHA1 signing | ✅ | ❌ | Not implemented |
| RSA with subuser | ✅ | ✅ | KeyId format verified |
| RSA fingerprint (MD5) | ✅ | ✅ | |
| DSA fingerprint (MD5) | ✅ | ✅ | |
| ECDSA fingerprint (MD5) | ✅ | ✅ | |
| Load RSA key | ✅ | ✅ | |
| Load DSA key | ✅ | ✅ | |
| Load ECDSA key | ✅ | ✅ | |
| Encrypted key error | ✅ | ✅ | |
| Encrypted key unlock | ✅ | ⚠️ | OpenSSH only |
| Invalid fingerprint error | ✅ | ✅ | |
| Unknown key error | ✅ | ✅ | |
| Agent RSA signing | ✅ | ✅ | Graceful skip if no agent |
| Agent DSA signing | ✅ | ✅ | Graceful skip if no agent |
| Agent ECDSA signing | ✅ | ✅ | Graceful skip if no agent |
| Agent key not found | ✅ | ✅ | |
| Agent with no socket | ✅ | ✅ | |
| RequestSigner format | ✅ | ✅ | |
| Algorithm strings | ✅ | ✅ | |
| 40-key stress test | ✅ | ❌ | Not ported |
| KeyRing list keys | ✅ | ❌ | Different architecture |
| Custom signer callback | ✅ | ❌ | Different architecture |

### Not Ported (By Design)

1. **RSA-SHA1 algorithm**: Deprecated; SHA256 is the modern default
2. **KeyRing abstraction**: Rust uses different patterns (explicit key loading)
3. **Custom signer callbacks**: Node.js callback pattern not applicable
4. **40-key stress test**: Tests agent performance, not library functionality
5. **Plugin system**: Different architecture approach

## Usage

### Basic File-Based Signing

```rust
use triton_auth::{KeyLoader, signature::{encode_signature, KeyType, RequestSigner}};

// Load key from file (any supported format)
let key = KeyLoader::load_legacy_from_file("~/.ssh/id_rsa", None)?;

// Create request signer
let signer = RequestSigner::new("myaccount", "aa:bb:cc:...", KeyType::Rsa);

// Generate signing string
let date = RequestSigner::date_header();
let signing_string = signer.signing_string("GET", "/myaccount/machines", &date);

// Sign and encode
let sig_bytes = key.sign(signing_string.as_bytes())?;
let signature = encode_signature(&sig_bytes);

// Generate Authorization header
let auth_header = signer.authorization_header(&signature);
```

### SSH Agent Signing

```rust
use triton_auth::agent;

let fingerprint = "aa:bb:cc:...";

// Find key in agent
let pub_key = agent::find_key_in_agent(fingerprint).await?;

// Sign with agent
let sig_bytes = agent::sign_with_agent(fingerprint, data).await?;
```

### High-Level API

```rust
use triton_auth::{AuthConfig, KeySource, sign_request};

let config = AuthConfig::new(
    "myaccount",
    "aa:bb:cc:...",
    KeySource::auto("aa:bb:cc:..."),  // Try agent, fall back to file
);

let (date_header, auth_header) = sign_request(&config, "GET", "/myaccount/machines").await?;
```

## Converting Encrypted Keys

If you have an encrypted PKCS#1 key (`Proc-Type: 4,ENCRYPTED`), convert it to OpenSSH format:

```bash
# Convert to OpenSSH format (will prompt for old and new passphrase)
ssh-keygen -p -o -f ~/.ssh/id_rsa

# Or generate a new OpenSSH format key
ssh-keygen -t rsa -o -f ~/.ssh/id_rsa_new
```

## Known Limitations

1. **RSA-SHA1 not supported**: Only RSA-SHA256 is implemented. This matches modern security practices.

2. **Encrypted PKCS#1 keys**: Traditional encrypted PEM keys with `Proc-Type: 4,ENCRYPTED` header are not supported. Convert to OpenSSH format.

3. **P-521 ECDSA from SEC1**: Only P-256 and P-384 are supported for SEC1 format. P-521 requires OpenSSH format.

4. **SHA256 fingerprints**: While SHA256 fingerprints can be parsed, MD5 is used for CloudAPI compatibility.

## Security Notes

- **RUSTSEC-2023-0071**: The `rsa` crate has a known timing side-channel vulnerability (Marvin Attack). This affects RSA decryption, not signing. For HTTP Signature authentication (signing only), the risk is minimal. Monitor for updates.

- **DSA deprecation**: DSA is considered weak by modern standards. Prefer RSA or ECDSA for new keys.

## License

MPL-2.0

## References

- [node-smartdc-auth](https://github.com/TritonDataCenter/node-smartdc-auth) - Original Node.js implementation
- [Triton CloudAPI Authentication](https://apidocs.tritondatacenter.com/cloudapi/#authentication)
- [HTTP Signatures](https://datatracker.ietf.org/doc/html/draft-cavage-http-signatures)
