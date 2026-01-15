# Phase 2: Update Crate

## Objective

Update the crate's dependencies and fix code to compile with modern versions.

## Steps

### 2.1 Update Cargo.toml

Create a new Cargo.toml with:

```toml
# Add license header
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. ...
# Copyright 2019 Joyent, Inc.  (keep original)
# Copyright 2026 Edgecast Cloud LLC.  (add new)

[package]
name = "<crate-name>"
version = "<bump-version>"  # e.g., 0.3.0 → 0.4.0
authors = [...]  # keep original
edition.workspace = true  # inherits "2024"

[dependencies]
# Use workspace deps where available:
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
clap = { workspace = true }

# Update others to modern versions:
bytes = "1"
tokio-util = { version = "0.7", features = ["codec"] }
futures = "0.3"
quickcheck = "1.0"
# etc.

[dev-dependencies]
# Similar updates
```

**Key patterns:**
- `edition.workspace = true` - inherits "2024" from workspace
- Use workspace deps when available (check root Cargo.toml)
- Remove obsolete deps: `tokio-codec`, `tokio-io`, `serde_derive`
- Add new deps if needed: `tokio-util`, `futures`

### 2.2 Enable in Workspace

Edit root `Cargo.toml`:

1. Add crate to "Modernized" section
2. Remove from "To be modernized" commented list

```toml
    # Modernized:
    "libs/fast",
    "libs/<your-crate>",  # ADD HERE

    # To be modernized:
    # Remove the commented line for your crate
```

### 2.3 Initial Build Attempt

```bash
make package-build PACKAGE=<crate-name>
```

Expect errors. This shows you exactly what needs fixing.

### 2.4 Fix Compilation Errors

Work through errors iteratively. See `reference.md` for pattern mappings.

**Common error categories:**

1. **Import errors** (`E0432`):
   - `tokio::prelude` → remove
   - `tokio::codec` → `tokio_util::codec`
   - `serde_derive` → `serde`

2. **Type errors** (`E0107`, `E0220`):
   - `Future<Item=X, Error=Y>` → async fn or `impl Future<Output=Result<X,Y>>`
   - `Encoder { type Item }` → `Encoder<Item>`

3. **Method errors** (`E0599`):
   - `put_u32_be()` → `put_slice(&x.to_be_bytes())`
   - `g.gen::<T>()` → `T::arbitrary(g)`
   - `hasher.result()` → `hasher.finalize()`

4. **Reserved keyword** (`gen` in edition 2024):
   - Any use of `gen` as identifier must change

### 2.5 Async/Await Migration (if needed)

For tokio 0.1 → 1.x, the main changes are:

**Before:**
```rust
pub fn make_task(...) -> impl Future<Item = (), Error = ()> + Send {
    stream.and_then(|x| process(x)).then(|_| Ok(()))
}
```

**After:**
```rust
pub async fn handle_connection(...) -> Result<(), Error> {
    while let Some(result) = stream.next().await {
        let x = result?;
        process(x)?;
    }
    Ok(())
}
```

Key changes:
- `impl Future<Item=X, Error=Y>` → `async fn ... -> Result<X, Y>`
- Combinator chains → `?` operator + loops
- `Box::new(future::ok(x))` → just `Ok(x)`
- Add `use futures::{SinkExt, StreamExt};` for stream methods

### 2.6 Update Examples (if present)

Examples often need:
- clap 2.x → 4.x derive API
- tokio 0.1 patterns → async/await
- `tokio::run()` → `#[tokio::main]`

### 2.7 Rebuild Until Clean

```bash
make package-build PACKAGE=<crate-name>
```

Repeat steps 2.4-2.6 until build succeeds with no errors.

### 2.8 Fix Error Handling Issues

**Replace panicking patterns with proper error handling:**

**unwrap() on JSON serialization:**
```rust
// Before (panics on serialization failure)
let data_str = serde_json::to_string(&msg.data).unwrap();

// After (propagates error)
let data_str = serde_json::to_string(&msg.data)
    .map_err(|e| Error::other(format!("Failed to serialize: {}", e)))?;
```

**expect() on user input:**
```rust
// Before (panics on bad input - DoS vulnerability!)
let arr = value.as_array().expect("should be array");

// After (returns error)
let arr = value.as_array()
    .ok_or_else(|| other_error("Expected JSON array"))?;
```

**Preserve error context in map_err:**
```rust
// Before (loses original error)
serde_json::from_value(data).map_err(|_| unspecified_error())

// After (includes original error)
serde_json::from_value(data).map_err(|e| {
    Error::other(format!("Failed to parse: {}", e))
})
```

**Use if-let instead of is_some() + expect():**
```rust
// Before (redundant, error-prone)
if value.is_some() {
    let v = value.expect("checked above");
    use_value(v);
}

// After (idiomatic)
if let Some(v) = value {
    use_value(v);
}
```

### 2.9 Apply Simplifications

**Replace manual Vec capacity management:**
```rust
// Before (unnecessary complexity)
if responses.len() + response.len() > responses.capacity() {
    responses.reserve(response.len());
}
response.drain(..).for_each(|r| responses.push(r));

// After (Vec handles this efficiently)
responses.append(&mut response);
```

**Simplify encoder error handling:**
```rust
// Before (allocates two Vecs, doesn't fail fast)
let results: Vec<Result<(), String>> = item.iter().map(|x| encode(x, buf)).collect();
let result: Result<Vec<()>, String> = results.iter().cloned().collect();

// After (fails fast, no allocations)
for msg in &item {
    encode(msg, buf).map_err(Error::other)?;
}
Ok(())
```

**Remove unnecessary clone() calls:**
```rust
// Before (unnecessary clone)
barrier.clone().wait();

// After (Arc methods work on &self)
barrier.wait();
```

## Output

At the end of this phase:
- Cargo.toml is updated
- All source files compile
- Examples compile (if any)
- **Error handling issues fixed (from Phase 1 analysis)**
- **Code simplified where applicable**

Proceed to Phase 3 for validation.
