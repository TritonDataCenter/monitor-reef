# Proposal: Replace Cueball with Qorb for Connection Pooling

**Author:** Engineering Team
**Date:** January 2026
**Status:** Proposal

## Executive Summary

This document proposes replacing the **cueball** connection pooling library with **qorb** as part of our ongoing Rust modernization effort. Qorb is a modern, async-native connection pooling library inspired by cueball but built for the tokio 1.x ecosystem.

**Recommendation:** Adopt qorb for all new development and migrate existing cueball usage incrementally. The migration is low-risk for most use cases, with the Manatee/ZooKeeper resolver being the only component requiring significant development effort.

## Background

### What is Cueball?

Cueball is a multi-node service connection pool library originally written in Node.js by Joyent, with a Rust port in this repository. It provides:

- Connection pooling across multiple backend servers
- Pluggable service discovery (resolvers)
- Automatic connection health checking and rebalancing
- Support for various backends (TCP, PostgreSQL, etc.)

### What is Qorb?

Qorb is a connection pooling library written by Oxide Computer Company, explicitly inspired by cueball. It provides similar functionality but is designed from the ground up for modern async Rust.

### Why Consider Migration?

Our cueball crates have significant technical debt:

| Crate | Edition | Async Runtime | Status |
|-------|---------|---------------|--------|
| `cueball` | 2024 | sync (threads) | Modernized |
| `cueball-static-resolver` | 2024 | sync | Modernized |
| `cueball-dns-resolver` | 2018 | **tokio 0.1** | Legacy |
| `cueball-postgres-connection` | 2018 | sync | Legacy |
| `cueball-tcp-stream-connection` | 2018 | sync | Legacy |
| `cueball-manatee-primary-resolver` | 2018 | **tokio 0.1** | Legacy |

The tokio 0.1 dependencies are particularly problematic:
- Incompatible with modern async Rust ecosystem
- `tokio-zookeeper 0.1.3` is unmaintained (last release: 2018)
- `trust-dns 0.19` is outdated (current: hickory-resolver 0.24+)
- Requires significant rewrite to modernize

## Technical Comparison

### Architecture

| Aspect | Cueball | Qorb |
|--------|---------|------|
| **Async Model** | Synchronous (blocking) | Fully async (tokio 1.x) |
| **Rust Edition** | Mixed (2018/2024) | 2021 |
| **API Style** | `pool.claim()` blocks | `pool.claim().await` |
| **State Management** | Implicit via mutex | Explicit 6-state slot machine |

### Feature Comparison

| Feature | Cueball | Qorb | Notes |
|---------|:-------:|:----:|-------|
| Connection pooling | ✅ | ✅ | |
| Multi-backend support | ✅ | ✅ | |
| Health checking | ✅ | ✅ | Both default to 30s intervals |
| Automatic rebalancing | ✅ | ✅ | |
| DNS SRV resolution | ✅ | ✅ | Qorb uses modern hickory-resolver |
| Static/fixed backends | ✅ | ✅ | |
| Claim timeout | ✅ | ✅ | |
| Exponential backoff | ✅ | ✅ | |
| **DTrace/USDT probes** | ❌ | ✅ | 24 probes for observability |
| **WebSocket monitoring** | ❌ | ✅ | qtop feature |
| **Per-backend limits** | ❌ | ✅ | `SetConfig::max_count` |
| Priority-weighted selection | ❌ | ✅ | Punitive scoring algorithm |
| ZooKeeper/Manatee resolver | ✅ | ❌ | Would need to be ported |

### Trait Interface Comparison

**Cueball Connection (synchronous):**
```rust
pub trait Connection: Send + Sized + 'static {
    type Error: error::Error;
    fn connect(&mut self) -> Result<(), Self::Error>;
    fn is_valid(&mut self) -> bool;
    fn has_broken(&self) -> bool;
    fn close(&mut self) -> Result<(), Self::Error>;
}
```

**Qorb Connector (async):**
```rust
#[async_trait]
pub trait Connector: Send + Sync {
    type Connection: Connection;  // Just needs Send + 'static

    async fn connect(&self, backend: &Backend) -> Result<Self::Connection, Error>;
    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Error>;
    async fn on_acquire(&self, conn: &mut Self::Connection) -> Result<(), Error>;
    async fn on_recycle(&self, conn: &mut Self::Connection) -> Result<(), Error>;
}
```

**Key Difference:** Cueball's `Connection` trait is implemented on the pooled object itself. Qorb separates the `Connector` (factory) from the `Connection` (pooled object), which is a cleaner design.

**Resolver Interface:**

| Cueball | Qorb |
|---------|------|
| `fn run(&mut self, s: Sender<BackendMsg>)` | `fn monitor(&mut self) -> watch::Receiver<AllBackends>` |
| Blocking, sends incremental add/remove | Async, publishes full backend set |

### Dependencies

**Cueball ecosystem:**
- Core: `backoff`, `chrono`, `rand`, `sha1`, `slog`, `timer`
- DNS resolver: `tokio 0.1`, `futures 0.1`, `trust-dns 0.19` (outdated)
- Manatee resolver: `tokio-zookeeper 0.1.3` (unmaintained)

**Qorb:**
- `tokio 1.43`, `async-trait`, `thiserror 2.0`
- `hickory-resolver 0.24` (modern DNS)
- Optional: `diesel 2.2.9`, `dropshot 0.16`, `usdt 0.5`

## Current Usage in monitor-reef

### Crates Using Cueball

1. **`libs/moray`** - Uses cueball with static resolver and TCP connection
2. **`cli/manatee-echo-resolver`** - Debug tool for Manatee resolver

### Affected Components

```
libs/moray
├── cueball (core)
├── cueball-static-resolver
└── cueball-tcp-stream-connection

cli/manatee-echo-resolver
├── cueball (core)
└── cueball-manatee-primary-resolver
```

## Migration Path

### Phase 1: New Development (Immediate)

- Use qorb for all new services requiring connection pooling
- Leverage existing qorb connectors:
  - `TcpConnector` for raw TCP
  - `DieselPgConnector` for PostgreSQL (with diesel feature)
  - `FixedResolver` for static backends
  - `DnsResolver` for DNS-based discovery

### Phase 2: Moray Migration (Low Effort)

The `libs/moray` crate uses:
- Static resolver → Direct equivalent in qorb (`FixedResolver`)
- TCP stream connection → Direct equivalent in qorb (`TcpConnector`)

**Estimated effort:** 1-2 days

**Changes required:**
1. Replace `cueball::ConnectionPool` with `qorb::Pool`
2. Replace `cueball_static_resolver::StaticIpResolver` with `qorb::resolvers::FixedResolver`
3. Implement simple TCP connector or use `qorb::connectors::TcpConnector`
4. Update call sites from sync `claim()` to async `claim().await`

### Phase 3: Manatee Resolver (Higher Effort)

If Manatee/ZooKeeper support is required:

**Option A: Port the Manatee resolver to qorb (~500-800 lines)**

1. Use modern ZooKeeper client (`zookeeper-client` crate, tokio 1.x compatible)
2. Adapt the watch loop logic from cueball's implementation
3. Reuse the JSON parsing logic (`process_value()`) nearly verbatim
4. Implement qorb's `Resolver` trait interface

**Option B: Evaluate alternatives**
- If services are moving away from Manatee, this may not be needed
- Consider DNS-based discovery as an alternative

**Estimated effort:** 3-5 days for Option A

### Phase 4: Deprecate Cueball Crates

Once migration is complete:
1. Remove cueball crates from workspace
2. Archive or delete the cueball code
3. Update documentation

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| API incompatibility | Low | Medium | Both libraries have similar concepts; mapping is straightforward |
| Missing Manatee support | Medium | High | Port resolver or evaluate alternatives |
| Performance regression | Low | Low | Qorb has benchmarks; async should perform better |
| Learning curve | Low | Low | Qorb has good documentation and examples |

## Benefits of Migration

### Immediate Benefits

1. **Modern async/await** - Native tokio 1.x, no legacy runtime issues
2. **Better observability** - 24 DTrace probes built-in, WebSocket monitoring
3. **Cleaner API** - Separate Connector/Connection pattern
4. **Active maintenance** - Qorb is actively developed by Oxide

### Long-term Benefits

1. **Ecosystem compatibility** - Works with modern Rust async ecosystem
2. **Reduced maintenance** - No need to maintain legacy tokio 0.1 code
3. **Better debugging** - USDT probes enable production debugging
4. **Performance insights** - Built-in benchmarking, qtop monitoring tool

## Recommendation

**Adopt qorb as the standard connection pooling library for monitor-reef.**

### Rationale

1. **Technical superiority** - Modern async design, better observability
2. **Maintenance burden** - Modernizing cueball's legacy crates requires similar effort to just using qorb
3. **Future-proof** - Qorb aligns with the modern Rust ecosystem
4. **Low migration risk** - Core concepts map directly; only Manatee resolver needs significant work

### Proposed Timeline

| Phase | Scope | Effort | Priority |
|-------|-------|--------|----------|
| 1 | New development uses qorb | Immediate | High |
| 2 | Migrate moray | 1-2 days | Medium |
| 3 | Port Manatee resolver (if needed) | 3-5 days | As needed |
| 4 | Remove cueball crates | 1 day | Low |

## Appendix A: Example Migration

### Before (Cueball)

```rust
use cueball::connection_pool::ConnectionPool;
use cueball_static_resolver::StaticIpResolver;

let resolver = StaticIpResolver::new(vec![backend1, backend2]);
let pool = ConnectionPool::new(options, resolver, |backend| {
    MyConnection::new(backend)
});

// Blocking call
let conn = pool.claim()?;
conn.do_something();
// conn returned to pool on drop
```

### After (Qorb)

```rust
use qorb::pool::Pool;
use qorb::resolvers::FixedResolver;
use qorb::policy::Policy;

let resolver = FixedResolver::new(HashMap::from([
    ("backend1".into(), backend1),
    ("backend2".into(), backend2),
]));
let pool = Pool::new("my-pool", Box::new(resolver), connector, Policy::default())?;

// Async call
let handle = pool.claim().await?;
handle.do_something().await;
// handle returned to pool on drop
```

## Appendix B: Qorb Resources

- **Repository:** (Oxide Computer Company)
- **Documentation:** Inline rustdoc with examples
- **Examples:** TCP echo server/client, Dropshot HTTP integration
- **Monitoring:** qtop WebSocket server + TUI tool

## Appendix C: Files to Modify

For moray migration:
- `libs/moray/Cargo.toml` - Update dependencies
- `libs/moray/src/*.rs` - Update pool usage (grep for `use cueball`)

For full migration:
- `Cargo.toml` - Remove cueball workspace members
- `libs/cueball*` - Archive or delete
- `cli/manatee-echo-resolver` - Update or deprecate
