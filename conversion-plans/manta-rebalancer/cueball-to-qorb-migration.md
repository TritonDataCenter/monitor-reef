# Required: Replace Cueball with Qorb for Connection Pooling

**Author:** Engineering Team
**Date:** January 2026
**Status:** REQUIRED - Migration must be completed

## Executive Summary

This document describes the **required** migration from the **cueball** connection pooling library to **qorb**. Qorb is a modern, async-native connection pooling library inspired by cueball but built for the tokio 1.x ecosystem.

**Status:** Migration is REQUIRED. The cueball crates were temporarily modernized to edition 2024 but must be replaced with qorb and then deleted. The Manatee/ZooKeeper resolver (`qorb-manatee-resolver`) must be created for production use.

## Background

### What is Cueball?

Cueball is a multi-node service connection pool library originally written in Node.js by Joyent, with a Rust port in this repository. It provides:

- Connection pooling across multiple backend servers
- Pluggable service discovery (resolvers)
- Automatic connection health checking and rebalancing
- Support for various backends (TCP, PostgreSQL, etc.)

### What is Qorb?

Qorb is a connection pooling library written by Oxide Computer Company, explicitly inspired by cueball. It provides similar functionality but is designed from the ground up for modern async Rust.

### Why Migration is Required

Our cueball crates were temporarily modernized but have fundamental limitations:

| Crate | Edition | Async Runtime | Status |
|-------|---------|---------------|--------|
| `cueball` | 2024 | sync (threads) | Modernized (temporary) |
| `cueball-static-resolver` | 2024 | sync | Modernized (temporary) |
| `cueball-tcp-stream-connection` | 2024 | sync | Modernized (temporary) |
| `cueball-dns-resolver` | 2018 | **tokio 0.1** | Legacy (delete) |
| `cueball-postgres-connection` | 2018 | sync | Legacy (delete) |
| `cueball-manatee-primary-resolver` | 2018 | **tokio 0.1** | Legacy (port to qorb) |

**Key reasons migration is required:**

1. **Manatee support**: Production requires Manatee/ZooKeeper service discovery, which cueball-manatee-primary-resolver provides. That resolver uses unmaintained tokio 0.1 dependencies and cannot be modernized - it must be rewritten for qorb.

2. **Fundamental architecture**: Cueball is synchronous (blocking). Even the "modernized" crates spawn threads for blocking operations. Qorb is native async.

3. **No upstream development**: Cueball is legacy code with no active development.

4. **Observability**: Qorb has 24 DTrace probes built-in; cueball has none.

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

### Phase 1: Create qorb-manatee-resolver (DONE)

✅ **Completed:** `libs/qorb-manatee-resolver` provides Manatee/ZooKeeper service discovery for qorb.

Implementation details:
1. Uses `zookeeper-client` crate (tokio 1.x compatible)
2. Watches the Manatee cluster state node for changes
3. Parses JSON to extract primary's IP and port from `primary.ip` and `primary.pgUrl`
4. Implements qorb's `Resolver` trait with automatic reconnection and backoff

**Reference:** `libs/cueball-manatee-primary-resolver/` for logic to port

### Phase 2: Moray Migration (REQUIRED)

The `libs/moray` crate currently uses cueball. Migrate to qorb:

- Static resolver → `qorb::resolvers::FixedResolver`
- TCP stream connection → `qorb::connectors::TcpConnector`
- For production: Use `qorb-manatee-resolver` from Phase 1

**Estimated effort:** 1-2 days

**Changes required:**
1. Replace `cueball::ConnectionPool` with `qorb::Pool`
2. Replace `cueball_static_resolver::StaticIpResolver` with `qorb::resolvers::FixedResolver`
3. Implement simple TCP connector or use `qorb::connectors::TcpConnector`
4. Update call sites from sync `claim()` to async `claim().await`

### Phase 3: Delete Cueball Crates (REQUIRED)

Once migration is complete, delete all cueball crates:

**Modernized crates (delete after moray migration):**
- `libs/cueball/`
- `libs/cueball-static-resolver/`
- `libs/cueball-tcp-stream-connection/`

**Legacy crates (delete immediately - never enabled):**
- `libs/cueball-dns-resolver/`
- `libs/cueball-postgres-connection/`
- `libs/cueball-manatee-primary-resolver/`
- `cli/manatee-echo-resolver/`

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

## Required Migration

**Qorb is the required connection pooling library for monitor-reef.** All cueball usage must be migrated.

### Rationale

1. **Production requirement** - Manatee/ZooKeeper service discovery is required; cueball's resolver is unmaintainable
2. **Technical superiority** - Modern async design, better observability
3. **Maintenance burden** - Cueball is legacy code with no upstream development
4. **Future-proof** - Qorb aligns with the modern Rust ecosystem

### Required Timeline

| Phase | Scope | Effort | Status |
|-------|-------|--------|--------|
| 1 | Create qorb-manatee-resolver | 3-5 days | ✅ DONE |
| 2 | Migrate moray to qorb | 1-2 days | 🔴 TODO |
| 3 | Delete cueball crates | 1 day | 🔴 TODO |

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

## Appendix C: Files to Create/Modify

### New crate: qorb-manatee-resolver
- `libs/qorb-manatee-resolver/Cargo.toml` - New crate
- `libs/qorb-manatee-resolver/src/lib.rs` - Qorb Resolver implementation
- Reference: `libs/cueball-manatee-primary-resolver/src/` for logic to port

### Moray migration:
- `libs/moray/Cargo.toml` - Update dependencies (cueball → qorb)
- `libs/moray/src/*.rs` - Update pool usage (grep for `use cueball`)

### Deletion (after migration complete):
- `Cargo.toml` - Remove cueball workspace members
- `libs/cueball/` - Delete
- `libs/cueball-static-resolver/` - Delete
- `libs/cueball-tcp-stream-connection/` - Delete
- `libs/cueball-dns-resolver/` - Delete
- `libs/cueball-postgres-connection/` - Delete
- `libs/cueball-manatee-primary-resolver/` - Delete
- `cli/manatee-echo-resolver/` - Delete
