<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Building CLI Applications

Once you have a generated client library, you can build command-line tools on top of it.

## Setup

```bash
# 1. Create CLI directory structure
mkdir -p cli/my-service-cli/src

# 2. Create Cargo.toml
cat > cli/my-service-cli/Cargo.toml <<EOF
[package]
name = "my-service-cli"
version = "0.1.0"
edition.workspace = true

[[bin]]
name = "my-service"
path = "src/main.rs"

[dependencies]
my-service-client = { path = "../../clients/internal/my-service-client" }
clap = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
serde_json = { workspace = true }
EOF

# 3. Implement CLI in src/main.rs using the generated client
# 4. Add 'cli/my-service-cli' to workspace Cargo.toml members list
# 5. Build
make package-build PACKAGE=my-service-cli
```

## Example

See `cli/bugview-cli` for a complete working CLI that uses `bugview-client`.

## Benefits

- Type-safe client library handles all API communication
- CLI focuses on user experience (argument parsing, output formatting)
- API changes automatically flow through client regeneration
- Client library can be reused by other applications
