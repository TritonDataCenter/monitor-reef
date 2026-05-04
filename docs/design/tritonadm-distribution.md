<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Shipping `tritonadm` to operators

## Background

`docs/design/tritonadm.md` leaves three distribution-shaped questions open:

- **Open Question §2** — self-update mechanism
- **Open Question §3** — GZ vs zone install
- **Open Question §4** — operator migration path

This doc closes §2 and §3, and lays out a staged path that lets us put
`tritonadm` in operators' hands quickly so the rest of the
`tritonapi-skeleton` work is testable without requiring people to clone
this repo and `cargo build`.

## Recommended approach (the headline)

Ship `tritonadm` as a **GZ tool tarball** published to
`updates.tritondatacenter.com` as an IMGAPI image of `type: "other"`, and
bootstrap installation via a single `install-tritonadm.sh` script run
once on the headnode GZ. Subsequent updates use `tritonadm self-update`
(already stubbed in the CLI scaffold) against the same updates server.

A first-class `sdcadm` integration (`sdcadm experimental
install-tritonadm`) is a small upstream PR we can do later — the bootstrap
script unblocks operator testing today without it.

Why this shape:

- **GZ install matches sdcadm.** Removes the chicken-and-egg of needing
  `tritonadm` to install `tritonadm`. Avoids the full zone-image overhead
  for what is a single static-ish binary.
- **Updates server is already on the table.** `tritonadm` itself knows the
  URL (`DEFAULT_UPDATES_URL` in `cli/tritonadm/src/main.rs`), and the eng
  pipeline (`deps/eng/tools/bits-upload.sh`) already publishes there. No
  new infrastructure.
- **Single binary keeps the tarball trivial.** Portable-binary handling
  is one `elfedit` invocation per `docs/design/portable-binaries.md`. The
  tarball is `tritonadm` binary + completions + a small wrapper.

## Build artifact

A release tarball with this layout:

```
tritonadm-<stamp>.tgz
└── root/
    └── opt/
        └── triton/
            └── tritonadm/
                ├── bin/tritonadm        # cargo build --release, RPATH stripped
                ├── etc/version          # build stamp + git SHA
                └── share/completions/   # bash, zsh, fish
```

Build steps (encapsulated in `images/tritonadm/Makefile`):

1. `cargo build --release -p tritonadm`
2. `/usr/bin/elfedit -e 'dyn:delete RUNPATH' …` and the same for RPATH
   (skipped on non-illumos build hosts; the result is then a dev-only
   tarball).
3. Run the freshly-built binary to emit shell completions
   (`tritonadm completion bash|zsh|fish`).
4. Stage into `proto/root/opt/triton/tritonadm/`.
5. `tar -czf tritonadm-<stamp>.tgz -C proto root`.
6. Generate an IMGAPI-compatible manifest
   (`tritonadm-<stamp>.imgmanifest`) that points at the tarball with
   `sha1`, `size`, and `type: "other"`.

Both files are dropped into `bits/tritonadm/` so the standard eng
`bits-upload.sh -p` flow can publish them to
`updates.tritondatacenter.com`.

## Publishing

Reuse the existing CI pattern: `images/tritonadm/Jenkinsfile` calls
`joyBuildImageAndUpload(dir: 'images/tritonadm')` (the `dir` parameter
already documented in `docs/design/zone-image-builds.md`). The Makefile's
`bits-upload` target then publishes via `updates-imgadm` to the
`experimental` channel during early bake, promoting to `release` after
operators have validated it.

The IMGAPI manifest produced by `images/tritonadm/scripts/make-manifest.sh`
follows the `type: "other"` convention used by sdcadm itself, so
`updates-imgadm import` and the IMGAPI client already handle it.

## Installation (initial — no sdcadm change required)

`tools/install-tritonadm.sh` runs on a headnode GZ and:

1. Queries `updates.tritondatacenter.com` for the latest `tritonadm`
   manifest (defaulting to `--channel experimental`), or accepts an
   explicit image UUID.
2. Downloads the tarball, verifies the `sha1` from the manifest.
3. Extracts under `/opt/triton/tritonadm/`.
4. Symlinks `/opt/local/bin/tritonadm` → `/opt/triton/tritonadm/bin/tritonadm`.
5. Records the installed UUID + version in `/opt/triton/tritonadm/etc/version`.

Operators run:

```sh
curl -sSf https://raw.githubusercontent.com/.../tools/install-tritonadm.sh \
    | bash -s -- --channel experimental
```

(or fetch + inspect first, per usual operator hygiene). For air-gapped
DCs, the same script accepts `--tarball <path> --manifest <path>`.

## `sdcadm` integration (deferred)

Once the bootstrap path is shaken out, propose an upstream PR to
`TritonDataCenter/sdcadm` that adds `sdcadm experimental
install-tritonadm`. It does the same fetch/install dance but reuses
sdcadm's existing IMGAPI/updates plumbing. Putting it under
`experimental` avoids committing to a permanent surface, and the
long-term plan is for `tritonadm` to assume sdcadm's role anyway, so
deeper integration would be wasted work.

## `tritonadm self-update`

Already a top-level command in `docs/design/tritonadm.md`. Implementation:

- Reuse the same updates-server client that `tritonadm avail` /
  `tritonadm image import` already use.
- Fetch the latest manifest for `name=tritonadm` (channel from the
  `--channel` flag or `UPDATES_URL`-equivalent env var).
- Compare the manifest UUID to the installed UUID in
  `/opt/triton/tritonadm/etc/version`. Bail if same.
- Download the new tarball, verify SHA, stage to a temp dir.
- Atomically replace `/opt/triton/tritonadm/bin/tritonadm` (rename-over).
  Running processes finish on the old binary; new invocations pick up
  the new one.

## Channels & versioning

- **Versions** follow the eng `STAMP` convention: `<branch>-<UTC>-g<sha>`,
  matching every other Triton component.
- **Channels**: `experimental` (default during the tritonapi-skeleton
  rollout), `dev`, `release`. `tritonadm`'s existing `--channel` flag
  selects.

## Open questions

1. **Manifest signing**: sdcadm relies on TLS + manifest SHA-1 for
   integrity. Match for now; revisit if Triton adopts signed manifests
   uniformly.
2. **`install-tritonadm.sh` hosting**: shipped in the repo today
   (`tools/install-tritonadm.sh`). Should we also push a copy to the
   updates server so a single curl URL is the canonical install path?
   Recommendation: yes, as part of the same `bits-upload` step.
3. **Cross-platform builds**: the headnode GZ is illumos-x86_64 only, so
   that's the minimum. Developers may want macOS/Linux dev tarballs for
   local testing — out of scope for the first cut, can add later via a
   matrix Jenkins job.
4. **`pkg_install`-style sigverify**: not on the table for V1, but worth
   noting if Triton tooling moves toward signed releases.
