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

### 1.4 Check for Examples

```bash
# Check if examples directory exists
```

Examples often use the same patterns and need updating too.

### 1.5 Check for Tests

Look in:
- `src/` files for `#[cfg(test)]` modules
- `tests/` directory for integration tests

### 1.6 Estimate Complexity

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

### 1.7 Check Dependencies on Other libs/ Crates

Some crates depend on others in the monorepo:
- `moray` depends on `fast-rpc`
- `cueball-*` depend on `cueball`
- `sharkspotter` depends on `moray`, `libmanta`

If the dependency isn't modernized yet, either:
1. Modernize the dependency first
2. Or note that this crate is blocked

## Output

After analysis, you should know:
1. Which dependencies need updating
2. Which code patterns need fixing
3. Estimated complexity (Low/Medium/High)
4. Any blockers (unmodernized dependencies)

Proceed to Phase 2 with this information.
