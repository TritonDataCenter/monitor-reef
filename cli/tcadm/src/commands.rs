// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Subcommand implementations for `tcadm`.

use anyhow::{Context, Result};
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
use crate::session::{Session, anonymous_client, build_http_client};

/// Hit `/v2/health` to confirm the control plane is reachable.
/// Anonymous-allowed; this is the same Phase 0 contract as before.
pub async fn bootstrap(endpoint: &str, json_output: bool) -> Result<()> {
    let client = anonymous_client(endpoint)?;
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

/// Configure the tenant's OIDC IdP. Eager discovery happens
/// server-side; a bad URL or unreachable IdP fails the call with
/// a 4xx; a duplicate issuer (claimed by another tenant) is 409.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn tenant_idp_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
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
        .put_tenant_idp()
        .tenant_id(tenant_id)
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
        println!("IdP configured for tenant {tenant_id}");
        println!("  issuer_url: {}", response.issuer_url);
        println!("  client_id:  {}", response.client_id);
        if let Some(aud) = response.audience {
            println!("  audience:   {aud}");
        }
    }
    Ok(())
}

/// Read the tenant's IdP config (with the client secret never returned).
pub async fn tenant_idp_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let response = client
        .get_tenant_idp()
        .tenant_id(tenant_id)
        .send()
        .await
        .context("get idp config")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        println!("IdP for tenant {tenant_id}");
        println!("  issuer_url: {}", response.issuer_url);
        println!("  client_id:  {}", response.client_id);
        if let Some(aud) = response.audience {
            println!("  audience:   {aud}");
        }
    }
    Ok(())
}

/// Remove the tenant's IdP config.
pub async fn tenant_idp_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_tenant_idp()
        .tenant_id(tenant_id)
        .send()
        .await
        .context("delete idp config")?;
    println!("Removed IdP config for tenant {tenant_id}");
    Ok(())
}

/// List the tenants in a silo.
pub async fn tenant_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let tenants = client
        .list_silo_tenants()
        .silo_id(silo_id)
        .send()
        .await
        .context("list tenants")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&tenants)?);
        return Ok(());
    }
    if tenants.is_empty() {
        println!("(no tenants)");
        return Ok(());
    }
    for t in tenants {
        println!("{}  {}  {}  {}", t.id, t.name, t.description, t.created_at);
    }
    Ok(())
}

/// Show a single tenant.
pub async fn tenant_show(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let tenant = client
        .get_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .send()
        .await
        .context("get tenant")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&tenant)?);
    } else {
        println!("Tenant {} in silo {}", tenant.id, tenant.silo_id);
        println!("  name:        {}", tenant.name);
        println!("  description: {}", tenant.description);
        println!("  created:     {}", tenant.created_at);
    }
    Ok(())
}

/// Create a new tenant in a silo.
pub async fn tenant_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    name: String,
    description: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let tenant = client
        .create_silo_tenant()
        .silo_id(silo_id)
        .body(tritond_client::types::NewTenant { name, description })
        .send()
        .await
        .context("create tenant")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&tenant)?);
    } else {
        println!("Created tenant {} in silo {}", tenant.id, tenant.silo_id);
        println!("  name:        {}", tenant.name);
        println!("  description: {}", tenant.description);
        println!("  created:     {}", tenant.created_at);
    }
    Ok(())
}

/// Delete a tenant.
pub async fn tenant_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    tenant_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_silo_tenant()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .send()
        .await
        .context("delete tenant")?;
    println!("Deleted tenant {tenant_id}");
    Ok(())
}

/// List the projects in a silo.
pub async fn tenant_project_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let projects = client
        .list_tenant_projects()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    name: String,
    description: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let project = client
        .create_tenant_project()
        .tenant_id(tenant_id)
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
        println!("Created project {} in silo {tenant_id}", project.id);
        println!("  name:        {}", project.name);
        println!("  description: {}", project.description);
        println!("  created:     {}", project.created_at);
    }
    Ok(())
}

/// Read a single project.
pub async fn tenant_project_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let project = client
        .get_tenant_project()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .send()
        .await
        .context("get project")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&project)?);
    } else {
        println!("Project {} in silo {tenant_id}", project.id);
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
pub async fn tenant_project_instance_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instances = client
        .list_project_instances()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
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
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instance = client
        .get_project_instance()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_instance()
        .tenant_id(tenant_id)
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

fn parse_address_family(family: &str) -> Result<tritond_client::types::AddressFamily> {
    match family.to_ascii_lowercase().as_str() {
        "v4" | "ipv4" | "4" => Ok(tritond_client::types::AddressFamily::V4),
        "v6" | "ipv6" | "6" => Ok(tritond_client::types::AddressFamily::V6),
        other => anyhow::bail!("--family must be `v4` or `v6`, got {other:?}"),
    }
}

fn parse_route_target(target: &str) -> Result<tritond_client::types::RouteTarget> {
    let target = target.trim();
    match target.to_ascii_lowercase().as_str() {
        "blackhole" => Ok(tritond_client::types::RouteTarget::Blackhole),
        "reject" => Ok(tritond_client::types::RouteTarget::Reject),
        "virtual-gateway" | "virtual_gateway" | "vgw" => {
            Ok(tritond_client::types::RouteTarget::VirtualGateway)
        }
        _ => {
            if let Some(id) = target
                .strip_prefix("nat-gateway:")
                .or_else(|| target.strip_prefix("nat-gw:"))
            {
                return Ok(tritond_client::types::RouteTarget::NatGateway {
                    nat_gateway_id: id.parse().context("parse nat gateway target uuid")?,
                });
            }
            if let Some(id) = target.strip_prefix("floating-ip:") {
                return Ok(tritond_client::types::RouteTarget::FloatingIp {
                    floating_ip_id: id.parse().context("parse floating ip target uuid")?,
                });
            }
            anyhow::bail!(
                "--target must be blackhole, reject, virtual-gateway, nat-gateway:<uuid>, or floating-ip:<uuid>"
            )
        }
    }
}

/// List FloatingIps in a project.
pub async fn tenant_project_floating_ip_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fips = client
        .list_project_floating_ips()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_floating_ip_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    name: String,
    description: String,
    family: String,
    json_output: bool,
) -> Result<()> {
    let family = parse_address_family(&family)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .create_project_floating_ip()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_floating_ip_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .get_project_floating_ip()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_floating_ip_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_floating_ip()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_floating_ip_attach(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
    nic_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .attach_project_floating_ip()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_floating_ip_detach(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    floating_ip_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let fip = client
        .detach_project_floating_ip()
        .tenant_id(tenant_id)
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

fn print_nat_gateway(n: &tritond_client::types::NatGateway) {
    println!("NatGateway {} in vpc {}", n.id, n.vpc_id);
    println!("  name:               {}", n.name);
    println!("  description:        {}", n.description);
    println!("  public_address:     {}", n.public_address);
    println!("  family:             {:?}", n.family);
    println!("  desired_generation: {}", n.desired_generation);
    match n.realized.applied_generation {
        Some(generation) => println!("  applied_generation: {}", generation),
        None => println!("  applied_generation: (none)"),
    }
    match n.edge_cluster_id {
        Some(edge_cluster_id) => println!("  edge_cluster_id:    {}", edge_cluster_id),
        None => println!("  edge_cluster_id:    (unplaced)"),
    }
    println!("  created:            {}", n.created_at);
    println!("  updated:            {}", n.updated_at);
}

fn print_route_table(rt: &tritond_client::types::RouteTable) {
    println!("RouteTable {} in vpc {}", rt.id, rt.vpc_id);
    println!("  name:        {}", rt.name);
    println!("  description: {}", rt.description);
    println!("  is_main:     {}", rt.is_main);
    println!("  created:     {}", rt.created_at);
}

fn route_target_label(target: &tritond_client::types::RouteTarget) -> String {
    match target {
        tritond_client::types::RouteTarget::Blackhole => "blackhole".to_string(),
        tritond_client::types::RouteTarget::Reject => "reject".to_string(),
        tritond_client::types::RouteTarget::VirtualGateway => "virtual-gateway".to_string(),
        tritond_client::types::RouteTarget::NatGateway { nat_gateway_id } => {
            format!("nat-gateway:{nat_gateway_id}")
        }
        tritond_client::types::RouteTarget::FloatingIp { floating_ip_id } => {
            format!("floating-ip:{floating_ip_id}")
        }
    }
}

fn print_route(route: &tritond_client::types::Route) {
    println!("Route {} in table {}", route.id, route.route_table_id);
    println!("  name:        {}", route.name);
    println!("  description: {}", route.description);
    println!("  destination: {}", route.destination);
    println!("  target:      {}", route_target_label(&route.target));
    println!("  created:     {}", route.created_at);
}

/// List route tables in a VPC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_table_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let route_tables = client
        .list_vpc_route_tables()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .context("list route tables")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&route_tables)?);
        return Ok(());
    }
    if route_tables.is_empty() {
        println!("(no route tables)");
        return Ok(());
    }
    for rt in route_tables {
        println!(
            "{}  main={}  {}",
            rt.id,
            if rt.is_main { "yes" } else { "no" },
            rt.name
        );
    }
    Ok(())
}

/// Create a route table in a VPC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_table_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    name: String,
    description: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let route_table = client
        .create_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(tritond_client::types::NewRouteTable {
            name,
            description: Some(description),
        })
        .send()
        .await
        .context("create route table")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&route_table)?);
    } else {
        println!("Created route table {} in vpc {vpc_id}", route_table.id);
        print_route_table(&route_table);
    }
    Ok(())
}

/// Read a single route table.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_table_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    route_table_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let route_table = client
        .get_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .send()
        .await
        .context("get route table")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&route_table)?);
    } else {
        print_route_table(&route_table);
    }
    Ok(())
}

/// Delete a route table.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_table_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    route_table_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_vpc_route_table()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .send()
        .await
        .context("delete route table")?;
    println!("Deleted route table {route_table_id} from vpc {vpc_id}");
    Ok(())
}

/// List routes in a route table.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    route_table_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let routes = client
        .list_vpc_route_table_routes()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .send()
        .await
        .context("list routes")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&routes)?);
        return Ok(());
    }
    if routes.is_empty() {
        println!("(no routes)");
        return Ok(());
    }
    for route in routes {
        println!(
            "{}  {}  {}  {}",
            route.id,
            route.destination,
            route_target_label(&route.target),
            route.name
        );
    }
    Ok(())
}

/// Create a route in a route table.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    route_table_id: Uuid,
    name: String,
    description: String,
    destination: String,
    target: String,
    json_output: bool,
) -> Result<()> {
    let target = parse_route_target(&target)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let route = client
        .create_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .body(tritond_client::types::NewRoute {
            name,
            description: Some(description),
            destination,
            target,
        })
        .send()
        .await
        .context("create route")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&route)?);
    } else {
        println!("Created route {} in route table {route_table_id}", route.id);
        print_route(&route);
    }
    Ok(())
}

/// Read a single route.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    route_table_id: Uuid,
    route_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let route = client
        .get_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route_id)
        .send()
        .await
        .context("get route")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&route)?);
    } else {
        print_route(&route);
    }
    Ok(())
}

/// Delete a route.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_route_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    route_table_id: Uuid,
    route_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_vpc_route_table_route()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .route_table_id(route_table_id)
        .route_id(route_id)
        .send()
        .await
        .context("delete route")?;
    println!("Deleted route {route_id} from route table {route_table_id}");
    Ok(())
}

/// List NAT gateways in a VPC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_nat_gw_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nat_gateways = client
        .list_vpc_nat_gateways()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .context("list nat gateways")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&nat_gateways)?);
        return Ok(());
    }
    if nat_gateways.is_empty() {
        println!("(no nat gateways)");
        return Ok(());
    }
    for n in nat_gateways {
        println!(
            "{}  {}  desired={}  applied={}  {}",
            n.id,
            n.public_address,
            n.desired_generation,
            n.realized
                .applied_generation
                .map(|generation| generation.to_string())
                .unwrap_or_else(|| "(none)".to_string()),
            n.name
        );
    }
    Ok(())
}

/// Create a NAT gateway in a VPC.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_nat_gw_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    name: String,
    description: String,
    family: String,
    json_output: bool,
) -> Result<()> {
    let family = parse_address_family(&family)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nat_gateway = client
        .create_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .body(tritond_client::types::NewNatGateway {
            name,
            description: Some(description),
            family,
        })
        .send()
        .await
        .context("create nat gateway")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&nat_gateway)?);
    } else {
        println!("Created nat gateway {} in vpc {vpc_id}", nat_gateway.id);
        print_nat_gateway(&nat_gateway);
    }
    Ok(())
}

/// Read a single NAT gateway.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_nat_gw_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    nat_gateway_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nat_gateway = client
        .get_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .nat_gateway_id(nat_gateway_id)
        .send()
        .await
        .context("get nat gateway")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&nat_gateway)?);
    } else {
        print_nat_gateway(&nat_gateway);
    }
    Ok(())
}

/// Delete a NAT gateway.
#[allow(clippy::too_many_arguments)] // CLI subcommand args; bundling
// into a struct here just adds
// ceremony.
pub async fn net_nat_gw_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    nat_gateway_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_vpc_nat_gateway()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .nat_gateway_id(nat_gateway_id)
        .send()
        .await
        .context("delete nat gateway")?;
    println!("Deleted nat gateway {nat_gateway_id} from vpc {vpc_id}");
    Ok(())
}

/// List the disks attached to an instance.
pub async fn tenant_project_instance_disk_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let disks = client
        .list_instance_disks()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_disk_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    disk_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let disk = client
        .get_instance_disk()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_nic_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nics = client
        .list_instance_nics()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_nic_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    nic_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let nic = client
        .get_instance_nic()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_instance_lifecycle(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    transition: &str,
    tenant_id: Uuid,
    project_id: Uuid,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let instance = match transition {
        "start" => client
            .start_project_instance()
            .tenant_id(tenant_id)
            .project_id(project_id)
            .instance_id(instance_id)
            .send()
            .await
            .context("start instance")?
            .into_inner(),
        "stop" => client
            .stop_project_instance()
            .tenant_id(tenant_id)
            .project_id(project_id)
            .instance_id(instance_id)
            .send()
            .await
            .context("stop instance")?
            .into_inner(),
        "restart" => client
            .restart_project_instance()
            .tenant_id(tenant_id)
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
pub async fn tenant_project_quota_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
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
        .tenant_id(tenant_id)
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
pub async fn tenant_project_quota_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let quota = client
        .get_project_quota()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_quota_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_quota()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .send()
        .await
        .context("delete project quota")?;
    println!("Removed quota from project {project_id} (now unlimited)");
    Ok(())
}

/// Delete a project.
pub async fn tenant_project_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_tenant_project()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .send()
        .await
        .context("delete project")?;
    println!("Deleted project {project_id} from silo {tenant_id}");
    Ok(())
}

fn fmt_opt_cidr(opt: Option<&String>) -> &str {
    opt.map(|s| s.as_str()).unwrap_or("(none)")
}

/// List the VPCs in a project.
pub async fn tenant_project_vpc_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let vpcs = client
        .list_project_vpcs()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_vpc_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
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
        .tenant_id(tenant_id)
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
pub async fn tenant_project_vpc_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let vpc = client
        .get_project_vpc()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_vpc_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_project_vpc()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .send()
        .await
        .context("delete vpc")?;
    println!("Deleted vpc {vpc_id} from project {project_id}");
    Ok(())
}

/// List the subnets in a VPC.
pub async fn tenant_project_vpc_subnet_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let subnets = client
        .list_vpc_subnets()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_vpc_subnet_create(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
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
        .tenant_id(tenant_id)
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
pub async fn tenant_project_vpc_subnet_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    subnet_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let subnet = client
        .get_vpc_subnet()
        .tenant_id(tenant_id)
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
pub async fn tenant_project_vpc_subnet_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    vpc_id: Uuid,
    subnet_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_vpc_subnet()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .vpc_id(vpc_id)
        .subnet_id(subnet_id)
        .send()
        .await
        .context("delete subnet")?;
    println!("Deleted subnet {subnet_id} from vpc {vpc_id}");
    Ok(())
}

/// Resolve `--public-key` / `--public-key-file` into the openssh
/// string the API edge expects. Used by every per-scope ssh-key
/// add command.
fn resolve_public_key(
    public_key: Option<String>,
    public_key_file: Option<String>,
) -> Result<String> {
    match (public_key, public_key_file) {
        (Some(s), None) => Ok(s),
        (None, Some(path)) => {
            std::fs::read_to_string(&path).with_context(|| format!("read public key from {path}"))
        }
        (None, None) => {
            anyhow::bail!("--public-key or --public-key-file is required")
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with should prevent this"),
    }
}

fn print_ssh_keys(keys: Vec<tritond_client::types::SshKey>, json_output: bool) -> Result<()> {
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

fn print_ssh_key_details(key: &tritond_client::types::SshKey) {
    println!("  scope:       {:?}", key.scope);
    println!("  name:        {}", key.name);
    println!("  description: {}", key.description);
    println!("  fingerprint: {}", key.fingerprint);
    println!("  public_key:  {}", key.public_key);
    println!("  created:     {}", key.created_at);
}

/// List Public SSH keys. Anonymous-accessible.
pub async fn public_ssh_key_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let keys = client
        .list_public_ssh_keys()
        .send()
        .await
        .context("list public ssh keys")?
        .into_inner();
    print_ssh_keys(keys, json_output)
}

/// Register a `Public` SSH key (root-only).
pub async fn public_ssh_key_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    name: String,
    description: String,
    public_key: Option<String>,
    public_key_file: Option<String>,
    json_output: bool,
) -> Result<()> {
    let public_key = resolve_public_key(public_key, public_key_file)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .create_public_ssh_key()
        .body(tritond_client::types::NewSshKey {
            name,
            description: Some(description),
            public_key,
        })
        .send()
        .await
        .context("create public ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!("Registered public ssh key {}", key.id);
        print_ssh_key_details(&key);
    }
    Ok(())
}

/// List SSH keys whose scope is exactly `Silo { silo_id }` (does
/// NOT include Public; use `tcadm tenant ssh-key list` for the
/// unioned tenant view).
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
    print_ssh_keys(keys, json_output)
}

/// Register a new `Silo`-scoped SSH key.
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
    let public_key = resolve_public_key(public_key, public_key_file)?;
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
        print_ssh_key_details(&key);
    }
    Ok(())
}

/// List SSH keys visible to a tenant (Public + Silo + Tenant).
pub async fn tenant_ssh_key_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let keys = client
        .list_tenant_ssh_keys()
        .tenant_id(tenant_id)
        .send()
        .await
        .context("list tenant ssh keys")?
        .into_inner();
    print_ssh_keys(keys, json_output)
}

/// Register a `Tenant`-scoped SSH key.
#[allow(clippy::too_many_arguments)]
pub async fn tenant_ssh_key_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    name: String,
    description: String,
    public_key: Option<String>,
    public_key_file: Option<String>,
    json_output: bool,
) -> Result<()> {
    let public_key = resolve_public_key(public_key, public_key_file)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .create_tenant_ssh_key()
        .tenant_id(tenant_id)
        .body(tritond_client::types::NewSshKey {
            name,
            description: Some(description),
            public_key,
        })
        .send()
        .await
        .context("create tenant ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!("Registered tenant ssh key {} in tenant {tenant_id}", key.id);
        print_ssh_key_details(&key);
    }
    Ok(())
}

/// List SSH keys visible to a project (Public + Silo + Tenant + Project).
pub async fn project_ssh_key_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let keys = client
        .list_project_ssh_keys()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .send()
        .await
        .context("list project ssh keys")?
        .into_inner();
    print_ssh_keys(keys, json_output)
}

/// Register a `Project`-scoped SSH key.
#[allow(clippy::too_many_arguments)]
pub async fn project_ssh_key_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    name: String,
    description: String,
    public_key: Option<String>,
    public_key_file: Option<String>,
    json_output: bool,
) -> Result<()> {
    let public_key = resolve_public_key(public_key, public_key_file)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .create_project_ssh_key()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .body(tritond_client::types::NewSshKey {
            name,
            description: Some(description),
            public_key,
        })
        .send()
        .await
        .context("create project ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!(
            "Registered project ssh key {} in project {project_id}",
            key.id
        );
        print_ssh_key_details(&key);
    }
    Ok(())
}

/// List the caller's `User`-scoped SSH keys.
pub async fn auth_ssh_key_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let keys = client
        .list_my_ssh_keys()
        .send()
        .await
        .context("list my ssh keys")?
        .into_inner();
    print_ssh_keys(keys, json_output)
}

/// Register a `User`-scoped SSH key owned by the caller.
pub async fn auth_ssh_key_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    name: String,
    description: String,
    public_key: Option<String>,
    public_key_file: Option<String>,
    json_output: bool,
) -> Result<()> {
    let public_key = resolve_public_key(public_key, public_key_file)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .create_my_ssh_key()
        .body(tritond_client::types::NewSshKey {
            name,
            description: Some(description),
            public_key,
        })
        .send()
        .await
        .context("create my ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!("Registered user-scoped ssh key {}", key.id);
        print_ssh_key_details(&key);
    }
    Ok(())
}

/// Read a single SSH key by id (works regardless of scope; the
/// server applies the visibility filter).
pub async fn ssh_key_show(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    key_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let key = client
        .get_ssh_key()
        .key_id(key_id)
        .send()
        .await
        .context("get ssh key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&key)?);
    } else {
        println!("SshKey {}", key.id);
        print_ssh_key_details(&key);
    }
    Ok(())
}

/// Delete an SSH key by id. Ownership is enforced server-side
/// based on the key's scope.
pub async fn ssh_key_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    key_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_ssh_key()
        .key_id(key_id)
        .send()
        .await
        .context("delete ssh key")?;
    println!("Deleted ssh key {key_id}");
    Ok(())
}

/// List images whose scope is exactly `Silo { silo_id }` (not
/// the unioned tenant view; use `tcadm tenant image list` for
/// that).
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
    print_images(images, json_output)
}

/// Register a new `Silo`-scoped image.
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
            // tcadm's explicit-fields image-create path doesn't
            // populate compatibility — operators who want
            // compatibility gates should use the bundle path
            // (`tritonimg-build` + `tcadm silo image add-bundle`,
            // future flag).
            compatibility: None,
        })
        .send()
        .await
        .context("create image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Registered image {} in silo {silo_id}", image.id);
        print_image_details(&image);
    }
    Ok(())
}

fn print_image_details(image: &tritond_client::types::Image) {
    println!("  scope:       {:?}", image.scope);
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

/// Generic body builder for the explicit-fields image-create
/// path. Used by every per-scope `*_image_add` command so the
/// field set stays consistent across scopes.
#[allow(clippy::too_many_arguments)]
fn build_new_image(
    name: String,
    description: String,
    os: String,
    version: String,
    size_bytes: u64,
    sha256: String,
    source_url: Option<String>,
    id: Option<Uuid>,
) -> tritond_client::types::NewImage {
    tritond_client::types::NewImage {
        name,
        description: Some(description),
        os,
        version,
        size_bytes,
        sha256,
        source_url,
        id,
        compatibility: None,
    }
}

/// List Public images. Anonymous-accessible.
pub async fn public_image_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let images = client
        .list_public_images()
        .send()
        .await
        .context("list public images")?
        .into_inner();
    print_images(images, json_output)
}

/// Register a `Public` image (root-only).
#[allow(clippy::too_many_arguments)]
pub async fn public_image_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
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
        .create_public_image()
        .body(build_new_image(
            name,
            description,
            os,
            version,
            size_bytes,
            sha256,
            source_url,
            id,
        ))
        .send()
        .await
        .context("create public image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Registered public image {}", image.id);
        print_image_details(&image);
    }
    Ok(())
}

/// List images visible to a tenant (Public + Silo + Tenant).
pub async fn tenant_image_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let images = client
        .list_tenant_images()
        .tenant_id(tenant_id)
        .send()
        .await
        .context("list tenant images")?
        .into_inner();
    print_images(images, json_output)
}

/// Register a `Tenant`-scoped image.
#[allow(clippy::too_many_arguments)]
pub async fn tenant_image_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
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
        .create_tenant_image()
        .tenant_id(tenant_id)
        .body(build_new_image(
            name,
            description,
            os,
            version,
            size_bytes,
            sha256,
            source_url,
            id,
        ))
        .send()
        .await
        .context("create tenant image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Registered tenant image {} in tenant {tenant_id}", image.id);
        print_image_details(&image);
    }
    Ok(())
}

/// List images visible to a project (Public + Silo + Tenant + Project).
pub async fn project_image_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let images = client
        .list_project_images()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .send()
        .await
        .context("list project images")?
        .into_inner();
    print_images(images, json_output)
}

/// Register a `Project`-scoped image.
#[allow(clippy::too_many_arguments)]
pub async fn project_image_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant_id: Uuid,
    project_id: Uuid,
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
        .create_project_image()
        .tenant_id(tenant_id)
        .project_id(project_id)
        .body(build_new_image(
            name,
            description,
            os,
            version,
            size_bytes,
            sha256,
            source_url,
            id,
        ))
        .send()
        .await
        .context("create project image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!(
            "Registered project image {} in project {project_id}",
            image.id
        );
        print_image_details(&image);
    }
    Ok(())
}

/// List the caller's `User`-scoped images.
pub async fn auth_image_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let images = client
        .list_my_images()
        .send()
        .await
        .context("list my images")?
        .into_inner();
    print_images(images, json_output)
}

/// Register a `User`-scoped image owned by the caller.
#[allow(clippy::too_many_arguments)]
pub async fn auth_image_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
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
        .create_my_image()
        .body(build_new_image(
            name,
            description,
            os,
            version,
            size_bytes,
            sha256,
            source_url,
            id,
        ))
        .send()
        .await
        .context("create my image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Registered user-scoped image {}", image.id);
        print_image_details(&image);
    }
    Ok(())
}

fn print_images(images: Vec<tritond_client::types::Image>, json_output: bool) -> Result<()> {
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

/// Read a single image by id (works regardless of scope; the
/// server applies the visibility filter).
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
        .get_image()
        .image_id(image_id)
        .send()
        .await
        .context("get image")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&image)?);
    } else {
        println!("Image {} (silo context {silo_id})", image.id);
        println!("  scope:       {:?}", image.scope);
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

/// Delete an image by id. Ownership is enforced server-side
/// based on the image's scope.
pub async fn silo_image_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    image_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_image()
        .image_id(image_id)
        .send()
        .await
        .context("delete image")?;
    println!("Deleted image {image_id} (silo context {silo_id})");
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

/// Wire-format label for a [`CnState`]. Matches the JSON serialisation
/// (`pending`, `approved`, `disabled`) so operators see the same string
/// they'd type into a `--state` filter or a JSON request body.
fn cn_state_label(state: &tritond_client::types::CnState) -> &'static str {
    match state {
        tritond_client::types::CnState::Pending => "pending",
        tritond_client::types::CnState::Approved => "approved",
        tritond_client::types::CnState::Disabled => "disabled",
    }
}

fn fmt_opt_ipv4(opt: &Option<std::net::Ipv4Addr>) -> String {
    opt.as_ref()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_opt_ts(opt: &Option<chrono::DateTime<chrono::Utc>>) -> String {
    opt.as_ref()
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| "-".to_string())
}

/// List CNs, optionally filtered by state.
pub async fn cn_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    state: Option<tritond_client::types::CnState>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_cns();
    if let Some(s) = state {
        req = req.state(s);
    }
    let cns = req.send().await.context("list cns")?.into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&cns)?);
        return Ok(());
    }
    if cns.is_empty() {
        println!("(no compute nodes)");
        return Ok(());
    }
    // Tab-separated table mirroring the existing `silo project vpc list`
    // approach: hand-rolled, no extra dependency, easy to grep / awk.
    println!(
        "{:<36}  {:<24}  {:<9}  {:<15}  REGISTERED_AT",
        "SERVER_UUID", "HOSTNAME", "STATE", "ADMIN_IP"
    );
    for cn in cns {
        println!(
            "{:<36}  {:<24}  {:<9}  {:<15}  {}",
            cn.server_uuid,
            cn.hostname,
            cn_state_label(&cn.state),
            fmt_opt_ipv4(&cn.admin_ip),
            cn.registered_at.to_rfc3339(),
        );
    }
    Ok(())
}

/// Read a single CN by `server_uuid`.
pub async fn cn_show(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    server_uuid: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let cn = client
        .get_cn()
        .server_uuid(server_uuid)
        .send()
        .await
        .context("get cn")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&cn)?);
        return Ok(());
    }
    println!("Cn {}", cn.server_uuid);
    println!("  state:            {}", cn_state_label(&cn.state));
    println!("  hostname:         {}", cn.hostname);
    println!("  admin_ip:         {}", fmt_opt_ipv4(&cn.admin_ip));
    println!("  registered_at:    {}", cn.registered_at.to_rfc3339());
    println!("  approved_at:      {}", fmt_opt_ts(&cn.approved_at));
    println!("  last_seen:        {}", fmt_opt_ts(&cn.last_seen));
    if let Some(code) = &cn.claim_code {
        println!("  claim_code:       {code}");
        println!(
            "  claim_code_expires_at: {}",
            fmt_opt_ts(&cn.claim_code_expires_at)
        );
    }
    if let Some(key_id) = cn.bound_api_key_id {
        println!("  bound_api_key_id: {key_id}");
    }
    println!("  sysinfo:");
    // Pretty-print the nested sysinfo blob; indent each line by two
    // spaces so it visually nests under the parent block.
    let sysinfo_pretty = serde_json::to_string_pretty(&cn.sysinfo)?;
    for line in sysinfo_pretty.lines() {
        println!("    {line}");
    }
    Ok(())
}

/// Approve a Pending CN by claim code.
///
/// Note: the per-CN API key plaintext is **never** shown to the
/// operator. It is delivered to the agent via the long-poll on
/// `/v2/agent/register/status`. The operator only sees the bound key
/// id so they can correlate audit events.
pub async fn cn_approve(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    code: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let cn = client
        .approve_cn()
        .body(tritond_client::types::ApproveCnRequest { code })
        .send()
        .await
        .context("approve cn")?
        .into_inner();

    if json_output {
        // The wire shape is the redacted CnView; it never contains the
        // plaintext API key, so emitting it as-is is safe.
        println!("{}", serde_json::to_string_pretty(&cn)?);
        return Ok(());
    }
    let key_id = cn
        .bound_api_key_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "(none)".to_string());
    println!(
        "Approved CN {}; bound api key id {}",
        cn.server_uuid, key_id
    );
    Ok(())
}

/// Disable a CN; revokes the bound API key.
pub async fn cn_disable(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    server_uuid: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let cn = client
        .disable_cn()
        .server_uuid(server_uuid)
        .send()
        .await
        .context("disable cn")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&cn)?);
        return Ok(());
    }
    println!(
        "Disabled CN {} (state={})",
        cn.server_uuid,
        cn_state_label(&cn.state)
    );
    Ok(())
}

fn print_auto_approve_window(w: &tritond_client::types::AutoApproveWindow) {
    println!("  opened_at:       {}", w.opened_at.to_rfc3339());
    println!("  expires_at:      {}", w.expires_at.to_rfc3339());
    println!(
        "  remaining_count: {}",
        w.remaining_count
            .map(|n| n.to_string())
            .unwrap_or_else(|| "(unlimited)".to_string())
    );
    println!("  opened_by:       {}", w.opened_by);
}

/// Read the current auto-approve window.
///
/// The trait surface returns `Option<AutoApproveWindow>` (so the wire
/// is `null` when no window is open), but the OpenAPI spec — and
/// therefore the generated client — only models the present case. To
/// faithfully render both shapes without modifying the spec, we make
/// the GET ourselves and parse the body as `Option<AutoApproveWindow>`.
pub async fn cn_auto_approve_status(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;

    // Same TLS posture as Session::client() so the raw and typed
    // paths are interchangeable from the server's perspective.
    let http = build_http_client(session.bearer.as_deref())?;

    let url = format!("{}/v2/cn-auto-approve", session.endpoint);
    // Match the api-version header that the generated client sends
    // on every call so behaviour stays identical between this raw GET
    // and the typed callers below.
    let api_version = <tritond_client::Client as tritond_client::ClientInfo<()>>::api_version();
    let response = http
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("api-version", api_version)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("read body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!("get auto-approve window: {status}: {body}");
    }
    let window: Option<tritond_client::types::AutoApproveWindow> = serde_json::from_str(&body)
        .with_context(|| format!("parse auto-approve window response (body={body:?})"))?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&window)?);
        return Ok(());
    }
    match window {
        None => println!("No window open."),
        Some(w) => {
            println!("Auto-approve window:");
            print_auto_approve_window(&w);
        }
    }
    Ok(())
}

/// Open (or replace) the auto-approve window.
pub async fn cn_auto_approve_open(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    duration_secs: u64,
    count: Option<u64>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let window = client
        .open_auto_approve_window()
        .body(tritond_client::types::OpenAutoApproveRequest {
            duration_secs,
            count,
        })
        .send()
        .await
        .context("open auto-approve window")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&window)?);
    } else {
        println!("Auto-approve window opened:");
        print_auto_approve_window(&window);
    }
    Ok(())
}

/// Close the auto-approve window. Idempotent.
pub async fn cn_auto_approve_close(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .close_auto_approve_window()
        .send()
        .await
        .context("close auto-approve window")?;
    println!("Auto-approve window closed.");
    Ok(())
}

async fn exchange_password(endpoint: &str, username: &str, password: &str) -> Result<Tokens> {
    let client = anonymous_client(endpoint)?;
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
