# Making Rust Binaries Portable on illumos

## Problem

Rust binaries built on SmartOS link against `libgcc_s.so.1` from the pkgsrc GCC toolchain (e.g. `/opt/local/gcc13/x86_64-sun-solaris2.11/lib/amd64/libgcc_s.so.1`). This makes the binary non-portable to systems without that exact pkgsrc package installed.

The root cause is twofold:
1. The rustc illumos target spec hardcodes `-lgcc_s` in the linker invocation (repeated for every native library group), so you cannot avoid linking it.
2. GCC embeds an RPATH/RUNPATH in the binary pointing to its own lib directories under `/opt/local/`.

The platform ships its own `libgcc_s.so.1` at `/usr/lib/64/libgcc_s.so.1`, which is sufficient for Rust binaries.

## Fix

Strip the RPATH/RUNPATH after building so the dynamic linker finds the system copy instead:

```bash
cargo build --release --bin tritonadm
/usr/bin/elfedit -e 'dyn:delete RUNPATH' ./target/release/tritonadm
/usr/bin/elfedit -e 'dyn:delete RPATH' ./target/release/tritonadm
```

## Verification

```bash
# Before: resolves to pkgsrc gcc13
ldd ./target/release/tritonadm
#   libgcc_s.so.1 => /opt/local/gcc13/x86_64-sun-solaris2.11/lib/amd64/libgcc_s.so.1

# After: resolves to system lib
ldd ./target/release/tritonadm
#   libgcc_s.so.1 => /usr/lib/64/libgcc_s.so.1
```

## References

- [RFD 161](https://github.com/TritonDataCenter/rfd/blob/master/rfd/0161/README.md) — documents this approach for platform-delivered Rust binaries
- RPATH/RUNPATH entries can be inspected with `/usr/bin/elfedit -e 'dyn:runpath' <binary>`
