<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Phase 4: Generate CLI

**Standalone skill for generating the command-line interface.**

## Inputs

- **Service name**: Name of the service (e.g., "vmapi")
- **Plan file**: `conversion-plans/<service>/plan.md`

## Outputs

- **CLI crate**: `cli/<service>-cli/`
- **Updated plan file** with Phase 4 status

## Prerequisites

- Phase 3 complete
- Client crate builds successfully

## Tasks

### 1. Create All CLI Files FIRST

**IMPORTANT:** Create ALL files before adding to workspace.

```
cli/<service>-cli/
├── Cargo.toml
└── src/
    └── main.rs
```

### 2. Create Cargo.toml

```toml
[package]
name = "<service>-cli"
version = "<version-from-plan>"
edition.workspace = true
description = "CLI for <Service>"

[[bin]]
name = "<service>"
path = "src/main.rs"

[dependencies]
<service>-api = { path = "../../apis/<service>-api" }
<service>-client = { path = "../../clients/internal/<service>-client" }
clap = { workspace = true, features = ["env"] }  # "env" REQUIRED
tokio = { workspace = true }
anyhow = { workspace = true }
serde_json = { workspace = true }
```

### 3. Create src/main.rs

**CRITICAL: The CLI must expose EVERY API endpoint for validation testing.**

Key implementation notes:

1. **Clap `env` feature** - Required for `#[arg(env = "VAR")]`

2. **Progenitor builder pattern:**
   ```rust
   // CORRECT:
   client.get_server().server_uuid(&uuid).send().await?
   // WRONG:
   client.get_server(&uuid).await?
   ```

3. **Method names from path params:**
   - `/tasks/{taskid}` → `.taskid()` (not `.task_id()`)
   - `/servers/{server_uuid}` → `.server_uuid()`

4. **Unwrap ResponseValue:**
   ```rust
   let response = client.list().send().await?;
   let items = response.into_inner();
   ```

5. **Use typed enums for CLI args, not strings:**
   ```rust
   // GOOD: typed enum with value_enum
   #[arg(long, short, value_enum)]
   pub state: Option<Vec<MachineState>>,

   // BAD: string that must be parsed manually
   #[arg(long, short)]
   pub state: Option<Vec<String>>,
   ```

6. **Display enum values with `enum_to_display()`, NEVER with Debug format:**
   ```rust
   // GOOD: produces wire-format string like "running"
   use crate::output::enum_to_display;
   println!("State: {}", enum_to_display(&machine.state));

   // BAD: produces "Running" (Debug format) — WRONG case, breaks comparisons
   println!("State: {}", format!("{:?}", machine.state).to_lowercase());
   ```
   The `enum_to_display()` utility uses `serde_json::to_value()` to get the exact
   wire-format string (respecting `rename_all`). If this CLI is standalone (not part of
   triton-cli), add a local helper:
   ```rust
   fn enum_to_display(val: &(impl serde::Serialize + std::fmt::Debug)) -> String {
       serde_json::to_value(val)
           .ok()
           .and_then(|v| v.as_str().map(String::from))
           .unwrap_or_else(|| format!("{:?}", val))
   }
   ```

7. **Compare enum values directly, not as strings:**
   ```rust
   // GOOD: type-safe comparison
   if machine.state == MachineState::Failed { ... }

   // BAD: fragile string comparison with latent bugs
   if format!("{:?}", machine.state).to_lowercase() == "failed" { ... }
   ```

**Basic structure:**

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};
use <service>_client::Client;

#[derive(Parser)]
#[command(name = "<service>")]
#[command(about = "CLI for <Service>")]
struct Cli {
    #[arg(long, env = "<SERVICE>_URL", default_value = "http://localhost")]
    base_url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ping the service
    Ping,

    /// List all resources
    List {
        #[arg(long)]
        raw: bool,
    },

    /// Get a specific resource
    Get {
        id: String,
        #[arg(long)]
        raw: bool,
    },

    // ONE SUBCOMMAND FOR EVERY ENDPOINT!
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new(&cli.base_url);

    match cli.command {
        Commands::Ping => {
            let resp = client.ping().send().await
                .map_err(|e| anyhow::anyhow!("Ping failed: {}", e))?;
            println!("{}", serde_json::to_string_pretty(&resp.into_inner())?);
        }
        Commands::List { raw } => {
            let resp = client.list_resources().send().await
                .map_err(|e| anyhow::anyhow!("List failed: {}", e))?;
            let items = resp.into_inner();
            if raw {
                println!("{}", serde_json::to_string_pretty(&items)?);
            } else {
                for item in &items {
                    println!("{}: {}", item.id, item.name);
                }
            }
        }
        // Handle ALL commands
    }

    Ok(())
}
```

### 4. Implement ALL Endpoints

Read the plan file's endpoint list. Every endpoint needs a CLI command:

**For simple endpoints:**
```
<service> ping
<service> list [--raw]
<service> get <id> [--raw]
<service> delete <id>
```

**For action-dispatch endpoints:**
```
<service> start <uuid>
<service> stop <uuid>
<service> update <uuid> --ram 1024 --cpu-cap 100
```

**For nested resources:**
```
<service> snapshot list <vm-uuid>
<service> snapshot create <vm-uuid> --name <name>
<service> nic add <vm-uuid> --network <uuid>
```

### 5. Add to Workspace

**Only after ALL files exist**, edit root `Cargo.toml`:

```toml
members = [
    # ... existing
    "cli/<service>-cli",
]
```

### 6. Build CLI

```bash
make format package-build PACKAGE=<service>-cli
```

Common errors:
- Missing `features = ["env"]` on clap
- Wrong method names (check generated client)
- Missing response field names

### 7. Full Workspace Build

```bash
make format build
make openapi-check
```

### 8. Update Plan File

Add to `conversion-plans/<service>/plan.md`:

```markdown
## Phase 4 Complete

- CLI crate: `cli/<service>-cli/`
- Binary name: `<service>`
- Commands implemented: <count>
- Build status: SUCCESS

### CLI Commands
- `<service> ping` - Health check
- `<service> list` - List resources
- ... (list all)

## Phase Status
- [x] Phase 1: Analyze - COMPLETE
- [x] Phase 2: Generate API - COMPLETE
- [x] Phase 3: Generate Client - COMPLETE
- [x] Phase 4: Generate CLI - COMPLETE
- [ ] Phase 5: Validate
```

## Success Criteria

Phase 4 is complete when:
- [ ] All CLI files created before workspace addition
- [ ] Cargo.toml has `clap = { ..., features = ["env"] }`
- [ ] **EVERY** API endpoint has a CLI command
- [ ] `--raw` flag on read operations
- [ ] Environment variable for base URL
- [ ] Added to workspace Cargo.toml
- [ ] `make format package-build PACKAGE=<service>-cli` succeeds
- [ ] `make format build` succeeds
- [ ] `make openapi-check` passes
- [ ] Plan file updated with command list

## Error Handling

If build fails:
- Document specific errors in plan.md
- Set Phase 4 status to "FAILED: <reason>"

## After Phase Completion

The orchestrator will run:
```bash
make check
git add cli/<service>-cli/ conversion-plans/<service>/plan.md Cargo.toml Cargo.lock
git commit -m "Add <service> CLI (Phase 4)"
```
