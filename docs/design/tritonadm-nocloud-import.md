<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# tritonadm: nocloud image import from upstream vendors

## Motivation

Today the only way to get a CloudInit `nocloud` VM image onto a SmartOS host
or into a Triton IMGAPI is to build artifacts in the
[triton-nocloud-images](../../target/triton-nocloud-images) repo and ship them
out-of-band. The build pipeline is small: download a vendor image, convert it
to raw, write it onto a zvol, `zfs send` the snapshot, gzip it, and render an
IMGAPI manifest from a template.

That repo's reason for existing is mostly mechanical. The vendor already
publishes a `nocloud`-flavored cloud image; we are not adding content, we are
just translating one container format into another (`qcow2`/`raw.xz` → gzipped
ZFS stream + IMGAPI manifest).

If that mechanical step lives in a CLI rather than a build pipeline, the value
proposition changes: operators can pull stock vendor images directly onto a
SmartOS host on demand, without going through us, and without inheriting an
image whose contents we have modified. The customer answers "what is in this
image?" by reading the vendor's documentation, not ours.

This design adds a `tritonadm image fetch-nocloud` subcommand that does the
fetch-and-convert pipeline end-to-end, with one vendor profile per
distribution, against either a local SmartOS host (`imgadm install`) or an
IMGAPI (the existing `tritonadm image import` path).

## Goals

- Replace the bash pipeline in `target/triton-nocloud-images/build.sh` with
  a Rust subcommand of `tritonadm`.
- One baked-in vendor profile per supported distro, with vendor-specific
  release resolution (`latest`, named series, pinned version).
- Trust chain encoded per vendor at the highest level the vendor supports;
  TLS-fetched checksums are an acceptable floor.
- Run unprivileged inside an existing Triton builder NGZ with a delegated
  dataset (mirrors current bash constraints).
- Produce binaries portable into the SmartOS GZ via the existing
  [portable-binaries](portable-binaries.md) elfedit dance.
- Output to one of: a pair of files (`*.zfs.gz` + `*.json`), an `imgadm
  install` invocation against the local SmartOS host, or push to an IMGAPI
  using the existing `tritonadm image import` machinery.

## Non-goals

- Replacing all `target/triton-nocloud-images` configs in one go. Other
  distros are deferred to follow-up work; the design supports them but the
  POC ships only Ubuntu.
- GPG signature verification. The infrastructure for it is part of the
  design (verifier strategy is a trait), but the initial implementation
  may verify only the TLS-fetched `SHA256SUMS` for vendors that publish
  one. Wiring up vendor pubkeys and `SHA256SUMS.gpg` verification is a
  follow-up.
- A separate SmartOS-only single-purpose binary. The same `tritonadm`
  binary is used; an extracted standalone variant can come later if there
  is demand.
- Replacing the `tritonadm image import` path. That subcommand already
  pushes a manifest+file pair to IMGAPI; this work just produces inputs
  for it.

## Command surface

```
tritonadm image fetch-nocloud --vendor <name> --release <name|latest>
                              [--target file|smartos|imgapi]
                              [--output-dir DIR]
                              [--workdir DIR]
                              [--dataset NAME]
                              [--expected-sha256 HEX]
                              [--profile-dir DIR]
                              [--insecure-no-verify]
                              [--dry-run]
```

| Flag | Meaning |
|---|---|
| `--vendor` | Vendor profile name (clap `ValueEnum`; help auto-lists supported vendors). |
| `--release` | Vendor-specific release token: a series name, a version, or `latest`. Resolution is vendor-specific. |
| `--target` | Where the image goes after build. Default: `file`. `smartos` shells `imgadm install -m <manifest> -f <gz>` (GZ-only). `imgapi` calls into the `tritonadm image import` machinery using the auto-detected IMGAPI URL. |
| `--output-dir` | Directory for `*.zfs.gz` and `*.json`. Default: `/var/tmp/tritonadm/nocloud/image/<vendor>-<series>/`. |
| `--workdir` | Working directory for downloads and intermediate raw files. Default: `/var/tmp/tritonadm/nocloud/cache/<vendor>-<series>/`. |
| `--dataset` | Override the parent ZFS dataset. Default: `zones` in the GZ; `zones/<zonename>/data` in an NGZ. |
| `--expected-sha256` | Override the vendor's verifier with a pinned 64-char hex sha256. Useful for vendors that don't publish per-image hashes (Talos), or for one-off pinning. |
| `--profile-dir` | Optional directory of TOML profile files for custom (non-baked-in) vendors. _Not implemented yet — see follow-ups._ |
| `--insecure-no-verify` | Skip checksum/signature verification. Development only. |
| `--dry-run` | Resolve vendor metadata and print the build plan, then exit. Auto-promoted on non-SmartOS hosts (Mac/Linux dev boxes) so `--vendor X --release latest` always exercises release resolution + verifier wiring without requiring `zfs(8)`. |

The subcommand lives at `tritonadm image fetch-nocloud` to share the
existing `imgapi_url` resolution and the manifest-import code path. A
top-level `tritonadm nocloud` group is rejected because the operation is
fundamentally an IMGAPI-shaped one.

## Vendor profiles

### Built-in: a `VendorProfile` trait

```rust
pub trait VendorProfile: Send + Sync {
    fn name(&self) -> &str;
    async fn resolve(&self, release: &str) -> Result<ResolvedImage>;
}

pub struct ResolvedImage {
    pub url: Url,
    pub format: SourceFormat,        // Qcow2 | Xz | Raw
    pub os: ImageOs,                 // linux, bsd, ...
    pub series: String,              // "noble", "trixie", ...
    pub version: String,             // "24.04.20260415", date-stamped
    pub description: String,
    pub homepage: Url,
    pub ssh_key: bool,               // requirements.ssh_key in manifest
    pub verifier: Box<dyn Verifier>, // checksum / signature strategy
}

#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(&self, file: &Path) -> Result<()>;
}
```

Built-in verifier strategies:

- `Sha256Pinned(&'static str)` — pinned literal, for static profiles.
- `Sha256SumsTls { url, filename }` — fetch a `SHA256SUMS`-style file
  over TLS, parse, match by filename. **The POC floor.**
- `Sha256SumsGpg { sums_url, sig_url, key_id }` — adds detached-signature
  verification with an embedded vendor pubkey. Future work; trait shape
  is fixed now to avoid churn later.

### Built-in vendor list

Fifteen vendor profiles ship today. The vendor-specific code is the whole
point of this tool; keeping it under code review rather than in data files
is intentional. Adding a new vendor is a self-contained change:
`vendor/<name>.rs` + a `vendor/<name>/<resolver>.rs` for release
discovery, plus a single new variant in the `Vendor` enum.

| Vendor | Release discovery | Verifier |
|---|---|---|
| `alma` | Directory listing at `https://repo.almalinux.org/almalinux/` for major versions; CHECKSUM file in each major's images dir gives the hash for the `-latest` rolling pointer and resolves it to the dated alias. Accepts `latest`, `8`, `9`, `10`. | `Sha256Pinned` (Linux-style CHECKSUM, parsed at resolve time) |
| `alpine` | `https://alpinelinux.org/releases.json`. Accepts `latest` (newest in `latest_stable` branch), branch (`3.23` or `v3.23`), or full version (`3.23.4`). | `Sha512SidecarTls` — per-image `<file>.sha512` containing only the bare hex hash |
| `arch` | Per-build directory listing at `https://geo.mirror.pkgbuild.com/images/`. Accepts `latest` (highest `v<date>.<build>/`), `v20260501.523211`, or `20260501.523211`. | `Sha256Pinned` from the `<file>.SHA256` sidecar |
| `centos-stream` | Top-level listing at `https://cloud.centos.org/centos/` for `<n>-stream/` dirs; per-stream image dir for the highest dated `CentOS-Stream-GenericCloud-<n>-<date>.<build>.x86_64.qcow2`. Sets a non-empty User-Agent (CloudFront 403s without it). Accepts `latest`, `8`/`8-stream`, `9`/`9-stream`, `10`/`10-stream`. | `Sha256Pinned` from the per-file `.SHA256SUM` sidecar (BSD-style) |
| `debian` | apt `Release` file at `https://deb.debian.org/debian/dists/<suite>/Release` — same file apt uses to know what `stable` means today. Accepts symbolic suites (`stable`, `oldstable`, ...), codenames (`trixie`, `bookworm`, ...), and `latest` as an alias for `stable`. | `Sha512SumsTls` (Debian publishes SHA-512) |
| `fedora` | `https://fedoraproject.org/releases.json` filtered to `Cloud_Base` x86_64 qcow2 entries; sha256 inline. Accepts `latest`, `42`, `f42`, `Fedora-42`. | `Sha256Pinned` from the JSON feed |
| `freebsd` | HTML directory listing at `https://download.freebsd.org/releases/VM-IMAGES/`. Accepts `latest` (highest `X.Y-RELEASE/`) and explicit versions like `15.0` or `15.0-RELEASE`. | `Sha256BsdSumsTls` — BSD-traditional `SHA256 (filename) = hex` |
| `omnios` | One cloud image per release channel at `https://downloads.omnios.org/media/<channel>/`, where channel ∈ {`stable`, `lts`, `bloody`}. Lex sort handles the LTS `r…r` refresh suffix. Accepts the three channel names plus `latest` (alias for `stable`). | `Sha256Pinned` from the bare-hash `<file>.sha256` sidecar |
| `openbsd` | GitHub Releases at `hcartiaux/openbsd-cloud-image`. Always picks the `min` flavor (the cloud-init NoCloud target documented at bsd-cloud-image.org). Accepts `latest`, `7.8`, `v7.8`, or the full upstream tag. | `Sha256Pinned` parsed from the asset's `.sha256` sidecar (single-hash form, since the embedded filename has an `images/` prefix the asset name lacks) |
| `opensuse` | MirrorCache JSON listings at `https://download.opensuse.org/distribution/leap/` (and per-version `appliances/`). Walks versions descending and skips empty appliance dirs (Leap 16.1 currently is one). Handles both 15.x (`openSUSE-Leap-…`) and 16.x (`Leap-…`) naming conventions. Accepts `latest`, `15.6`, `16.0`. | `Sha256Pinned` from the per-file `.sha256` sidecar |
| `oracle` | Hashes are embedded in the templates landing page HTML at `https://yum.oracle.com/oracle-linux-templates.html`; we split on `</tr>` and pair each `<a class="kvm-image">` with its `<tt class="kvm-sha256">`. Accepts `latest`, `8`, `9`, `10` (with optional `OL` prefix). | `Sha256Pinned` from the page HTML |
| `rocky` | Per-major image dir at `https://download.rockylinux.org/pub/rocky/<major>/images/x86_64/`; pick the highest dated `Rocky-<n>-GenericCloud-Base-<ver>.x86_64.qcow2`, ignoring the rolling pointer and the LVM flavor. Accepts `latest`, `8`, `9`, `10`. | `Sha256Pinned` from the per-file BSD-style `.CHECKSUM` sidecar |
| `smartos` | Manta release dirs at `https://us-central.manta.mnx.io/Joyent_Dev/public/SmartOS/<rel>/`; sibling `latest` text file points at the current dated dir. Accepts `latest` and any explicit `<YYYYMMDD>T<HHMMSS>Z` timestamp. **Not cloud-init NoCloud — SmartOS uses `mdata-get`.** Included for ouroboros use. | `Sha256Pinned` from the dated dir's `sha256sums.txt` |
| `talos` | `https://api.github.com/repos/siderolabs/talos/releases/latest` for `latest`; explicit semver (`1.12.7` or `v1.12.7`) accepted. Image fetched from the Talos Image Factory at `https://factory.talos.dev/image/<schematic>/v<ver>/nocloud-amd64.raw.xz` with the canonical empty schematic baked in. | `TlsTrustOnly` — Talos Factory does not publish per-image hashes; users wanting a hash check should pass `--expected-sha256 <hex>`. `ssh_key=false` because Talos rejects ssh-key injection. |
| `ubuntu` | Canonical Simple Streams (`com.ubuntu.cloud:released:download.json`); fallback to a small hardcoded series table if streams is unreachable. | `Sha256Pinned` from streams JSON (primary); `Sha256SumsTls` in fallback |

Two patterns recur across these:

- **Pre-fetch the hash at resolve time** when the upstream feed exposes
  it (Ubuntu Simple Streams, Fedora `releases.json`, Oracle's HTML
  table, Alma's CHECKSUM, Rocky's per-file `.CHECKSUM`, OpenBSD's
  asset sidecar, Arch's `.SHA256`, OmniOS's `.sha256`, SmartOS's
  `sha256sums.txt`, etc.) and pin via `Sha256Pinned`. This lets
  `--dry-run` show the manifest UUID without downloading.
- **Fetch the hash at verify time** when the sidecar is non-trivially
  large or vendor-specific (Debian's SHA512SUMS, FreeBSD's
  CHECKSUM.SHA256, Alpine's per-image SHA-512). The verifier
  trait keeps the choice orthogonal.

### External: TOML profiles for pinned URLs

For private/internal builds and quick experiments, a directory of TOML files
can contribute additional vendor profiles. These do not implement custom
release resolution — they pin a single (URL, sha256) pair plus metadata.

```toml
# ~/.config/tritonadm/nocloud-vendors/internal-rocky-9.toml
name = "internal-rocky-9"
url = "https://internal.example.com/rocky-9-cloudinit.qcow2"
format = "qcow2"
os = "linux"
version = "9.4-2026-05-01"
description = "Internal Rocky 9 nocloud build"
homepage = "https://internal.example.com/"
sha256 = "..."
ssh_key = true
```

Loaded only when `--profile-dir` is passed, or from
`~/.config/tritonadm/nocloud-vendors/` if it exists. Not part of the POC.

## Pipeline

Disk-image decoding lives in-process: the
[`qcow`](https://crates.io/crates/qcow) crate (panda-re, v1.2.0,
vendored at `libs/qcow`) handles qcow2; `lzma_rs` decodes xz; a
vendored fork of [`vmdk-rs`](https://github.com/strozfriedberg/vmdk-rs)
at `libs/vmdk` reads VMware disks. No `qemu-img` dependency, no
intermediate raw file, no `dd`. The tool is expected to run as root
(GZ default; `pfexec tritonadm …` in NGZ); the zvol's character
device is opened directly.

1. `vendor.resolve(release)` → `ResolvedImage`.
2. Download `resolved.url` to `<workdir>/<filename>` with an
   `indicatif` progress bar (uses `Content-Length` if present; falls
   back to a byte-count spinner). Skip if cached.
3. `verifier.verify(downloaded_file)`.
4. Read the source's virtual disk size:
   - `Qcow2` — parse the header via `qcow::open` and read
     `qcow2.header.size`.
   - `Raw` — `len()` of the file.
   - `Xz` — read the trailing Stream Footer + Index (no
     decompression) and sum the per-Record Uncompressed Size VLIs.
     Single-stream xz only; cloud images we've seen all qualify.
   - `Vmdk` — open via `vmdkrs::VmdkReader::open` (inside
     `spawn_blocking` because the crate spins its own internal
     tokio runtime) and read `image_size` from the header chain.
   - `RawGz` — read the trailing 4-byte gzip `ISIZE` field
     (uncompressed size mod 2^32). SmartOS USB images are well
     under 4 GiB so the modulus is not a concern; we sanity-check
     the gzip magic at offset 0 to refuse non-gzip input early.
5. Create a zvol of exactly that size (rounded up to MiB):
   `zfs create -V <size>m <dataset>/<build-uuid>`.
6. Stream the decoded raw bytes into `/dev/zvol/rdsk/<dataset>`:
   - `Qcow2` — `qcow2.reader(&mut file)`, run inside
     `tokio::task::spawn_blocking`, copy 1 MiB chunks into the zvol
     with a second `indicatif` progress bar.
   - `Raw` — same loop, source is `std::fs::File`.
   - `Xz` — `lzma_rs::xz_decompress` driving a `ProgressWriter`
     wrapper on the zvol, again inside `spawn_blocking`. No
     intermediate `.raw` file.
   - `Vmdk` — `VmdkReadAdapter` wraps the crate's offset-addressed
     `read_at_offset` API as a `Read` impl so the same 1 MiB copy
     loop drives it.
   - `RawGz` — `flate2::read::GzDecoder` wraps the source file and
     the same 1 MiB copy loop drives it. SmartOS uses this format.

   **Sparse skip**: each 1 MiB chunk is checked for `all == 0`
   bytes. If yes, the writer seeks past the chunk instead of
   writing. ZFS zvols are sparse on creation, so unwritten regions
   stay unallocated logically (no on-disk block), and the resulting
   `zfs send` stream skips them. Free for qcow2/raw/vmdk (which
   surface unallocated regions as zero buffers via their reader
   APIs); the xz path doesn't get this since it goes through a
   `Write`-trait push.
7. `zfs snap <dataset>@image`.
8. `zfs send <dataset>@image > <output>.zfs`.
9. Compress with `gzip -f` (shell out; no value in pulling in `flate2`
   for the POC).
10. `zfs destroy -r <dataset>` (always, success or failure).
11. Render the IMGAPI manifest. The `manifest.in.json` template moves
    into the binary as a `serde_json::Value` skeleton; field
    substitution is typed instead of `sed`. `image_size` reflects the
    actual virtual disk size, not a hardcoded constant.
12. According to `--target`:
    - `file`: leave files in `--output-dir`.
    - `smartos`: shell out to `imgadm install -m <manifest> -f <gz>`
      (GZ-only).
    - `imgapi`: invoke the existing import code path
      (`commands::image::ImageCommand::Import` factored into a callable
      function `import_manifest_and_file`).

### Zone awareness

The tool detects whether it is running in the GZ or an NGZ via
`zonename`:

- **GZ** (`zonename` = `global`): default zvol parent dataset is
  `zones`. `--target smartos` is allowed (this is the only zone where
  `imgadm install` can write to the global SmartOS image store).
- **NGZ** (`zonename` ≠ `global`): default zvol parent dataset is
  `zones/<zonename>/data`. The tool checks that this dataset exists
  with `zoned=on` (i.e. is a delegated dataset) and bails otherwise.
  `--target smartos` is rejected; the operator should produce files
  with `--target file` and run `imgadm install` from the GZ.

Both modes are explicitly allowed and tested; `--dataset` overrides
the default for either.

### `zfs` shellout module

A small `zfs` module wraps the `zfs(8)` CLI: `create_zvol`, `snap`,
`send_to_file`, `destroy_recursive`, plus the listing/age helpers
the startup sweep needs. All assume the process has sufficient
privileges (root in the GZ, `pfexec tritonadm …` in NGZ). This
replaces the earlier `Privileged` trait — once qcow2 decoding moved
in-process, the only privileged ops left were `zfs(8)` calls and
opening the zvol's char device, both of which are uniform enough
that a trait-based seam wasn't pulling its weight.

## Trust chain

Every vendor roots trust in TLS to its canonical host (Mozilla CA
bundle, via `triton-tls::build_http_client`); the verifier strategy
then narrows the trust to a specific image. The sums-parser helpers
(`verify::parse_sums_file` for Linux-style `<hex>  <filename>` and
`verify::parse_bsd_sums_file` for BSD-style `SHA256 (filename) = hex`)
are shared across vendors that need them.

| Vendor | Verifier | Source of truth |
|---|---|---|
| `alma` | `Sha256Pinned` | Linux-style `CHECKSUM` in the major's images dir, resolved at metadata time |
| `alpine` | `Sha512SidecarTls` | per-image `<file>.sha512` next to the image |
| `arch` | `Sha256Pinned` | per-file `<file>.SHA256` in the versioned build dir |
| `centos-stream` | `Sha256Pinned` | per-file `<file>.SHA256SUM` (BSD-style) in the stream's images dir |
| `debian` | `Sha512SumsTls` | `SHA512SUMS` in the codename's image directory |
| `fedora` | `Sha256Pinned` | `releases.json` at fedoraproject.org carries the sha256 inline |
| `freebsd` | `Sha256BsdSumsTls` | `CHECKSUM.SHA256` in the release directory (BSD `SHA256 (file) = hex` lines) |
| `omnios` | `Sha256Pinned` | bare-hash `<file>.sha256` sidecar in the channel directory |
| `openbsd` | `Sha256Pinned` | `<file>.sha256` GitHub release asset alongside the qcow2 |
| `opensuse` | `Sha256Pinned` | per-file `<file>.sha256` (Linux-style) in the appliances dir |
| `oracle` | `Sha256Pinned` | hashes embedded in `oracle-linux-templates.html`, paired with kvm-image links per `<tr>` |
| `rocky` | `Sha256Pinned` | per-file `<file>.CHECKSUM` (BSD-style) sidecar |
| `smartos` | `Sha256Pinned` | dated dir's `sha256sums.txt` |
| `talos` | `TlsTrustOnly` | Talos Image Factory does not publish per-image hashes; operators wanting a hash check should pass `--expected-sha256 <hex>` |
| `ubuntu` | `Sha256Pinned` from Simple Streams (primary) / `Sha256SumsTls` (fallback) | `com.ubuntu.cloud:released:download.json`, with a hardcoded series table as the air-gapped escape hatch |

This is weaker than a GPG-verified `SHA256SUMS.gpg` would be, but no
weaker than what the original `target/triton-nocloud-images/*.conf`
files gave us (static hashes in our repo, equally TLS-rooted).

GPG verification is a follow-up and is plumbed for via the
`Verifier` trait — adding a `Sha256SumsGpg` strategy with an
embedded vendor pubkey is a localized change.

## Code layout

```
cli/tritonadm/src/commands/
    image.rs                     # FetchNocloud + Import dispatch; shared
                                 #   import_manifest_and_file helper
    image/
        nocloud.rs               # subcommand wiring, Target dispatch,
                                 #   non-SmartOS auto-promote-to-dry-run
        nocloud/
            vendor.rs            # VendorProfile trait, ResolvedImage,
                                 #   Vendor enum (clap ValueEnum), SourceFormat
            vendor/              # one <vendor>.rs + <vendor>/<resolver>.rs each:
                alma.rs          alma/releases.rs
                alpine.rs        alpine/releases.rs
                arch.rs          arch/releases.rs
                centosstream.rs  centosstream/releases.rs
                debian.rs        debian/release_file.rs
                fedora.rs        fedora/releases.rs
                freebsd.rs       freebsd/releases.rs
                omnios.rs        omnios/releases.rs
                openbsd.rs       openbsd/releases.rs
                opensuse.rs      opensuse/releases.rs
                oracle.rs        oracle/releases.rs
                rocky.rs         rocky/releases.rs
                smartos.rs       smartos/releases.rs
                talos.rs         talos/releases.rs
                ubuntu.rs        ubuntu/streams.rs
            verify.rs            # Verifier trait + Sha256Pinned, Sha256SumsTls,
                                 #   Sha512SumsTls, Sha256BsdSumsTls,
                                 #   Sha512SidecarTls, TlsTrustOnly,
                                 #   shared parse_sums_file / parse_bsd_sums_file
            pipeline.rs          # download → verify → zvol → snap → send → gzip;
                                 #   sparse-skip in copy_with_progress;
                                 #   gzip ISIZE for RawGz virtual-size
            zfs.rs               # zfs(8) shellout (create_zvol, snap,
                                 #   send_to_file, destroy_recursive, sweep helpers)
            manifest.rs          # build serde_json::Value manifest from ResolvedImage

libs/qcow/                       # vendored panda-re/qcow-rs (drops zlib-ng-compat
                                 #   feature so flate2 falls back to miniz_oxide)
libs/vmdk/                       # vendored strozfriedberg/vmdk-rs minus the s3
                                 #   client, foyer cache, and CLI binary
                                 #   (318 → 112 transitive crates)
```

`image.rs` is already large; the new code lives in a sibling
`image/nocloud/` tree to keep the diff readable. Each vendor lives
in its own `<vendor>.rs` file with its release-resolution logic
under `<vendor>/`, so adding a vendor is a self-contained change.

## Testability

In this builder NGZ (`5f7163ee-bdde-4638-a22e-a1915233e159`,
`zones/<zone>/data` zoned=on):

- **Unit (no zone needed)**: vendor URL resolution against a wiremock,
  `SHA256SUMS` parsing, manifest rendering, `Verifier` strategies against
  fixture blobs.
- **Integration, gated**: a `--features integration` test or a Make target
  that runs the full pipeline against a small synthetic raw image (a 4 MiB
  file of zeros), expecting `pfexec` and a delegated dataset. Asserts the
  final `*.zfs.gz` deserializes and that `zfs receive` round-trips.
- **End-to-end, network-gated**: one Ubuntu run per supported series. Slow
  (~hundreds of MB downloaded). Off by default; opt-in via env var or
  separate Make target.

For GZ testing, build the binary, run elfedit per `portable-binaries.md`,
copy out, and run `tritonadm image fetch-nocloud --target smartos`.

## Status

Implemented and verified in this builder NGZ:

- `tritonadm image fetch-nocloud` end-to-end for **fifteen** vendors:
  alma, alpine, arch, centos-stream, debian, fedora, freebsd,
  omnios, openbsd, opensuse, oracle, rocky, smartos, talos, ubuntu —
  producing a valid `*.zfs.gz` + `*.json` pair.
- Source formats: **qcow2**, **raw**, **xz** (single-stream, virtual
  size read from the trailing Index without decompressing), **vmdk**
  (via vendored `libs/vmdk`), and **raw.gz** (gzip-decoded straight
  to the zvol; SmartOS USB image).
- `image_size` is derived from the actual upstream virtual disk size
  rather than a hardcoded constant.
- `--vendor` is a clap `ValueEnum`; manifest UUIDs are stable
  (derived from the upstream sha256), so re-running for the same
  upstream image produces an identical UUID.
- Workdir flock prevents concurrent same-`(vendor, release)` builds;
  startup sweep cleans older leftover datasets; SIGINT handler
  best-effort-cleans the in-flight dataset.
- `--dry-run`, `--expected-sha256` override, and Content-Length
  assertion on download.
- Auto-promote to `--dry-run` when `uname -v` doesn't start with
  `joyent_`, so the common dev-box use case
  (`tritonadm image fetch-nocloud --vendor X --release latest` on
  macOS / Linux) exercises the resolver and prints a plan instead
  of erroring.
- `--target` selects the delivery mode: `file` (default), `smartos`
  (shells `imgadm install -m <manifest> -f <gz>`, GZ-only) or
  `imgapi` (reuses the `tritonadm image import` code path via the
  shared `import_manifest_and_file` helper, against the
  auto-detected IMGAPI URL).
- Tests: 99 unit tests pass; clippy is clean.
- No external binary dependencies beyond `zfs(8)`, `gzip(1)`,
  `digest(1)` (illumos SHA-1), and — for `--target smartos` —
  `imgadm(1M)`. qcow2, xz, vmdk, and tar+gzip decoding are fully
  in-process (pure-Rust crates).

Outstanding follow-ups:

- `Sha256SumsGpg` verifier with embedded vendor pubkeys (Debian,
  Ubuntu canonical-signed `SHA256SUMS.gpg`; arch, openbsd, and
  several others also publish detached signatures).
- TOML profile loading from `--profile-dir`.
- Debian `testing` / `sid` / `unstable` (deferred; see below).
- Sparse-skip on the xz path (`Write`-driven; needs a buffering
  zero-detecting `Write` adapter).
- Two notes on the SmartOS / OmniOS additions: the `os` field
  reports `illumos` for both (matches OmniOS's existing convention),
  and SmartOS does **not** support cloud-init NoCloud — guests
  provision via `mdata-get`. SmartOS is included in "ouroboros
  mode" because the same machinery that turns Linux/BSD nocloud
  images into Triton-importable manifests is mechanically the
  same operation.

### Parallel builds

Different `(vendor, release)` pairs run in parallel without conflict.

Same `(vendor, release)` pair in parallel is rejected fast: the
pipeline takes a `std::fs::File::try_lock` (LOCK_EX | LOCK_NB) on
`<workdir>/.lock` before doing any I/O. A second invocation prints
a clear error and exits non-zero. The lock state lives on the file
descriptor; the kernel releases it on any process exit (clean or
otherwise), so a SIGKILL'd run never leaves a stuck lock.

The startup sweep skips datasets younger than `SWEEP_MIN_AGE_SECS`
(currently 1 hour) so it can't accidentally destroy another concurrent
build's in-flight dataset. Older leftovers that happen to be busy
are detected via the failed `zfs destroy` and logged as
`busy/refused …; leaving in place` rather than silently dropped.

### Cleanup gaps

- The SIGINT handler best-effort-cleans-up the in-flight dataset.
  SIGKILL (`kill -9`) bypasses it; the dataset stays. The startup
  sweep on the next run is the safety net for that case.
- Children spawned by the build (`zfs send`, `gzip`) inherit the
  process group, so a TTY-delivered SIGINT reaches them directly.
  A signal delivered via `kill <pid>` to just our PID would not
  reach them, and the cleanup would race with their completion.

### Debian: testing / unstable / sid support

The current Debian profile only resolves released suites (stable,
oldstable, oldoldstable, plus their codenames). `testing`, `sid`,
and `unstable` fail with `no Version field in Release file`.
Three things would need to change to support them:

1. **Tolerate a missing `Version` field.** Debian doesn't assign
   point-release version numbers to development suites. The
   manifest version would have to come from somewhere else — most
   plausibly the `Last-Modified` header of the daily SHA512SUMS
   file, or simply today's date (matches what the bash builder
   does).
2. **Use the daily-build URL prefix.** Released images live at
   `cloud.debian.org/images/cloud/<codename>/latest/`. Development
   images live at `cloud.debian.org/images/cloud/<codename>/daily/latest/`.
3. **Branch the filename pattern.** Released suites use
   `debian-<major>-genericcloud-amd64.qcow2`. Testing
   (currently forky) uses `debian-<upcoming-major>-genericcloud-amd64-daily.qcow2`
   — so the major comes from somewhere outside the apt Release
   file. sid uses `debian-sid-genericcloud-amd64-daily.qcow2`,
   substituting the codename for the major.

Roughly ~30 lines of branching in the Debian vendor module.
Tracked here so we can revisit if there's demand for "try the
next Debian on SmartOS."

## Manifest field decisions

- `min_platform`: `{"7.0": "20260306T044811Z"}`, hardcoded. This marks
  when CloudInit `nocloud` datasource support landed in SmartOS — it is
  a property of the platform, not of the source image, and is identical
  for every image this tool produces.
- `owner`: `00000000-0000-0000-0000-000000000000` ("admin"), matches the
  template.
- `tags.role` = `os`; `tags.org.smartos:cloudinit_datasource` = `nocloud`.
  Constants in the typed builder.
- Cache: keyed on the resolved URL (matches the bash's per-URL-hash cache).
  Re-running with the same `--release` does not re-download. `--keep-cache`
  is the off switch for cache cleanup on success.
