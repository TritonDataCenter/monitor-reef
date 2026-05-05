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
                              [--profile-dir DIR]
                              [--insecure-no-verify]
                              [--keep-cache]
```

| Flag | Meaning |
|---|---|
| `--vendor` | Vendor profile name (e.g. `ubuntu`, `debian`, `alpine`). For the POC: only `ubuntu`. |
| `--release` | Vendor-specific release token: a series name (`noble`), a version (`24.04`), or `latest`. Resolution is vendor-specific. |
| `--target` | Where the image goes after build. Default: `file`. `smartos` shells out to `imgadm install -f <file> -m <manifest>` (GZ-only). `imgapi` calls into `tritonadm image import` machinery using the auto-detected IMGAPI URL. |
| `--output-dir` | Directory for `*.zfs.gz` and `*.json`. Default: `/var/tmp/tritonadm/nocloud/image/<vendor>-<series>/`. |
| `--workdir` | Working directory for downloads and intermediate raw files. Default: `/var/tmp/tritonadm/nocloud/cache/<vendor>-<series>/`. |
| `--profile-dir` | Optional directory of TOML profile files for custom (non-baked-in) vendors. |
| `--insecure-no-verify` | Skip checksum/signature verification. Development only. |
| `--keep-cache` | Don't clean the working directory on success. |

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

### Built-in vendor list (this design)

The POC ships only `ubuntu`. The trait shape accommodates these follow-ups:

| Vendor | Release discovery | Verifier (target) |
|---|---|---|
| `ubuntu` | Canonical Simple Streams (`com.ubuntu.cloud:released:download.json`); fallback to a small hardcoded series table if streams is unreachable | `Sha256Pinned` from streams JSON (primary); `Sha256SumsTls` in fallback |
| `debian` | hardcoded codename → URL table | `Sha256SumsGpg` |
| `alpine` | hardcoded `MAJOR.MINOR` table | `Sha256Pinned` from vendor's per-image checksum file |
| `freebsd` | hardcoded `MAJOR.MINOR` table | `CHECKSUM.SHA256` over TLS |
| `talos` | factory API (vendor-specific) | factory API (vendor-specific) |

Ubuntu's Simple Streams gives us three things over a hardcoded table:
no tool update needed when a new LTS ships (`latest` resolves to whatever
is currently flagged supported and LTS); the manifest `version` is the
canonical upstream build serial (e.g. `20260321`) instead of "today's
date"; and the streams JSON has the sha256 inline, so the verifier is a
plain `Sha256Pinned` rather than a second TLS roundtrip to fetch a
`SHA256SUMS` file. The fallback table is the air-gapped escape hatch.

Vendor-specific code is the whole point of this tool; it is appropriate to
keep it under code review rather than as data files.

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

The pipeline keeps qcow2 decoding in-process via the
[`qcow`](https://crates.io/crates/qcow) crate (panda-re, v1.2.0). No
`qemu-img` dependency, no intermediate raw file, no `dd`. The tool is
expected to run as root (GZ default; `pfexec tritonadm …` in NGZ); the
zvol's character device is opened directly.

1. `vendor.resolve(release)` → `ResolvedImage`.
2. Download `resolved.url` to `<workdir>/<filename>` with an
   `indicatif` progress bar (uses `Content-Length` if present; falls
   back to a byte-count spinner). Skip if cached.
3. `verifier.verify(downloaded_file)`.
4. Read the source's virtual disk size:
   - `Qcow2` — parse the header via `qcow::open` and read
     `qcow2.header.size`.
   - `Raw` — `len()` of the file.
   - `Xz` — deferred.
5. Create a zvol of exactly that size (rounded up to MiB):
   `zfs create -V <size>m <dataset>/<build-uuid>`.
6. Stream the decoded raw bytes into `/dev/zvol/rdsk/<dataset>`:
   - `Qcow2` — `qcow2.reader(&mut file)`, run inside
     `tokio::task::spawn_blocking`, copy 1 MiB chunks into the zvol
     with a second `indicatif` progress bar.
   - `Raw` — same loop, source is `std::fs::File`.
   - `Xz` — deferred (xz2 + same loop).
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
    - `smartos`: shell out to `imgadm install -f <gz> -m <manifest>`
      (GZ-only).
    - `imgapi`: invoke the existing import code path
      (`commands::image::ImageCommand::Import` factored into a callable
      function).

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
`send_to_file`, `destroy_recursive`. All assume the process has
sufficient privileges (root in the GZ, `pfexec tritonadm …` in NGZ).
This replaces the earlier `Privileged` trait — once qcow2 decoding
moved in-process, the only privileged ops left were `zfs(8)` calls and
opening the zvol's char device, both of which are uniform enough that
a trait-based seam wasn't pulling its weight.

Two implementations: `PfexecPrivileged` (real) and `FakePrivileged` (test —
operates against a tmpfile-backed sparse file pretending to be a zvol).

## Trust chain

For Ubuntu (POC):

- Root: TLS to `cloud-images.ubuntu.com` (Mozilla CA bundle, via
  `triton-tls::build_http_client`).
- Mid: fetch `https://cloud-images.ubuntu.com/<series>/current/SHA256SUMS`
  over TLS, parse, find the line matching the downloaded filename.
- Leaf: SHA-256 of the downloaded file matches.

This is weaker than a GPG-verified `SHA256SUMS.gpg`, but no weaker than what
the existing `target/triton-nocloud-images/*.conf` files give us today
(static SHA-256 in our repo, equally TLS-rooted).

GPG verification is a follow-up and is plumbed for via the `Verifier` trait.

## Code layout

```
cli/tritonadm/src/commands/
    image.rs                     # extend with `FetchNocloud { ... }` arm
    image/
        nocloud/
            mod.rs               # subcommand wiring
            vendor.rs            # VendorProfile trait + ResolvedImage
            vendor/
                ubuntu.rs        # Ubuntu impl (POC)
            verify.rs            # Verifier trait + Sha256SumsTls + Sha256Pinned
            pipeline.rs          # the steps above, takes &dyn Privileged
            privileged.rs        # PfexecPrivileged + FakePrivileged
            manifest.rs          # build serde_json::Value manifest from ResolvedImage
```

`image.rs` is already large; the new code lives in a sibling
`image/nocloud/` tree to keep the diff readable.

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

## POC status

Implemented and verified in this builder NGZ on 2026-05-05:

- `tritonadm image fetch-nocloud --vendor ubuntu --release noble`
  end-to-end produces a valid `*.zfs.gz` + `*.json` pair.
- Run time on first invocation (cold cache): ~3 minutes; download
  dominates.
- Output: `image_size = 3584 MiB` (the actual qcow2 virtual disk size),
  not the previously-hardcoded 10240.
- 8 unit tests pass (`cargo test -p tritonadm`); clippy is clean.
- No external binary dependencies beyond `zfs(8)`, `gzip(1)`, and
  `digest(1)` (illumos SHA-1) — qcow2 decoding is fully in-process.

Follow-ups (not in POC scope):

- `--target smartos` (shells out to `imgadm install`).
- `--target imgapi` (reuses `tritonadm image import` machinery).
- Other vendors: Debian, Alpine, FreeBSD, Talos.
- `Sha256SumsGpg` verifier with embedded vendor pubkeys.
- Xz source format (Talos, FreeBSD).
- TOML profile loading from `--profile-dir`.

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

### Image format / vendor follow-ups

- `Xz` source format (Talos, FreeBSD).
- Other vendors (Debian, Alpine, FreeBSD, Talos).
- `Sha256SumsGpg` verifier for vendors that publish detached
  signatures (Debian, Ubuntu canonical-signed `SHA256SUMS.gpg`).
- TOML profile loading from `--profile-dir`.
- `--target smartos` (shells `imgadm install`).
- `--target imgapi` (reuses `tritonadm image import` machinery).

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
