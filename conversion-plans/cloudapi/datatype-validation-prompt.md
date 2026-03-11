<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Agent Prompt: Validate CloudAPI Response/Request Datatypes

## Objective

Systematically validate that all request and response types in the Rust CloudAPI trait
(`apis/cloudapi-api/src/types/`) correctly match the actual JSON structures returned by
the Node.js CloudAPI implementation. Fix any type mismatches discovered.

## Background

We discovered that `list_services` and `list_datacenters` returned maps (`HashMap<String, String>`)
but our Rust types incorrectly defined them as arrays (`Vec<Service>` and `Vec<Datacenter>`).
This validation ensures all other types are correct.

### Case Study: How We Found the Services/Datacenters Bug

The bug was discoverable by reading the Node.js source code. Here's what we found:

**`lib/services.js` (line 29)**:
```javascript
function list(req, res, next) {
    assert.ok(req.config);
    if (req.accountMgmt) {
        resources.getRoleTags(req, res);
    }
    res.send(req.config.services || {});  // <-- KEY: sends config object directly
    next();
}
```

**`lib/datacenters.js` (lines 43-50)**:
```javascript
function list(req, res, next) {
    var datacenters = req.config.datacenters || {};  // <-- KEY: object from config
    res.send(datacenters);  // <-- KEY: sends object directly, no transformation
    return next();
}
```

**What to look for**:
1. `res.send(someObject || {})` - The `|| {}` fallback indicates an object/map, not an array
2. No `translate()` or mapping function - data sent directly from config
3. Config structure is `{ "name": "url", ... }` not `[{ name: "...", url: "..." }]`

## Source Materials

- **Node.js CloudAPI source**: `target/sdc-cloudapi/lib/` - **PRIMARY SOURCE FOR VALIDATION**
- **Current Rust API types**: `apis/cloudapi-api/src/types/`
- **Current Rust API trait**: `apis/cloudapi-api/src/lib.rs`
- **Live API responses**: Use cloudapi-cli with `--raw` flag (secondary validation)

## Validation Methodology

**IMPORTANT**: The primary validation method is reading the Node.js source code. Live API
testing is secondary confirmation, not the primary source of truth.

For each endpoint:

### Step 1: Find the Node.js Handler

Locate the route handler in `target/sdc-cloudapi/lib/`. The file structure maps to endpoints:
- `lib/services.js` → `GET /{account}/services`
- `lib/datacenters.js` → `GET /{account}/datacenters`
- `lib/machines.js` → `GET /{account}/machines`, etc.

### Step 2: Trace the Response Data

In each handler function, look for:

1. **`res.send()` calls** - What exactly is being sent?
   ```javascript
   res.send(datacenters);           // Sends variable directly
   res.send(req.config.services);   // Sends config value directly
   res.send(machines.map(translate)); // Transforms before sending
   ```

2. **`translate()` functions** - How is internal data transformed?
   ```javascript
   function translate(machine) {
       return {
           id: machine.uuid,
           name: machine.alias,
           // ... field mappings
       };
   }
   ```

3. **Default/fallback values** - These reveal the expected type:
   ```javascript
   req.config.services || {}   // Object/map (empty object default)
   machines || []              // Array (empty array default)
   ```

4. **Object construction patterns**:
   ```javascript
   // Map pattern - keys are dynamic
   var result = {};
   items.forEach(item => { result[item.name] = item.url; });

   // Array pattern - returns list
   var result = items.map(item => ({ id: item.id, name: item.name }));
   ```

### Step 3: Compare with Rust Type

Check the Rust type definition matches what the Node.js code actually sends.

### Step 4: Fix Mismatches

Update Rust types to match the actual API behavior.

## Endpoints to Validate

### Priority 1: Likely Map-Like Responses

Look for patterns like `res.send(config.something || {})`:

| Endpoint | Node.js File | Look For |
|----------|--------------|----------|
| `GET /{account}/config` | `lib/endpoints/config.js` | Is response a struct or `HashMap`? |
| `GET /{account}/machines/{id}/metadata` | `lib/metadata.js` | Check if `HashMap<String, String>` |
| `GET /{account}/machines/{id}/tags` | `lib/tags.js` | Check if `HashMap<String, String>` |

### Priority 2: Responses with translate() Functions

Look for `items.map(translate)` patterns - verify struct fields match:

| Endpoint | Node.js File | Check translate() Function |
|----------|--------------|---------------------------|
| `GET /{account}/machines` | `lib/machines.js` | Verify `Machine` struct fields |
| `GET /{account}/images` | `lib/images.js` or `lib/datasets.js` | Verify `Image` struct fields |
| `GET /{account}/packages` | `lib/packages.js` | Verify `Package` struct fields |
| `GET /{account}/networks` | `lib/networks.js` | Verify `Network` struct fields |
| `GET /{account}/volumes` | `lib/endpoints/volumes.js` | Verify `Volume` struct fields |

### Priority 3: RBAC Types

| Endpoint | Node.js File | What to Check |
|----------|--------------|---------------|
| `GET /{account}/users` | `lib/users.js` | User struct fields |
| `GET /{account}/roles` | `lib/roles.js` | Role struct fields |
| `GET /{account}/policies` | `lib/policies.js` | Policy struct fields |

### Priority 4: All Other Endpoints

Systematically review each file in `target/sdc-cloudapi/lib/`:

| Node.js File | Rust Type File | Endpoints |
|--------------|----------------|-----------|
| `lib/account.js` | `account.rs` | Account operations |
| `lib/keys.js` | `key.rs` | SSH key operations |
| `lib/endpoints/accesskeys.js` | `key.rs` | Access key operations |
| `lib/rules.js` | `firewall.rs` | Firewall rules |
| `lib/nics.js` | `machine_resources.rs` | NIC operations |
| `lib/snapshots.js` | `machine_resources.rs` | Snapshot operations |
| `lib/endpoints/disks.js` | `machine_resources.rs` | Disk operations |
| `lib/migrations.js` | `misc.rs` | Migration operations |

## Code Patterns to Watch For

### Pattern 1: Direct Config Send (Map Type)
```javascript
// Node.js
res.send(req.config.datacenters || {});
```
```rust
// Rust - should be HashMap
pub type Datacenters = HashMap<String, String>;
// NOT Vec<Datacenter>
```

### Pattern 2: Array with Transform (Vec Type)
```javascript
// Node.js
function translate(m) { return { id: m.uuid, name: m.alias }; }
res.send(machines.map(translate));
```
```rust
// Rust - should be Vec<Struct>
pub struct Machine { pub id: Uuid, pub name: String }
// Response: Vec<Machine>
```

### Pattern 3: Single Object Response
```javascript
// Node.js
res.send({ id: account.uuid, login: account.login });
```
```rust
// Rust - single struct
pub struct Account { pub id: Uuid, pub login: String }
```

### Pattern 4: Optional Fields
```javascript
// Node.js - field may be omitted
var result = { id: m.uuid };
if (m.alias) result.name = m.alias;
res.send(result);
```
```rust
// Rust - use Option
pub struct Machine {
    pub id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}
```

### Pattern 5: Dynamic Keys (HashMap)
```javascript
// Node.js - keys are from data, not schema
var meta = {};
Object.keys(vm.customer_metadata).forEach(k => { meta[k] = vm.customer_metadata[k]; });
res.send(meta);
```
```rust
// Rust - HashMap, not struct
pub type Metadata = HashMap<String, String>;
```

## Validation Checklist Template

For each type, document:

```markdown
### TypeName (file.rs)

**Endpoint**: `GET /{account}/resource`

**Node.js source**: `lib/resource.js`

**Key code** (from Node.js):
```javascript
// Line XX - shows what's actually sent
res.send(items.map(translate));

// translate function shows field structure
function translate(item) {
    return {
        id: item.uuid,
        name: item.name,
        created: item.created_at
    };
}
```

**Current Rust definition**:
```rust
pub struct TypeName {
    pub id: Uuid,
    pub name: String,
    pub created: Timestamp,
}
```

**Status**: ✅ Correct / ❌ Needs fix

**Evidence**: (explain why it's correct or what needs fixing)
```

## Output

Create/update `conversion-plans/cloudapi/datatype-validation-report.md` with:

1. Summary of types validated
2. For each type: Node.js code evidence and Rust comparison
3. List of discrepancies found with fixes
4. Any types that remain uncertain

## Verification Steps

After making fixes:

1. **Regenerate OpenAPI spec**:
   ```bash
   make openapi-generate
   ```

2. **Rebuild everything**:
   ```bash
   make build
   ```

3. **Run tests**:
   ```bash
   make test
   ```

4. **Test against live API** (secondary confirmation):
   ```bash
   ./target/debug/cloudapi list-machines --raw
   ./target/debug/cloudapi list-images --raw
   ```

## Commit Strategy

Make atomic commits for each category of fixes:

1. `Fix metadata/tags types to use HashMap<String, String>`
2. `Fix nullable fields in Machine type`
3. `Add datatype validation report`

## Success Criteria

- [ ] All Node.js handler files in `target/sdc-cloudapi/lib/` reviewed
- [ ] All `res.send()` calls traced to understand actual response structure
- [ ] All `translate()` functions compared against Rust struct definitions
- [ ] Any map-vs-array mismatches fixed
- [ ] All field types verified (especially optional vs required)
- [ ] Validation report created with Node.js code evidence
- [ ] All tests pass
