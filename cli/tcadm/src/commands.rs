// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Subcommand implementations for `tcadm`.

use anyhow::{Context, Result};
use tritond_client::Client;
use tritond_client::types::{ApiKeyScope, LoginRequest, NewApiKey, TokenResponse};

/// Wire-format label for an API-key scope. Matches the JSON
/// serialisation (`full`, `read_only`, `audit_only`) so operators
/// see the same string they'd type into a JSON request body.
fn scope_label(scope: &ApiKeyScope) -> &'static str {
    match scope {
        ApiKeyScope::Full => "full",
        ApiKeyScope::ReadOnly => "read_only",
        ApiKeyScope::AuditOnly => "audit_only",
        ApiKeyScope::Agent => "agent",
    }
}
use uuid::Uuid;

use crate::config::{Config, Tokens};
use crate::session::Session;

/// Hit `/v2/health` to confirm the control plane is reachable.
/// Anonymous-allowed; this is the same Phase 0 contract as before.
pub async fn bootstrap(endpoint: &str, json_output: bool) -> Result<()> {
    let client = Client::new(endpoint);
    let response = client
        .health()
        .send()
        .await
        .with_context(|| format!("failed to reach tritond at {endpoint}"))?;
    let body = response.into_inner();

    if json_output {
        let payload = serde_json::json!({
            "endpoint": endpoint,
            "status": body.status,
            "version": body.version,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("tritond at {endpoint}");
        println!("  status:  {}", body.status);
        println!("  version: {}", body.version);
    }

    if body.status != "ok" {
        anyhow::bail!("tritond reported non-ok status: {}", body.status);
    }
    Ok(())
}

/// Interactive: prompt for endpoint + username + password, exchange
/// for tokens, persist to `~/.config/tcadm/config.json`.
pub async fn configure(
    endpoint: Option<String>,
    username: Option<String>,
    password_stdin: bool,
) -> Result<()> {
    let endpoint = match endpoint {
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
            .default("root".to_string())
            .interact_text()
            .context("read username")?,
    };
    let password = read_password(password_stdin)?;

    let tokens = exchange_password(&endpoint, &username, &password).await?;
    let config = Config {
        endpoint: endpoint.clone(),
        tokens: Some(tokens),
    };
    config.save().context("save config")?;

    println!("Configured. Logged in as {username} at {endpoint}.");
    println!("Config written to {}", Config::path()?.display());
    Ok(())
}

/// Re-authenticate against a previously-configured endpoint. Useful
/// after the refresh token has expired.
pub async fn login(
    endpoint: Option<String>,
    username: Option<String>,
    password_stdin: bool,
) -> Result<()> {
    let stored = Config::load().context("load config")?;
    let endpoint = endpoint
        .or_else(|| stored.as_ref().map(|c| c.endpoint.clone()))
        .context("no endpoint known: pass --endpoint or run `tcadm configure` first")?;
    let username = match username {
        Some(u) => u,
        None => dialoguer::Input::new()
            .with_prompt("Username")
            .default("root".to_string())
            .interact_text()
            .context("read username")?,
    };
    let password = read_password(password_stdin)?;

    let tokens = exchange_password(&endpoint, &username, &password).await?;
    let config = Config {
        endpoint: endpoint.clone(),
        tokens: Some(tokens),
    };
    config.save().context("save config")?;
    println!("Logged in as {username} at {endpoint}.");
    Ok(())
}

/// Delete the on-disk config.
pub fn logout() -> Result<()> {
    Config::delete()?;
    println!("Logged out (config removed).");
    Ok(())
}

/// Emit shell exports for the current session so the operator can
/// embed the access token in scripts that don't share a config file
/// (CI runners, sudo escalation).
pub async fn env(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    println!("export TCADM_ENDPOINT={:?}", session.endpoint);
    if let Some(bearer) = session.bearer {
        // We can't tell whether the bearer is a JWT or an API key
        // without inspecting it; emit both env-var names so consumers
        // pick the one they want.
        println!("export TCADM_ACCESS_TOKEN={bearer:?}");
    }
    println!("# eval \"$(tcadm env)\" to load these into the current shell");
    Ok(())
}

/// Mint an API key for the calling user.
pub async fn api_key_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    description: String,
    scope: ApiKeyScope,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let response = client
        .create_api_key()
        .body(NewApiKey { description, scope })
        .send()
        .await
        .context("create api key")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("API key created.");
        println!("  id:          {}", response.id);
        println!("  description: {}", response.description);
        println!("  scope:       {}", scope_label(&response.scope));
        println!("  created:     {}", response.created_at);
        println!();
        println!("  secret: {}", response.secret);
        println!();
        println!("Save this secret now. It will not be shown again.");
    }
    Ok(())
}

/// List the calling user's API keys.
pub async fn api_key_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let keys = client
        .list_api_keys()
        .send()
        .await
        .context("list api keys")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&keys)?);
        return Ok(());
    }
    if keys.is_empty() {
        println!("(no api keys)");
        return Ok(());
    }
    for key in keys {
        println!(
            "{}  {}  scope={}  {}",
            key.id,
            key.created_at,
            scope_label(&key.scope),
            key.description,
        );
    }
    Ok(())
}

/// Delete one of the calling user's API keys.
pub async fn api_key_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    api_key_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_api_key()
        .api_key_id(api_key_id)
        .send()
        .await
        .context("delete api key")?;
    println!("Deleted api key {api_key_id}.");
    Ok(())
}

/// Page through audit events.
pub async fn audit_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    after_seq: Option<u64>,
    limit: Option<u32>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_audit_events();
    if let Some(s) = after_seq {
        req = req.after_seq(s);
    }
    if let Some(l) = limit {
        req = req.limit(l);
    }
    let response = req.send().await.context("list audit events")?.into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    if response.events.is_empty() {
        println!("(no events)");
    } else {
        for ev in &response.events {
            println!(
                "{:>6}  {}  {:?}  {}  {:?}",
                ev.seq, ev.ts, ev.actor, ev.action, ev.decision
            );
        }
    }
    if let Some(head) = response.head {
        println!();
        println!("head: seq={} hash={}", head.seq, head.hash);
    }
    Ok(())
}

/// Fetch a single audit event by sequence.
///
/// `json_output` is accepted for symmetry with the other audit
/// subcommands; the human form is just pretty-printed JSON because an
/// AuditEvent has no shorter useful textual representation.
pub async fn audit_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    seq: u64,
    _json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let event = client
        .get_audit_event()
        .seq(seq)
        .send()
        .await
        .context("get audit event")?
        .into_inner();
    println!("{}", serde_json::to_string_pretty(&event)?);
    Ok(())
}

/// Configure the silo's OIDC IdP. Eager discovery happens server-side;
/// a bad URL or unreachable IdP fails the call with a 4xx.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_idp_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    issuer_url: String,
    client_id: String,
    client_secret_stdin: bool,
    audience: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;

    let secret = if client_secret_stdin {
        let mut s = String::new();
        std::io::stdin()
            .read_line(&mut s)
            .context("read client secret from stdin")?;
        s.trim_end_matches(['\n', '\r']).to_string()
    } else {
        rpassword::prompt_password("OIDC client secret: ")
            .context("read client secret from terminal")?
    };

    let response = client
        .put_silo_idp()
        .silo_id(silo_id)
        .body(tritond_client::types::NewIdpConfig {
            issuer_url,
            client_id,
            client_secret: secret,
            audience,
        })
        .send()
        .await
        .context("set idp config")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("IdP configured for silo {silo_id}");
        println!("  issuer_url: {}", response.issuer_url);
        println!("  client_id:  {}", response.client_id);
        if let Some(aud) = response.audience {
            println!("  audience:   {aud}");
        }
    }
    Ok(())
}

/// Read the silo's IdP config (with the client secret never returned).
pub async fn silo_idp_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let response = client
        .get_silo_idp()
        .silo_id(silo_id)
        .send()
        .await
        .context("get idp config")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("IdP for silo {silo_id}");
        println!("  issuer_url: {}", response.issuer_url);
        println!("  client_id:  {}", response.client_id);
        if let Some(aud) = response.audience {
            println!("  audience:   {aud}");
        }
    }
    Ok(())
}

/// Remove the silo's IdP config.
pub async fn silo_idp_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_silo_idp()
        .silo_id(silo_id)
        .send()
        .await
        .context("delete idp config")?;
    println!("Removed IdP config for silo {silo_id}");
    Ok(())
}

/// List the projects in a silo.
pub async fn silo_project_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let projects = client
        .list_silo_projects()
        .silo_id(silo_id)
        .send()
        .await
        .context("list projects")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&projects)?);
        return Ok(());
    }
    if projects.is_empty() {
        println!("(no projects)");
        return Ok(());
    }
    for p in projects {
        println!("{}  {}  {}", p.id, p.created_at, p.name);
    }
    Ok(())
}

/// Create a new project in a silo.
pub async fn silo_project_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    name: String,
    description: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let project = client
        .create_silo_project()
        .silo_id(silo_id)
        .body(tritond_client::types::NewProject {
            name,
            description: Some(description),
        })
        .send()
        .await
        .context("create project")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&project)?);
    } else {
        println!("Created project {} in silo {silo_id}", project.id);
        println!("  name:        {}", project.name);
        println!("  description: {}", project.description);
        println!("  created:     {}", project.created_at);
    }
    Ok(())
}

/// Read a single project.
pub async fn silo_project_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let project = client
        .get_silo_project()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("get project")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&project)?);
    } else {
        println!("Project {} in silo {silo_id}", project.id);
        println!("  name:        {}", project.name);
        println!("  description: {}", project.description);
        println!("  created:     {}", project.created_at);
    }
    Ok(())
}

fn print_instance(i: &tritond_client::types::Instance) {
    println!("Instance {} in project {}", i.id, i.project_id);
    println!("  name:        {}", i.name);
    println!("  description: {}", i.description);
    println!("  lifecycle:   {:?}", i.lifecycle);
    println!("  image:       {}", i.image_id);
    println!("  subnet:      {}", i.primary_subnet_id);
    println!("  cpu:         {}", i.cpu);
    println!("  memory:      {}", i.memory_bytes);
    if !i.ssh_key_ids.is_empty() {
        println!("  ssh-keys:    {} key(s)", i.ssh_key_ids.len());
    }
    println!("  created:     {}", i.created_at);
    println!("  updated:     {}", i.updated_at);
}

/// List instances in a project.
pub async fn silo_project_instance_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instances = client
        .list_project_instances()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("list instances")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&instances)?);
        return Ok(());
    }
    if instances.is_empty() {
        println!("(no instances)");
        return Ok(());
    }
    for i in instances {
        println!(
            "{}  {:?}  cpu={} mem={}MB  {}",
            i.id,
            i.lifecycle,
            i.cpu,
            i.memory_bytes / 1_048_576,
            i.name
        );
    }
    Ok(())
}

/// Create an instance.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_instance_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    name: String,
    description: String,
    image_id: Uuid,
    primary_subnet_id: Uuid,
    ssh_key_ids: Vec<Uuid>,
    cpu: u32,
    memory_bytes: u64,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instance = client
        .create_project_instance()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(tritond_client::types::NewInstance {
            name,
            description: Some(description),
            image_id,
            primary_subnet_id,
            ssh_key_ids,
            cpu,
            memory_bytes,
            // tcadm doesn't yet surface multi-NIC at create
            // time on the CLI; operators that want extra NICs
            // build the JSON body directly via curl. Future:
            // a `--extra-nic SUBNET_ID:NAME` repeated flag.
            extra_nics: Vec::new(),
        })
        .send()
        .await
        .context("create instance")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&instance)?);
    } else {
        println!("Created instance {} in project {project_id}", instance.id);
        print_instance(&instance);
    }
    Ok(())
}

/// Read a single instance.
pub async fn silo_project_instance_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instance = client
        .get_project_instance()
        .silo_id(silo_id)
        .project_id(project_id)
        .instance_id(instance_id)
        .send()
        .await
        .context("get instance")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&instance)?);
    } else {
        print_instance(&instance);
    }
    Ok(())
}

/// Delete an instance.
pub async fn silo_project_instance_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_instance()
        .silo_id(silo_id)
        .project_id(project_id)
        .instance_id(instance_id)
        .send()
        .await
        .context("delete instance")?;
    println!("Deleted instance {instance_id} from project {project_id}");
    Ok(())
}

fn print_floating_ip(f: &tritond_client::types::FloatingIp) {
    println!("FloatingIp {} in project {}", f.id, f.project_id);
    println!("  name:        {}", f.name);
    println!("  description: {}", f.description);
    println!("  address:     {}", f.address);
    match &f.attached_to {
        Some(a) => {
            println!(
                "  attached_to: nic={} instance={} (since {})",
                a.nic_id, a.instance_id, a.attached_at
            );
        }
        None => {
            println!("  attached_to: (unattached)");
        }
    }
    println!("  created:     {}", f.created_at);
    println!("  updated:     {}", f.updated_at);
}

/// List FloatingIps in a project.
pub async fn silo_project_floating_ip_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fips = client
        .list_project_floating_ips()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("list floating ips")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&fips)?);
        return Ok(());
    }
    if fips.is_empty() {
        println!("(no floating ips)");
        return Ok(());
    }
    for f in fips {
        let attached = match &f.attached_to {
            Some(a) => format!("nic={}", a.nic_id),
            None => "(unattached)".to_string(),
        };
        println!("{}  {}  {attached}  {}", f.id, f.address, f.name);
    }
    Ok(())
}

/// Allocate a new FloatingIp.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_floating_ip_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    name: String,
    description: String,
    family: String,
    json_output: bool,
) -> Result<()> {
    let family = match family.to_ascii_lowercase().as_str() {
        "v4" | "ipv4" | "4" => tritond_client::types::AddressFamily::V4,
        "v6" | "ipv6" | "6" => tritond_client::types::AddressFamily::V6,
        other => anyhow::bail!("--family must be `v4` or `v6`, got {other:?}"),
    };
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .create_project_floating_ip()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(tritond_client::types::NewFloatingIp {
            name,
            description: Some(description),
            family,
        })
        .send()
        .await
        .context("create floating ip")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&fip)?);
    } else {
        println!("Allocated floating ip {} from pool", fip.id);
        print_floating_ip(&fip);
    }
    Ok(())
}

/// Read a single FloatingIp.
pub async fn silo_project_floating_ip_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .get_project_floating_ip()
        .silo_id(silo_id)
        .project_id(project_id)
        .floating_ip_id(floating_ip_id)
        .send()
        .await
        .context("get floating ip")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&fip)?);
    } else {
        print_floating_ip(&fip);
    }
    Ok(())
}

/// Release a FloatingIp.
pub async fn silo_project_floating_ip_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_floating_ip()
        .silo_id(silo_id)
        .project_id(project_id)
        .floating_ip_id(floating_ip_id)
        .send()
        .await
        .context("delete floating ip")?;
    println!("Released floating ip {floating_ip_id} back to pool");
    Ok(())
}

/// Attach a FloatingIp to a NIC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_floating_ip_attach(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
    nic_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .attach_project_floating_ip()
        .silo_id(silo_id)
        .project_id(project_id)
        .floating_ip_id(floating_ip_id)
        .body(tritond_client::types::AttachFloatingIpRequest { nic_id })
        .send()
        .await
        .context("attach floating ip")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&fip)?);
    } else {
        print_floating_ip(&fip);
    }
    Ok(())
}

/// Detach a FloatingIp.
pub async fn silo_project_floating_ip_detach(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .detach_project_floating_ip()
        .silo_id(silo_id)
        .project_id(project_id)
        .floating_ip_id(floating_ip_id)
        .send()
        .await
        .context("detach floating ip")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&fip)?);
    } else {
        print_floating_ip(&fip);
    }
    Ok(())
}

/// List the disks attached to an instance.
pub async fn silo_project_instance_disk_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let disks = client
        .list_instance_disks()
        .silo_id(silo_id)
        .project_id(project_id)
        .instance_id(instance_id)
        .send()
        .await
        .context("list disks")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&disks)?);
        return Ok(());
    }
    if disks.is_empty() {
        println!("(no disks)");
        return Ok(());
    }
    for d in disks {
        println!(
            "{}  {:?}  {}MB  {}",
            d.id,
            d.kind,
            d.size_bytes / 1_048_576,
            d.name
        );
    }
    Ok(())
}

/// Read a single disk.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_instance_disk_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    disk_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let disk = client
        .get_instance_disk()
        .silo_id(silo_id)
        .project_id(project_id)
        .instance_id(instance_id)
        .disk_id(disk_id)
        .send()
        .await
        .context("get disk")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&disk)?);
    } else {
        println!("Disk {} on instance {}", disk.id, disk.instance_id);
        println!("  name:        {}", disk.name);
        println!("  description: {}", disk.description);
        println!("  kind:        {:?}", disk.kind);
        println!("  size_bytes:  {}", disk.size_bytes);
        println!(
            "  source_image:{}",
            disk.source_image_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "(none)".to_string())
        );
        println!("  created:     {}", disk.created_at);
    }
    Ok(())
}

/// List the NICs attached to an instance.
pub async fn silo_project_instance_nic_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nics = client
        .list_instance_nics()
        .silo_id(silo_id)
        .project_id(project_id)
        .instance_id(instance_id)
        .send()
        .await
        .context("list nics")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&nics)?);
        return Ok(());
    }
    if nics.is_empty() {
        println!("(no nics)");
        return Ok(());
    }
    for n in nics {
        let v4 = n
            .primary_ipv4
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "-".to_string());
        let v6 = n
            .primary_ipv6
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!("{}  {}  v4={v4}  v6={v6}  {}", n.id, n.mac, n.name);
    }
    Ok(())
}

/// Read a single NIC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_instance_nic_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    nic_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nic = client
        .get_instance_nic()
        .silo_id(silo_id)
        .project_id(project_id)
        .instance_id(instance_id)
        .nic_id(nic_id)
        .send()
        .await
        .context("get nic")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&nic)?);
    } else {
        println!("Nic {} on instance {}", nic.id, nic.instance_id);
        println!("  name:         {}", nic.name);
        println!("  mac:          {}", nic.mac);
        println!(
            "  primary_ipv4: {}",
            nic.primary_ipv4
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "(none)".to_string())
        );
        println!(
            "  primary_ipv6: {}",
            nic.primary_ipv6
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "(none)".to_string())
        );
        println!("  vpc:          {}", nic.vpc_id);
        println!("  subnet:       {}", nic.subnet_id);
        println!("  created:      {}", nic.created_at);
    }
    Ok(())
}

/// Drive a lifecycle transition (start/stop/restart). Single function
/// for all three so the dispatch table in main.rs stays terse.
pub async fn silo_project_instance_lifecycle(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    transition: &str,
    silo_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instance = match transition {
        "start" => client
            .start_project_instance()
            .silo_id(silo_id)
            .project_id(project_id)
            .instance_id(instance_id)
            .send()
            .await
            .context("start instance")?
            .into_inner(),
        "stop" => client
            .stop_project_instance()
            .silo_id(silo_id)
            .project_id(project_id)
            .instance_id(instance_id)
            .send()
            .await
            .context("stop instance")?
            .into_inner(),
        "restart" => client
            .restart_project_instance()
            .silo_id(silo_id)
            .project_id(project_id)
            .instance_id(instance_id)
            .send()
            .await
            .context("restart instance")?
            .into_inner(),
        other => anyhow::bail!("unknown lifecycle transition: {other}"),
    };
    if json_output {
        println!("{}", serde_json::to_string_pretty(&instance)?);
    } else {
        println!("Instance {} now {:?}", instance.id, instance.lifecycle);
    }
    Ok(())
}

/// Set (or replace) a project's quota.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_quota_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    cpu_limit: u32,
    memory_bytes: u64,
    disk_bytes: u64,
    instance_limit: u32,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let quota = client
        .put_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(tritond_client::types::NewQuota {
            cpu_limit,
            memory_bytes,
            disk_bytes,
            instance_limit,
        })
        .send()
        .await
        .context("set project quota")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&quota)?);
    } else {
        println!("Set quota on project {project_id}");
        println!("  cpu_limit:      {}", quota.cpu_limit);
        println!("  memory_bytes:   {}", quota.memory_bytes);
        println!("  disk_bytes:     {}", quota.disk_bytes);
        println!("  instance_limit: {}", quota.instance_limit);
        println!("  updated:        {}", quota.updated_at);
    }
    Ok(())
}

/// Read a project's quota.
pub async fn silo_project_quota_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let quota = client
        .get_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("get project quota")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&quota)?);
    } else {
        println!("Quota on project {project_id}");
        println!("  cpu_limit:      {}", quota.cpu_limit);
        println!("  memory_bytes:   {}", quota.memory_bytes);
        println!("  disk_bytes:     {}", quota.disk_bytes);
        println!("  instance_limit: {}", quota.instance_limit);
        println!("  updated:        {}", quota.updated_at);
    }
    Ok(())
}

/// Remove a project's quota.
pub async fn silo_project_quota_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_quota()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("delete project quota")?;
    println!("Removed quota from project {project_id} (now unlimited)");
    Ok(())
}

/// Delete a project.
pub async fn silo_project_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_silo_project()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("delete project")?;
    println!("Deleted project {project_id} from silo {silo_id}");
    Ok(())
}

fn fmt_opt_cidr(opt: Option<&String>) -> &str {
    opt.map(|s| s.as_str()).unwrap_or("(none)")
}

/// List the VPCs in a project.
pub async fn silo_project_vpc_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let vpcs = client
        .list_project_vpcs()
        .silo_id(silo_id)
        .project_id(project_id)
        .send()
        .await
        .context("list vpcs")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&vpcs)?);
        return Ok(());
    }
    if vpcs.is_empty() {
        println!("(no vpcs)");
        return Ok(());
    }
    for v in vpcs {
        println!(
            "{}  vni={:>8}  v4={}  v6={}  {}",
            v.id,
            v.vni,
            fmt_opt_cidr(v.ipv4_block.as_ref()),
            fmt_opt_cidr(v.ipv6_block.as_ref()),
            v.name
        );
    }
    Ok(())
}

/// Create a new VPC in a project.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_vpc_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    name: String,
    description: String,
    ipv4_block: Option<String>,
    ipv6_block: Option<String>,
    json_output: bool,
) -> Result<()> {
    if ipv4_block.is_none() && ipv6_block.is_none() {
        anyhow::bail!("at least one of --ipv4-block or --ipv6-block is required");
    }
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let vpc = client
        .create_project_vpc()
        .silo_id(silo_id)
        .project_id(project_id)
        .body(tritond_client::types::NewVpc {
            name,
            description: Some(description),
            ipv4_block,
            ipv6_block,
        })
        .send()
        .await
        .context("create vpc")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&vpc)?);
    } else {
        println!("Created vpc {} in project {project_id}", vpc.id);
        println!("  name:        {}", vpc.name);
        println!("  description: {}", vpc.description);
        println!("  vni:         {}", vpc.vni);
        println!("  ipv4_block:  {}", fmt_opt_cidr(vpc.ipv4_block.as_ref()));
        println!("  ipv6_block:  {}", fmt_opt_cidr(vpc.ipv6_block.as_ref()));
        println!("  created:     {}", vpc.created_at);
    }
    Ok(())
}

/// Read a single VPC.
pub async fn silo_project_vpc_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let vpc = client
        .get_project_vpc()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .context("get vpc")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&vpc)?);
    } else {
        println!("Vpc {} in project {project_id}", vpc.id);
        println!("  name:        {}", vpc.name);
        println!("  description: {}", vpc.description);
        println!("  vni:         {}", vpc.vni);
        println!("  ipv4_block:  {}", fmt_opt_cidr(vpc.ipv4_block.as_ref()));
        println!("  ipv6_block:  {}", fmt_opt_cidr(vpc.ipv6_block.as_ref()));
        println!("  created:     {}", vpc.created_at);
    }
    Ok(())
}

/// Delete a VPC.
pub async fn silo_project_vpc_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_vpc()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .context("delete vpc")?;
    println!("Deleted vpc {vpc_id} from project {project_id}");
    Ok(())
}

/// List the subnets in a VPC.
pub async fn silo_project_vpc_subnet_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let subnets = client
        .list_vpc_subnets()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .context("list subnets")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&subnets)?);
        return Ok(());
    }
    if subnets.is_empty() {
        println!("(no subnets)");
        return Ok(());
    }
    for s in subnets {
        println!(
            "{}  v4={}  v6={}  {}",
            s.id,
            fmt_opt_cidr(s.ipv4_block.as_ref()),
            fmt_opt_cidr(s.ipv6_block.as_ref()),
            s.name
        );
    }
    Ok(())
}

/// Create a new subnet in a VPC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_vpc_subnet_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    name: String,
    description: String,
    ipv4_block: Option<String>,
    ipv6_block: Option<String>,
    json_output: bool,
) -> Result<()> {
    if ipv4_block.is_none() && ipv6_block.is_none() {
        anyhow::bail!("at least one of --ipv4-block or --ipv6-block is required");
    }
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let subnet = client
        .create_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(tritond_client::types::NewSubnet {
            name,
            description: Some(description),
            ipv4_block,
            ipv6_block,
        })
        .send()
        .await
        .context("create subnet")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&subnet)?);
    } else {
        println!("Created subnet {} in vpc {vpc_id}", subnet.id);
        println!("  name:        {}", subnet.name);
        println!("  description: {}", subnet.description);
        println!(
            "  ipv4_block:  {}",
            fmt_opt_cidr(subnet.ipv4_block.as_ref())
        );
        println!(
            "  ipv6_block:  {}",
            fmt_opt_cidr(subnet.ipv6_block.as_ref())
        );
        println!("  created:     {}", subnet.created_at);
    }
    Ok(())
}

/// Read a single subnet.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_project_vpc_subnet_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    subnet_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let subnet = client
        .get_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .subnet_id(subnet_id)
        .send()
        .await
        .context("get subnet")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&subnet)?);
    } else {
        println!("Subnet {} in vpc {vpc_id}", subnet.id);
        println!("  name:        {}", subnet.name);
        println!("  description: {}", subnet.description);
        println!(
            "  ipv4_block:  {}",
            fmt_opt_cidr(subnet.ipv4_block.as_ref())
        );
        println!(
            "  ipv6_block:  {}",
            fmt_opt_cidr(subnet.ipv6_block.as_ref())
        );
        println!("  created:     {}", subnet.created_at);
    }
    Ok(())
}

/// Delete a subnet.
pub async fn silo_project_vpc_subnet_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    subnet_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_vpc_subnet()
        .silo_id(silo_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .subnet_id(subnet_id)
        .send()
        .await
        .context("delete subnet")?;
    println!("Deleted subnet {subnet_id} from vpc {vpc_id}");
    Ok(())
}

/// List SSH keys in a silo's catalog.
pub async fn silo_ssh_key_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let keys = client
        .list_silo_ssh_keys()
        .silo_id(silo_id)
        .send()
        .await
        .context("list ssh keys")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&keys)?);
        return Ok(());
    }
    if keys.is_empty() {
        println!("(no ssh keys)");
        return Ok(());
    }
    for k in keys {
        println!("{}  {}  {}", k.id, k.fingerprint, k.name);
    }
    Ok(())
}

/// Register a new SSH key in a silo's catalog.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_ssh_key_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    name: String,
    description: String,
    public_key: Option<String>,
    public_key_file: Option<String>,
    json_output: bool,
) -> Result<()> {
    let public_key = match (public_key, public_key_file) {
        (Some(s), None) => s,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("read public key from {path}"))?,
        (None, None) => {
            anyhow::bail!("--public-key or --public-key-file is required")
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with should prevent this"),
    };
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .create_silo_ssh_key()
        .silo_id(silo_id)
        .body(tritond_client::types::NewSshKey {
            name,
            description: Some(description),
            public_key,
        })
        .send()
        .await
        .context("create ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!("Registered ssh key {} in silo {silo_id}", key.id);
        println!("  name:        {}", key.name);
        println!("  description: {}", key.description);
        println!("  fingerprint: {}", key.fingerprint);
        println!("  created:     {}", key.created_at);
    }
    Ok(())
}

/// Read a single SSH key.
pub async fn silo_ssh_key_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    ssh_key_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .get_silo_ssh_key()
        .silo_id(silo_id)
        .ssh_key_id(ssh_key_id)
        .send()
        .await
        .context("get ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!("SshKey {} in silo {silo_id}", key.id);
        println!("  name:        {}", key.name);
        println!("  description: {}", key.description);
        println!("  fingerprint: {}", key.fingerprint);
        println!("  public_key:  {}", key.public_key);
        println!("  created:     {}", key.created_at);
    }
    Ok(())
}

/// Delete an SSH key.
pub async fn silo_ssh_key_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    ssh_key_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_silo_ssh_key()
        .silo_id(silo_id)
        .ssh_key_id(ssh_key_id)
        .send()
        .await
        .context("delete ssh key")?;
    println!("Deleted ssh key {ssh_key_id} from silo {silo_id}");
    Ok(())
}

/// List images in a silo's catalog.
pub async fn silo_image_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let images = client
        .list_silo_images()
        .silo_id(silo_id)
        .send()
        .await
        .context("list images")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&images)?);
        return Ok(());
    }
    if images.is_empty() {
        println!("(no images)");
        return Ok(());
    }
    for i in images {
        println!(
            "{}  {}/{} {}MB  {}",
            i.id,
            i.os,
            i.version,
            i.size_bytes / 1_048_576,
            i.name
        );
    }
    Ok(())
}

/// Register a new image in a silo's catalog.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn silo_image_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    name: String,
    description: String,
    os: String,
    version: String,
    size_bytes: u64,
    sha256: String,
    source_url: Option<String>,
    id: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let image = client
        .create_silo_image()
        .silo_id(silo_id)
        .body(tritond_client::types::NewImage {
            name,
            description: Some(description),
            os,
            version,
            size_bytes,
            sha256,
            source_url,
            id,
        })
        .send()
        .await
        .context("create image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Registered image {} in silo {silo_id}", image.id);
        println!("  name:        {}", image.name);
        println!("  description: {}", image.description);
        println!("  os/version:  {}/{}", image.os, image.version);
        println!("  size_bytes:  {}", image.size_bytes);
        println!("  sha256:      {}", image.sha256);
        println!(
            "  source_url:  {}",
            image.source_url.as_deref().unwrap_or("(none)")
        );
        println!("  created:     {}", image.created_at);
    }
    Ok(())
}

/// Read a single image.
pub async fn silo_image_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    image_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let image = client
        .get_silo_image()
        .silo_id(silo_id)
        .image_id(image_id)
        .send()
        .await
        .context("get image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Image {} in silo {silo_id}", image.id);
        println!("  name:        {}", image.name);
        println!("  description: {}", image.description);
        println!("  os/version:  {}/{}", image.os, image.version);
        println!("  size_bytes:  {}", image.size_bytes);
        println!("  sha256:      {}", image.sha256);
        println!(
            "  source_url:  {}",
            image.source_url.as_deref().unwrap_or("(none)")
        );
        println!("  created:     {}", image.created_at);
    }
    Ok(())
}

/// Delete an image.
pub async fn silo_image_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    image_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_silo_image()
        .silo_id(silo_id)
        .image_id(image_id)
        .send()
        .await
        .context("delete image")?;
    println!("Deleted image {image_id} from silo {silo_id}");
    Ok(())
}

/// Walk the chain and recompute hashes.
pub async fn audit_verify(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    from: Option<u64>,
    to: Option<u64>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.verify_audit_chain();
    if let Some(f) = from {
        req = req.from(f);
    }
    if let Some(t) = to {
        req = req.to(t);
    }
    let response = req.send().await.context("verify audit chain")?.into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }

    use tritond_client::types::VerifyOutcome;
    match &response.outcome {
        VerifyOutcome::Ok { verified_to } => {
            println!("OK: verified up to seq {verified_to}");
        }
        VerifyOutcome::Mismatch { seq, message } => {
            println!("MISMATCH at seq {seq}: {message}");
        }
    }
    if let Some(head) = response.head {
        println!("head: seq={} hash={}", head.seq, head.hash);
    }
    Ok(())
}

fn read_password(from_stdin: bool) -> Result<String> {
    if from_stdin {
        let mut s = String::new();
        std::io::stdin()
            .read_line(&mut s)
            .context("read password from stdin")?;
        Ok(s.trim_end_matches(['\n', '\r']).to_string())
    } else {
        Ok(rpassword::prompt_password("Password: ").context("read password from terminal")?)
    }
}

async fn exchange_password(endpoint: &str, username: &str, password: &str) -> Result<Tokens> {
    let client = Client::new(endpoint);
    let response: TokenResponse = client
        .login()
        .body(LoginRequest {
            username: username.to_string(),
            password: password.to_string(),
        })
        .send()
        .await
        .with_context(|| format!("login against {endpoint}"))?
        .into_inner();
    Ok(Tokens {
        access_token: response.access_token,
        refresh_token: response.refresh_token,
        access_expires_at: response.access_expires_at,
        refresh_expires_at: response.refresh_expires_at,
    })
}
