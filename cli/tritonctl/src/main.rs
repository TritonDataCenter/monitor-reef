// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! tritonctl — the Triton Cloud tenant CLI.
//!
//! Self-service surface for a tenant: instances, disks, networking, and
//! images, always bound to the caller's own tenant. Scope is inferred
//! from the bearer token; `--project` narrows within the tenant. This
//! binary never targets the operator `/v1/system/*` surface — that is
//! `tritonadm`'s job.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use triton_cli_core::{App, Config, OutputFormat, Session, Table, emit, login};
use uuid::Uuid;

/// Per-binary identity: config under `~/.config/triton/tritonctl/`,
/// env vars `TRITONCTL_*`.
const APP: App = App::new("tritonctl", "TRITONCTL");

#[derive(Parser)]
#[command(name = "tritonctl", version, about = "Triton Cloud tenant CLI")]
struct Cli {
    /// Cluster endpoint. Falls back to TRITONCTL_ENDPOINT, then config.
    #[arg(long, global = true)]
    endpoint: Option<String>,

    /// API-key bearer. Falls back to TRITONCTL_API_KEY.
    #[arg(long, global = true)]
    api_key: Option<String>,

    /// Project to scope tenant resources to (UUID). The tenant itself is
    /// inferred from your token.
    #[arg(long, global = true, env = "TRITONCTL_PROJECT")]
    project: Option<Uuid>,

    /// Output format. Defaults to a table on a TTY, JSON when piped.
    #[arg(short = 'o', long, global = true, value_enum)]
    output: Option<OutputFormat>,

    /// Omit table headers (table output only).
    #[arg(long, global = true)]
    no_headers: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authenticate and persist credentials.
    Configure {
        /// Endpoint to authenticate against.
        #[arg(long)]
        endpoint: Option<String>,
        /// Username (default: prompt).
        #[arg(long)]
        username: Option<String>,
        /// Read the password from stdin instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Re-authenticate against the configured endpoint.
    Login {
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password_stdin: bool,
    },
    /// Remove stored credentials.
    Logout,
    /// Show the current identity and effective scope.
    Whoami,
    /// Print shell exports for the current session.
    Env,
    /// Manage instances (VMs).
    Instance {
        #[command(subcommand)]
        cmd: InstanceCmd,
    },
}

#[derive(Subcommand)]
enum InstanceCmd {
    /// List your instances.
    List {
        /// Restrict to instances from a given image.
        #[arg(long)]
        image: Option<Uuid>,
        /// Restrict to a lifecycle state.
        #[arg(long)]
        state: Option<String>,
    },
    /// Show one instance by UUID.
    Show { id: Uuid },
    /// Create an instance under a project.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        image_id: Uuid,
        #[arg(long)]
        primary_subnet_id: Uuid,
        /// SSH key UUIDs to inject (repeatable).
        #[arg(long = "ssh-key-id")]
        ssh_key_ids: Vec<Uuid>,
        #[arg(long)]
        cpu: u32,
        #[arg(long)]
        memory_bytes: u64,
        #[arg(long)]
        disk_bytes: Option<u64>,
    },
    /// Delete an instance (must be Stopped or Failed).
    Delete {
        id: Uuid,
        /// Force-delete a non-terminal instance (server still enforces).
        #[arg(long)]
        force: bool,
    },
    /// Start an instance.
    Start { id: Uuid },
    /// Stop an instance.
    Stop { id: Uuid },
    /// Reboot an instance.
    Reboot { id: Uuid },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    // rustls 0.23 wants a process-default CryptoProvider before the
    // first ClientConfig::builder() call (see triton_cli_core::http).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    let format = OutputFormat::resolve(cli.output);

    match &cli.command {
        Command::Configure {
            endpoint,
            username,
            password_stdin,
        } => configure(endpoint.clone(), username.clone(), *password_stdin).await,
        Command::Login {
            endpoint,
            username,
            password_stdin,
        } => login_cmd(endpoint.clone(), username.clone(), *password_stdin).await,
        Command::Logout => {
            Config::remove(&APP)?;
            println!("Logged out (credentials removed).");
            Ok(())
        }
        Command::Whoami => whoami(&cli).await,
        Command::Env => env_cmd(&cli).await,
        Command::Instance { cmd } => instance(&cli, cmd, format).await,
    }
}

/// Resolve the session and build a tritond client on top of the shared
/// SmartOS-safe HTTPS client.
async fn connect(cli: &Cli) -> Result<tritond_client::Client> {
    let session = Session::resolve(&APP, cli.endpoint.clone(), cli.api_key.clone()).await?;
    let http = session.http_client()?;
    Ok(tritond_client::Client::new_with_client(
        &session.endpoint,
        http,
    ))
}

async fn instance(cli: &Cli, cmd: &InstanceCmd, format: OutputFormat) -> Result<()> {
    let client = connect(cli).await?;
    match cmd {
        InstanceCmd::List { image, state } => {
            // Tenant is inferred from the token; we only ever narrow by
            // project and the reference selectors. We never set `tenant`
            // or `silo`, and never call the `/v1/system/*` fleet surface.
            let mut req = client.list_instances_v1();
            if let Some(p) = cli.project {
                req = req.project(p);
            }
            if let Some(i) = image {
                req = req.image(*i);
            }
            if let Some(s) = state {
                req = req.state(s.clone());
            }
            let page = req.send().await.context("list instances")?.into_inner();
            if emit(format, &page)? {
                return Ok(());
            }
            let mut t = Table::new(&["ID", "NAME", "STATE", "IMAGE"], cli.no_headers);
            for inst in &page.items {
                t.row([
                    inst.id.to_string(),
                    inst.name.clone(),
                    wire(&inst.lifecycle),
                    inst.image_id.to_string(),
                ]);
            }
            t.print();
            Ok(())
        }
        InstanceCmd::Show { id } => {
            let inst = client
                .get_instance_v1()
                .instance_id(*id)
                .send()
                .await
                .context("get instance")?
                .into_inner();
            if emit(format, &inst)? {
                return Ok(());
            }
            print_instance(&inst);
            Ok(())
        }
        InstanceCmd::Create {
            name,
            description,
            image_id,
            primary_subnet_id,
            ssh_key_ids,
            cpu,
            memory_bytes,
            disk_bytes,
        } => {
            let project = cli
                .project
                .context("--project is required to create an instance")?;
            let inst = client
                .create_instance_v1()
                .project(project)
                .body(tritond_client::types::NewInstance {
                    name: name.clone(),
                    description: description.clone(),
                    image_id: *image_id,
                    primary_subnet_id: *primary_subnet_id,
                    ssh_key_ids: ssh_key_ids.clone(),
                    cpu: *cpu,
                    memory_bytes: *memory_bytes,
                    disk_bytes: *disk_bytes,
                    extra_nics: Vec::new(),
                    mac: None,
                })
                .send()
                .await
                .context("create instance")?
                .into_inner();
            if emit(format, &inst)? {
                return Ok(());
            }
            print_instance(&inst);
            Ok(())
        }
        InstanceCmd::Delete { id, force } => {
            client
                .delete_instance_v1()
                .instance_id(*id)
                .force(*force)
                .send()
                .await
                .context("delete instance")?;
            println!("Instance {id} deleted.");
            Ok(())
        }
        InstanceCmd::Start { id } => lifecycle(&client, *id, format, Lifecycle::Start).await,
        InstanceCmd::Stop { id } => lifecycle(&client, *id, format, Lifecycle::Stop).await,
        InstanceCmd::Reboot { id } => lifecycle(&client, *id, format, Lifecycle::Reboot).await,
    }
}

enum Lifecycle {
    Start,
    Stop,
    Reboot,
}

async fn lifecycle(
    client: &tritond_client::Client,
    id: Uuid,
    format: OutputFormat,
    action: Lifecycle,
) -> Result<()> {
    // CLI `reboot` maps to the server's `restart` verb.
    let inst = match action {
        Lifecycle::Start => client
            .start_instance_v1()
            .instance_id(id)
            .send()
            .await
            .context("start instance")?
            .into_inner(),
        Lifecycle::Stop => client
            .stop_instance_v1()
            .instance_id(id)
            .send()
            .await
            .context("stop instance")?
            .into_inner(),
        Lifecycle::Reboot => client
            .restart_instance_v1()
            .instance_id(id)
            .send()
            .await
            .context("reboot instance")?
            .into_inner(),
    };
    if emit(format, &inst)? {
        return Ok(());
    }
    print_instance(&inst);
    Ok(())
}

fn print_instance(inst: &tritond_client::types::Instance) {
    println!("id:        {}", inst.id);
    println!("name:      {}", inst.name);
    println!("lifecycle: {}", wire(&inst.lifecycle));
    println!("image:     {}", inst.image_id);
    println!("(use -o json for the full record)");
}

// ── auth ────────────────────────────────────────────────────────────

async fn configure(
    endpoint: Option<String>,
    username: Option<String>,
    password_stdin: bool,
) -> Result<()> {
    let endpoint = match endpoint.or_else(|| std::env::var(APP.env("ENDPOINT")).ok()) {
        Some(e) => e,
        None => dialoguer::Input::new()
            .with_prompt("Endpoint")
            .default("http://localhost:8080".to_string())
            .interact_text()
            .context("read endpoint")?,
    };
    let username = match username {
        Some(u) => u,
        None => dialoguer::Input::new()
            .with_prompt("Username")
            .interact_text()
            .context("read username")?,
    };
    let password = read_password(password_stdin)?;

    let tokens = login(&endpoint, &username, &password).await?;
    Config {
        endpoint: endpoint.clone(),
        tokens: Some(tokens),
    }
    .save(&APP)?;
    println!("Configured. Logged in as {username} at {endpoint}.");
    println!("Config written to {}", Config::path(&APP)?.display());
    Ok(())
}

async fn login_cmd(
    endpoint: Option<String>,
    username: Option<String>,
    password_stdin: bool,
) -> Result<()> {
    let stored = Config::load(&APP)?;
    let endpoint = endpoint
        .or_else(|| std::env::var(APP.env("ENDPOINT")).ok())
        .or_else(|| stored.as_ref().map(|c| c.endpoint.clone()))
        .context("no endpoint known: pass --endpoint or run `tritonctl configure` first")?;
    let username = match username {
        Some(u) => u,
        None => dialoguer::Input::new()
            .with_prompt("Username")
            .interact_text()
            .context("read username")?,
    };
    let password = read_password(password_stdin)?;

    let tokens = login(&endpoint, &username, &password).await?;
    Config {
        endpoint: endpoint.clone(),
        tokens: Some(tokens),
    }
    .save(&APP)?;
    println!("Logged in as {username} at {endpoint}.");
    Ok(())
}

async fn whoami(cli: &Cli) -> Result<()> {
    let session = Session::resolve(&APP, cli.endpoint.clone(), cli.api_key.clone()).await?;
    println!("endpoint: {}", session.endpoint);
    match session.bearer.as_deref() {
        None => println!("auth:     not authenticated (run `tritonctl login`)"),
        Some(bearer) => match jwt_claims(bearer) {
            Some(claims) => {
                println!("auth:     identityd token");
                for key in ["sub", "tenant_id", "realm_scope", "scope", "exp"] {
                    if let Some(v) = claims.get(key) {
                        println!("  {key}: {}", render_claim(v));
                    }
                }
            }
            None => println!("auth:     API key"),
        },
    }
    Ok(())
}

async fn env_cmd(cli: &Cli) -> Result<()> {
    let session = Session::resolve(&APP, cli.endpoint.clone(), cli.api_key.clone()).await?;
    println!("export TRITONCTL_ENDPOINT={:?}", session.endpoint);
    if let Some(bearer) = session.bearer {
        println!("export TRITONCTL_ACCESS_TOKEN={bearer:?}");
    }
    println!("# eval \"$(tritonctl env)\" to load these into the current shell");
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────

fn read_password(from_stdin: bool) -> Result<String> {
    if from_stdin {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read password from stdin")?;
        Ok(buf.trim_end_matches(['\n', '\r']).to_string())
    } else {
        rpassword::prompt_password("Password: ").context("read password")
    }
}

/// Best-effort decode of a JWT payload (no signature check; display
/// only). Returns `None` for non-JWT bearers such as API keys.
fn jwt_claims(bearer: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let parts: Vec<&str> = bearer.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    serde_json::from_slice(&payload).ok()
}

fn render_claim(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Render a serde enum as its wire string (e.g. `Running` -> `running`),
/// avoiding Debug format in user-facing output.
fn wire<T: serde::Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}
