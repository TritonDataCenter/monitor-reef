<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Plan: Triton CLI Remaining Compatibility Gaps

**Created:** 2025-12-16
**Status:** Completed

## Summary

After thorough exploration, **most of the previously identified gaps are already implemented**. The actual remaining work is minimal.

## Gap Analysis Results

### Already Implemented (No Work Needed)

| Item | Status | Evidence |
|------|--------|----------|
| `image list -a/--all` | Done | image.rs:93-94 - filters to "active" unless `--all` |
| `image create --homepage` | Done | image.rs:138 |
| `image create --eula` | Done | image.rs:141 |
| `image create --acl` | Done | image.rs:143-144 |
| `image create --tag` | Done | image.rs:146-147 |
| `image create --dry-run` | Done | image.rs:152-153, 576-599 |
| `image copy DATACENTER` | Done | image.rs:179 - positional `#[arg(index = 2)]` + `--source` |
| `volume create --wait` | Done | volume.rs:64-65 - count-based action |
| `volume create --wait-timeout` | Done | volume.rs:68-69 |
| `volume create --tag` | Done | volume.rs:56-57, 177-198 |
| `volume create --affinity` | Done | volume.rs:60-61 (warns API unsupported) |
| Size parsing "20G" | Done | volume.rs:157-175 `parse_volume_size()` |
| `profile create --copy` | Done | profile.rs:58, 293-300 |
| `profile create -f/--file` | Done | profile.rs:55, 248-290 |
| `profile create --no-docker` | Done | profile.rs:61 (accepted, skipped) |

### Actual Remaining Gaps

Only **2 items** need implementation:

#### 1. RBAC Apply: `-f/--file` Flag Syntax with Default (P3)

**Current:** `file: PathBuf` is positional (required)
**Needed:** `-f/--file FILE` flag with `./rbac.json` default

**File:** `cli/triton-cli/src/commands/rbac/apply.rs`

**Change:**
```rust
// Before (line 22-23):
/// Path to RBAC configuration file (JSON format)
pub file: PathBuf,

// After:
/// Path to RBAC configuration file (JSON format, default: ./rbac.json)
#[arg(short = 'f', long = "file", default_value = "./rbac.json")]
pub file: PathBuf,
```

#### 2. RBAC Apply: `--dev-create-keys-and-profiles` Implementation (P3)

**Current:** Flag exists but returns "not implemented" error
**Needed:** Generate SSH keys + create CLI profiles for each user

**File:** `cli/triton-cli/src/commands/rbac/apply.rs`

**Implementation:**
1. For each user without keys in config: generate SSH keypair, add to plan
2. For each user: create/update CLI profile named `{current_profile}-user-{login}`
3. Profile contains: url, insecure, account from current profile + user login + key fingerprint

**Complexity:** Medium - requires SSH key generation and profile file writing

---

## Implementation Plan

### Task 1: RBAC Apply `-f/--file` Flag (5 min)

**Files to modify:**
- `cli/triton-cli/src/commands/rbac/apply.rs`

**Steps:**
1. Change `file` field from positional to flag with default
2. Update help text to mention default
3. Test with `triton rbac apply` (should use ./rbac.json)
4. Test with `triton rbac apply -f custom.json`

### Task 2: `--dev-create-keys-and-profiles` Implementation (30-60 min)

**Files to modify:**
- `cli/triton-cli/src/commands/rbac/apply.rs`

**Existing patterns to reuse:**
- `keys.rs:add_key_from_file()` - uploads SSH key via `create_user_key()` API
- `profile.rs:Profile::save()` - saves profile to `~/.triton/profiles.d/`
- `profile.rs:Profile` struct - profile data structure

**Implementation Steps:**

1. **Add SSH key generation function** (shell out to `ssh-keygen`):
   ```rust
   async fn generate_ssh_key(user_login: &str, profile_name: &str) -> Result<(String, String, String)> {
       // Returns: (private_key_path, public_key_content, fingerprint)
       let key_dir = dirs::home_dir().unwrap().join(".triton").join("dev-keys");
       std::fs::create_dir_all(&key_dir)?;
       let key_path = key_dir.join(format!("{}-{}", profile_name, user_login));

       // ssh-keygen -t ed25519 -N "" -f key_path -C "triton-dev-key"
       let output = Command::new("ssh-keygen")
           .args(["-t", "ed25519", "-N", "", "-f", key_path.to_str().unwrap(), "-C", &format!("{}-dev", user_login)])
           .output()?;

       let pub_key = std::fs::read_to_string(format!("{}.pub", key_path.display()))?;
       // Extract fingerprint from pub key
       Ok((key_path.to_string_lossy().to_string(), pub_key, fingerprint))
   }
   ```

2. **Extend plan items to include key generation**:
   ```rust
   enum DevPlanAction {
       GenerateKey { user_login: String },
       UploadKey { user_login: String, key_name: String, key_content: String },
       CreateProfile { profile_name: String, user_login: String, key_fingerprint: String },
   }
   ```

3. **For each user without keys** (after RBAC config load):
   - Generate SSH keypair via `ssh-keygen`
   - Add to plan: upload public key to CloudAPI
   - Add to plan: create profile `{current_profile}-user-{login}`

4. **Execute dev plan items** (after RBAC apply):
   - Call `create_user_key()` API for each key upload
   - Create profile using `Profile { ... }.save()` pattern

5. **Profile structure** (from profile.rs):
   ```rust
   Profile {
       url: current_profile.url.clone(),
       account: current_profile.account.clone(),
       key_id: uploaded_key_fingerprint,
       insecure: current_profile.insecure,
       user: Some(user_login.to_string()),
       roles: None,
       act_as_account: None,
   }
   ```

**Output format** (matching node-triton):
```
Create user alice key
Create user alice CLI profile (coal-user-alice)
Create user bob key
Create user bob CLI profile (coal-user-bob)
```

**Note:** Keys stored in `~/.triton/dev-keys/` for easy cleanup.

---

## Implementation Order

1. **Task 1** - Simple flag change (5 min)
2. **Task 2** - Full implementation of dev key/profile generation (30-60 min)

---

## Verification Commands

After implementation:
```bash
# Build
make package-build PACKAGE=triton-cli

# Test Task 1
cd /tmp && echo '{"users":[],"roles":[],"policies":[]}' > rbac.json
triton rbac apply --dry-run  # Should find ./rbac.json
triton rbac apply -f rbac.json --dry-run  # Explicit file

# Test Task 2 (requires configured profile with valid credentials)
cat > rbac.json <<EOF
{
  "users": [{"login": "testuser", "email": "test@example.com"}],
  "roles": [],
  "policies": []
}
EOF
triton rbac apply --dev-create-keys-and-profiles --dry-run
# Should show: "Generate key for user testuser", "Create profile my-profile-user-testuser"

# Full test
make package-test PACKAGE=triton-cli
```

---

## Post-Implementation

After implementation, update `conversion-plans/triton/reports/evaluation-report-2025-12-16.md`:
- Move "RBAC apply `-f/--file` flag" from P3 to Completed
- Move "--dev-create-keys-and-profiles" from P3 to Completed
- Update option compatibility percentage to ~95%
