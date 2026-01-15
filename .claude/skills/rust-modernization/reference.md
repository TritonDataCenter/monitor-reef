# Rust Modernization Reference

This document contains all known patterns for modernizing legacy Rust code.

## Dependency Version Mappings

| Old | New | Cargo.toml |
|-----|-----|------------|
| tokio 0.1.x | tokio 1.x | `tokio = { workspace = true }` |
| tokio-codec 0.1 | tokio-util 0.7 | `tokio-util = { version = "0.7", features = ["codec"] }` |
| tokio-io 0.1 | (removed) | Delete from deps |
| tokio-uds 0.2 | tokio | Use `tokio` with `net` feature |
| bytes 0.4 | bytes 1.x | `bytes = "1"` |
| futures 0.1 | futures 0.3 | `futures = "0.3"` |
| quickcheck 0.8 | quickcheck 1.0 | `quickcheck = "1.0"` |
| rand 0.6 | rand 0.8 | `rand = "0.8"` |
| clap 2.x | clap 4.x | `clap = { workspace = true }` |
| serde_derive 1.x | (merged) | Just use `serde = { workspace = true }` |
| base64 0.10 | base64 0.22 | `base64 = "0.22"` |
| md-5 0.8 | md-5 0.10 | `md-5 = "0.10"` |
| trust-dns-resolver | hickory-resolver | `hickory-resolver = "0.25"` |
| slog 2.x | slog 2.x | `slog = "2.7"` (minor update) |
| slog-stdlog 3 | slog-stdlog 4 | `slog-stdlog = "4"` |

---

## Edition 2024 Reserved Keywords

**CRITICAL**: In Edition 2024, `gen` is a reserved keyword (for generators).

Any code using `gen` as an identifier will fail:
```rust
// FAILS in edition 2024
let value = g.gen::<u32>();
```

Solutions:
1. Use `T::arbitrary(g)` for quickcheck (preferred)
2. Escape with `r#gen` (not recommended)

---

## Tokio 0.1 → Tokio 1.x

### Import Changes

| Old | New |
|-----|-----|
| `use tokio::prelude::*;` | Remove entirely |
| `use tokio::codec::Decoder;` | `use tokio_util::codec::Decoder;` |
| `use tokio_io::_tokio_codec::{Decoder, Encoder};` | `use tokio_util::codec::{Decoder, Encoder};` |

### Future Type Changes

| Old | New |
|-----|-----|
| `impl Future<Item=X, Error=Y>` | `async fn ... -> Result<X, Y>` |
| `impl Future<Item=(), Error=()>` | `async fn ... -> ()` or `impl Future<Output=()>` |
| `Box::new(future::ok(x))` | `Ok(x)` (in sync context) or just return |

### Combinator → async/await

**Before (futures 0.1 combinators):**
```rust
pub fn make_task<F>(socket: TcpStream, handler: F)
    -> impl Future<Item = (), Error = ()> + Send
{
    let (tx, rx) = codec.framed(socket).split();
    tx.send_all(rx.and_then(move |x| {
        process(x)
    }))
    .then(move |res| {
        if let Err(e) = res {
            error!("failed: {}", e);
        }
        Ok(())
    })
}
```

**After (async/await):**
```rust
pub async fn handle_connection<F>(socket: TcpStream, mut handler: F)
    -> Result<(), Error>
{
    let (mut tx, mut rx) = codec.framed(socket).split();
    while let Some(result) = rx.next().await {
        match result {
            Ok(x) => {
                let response = process(x)?;
                tx.send(response).await?;
            }
            Err(e) => {
                error!("failed: {}", e);
                return Err(e);
            }
        }
    }
    Ok(())
}
```

### Main Function

**Before:**
```rust
fn main() {
    tokio::run(future);
}
```

**After:**
```rust
#[tokio::main]
async fn main() {
    future.await;
}
```

### Required Imports for Streams

```rust
use futures::{SinkExt, StreamExt};  // For .send() and .next()
```

---

## bytes 0.4 → bytes 1.x

### Import Changes

```rust
// Add Buf for advance()
use bytes::{Buf, BufMut, BytesMut};
```

### Method Changes

| Old | New |
|-----|-----|
| `buf.put_u32_be(x)` | `buf.put_slice(&x.to_be_bytes())` |
| `buf.put_u16_be(x)` | `buf.put_slice(&x.to_be_bytes())` |
| `buf.put(string)` | `buf.put_slice(string.as_bytes())` |
| `buf.advance(n)` | Same, but needs `Buf` trait import |

---

## tokio-util Encoder Trait

The `Encoder` trait changed from associated type to generic parameter.

**Before (tokio-codec 0.1):**
```rust
impl Encoder for MyCodec {
    type Item = Vec<Message>;
    type Error = io::Error;

    fn encode(&mut self, item: Self::Item, buf: &mut BytesMut) -> Result<(), Self::Error> {
        // ...
    }
}
```

**After (tokio-util 0.7):**
```rust
impl Encoder<Vec<Message>> for MyCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Vec<Message>, buf: &mut BytesMut) -> Result<(), Self::Error> {
        // ...
    }
}
```

---

## quickcheck 0.8 → quickcheck 1.0

### Arbitrary Trait Signature

**Before:**
```rust
impl Arbitrary for MyType {
    fn arbitrary<G: Gen>(g: &mut G) -> MyType {
        // G is a trait bound
    }
}
```

**After:**
```rust
impl Arbitrary for MyType {
    fn arbitrary(g: &mut Gen) -> MyType {
        // Gen is now a concrete type
    }
}
```

### Random Value Generation

| Old | New |
|-----|-----|
| `g.gen::<u32>()` | `u32::arbitrary(g)` |
| `g.gen::<u8>()` | `u8::arbitrary(g)` |
| `g.gen::<bool>()` | `bool::arbitrary(g)` |
| `slice.choose(g).unwrap()` | `slice[usize::arbitrary(g) % slice.len()]` |

### Random String Generation

**Before:**
```rust
use rand::distributions::Alphanumeric;
fn random_string<G: Gen>(g: &mut G, len: usize) -> String {
    iter::repeat(())
        .map(|()| g.sample(Alphanumeric))
        .take(len)
        .collect()
}
```

**After:**
```rust
fn random_string(g: &mut Gen, len: usize) -> String {
    (0..len)
        .map(|_| {
            let c = u8::arbitrary(g);
            (b'a' + (c % 26)) as char
        })
        .collect()
}
```

---

## clap 2.x → clap 4.x

### Builder → Derive

**Before (builder pattern):**
```rust
use clap::{App, Arg, crate_version, value_t};

fn parse_opts() -> ArgMatches<'static> {
    App::new("myapp")
        .version(crate_version!())
        .arg(Arg::with_name("host")
            .long("host")
            .short("h")
            .takes_value(true)
            .default_value("localhost"))
        .arg(Arg::with_name("port")
            .long("port")
            .short("p")
            .takes_value(true))
        .get_matches()
}

fn main() {
    let matches = parse_opts();
    let host = matches.value_of("host").unwrap();
    let port = value_t!(matches, "port", u32).unwrap_or(8080);
}
```

**After (derive-based):**
```rust
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "myapp", version)]
struct Args {
    /// Description for host
    #[arg(short = 'H', long, default_value = "localhost")]
    host: String,

    /// Description for port
    #[arg(short, long, default_value_t = 8080)]
    port: u16,
}

fn main() {
    let args = Args::parse();
    // Use args.host, args.port directly
}
```

---

## base64 0.10 → base64 0.22

**Before:**
```rust
let encoded = base64::encode(&data);
let decoded = base64::decode(&encoded)?;
```

**After:**
```rust
use base64::prelude::*;

let encoded = BASE64_STANDARD.encode(&data);
let decoded = BASE64_STANDARD.decode(&encoded)?;
```

---

## md-5 0.8 → md-5 0.10

**Before:**
```rust
use md5::{Digest, Md5};
let hash = hasher.result();
```

**After:**
```rust
use md5::{Digest, Md5};
let hash = hasher.finalize();
```

---

## trust-dns-resolver → hickory-resolver

The trust-dns project was renamed to hickory-dns.

**Before:**
```rust
use trust_dns_resolver::Resolver;

let resolver = Resolver::from_system_conf()?;
let response = resolver.lookup_ip(host)?;
```

**After:**
```rust
use hickory_resolver::TokioResolver;
use hickory_resolver::name_server::TokioConnectionProvider;

// Now async-only with builder pattern
// Note: builder() returns Result, so use ? before .build()
let resolver = TokioResolver::builder(TokioConnectionProvider::default())
    .map_err(|e| e.to_string())?
    .build();
let response = resolver.lookup_ip(host).await?;
```

---

## std::io Trait Imports

In edition 2024, `Read` and `Write` traits need explicit import:

**Before (sometimes worked implicitly):**
```rust
use std::net::TcpStream;
stream.read(&mut buf)?;  // Might have worked
```

**After (explicit import required):**
```rust
use std::io::{Read, Write};
use std::net::TcpStream;
stream.read(&mut buf)?;
stream.write(&data)?;
```

---

## serde_derive → serde

**Before:**
```rust
use serde_derive::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct MyStruct { ... }
```

**After:**
```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct MyStruct { ... }
```

The `serde` crate now includes the derive macros when the `derive` feature is enabled (which is the default in our workspace).

---

## Error Handling Modernization

### Panic → Result Conversions

| Old Pattern | New Pattern |
|-------------|-------------|
| `value.unwrap()` | `value?` or `value.ok_or(...)?` |
| `value.expect("msg")` | `value.ok_or_else(\|\| Error::other("msg"))?` |
| `map_err(\|_\| e)` | `map_err(\|orig\| format!("{}: {}", msg, orig))?` |

### Error Construction

| Old | New |
|-----|-----|
| `Error::new(ErrorKind::Other, msg)` | `Error::other(msg)` |
| `panic!("error: {}", e)` | `return Err(Error::other(format!("error: {}", e)))` |

### Preserving Error Context

**Before (loses debugging info):**
```rust
serde_json::from_value(data).map_err(|_| unspecified_error())
```

**After (preserves original error):**
```rust
serde_json::from_value(data).map_err(|e| {
    Error::other(format!("Failed to parse response: {}", e))
})
```

### Converting expect() on User Input

**Before (DoS vulnerability - panics on bad input):**
```rust
let arr = value.as_array().expect("should be array");
```

**After (returns error to caller):**
```rust
let arr = value.as_array()
    .ok_or_else(|| Error::other("Expected JSON array"))?;
```

### Safe unwrap()/expect() Patterns

These patterns are acceptable and don't need changing:

```rust
// System invariants that truly can't fail
SystemTime::now().duration_since(UNIX_EPOCH)
    .expect("system time before Unix epoch")

// After explicit check (but prefer if-let)
if opt.is_some() {
    opt.unwrap()  // Safe, but use if-let instead
}

// In test code only
#[cfg(test)]
fn test_something() {
    result.unwrap();  // OK in tests
}
```

### Idiomatic Option Handling

**Before (redundant check + expect):**
```rust
if value.is_some() {
    let v = value.expect("checked above");
    use_value(v);
}
```

**After (if-let pattern):**
```rust
if let Some(v) = value {
    use_value(v);
}
```

---

## Code Simplification Patterns

### Vec Operations

**Manual capacity + drain → append:**
```rust
// Before
if responses.len() + response.len() > responses.capacity() {
    responses.reserve(response.len());
}
response.drain(..).for_each(|r| responses.push(r));

// After
responses.append(&mut response);
```

**Manual reserve before push:**
```rust
// Before (unnecessary - Vec handles this)
if msgs.len() + 1 > msgs.capacity() {
    msgs.reserve(1);
}
msgs.push(item);

// After
msgs.push(item);
```

### Encoder/Iterator Patterns

**Collect-then-check → early return:**
```rust
// Before (processes all items, then checks for errors)
let results: Vec<Result<(), E>> = items.iter().map(process).collect();
let _: Result<Vec<()>, E> = results.into_iter().collect();

// After (fails fast on first error)
for item in &items {
    process(item)?;
}
Ok(())
```

### Unnecessary Clones

**Arc clone for method call:**
```rust
// Before (unnecessary clone)
arc.clone().method();

// After (Arc methods take &self)
arc.method();
```
