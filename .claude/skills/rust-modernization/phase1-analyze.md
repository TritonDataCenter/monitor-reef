# Phase 1: Analyze Crate

## Objective

Understand the crate's current state and identify what needs to be modernized.

## Steps

### 1.1 Read Cargo.toml

```bash
# Read the crate's Cargo.toml
```

Identify:
- Current edition (likely "2018")
- Dependencies and their versions
- Dev-dependencies
- Features

### 1.2 Identify Outdated Dependencies

Look for these common outdated patterns:

| Dependency | Old Version | Modern Version | Complexity |
|------------|-------------|----------------|------------|
| tokio | 0.1.x | 1.x | High |
| tokio-codec | 0.1.x | (removed, use tokio-util) | Medium |
| tokio-io | 0.1.x | (removed, merged into tokio) | Low |
| bytes | 0.4.x | 1.x | Medium |
| quickcheck | 0.8.x | 1.0 | Medium |
| rand | 0.6.x | 0.8.x | Low |
| clap | 2.x | 4.x | Medium |
| serde_derive | (separate) | (merged into serde) | Low |
| base64 | 0.10.x | 0.22 | Low |
| md-5 | 0.8.x | 0.10 | Low |
| trust-dns-resolver | * | hickory-resolver | Medium |

### 1.3 Scan Source Files

```bash
# List all source files
```

Look for these patterns that indicate modernization needs:

**Tokio 0.1 patterns:**
- `use tokio::prelude::*;`
- `use tokio::codec::*;`
- `impl Future<Item=X, Error=Y>`
- `tokio::run(...)`
- `.and_then()`, `.then()`, `.map_err()` on futures

**bytes 0.4 patterns:**
- `put_u32_be()`, `put_u16_be()`
- `buf.put(string)` (String directly)

**quickcheck 0.8 patterns:**
- `fn arbitrary<G: Gen>(g: &mut G)`
- `g.gen::<T>()`
- `slice.choose(g)`

**Old clap patterns:**
- `App::new()`, `Arg::with_name()`
- `value_t!()` macro
- `ArgMatches<'a>`

### 1.4 Identify Dead Code (Library Crates)

**For library crates, deleting unused code is better than modernizing it.**

For each public function, type, and module in the crate:

1. Search for usages across the repo:
   ```bash
   # Search for imports of this crate's items
   rg "use crate_name::" --type rust
   rg "crate_name::" --type rust
   ```

2. Check Cargo.toml dependencies - which crates depend on this one?
   ```bash
   rg "crate-name" --glob "*/Cargo.toml"
   ```

3. For each public item, verify it's actually imported/used somewhere

**Decision matrix:**

| Used by other crates? | Action |
|-----------------------|--------|
| Yes, actively used | Modernize the code |
| No, never imported | Delete it |
| Only used in tests | Consider deleting or keep minimal |

**Benefits of deleting dead code:**
- Less code to modernize and maintain
- Smaller dependency footprint
- Cleaner API surface
- Faster builds

Document which items will be deleted vs modernized before proceeding.

### 1.5 Check for Examples

```bash
# Check if examples directory exists
```

Examples often use the same patterns and need updating too.

### 1.6 Check for Tests (VERIFY THEY RUN)

Look in:
- `src/` files for `#[cfg(test)]` modules
- `tests/` directory for integration tests
- **`src/tests/` directory (WARNING: won't run without mod declaration!)**

**CRITICAL**: Verify tests are actually discovered:
```bash
# List test binaries that will run
make package-test PACKAGE=<name> -- --list 2>/dev/null | head -20

# If tests exist in src/tests/ but don't appear, they're orphaned!
```

Common orphaned test locations:
- `src/tests/*.rs` without `mod tests;` in `lib.rs`
- Test files not declared as modules

**Fix orphaned tests by either:**
1. Moving to `tests/` directory (Cargo auto-discovers)
2. Adding `#[cfg(test)] mod tests;` to `lib.rs`

### 1.7 Estimate Complexity

**Low complexity:**
- Only needs Cargo.toml updates
- No async code
- Few source files (<200 lines total)

**Medium complexity:**
- Some API changes needed (bytes, quickcheck)
- No async/await migration
- Moderate source (<1000 lines)

**High complexity:**
- Tokio 0.1 → 1.x async/await migration
- Multiple interconnected modules
- Large source (>1000 lines)
- Examples that need updating

### 1.8 Check Dependencies on Other libs/ Crates

Some crates depend on others in the monorepo:
- `moray` depends on `fast-rpc` and `cueball`
- `sharkspotter` depends on `moray`, `libmanta`

If the dependency isn't modernized yet, either:
1. Modernize the dependency first
2. Or note that this crate is blocked

### 1.8a Check for Cueball Usage (Qorb Migration)

**IMPORTANT**: Cueball crates are being replaced with qorb, not modernized.

If the crate uses cueball, identify the migration path:

```bash
# Check for cueball imports
rg "use cueball" libs/<crate>/src/ --type rust
rg "cueball::" libs/<crate>/src/ --type rust
```

**Cueball → Qorb mapping:**

| Cueball Pattern | Qorb Equivalent |
|-----------------|-----------------|
| `cueball::ConnectionPool` | `qorb::Pool` |
| `cueball_static_resolver::StaticIpResolver` | `qorb::resolvers::FixedResolver` |
| `cueball_tcp_stream_connection` | `qorb::connectors::TcpConnector` |
| `cueball_dns_resolver` | `qorb::resolvers::DnsResolver` |
| `pool.claim()` (sync) | `pool.claim().await` (async) |

See `conversion-plans/manta-rebalancer/cueball-to-qorb-migration.md` for full migration details.

### 1.9 Identify Panic and Error Handling Issues

Search for patterns that arch-lint will flag or cause runtime crashes:

```bash
# unwrap() on fallible operations (potential panics)
rg "\.unwrap\(\)" libs/<crate>/src/ --type rust

# expect() on user/external input (potential panics)
rg "\.expect\(" libs/<crate>/src/ --type rust

# Error context being discarded
rg "map_err\(\|_\|" libs/<crate>/src/ --type rust
```

**Categorize each finding:**

| Pattern | When Safe | When Dangerous |
|---------|-----------|----------------|
| `.unwrap()` | After `is_some()`/`is_ok()` check | On I/O, parsing, user input |
| `.expect()` | Invariants that truly can't fail | On external data |
| `map_err(\|_\| ...)` | Never (loses context) | Always fix |

**Document findings for Phase 2 fixes.**

### 1.10 Review Documentation Accuracy

Check module-level documentation and doc comments for stale information:

```bash
# Look at lib.rs module docs
head -150 libs/<crate>/src/lib.rs

# Check for version numbers, protocol specs, API descriptions
rg "VERSION|version" libs/<crate>/src/ --type rust -C 2
```

**Common documentation issues after modernization:**

| Issue | Example | Fix |
|-------|---------|-----|
| Stale version numbers | Docs say "version 1" but code uses version 2 | Update docs to match code |
| Removed APIs still documented | Function deleted but still in examples | Remove from docs |
| Changed signatures | `make_task()` → `handle_connection()` | Update function names/signatures |
| Typos | "protcol" instead of "protocol" | Fix spelling |

**Document any docs that need updating in Phase 2.**

## Output

After analysis, you should know:
1. Which public items are dead code (to be deleted)
2. Which public items are used (to be modernized)
3. Which dependencies need updating
4. Which code patterns need fixing
5. Estimated complexity (Low/Medium/High)
6. Any blockers (unmodernized dependencies)
7. **Panic/error handling issues to fix**
8. **Documentation that needs updating**
9. **Cueball usage requiring qorb migration** (if applicable)

Proceed to Phase 2 with this information.
