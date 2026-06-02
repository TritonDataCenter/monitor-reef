// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Subcommand implementations for `tcadm`.

use anyhow::{Context, Result, bail};
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

/// Interactively log in and persist credentials to `~/.config/tcadm/config.json`.
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

pub fn logout() -> Result<()> {
    Config::delete()?;
    println!("Logged out (config removed).");
    Ok(())
}

/// Emit shell exports for the current session (for scripts that
/// don't share the on-disk config).
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

pub async fn operations_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    after_id: Option<Uuid>,
    limit: Option<u32>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_operations();
    if let Some(a) = after_id {
        req = req.after_id(a);
    }
    if let Some(l) = limit {
        req = req.limit(l);
    }
    let ops = req.send().await.context("list operations")?.into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&ops)?);
        return Ok(());
    }
    if ops.is_empty() {
        println!("(no operations)");
    } else {
        println!(
            "{:36}  {:24}  {:>3}  {:>8}  {:30}",
            "id", "kind", "ver", "state", "created"
        );
        for op in &ops {
            let state = serde_json::to_value(&op.state)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "?".to_string());
            println!(
                "{:36}  {:24}  {:>3}  {:>8}  {}",
                op.id, op.kind, op.version, state, op.time_created
            );
        }
    }
    Ok(())
}

/// Abandon (force-unwind) an in-flight operation. The running action
/// body completes its natural outcome before the catalog's undos fire.
pub async fn operations_abandon(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    operation_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let resp = client
        .abandon_operation()
        .operation_id(operation_id)
        .send()
        .await
        .context("abandon operation")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    } else {
        println!(
            "abandoned operation {} (poked {} saga nodes; next pending node will trigger unwind)",
            resp.id, resp.poked_nodes,
        );
    }
    Ok(())
}

/// Fetch the detail view (summary + persisted DAG) for one operation.
pub async fn operations_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    operation_id: Uuid,
    _json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let detail = client
        .get_operation()
        .operation_id(operation_id)
        .send()
        .await
        .context("get operation")?
        .into_inner();
    // Detail surface has no useful short form; always pretty-print
    // JSON so operators can grep the DAG.
    println!("{}", serde_json::to_string_pretty(&detail)?);
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

/// Mint a tenant-bound operator user.
pub async fn tenant_create_user(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    tenant_id: Uuid,
    username: String,
    password: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let user = client
        .create_silo_tenant_user()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .body(tritond_client::types::NewSiloTenantUser { username, password })
        .send()
        .await
        .context("create tenant user")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&user)?);
    } else {
        println!("Created user {} in tenant {tenant_id}", user.id);
        println!("  username: {}", user.username);
        if let Some(tid) = user.tenant_id {
            println!("  tenant:   {tid}");
        }
        println!("  created:  {}", user.created_at);
    }
    Ok(())
}

/// Drop the storage workspace binding from a tenant.
pub async fn tenant_drop_storage(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let tenant = client
        .drop_silo_tenant_storage()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .send()
        .await
        .context("drop tenant storage binding")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&tenant)?);
    } else {
        println!("Dropped storage binding for tenant {tenant_id}");
        println!("  (workspace archived on mantad; tenant row is now unbound)");
    }
    Ok(())
}

/// Retrofit a storage workspace binding onto an existing tenant.
pub async fn tenant_init_storage(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    silo_id: Uuid,
    tenant_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let tenant = client
        .init_silo_tenant_storage()
        .silo_id(silo_id)
        .tenant_id(tenant_id)
        .send()
        .await
        .context("init tenant storage binding")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&tenant)?);
    } else {
        println!("Initialised storage binding for tenant {tenant_id}");
        if let Some(workspace_id) = tenant.storage_workspace_id {
            println!("  workspace: t-{}", workspace_id.simple());
        }
        if let Some(cluster_id) = tenant.storage_cluster_id {
            println!("  cluster:   {cluster_id}");
        }
    }
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

/// Set (or replace) a workspace's storage quota. Routes through
/// mantad-client (not tritond-client) — the admin workspace surface
/// lives on mantad. After the PUT (204 No Content), the handler
/// re-GETs the quota to surface live `usage_bytes` plus the new
/// `limit_bytes` to the operator.
pub async fn workspace_quota_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    name: String,
    limit_bytes: i64,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.mantad_client()?;
    client
        .put_workspace_quota(&name, &mantad_client::types::QuotaRequest { limit_bytes })
        .await
        .context("set workspace quota")?;

    // PUT returns 204 — no body to echo. Re-GET to surface the post-state.
    let quota = client
        .get_workspace_quota(&name)
        .await
        .context("re-read workspace quota after set")?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&quota)?);
    } else {
        println!("Set quota on workspace {name}");
        println!("  usage_bytes: {}", quota.usage_bytes);
        println!("  limit_bytes: {}", quota.limit_bytes);
    }
    Ok(())
}

/// Read a workspace's live storage quota (usage + configured limit).
pub async fn workspace_quota_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    name: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.mantad_client()?;
    let quota = client
        .get_workspace_quota(&name)
        .await
        .context("get workspace quota")?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&quota)?);
    } else {
        println!("Workspace {name}");
        println!("  usage_bytes: {}", quota.usage_bytes);
        println!("  limit_bytes: {}", quota.limit_bytes);
    }
    Ok(())
}

/// Attach (or replace) an inline IAM policy on a mantad user.
///
/// `document` is either a literal JSON string or `-` to read the
/// document from stdin. The body is validated as JSON before it
/// ships so a typo here surfaces locally rather than as a generic
/// 400 from mantad.
pub async fn user_policy_put(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    user: String,
    policy: String,
    document: String,
    workspace: Option<String>,
    json_output: bool,
) -> Result<()> {
    let raw = if document == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read policy document from stdin")?;
        buf
    } else {
        document
    };

    let doc_value: serde_json::Value = serde_json::from_str(raw.trim())
        .context("policy document is not valid JSON; expected an object like {\"Version\":\"…\",\"Statement\":[…]}")?;

    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.mantad_client()?;
    client
        .put_user_policy(&user, &policy, &doc_value, workspace.as_deref())
        .await
        .context("put user policy")?;

    if json_output {
        println!("{{}}");
    } else {
        match workspace.as_deref() {
            Some(ws) => println!("Put policy {policy} on user {user} (workspace {ws})"),
            None => println!("Put policy {policy} on user {user}"),
        }
    }
    Ok(())
}

/// Read a user's inline policy by name.
pub async fn user_policy_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    user: String,
    policy: String,
    workspace: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.mantad_client()?;
    let doc = client
        .get_user_policy(&user, &policy, workspace.as_deref())
        .await
        .context("get user policy")?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        println!("User:   {user}");
        println!("Policy: {policy}");
        if let Some(ws) = workspace.as_deref() {
            println!("Workspace: {ws}");
        }
        println!("Document:");
        println!("{}", serde_json::to_string_pretty(&doc)?);
    }
    Ok(())
}

/// Delete a user's inline policy.
pub async fn user_policy_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    user: String,
    policy: String,
    workspace: Option<String>,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.mantad_client()?;
    client
        .delete_user_policy(&user, &policy, workspace.as_deref())
        .await
        .context("delete user policy")?;

    match workspace.as_deref() {
        Some(ws) => println!("Deleted policy {policy} from user {user} (workspace {ws})"),
        None => println!("Deleted policy {policy} from user {user}"),
    }
    Ok(())
}

/// List the names of inline policies attached to a user.
pub async fn user_policy_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    user: String,
    workspace: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.mantad_client()?;
    let names = client
        .list_user_policies(&user, workspace.as_deref())
        .await
        .context("list user policies")?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&names)?);
    } else {
        match workspace.as_deref() {
            Some(ws) => println!("Inline policies for user {user} (workspace {ws}):"),
            None => println!("Inline policies for user {user}:"),
        }
        if names.is_empty() {
            println!("  (none)");
        } else {
            for name in &names {
                println!("  {name}");
            }
        }
    }
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

fn cn_role_label(role: &tritond_client::types::CnRole) -> &'static str {
    match role {
        tritond_client::types::CnRole::Tenant => "tenant",
        tritond_client::types::CnRole::Edge => "edge",
        tritond_client::types::CnRole::Both => "both",
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

/// List CNs, optionally filtered by state. Capability: `SystemRead`.
pub async fn cn_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    state: Option<tritond_client::types::CnState>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_system_cns_v1();
    if let Some(s) = state {
        req = req.state(s);
    }
    let page = req
        .send()
        .await
        .context("/v1/system/cns list")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no compute nodes)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<9}  {:<7}  {:<15}  REGISTERED_AT",
        "SERVER_UUID", "HOSTNAME", "STATE", "ROLE", "ADMIN_IP"
    );
    for cn in &page.items {
        println!(
            "{:<36}  {:<24}  {:<9}  {:<7}  {:<15}  {}",
            cn.server_uuid,
            cn.hostname,
            cn_state_label(&cn.state),
            cn_role_label(&cn.role),
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
        .get_system_cn_v1()
        .cn_id(server_uuid)
        .send()
        .await
        .context("/v1/system/cns/{id} get")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&cn)?);
        return Ok(());
    }
    println!("Cn {}", cn.server_uuid);
    println!("  state:            {}", cn_state_label(&cn.state));
    println!("  role:             {}", cn_role_label(&cn.role));
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
/// `/v1/agent/register/status`. The operator only sees the bound key
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

/// Set a CN placement label.
pub async fn cn_label_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    server_uuid: Uuid,
    role: tritond_client::types::CnRole,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let cn = client
        .set_cn_role()
        .server_uuid(server_uuid)
        .body(tritond_client::types::SetCnRoleRequest { role })
        .send()
        .await
        .context("set cn role")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&cn)?);
        return Ok(());
    }
    println!(
        "Set CN {} role to {}",
        cn.server_uuid,
        cn_role_label(&cn.role)
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

/// Read the current auto-approve window. Issues the GET directly
/// (instead of the generated client) so a `null` body parses as
/// `Option::None` — the OpenAPI spec only models the present case.
pub async fn cn_auto_approve_status(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;

    // Same TLS posture as Session::client() so the raw and typed
    // paths are interchangeable from the server's perspective.
    let http = build_http_client(session.bearer.as_deref())?;

    let url = format!("{}/v1/cn-auto-approve", session.endpoint);
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

// ---------------------------------------------------------------------
// Legacy admin (fleet-scoped)
// ---------------------------------------------------------------------

/// List CNs with managed-vs-legacy zone counts. Fleet-admin only.
pub async fn legacy_cn_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let cns = client
        .list_legacy_cns()
        .send()
        .await
        .context("list legacy cns")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&cns)?);
        return Ok(());
    }
    if cns.is_empty() {
        println!("(no compute nodes)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<9}  {:>8}  {:>7}  LAST_SEEN",
        "SERVER_UUID", "HOSTNAME", "STATE", "MANAGED", "LEGACY"
    );
    for cn in cns {
        println!(
            "{:<36}  {:<24}  {:<9}  {:>8}  {:>7}  {}",
            cn.server_uuid,
            cn.hostname,
            cn_state_label(&cn.state),
            cn.managed_instance_count,
            cn.legacy_vm_count,
            fmt_opt_ts(&cn.last_seen),
        );
    }
    Ok(())
}

/// List legacy VMs across the fleet, optionally filtered by host CN.
pub async fn legacy_vm_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    host_cn: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_legacy_vms();
    if let Some(cn) = host_cn {
        req = req.host_cn(cn);
    }
    let vms = req.send().await.context("list legacy vms")?.into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&vms)?);
        return Ok(());
    }
    if vms.is_empty() {
        println!("(no legacy VMs)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<36}  {:<14}  {:<7}  {:<11}  LAST_SEEN_AT",
        "SMARTOS_UUID", "HOST_CN_UUID", "BRAND", "STATE", "ZONE_STATE"
    );
    for vm in vms {
        println!(
            "{:<36}  {:<36}  {:<14}  {:<7}  {:<11}  {}",
            vm.smartos_uuid,
            vm.host_cn_uuid,
            vm.brand.as_deref().unwrap_or("-"),
            // VmState is a Progenitor enum without a Display impl;
            // enum_to_display from rust_utils renders the wire-form
            // string, but for compactness here we strip the Debug
            // form to lowercase via a small inline helper.
            vm_state_short(vm.state.as_ref()),
            vm.zone_state.as_deref().unwrap_or("-"),
            vm.last_seen_at.to_rfc3339(),
        );
    }
    Ok(())
}

/// Show one legacy VM record in full (including NICs).
pub async fn legacy_vm_show(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    smartos_uuid: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let vm = client
        .get_legacy_vm()
        .smartos_uuid(smartos_uuid)
        .send()
        .await
        .context("get legacy vm")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&vm)?);
        return Ok(());
    }
    println!("LegacyVm {}", vm.smartos_uuid);
    println!("  host_cn_uuid:      {}", vm.host_cn_uuid);
    println!(
        "  legacy_owner:      {}",
        vm.legacy_owner_uuid
            .map(|u| u.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  brand:             {}",
        vm.brand.as_deref().unwrap_or("-")
    );
    println!("  state:             {}", vm_state_short(vm.state.as_ref()));
    println!(
        "  zone_state:        {}",
        vm.zone_state.as_deref().unwrap_or("-")
    );
    println!(
        "  memory_bytes:      {}",
        vm.memory_bytes
            .map(|m| m.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  quota_bytes:       {}",
        vm.quota_bytes
            .map(|q| q.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  cpu_cap:           {}",
        vm.cpu_cap
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  last_modified:     {}",
        vm.last_modified.as_deref().unwrap_or("-")
    );
    println!("  first_seen_at:     {}", vm.first_seen_at.to_rfc3339());
    println!("  last_seen_at:      {}", vm.last_seen_at.to_rfc3339());
    println!("  adoptable:         {:?}", vm.adoptable);
    if vm.nics.is_empty() {
        println!("  nics:              (none)");
    } else {
        println!("  nics:");
        for (i, nic) in vm.nics.iter().enumerate() {
            println!(
                "    [{i}] mac={} ip={} tag={} vlan={} primary={}",
                nic.mac.as_deref().unwrap_or("-"),
                nic.ip
                    .map(|ip| ip.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                nic.nic_tag.as_deref().unwrap_or("-"),
                nic.vlan_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                nic.primary,
            );
        }
    }
    Ok(())
}

fn vm_state_short(state: Option<&tritond_client::types::VmState>) -> &'static str {
    use tritond_client::types::VmState;
    match state {
        Some(VmState::Running) => "running",
        Some(VmState::Stopped) => "stopped",
        Some(VmState::Provisioning) => "prov",
        Some(VmState::Receiving) => "recv",
        Some(VmState::Sending) => "send",
        Some(VmState::Configured) => "config",
        Some(VmState::Incomplete) => "incomp",
        Some(VmState::Failed) => "failed",
        Some(VmState::Installed) => "instal",
        Some(VmState::Destroyed) => "destr",
        Some(VmState::Unknown) => "unknwn",
        None => "-",
    }
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

// ── Storage cluster registry ─────────────────────────────────────────

/// Resolve `ident` to a UUID. Tries to parse as a Uuid first; on
/// failure, calls `list_storage_clusters` and finds the cluster with
/// a matching `name` field. The fallback is one extra round trip but
/// keeps `tcadm storage cluster show <name>` working without making
/// the operator carry UUIDs around.
async fn resolve_storage_cluster_ident(
    client: &tritond_client::Client,
    ident: &str,
) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(ident) {
        return Ok(id);
    }
    let clusters = client
        .list_storage_clusters()
        .send()
        .await
        .context("list storage clusters")?
        .into_inner();
    clusters
        .into_iter()
        .find(|c| c.name == ident)
        .map(|c| c.id)
        .ok_or_else(|| anyhow::anyhow!("no storage cluster registered with name {ident:?}"))
}

fn storage_surface_label(s: &tritond_client::types::StorageClusterSurface) -> &'static str {
    use tritond_client::types::StorageClusterSurface;
    match s {
        StorageClusterSurface::S3 => "s3",
        StorageClusterSurface::Fs => "fs",
        StorageClusterSurface::Block => "block",
    }
}

fn storage_status_label(s: &tritond_client::types::StorageClusterStatus) -> &'static str {
    use tritond_client::types::StorageClusterStatus;
    match s {
        StorageClusterStatus::Healthy => "healthy",
        StorageClusterStatus::Degraded => "degraded",
        StorageClusterStatus::Unreachable => "unreachable",
        StorageClusterStatus::Unknown => "unknown",
    }
}

pub async fn storage_cluster_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let clusters = client
        .list_storage_clusters()
        .send()
        .await
        .context("list storage clusters")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&clusters)?);
        return Ok(());
    }
    if clusters.is_empty() {
        println!("(no storage clusters registered)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<7}  {:<11}  ENDPOINT",
        "CLUSTER_ID", "NAME", "SURFACE", "STATUS"
    );
    for c in clusters {
        println!(
            "{:<36}  {:<24}  {:<7}  {:<11}  {}",
            c.id,
            c.name,
            storage_surface_label(&c.surface),
            storage_status_label(&c.status),
            c.endpoint,
        );
    }
    Ok(())
}

pub async fn storage_cluster_show(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    ident: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let id = resolve_storage_cluster_ident(&client, &ident).await?;
    let cluster = client
        .get_storage_cluster()
        .id(id)
        .send()
        .await
        .context("get storage cluster")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&cluster)?);
        return Ok(());
    }
    println!("StorageCluster {}", cluster.id);
    println!("  name:             {}", cluster.name);
    println!(
        "  surface:          {}",
        storage_surface_label(&cluster.surface)
    );
    println!("  endpoint:         {}", cluster.endpoint);
    println!("  default_region:   {}", cluster.default_region);
    if let Some(label) = &cluster.display_name {
        println!("  display_name:     {label}");
    }
    println!(
        "  status:           {}",
        storage_status_label(&cluster.status)
    );
    println!("  created_at:       {}", cluster.created_at.to_rfc3339());
    println!(
        "  last_observed_at: {}",
        cluster
            .last_observed_at
            .as_ref()
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "(never)".to_string())
    );
    Ok(())
}

pub async fn storage_cluster_add(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    name: String,
    endpoint: String,
    admin_token: Option<String>,
    admin_token_stdin: bool,
    surface: tritond_client::types::StorageClusterSurface,
    default_region: String,
    display_name: Option<String>,
    json_output: bool,
) -> Result<()> {
    let admin_token = match (admin_token, admin_token_stdin) {
        (Some(t), false) => t,
        (None, true) => read_admin_token_from_stdin()?,
        (Some(_), true) => unreachable!("clap conflicts_with prevents this"),
        (None, false) => {
            anyhow::bail!("admin token required: pass --admin-token or --admin-token-stdin")
        }
    };
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let req = tritond_client::types::NewStorageCluster {
        name,
        endpoint,
        admin_token,
        surface,
        default_region,
        display_name,
    };
    let cluster = client
        .create_storage_cluster()
        .body(req)
        .send()
        .await
        .context("register storage cluster")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&cluster)?);
        return Ok(());
    }
    println!(
        "Registered storage cluster {} (id {}) at {}",
        cluster.name, cluster.id, cluster.endpoint
    );
    Ok(())
}

pub async fn storage_cluster_delete(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    cluster_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_storage_cluster()
        .id(cluster_id)
        .send()
        .await
        .context("delete storage cluster")?;
    println!("Deregistered storage cluster {cluster_id}");
    Ok(())
}

pub async fn storage_cluster_health(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    ident: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let id = resolve_storage_cluster_ident(&client, &ident).await?;
    let cluster = client
        .probe_storage_cluster_health()
        .id(id)
        .send()
        .await
        .context("probe storage cluster health")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&cluster)?);
        return Ok(());
    }
    println!(
        "Probe complete: {} → {} (observed {})",
        cluster.name,
        storage_status_label(&cluster.status),
        cluster
            .last_observed_at
            .as_ref()
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "(no timestamp)".to_string())
    );
    Ok(())
}

fn read_admin_token_from_stdin() -> Result<String> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .context("read admin token from stdin")?;
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("admin token from stdin was empty");
    }
    Ok(trimmed)
}

fn read_secret_from_stdin() -> Result<String> {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .context("read secret access key from stdin")?;
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        anyhow::bail!("secret access key from stdin was empty");
    }
    Ok(trimmed)
}

#[allow(clippy::too_many_arguments)]
pub async fn storage_cluster_set_presigner(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    ident: String,
    s3_endpoint: Option<String>,
    access_key_id: String,
    secret_access_key: Option<String>,
    secret_access_key_stdin: bool,
    json_output: bool,
) -> Result<()> {
    let secret_access_key = match (secret_access_key, secret_access_key_stdin) {
        (Some(s), false) => s,
        (None, true) => read_secret_from_stdin()?,
        (Some(_), true) => unreachable!("clap conflicts_with prevents this"),
        (None, false) => anyhow::bail!(
            "secret access key required: pass --secret-access-key or --secret-access-key-stdin"
        ),
    };
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let id = resolve_storage_cluster_ident(&client, &ident).await?;
    let req = tritond_client::types::SetPresignerRequest {
        s3_endpoint,
        access_key_id,
        secret_access_key,
    };
    let cluster = client
        .set_storage_cluster_presigner()
        .id(id)
        .body(req)
        .send()
        .await
        .context("set storage cluster presigner")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&cluster)?);
        return Ok(());
    }
    println!(
        "Presigner configured for {} (id {})",
        cluster.name, cluster.id
    );
    if let Some(akid) = &cluster.presigner_access_key_id {
        println!("  access_key_id:  {akid}");
    }
    if let Some(ep) = &cluster.s3_endpoint {
        println!("  s3_endpoint:    {ep}");
    }
    Ok(())
}

pub async fn storage_cluster_clear_presigner(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    ident: String,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let id = resolve_storage_cluster_ident(&client, &ident).await?;
    // Empty strings on the wire mean "clear the credentials" — the
    // server-side handler maps empty → None.
    let req = tritond_client::types::SetPresignerRequest {
        s3_endpoint: None,
        access_key_id: String::new(),
        secret_access_key: String::new(),
    };
    client
        .set_storage_cluster_presigner()
        .id(id)
        .body(req)
        .send()
        .await
        .context("clear storage cluster presigner")?;
    println!("Presigner cleared for storage cluster {id}");
    Ok(())
}

// ---------------------------------------------------------------------
// Cluster configuration (`tcadm config ...`)
// ---------------------------------------------------------------------

/// Render the `value` / `default` JSON for the `config` table compactly
/// (strings unquoted, everything else as JSON).
fn config_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "(unset)".to_string(),
        other => other.to_string(),
    }
}

/// Parse a CLI `<value>` argument: a bare token that parses as JSON
/// (e.g. `30`, `true`, `null`) is used as-is; anything else (e.g.
/// `clickhouse`, `http://ch:8123`) is treated as a JSON string.
fn parse_config_value(raw: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(raw)
        .unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

fn print_config_restart_hint(restart_required: bool) {
    if restart_required {
        println!("saved; restart tritond to apply");
    } else {
        println!("saved");
    }
}

pub async fn config_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let entries = client
        .list_config()
        .send()
        .await
        .context("list config")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    if entries.is_empty() {
        println!("(no configuration keys)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<20}  {:<20}  {:<8}  {:<24}  DESCRIPTION",
        "KEY", "VALUE", "DEFAULT", "RESTART", "ENV OVERRIDE"
    );
    for e in entries {
        println!(
            "{:<36}  {:<20}  {:<20}  {:<8}  {:<24}  {}",
            e.key,
            config_scalar(&e.value),
            config_scalar(&e.default),
            if e.restart_required { "yes" } else { "no" },
            e.env_override.as_deref().unwrap_or("-"),
            e.description,
        );
    }
    Ok(())
}

pub async fn config_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    key: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let entry = client
        .get_config()
        .key(&key)
        .send()
        .await
        .context("get config key")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&entry)?);
        return Ok(());
    }
    println!("{} = {}", entry.key, config_scalar(&entry.value));
    println!("  default:     {}", config_scalar(&entry.default));
    println!(
        "  restart:     {}",
        if entry.restart_required { "yes" } else { "no" }
    );
    if let Some(env) = &entry.env_override {
        println!("  env override: {env} (this is shadowing the stored value at boot)");
    }
    println!("  {}", entry.description);
    Ok(())
}

pub async fn config_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    key: String,
    value: String,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let req = tritond_client::types::SetConfigRequest {
        value: parse_config_value(&value),
    };
    let entry = client
        .set_config()
        .key(&key)
        .body(req)
        .send()
        .await
        .context("set config key")?
        .into_inner();
    println!("{} = {}", entry.key, config_scalar(&entry.value));
    print_config_restart_hint(entry.restart_required);
    if let Some(env) = &entry.env_override {
        println!("note: {env} is set in tritond's environment and will shadow this value at boot");
    }
    Ok(())
}

pub async fn config_reset(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    key: String,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let entry = client
        .reset_config()
        .key(&key)
        .send()
        .await
        .context("reset config key")?
        .into_inner();
    println!("{} = {} (default)", entry.key, config_scalar(&entry.value));
    print_config_restart_hint(entry.restart_required);
    Ok(())
}

use tritond_client::types::{MetaProvenance, MetaScope, RealizedMetaEntry, SetMetaRequest};

fn meta_scope_label(scope: &MetaScope) -> &'static str {
    match scope {
        MetaScope::Silo => "silo",
        MetaScope::Tenant => "tenant",
        MetaScope::Project => "project",
        MetaScope::Instance => "instance",
    }
}

fn provenance_label(p: &MetaProvenance) -> &'static str {
    match p {
        MetaProvenance::Silo => "silo",
        MetaProvenance::Tenant => "tenant",
        MetaProvenance::Project => "project",
        MetaProvenance::Instance => "instance",
        MetaProvenance::System => "system",
    }
}

/// Parse a `--value` CLI string into a JSON value. A bare string
/// that isn't valid JSON is wrapped as a JSON string so operators
/// can `--value foo` without quoting.
fn parse_meta_value(raw: &str) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v) => v,
        Err(_) => serde_json::Value::String(raw.to_string()),
    }
}

pub async fn meta_list(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    scope: MetaScope,
    scope_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let entries = client
        .list_meta()
        .scope(scope.clone())
        .scope_id(scope_id)
        .send()
        .await
        .context("list_meta")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    if entries.is_empty() {
        println!("(no metadata at {} {})", meta_scope_label(&scope), scope_id);
        return Ok(());
    }
    println!(
        "{} entries at {} {}:",
        entries.len(),
        meta_scope_label(&scope),
        scope_id
    );
    for entry in entries {
        let visible = if entry.guest_visible { "yes" } else { "no" };
        let writable = if entry.guest_writable { "yes" } else { "no" };
        let val = serde_json::to_string(&entry.value).unwrap_or_else(|_| "<bad-json>".into());
        println!(
            "  {:<40}  visible={visible:<3}  writable={writable:<3}  by={}  at={}",
            entry.key, entry.updated_by, entry.updated_at
        );
        println!("    {}", val);
    }
    Ok(())
}

pub async fn meta_get(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    scope: MetaScope,
    scope_id: Uuid,
    key: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let entry = client
        .get_meta()
        .scope(scope.clone())
        .scope_id(scope_id)
        .key(&key)
        .send()
        .await
        .context("get_meta")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&entry)?);
        return Ok(());
    }
    let val = serde_json::to_string_pretty(&entry.value).unwrap_or_else(|_| "<bad-json>".into());
    println!("key:            {}", entry.key);
    println!("guest_visible:  {}", entry.guest_visible);
    println!("guest_writable: {}", entry.guest_writable);
    println!("updated_by:     {}", entry.updated_by);
    println!("updated_at:     {}", entry.updated_at);
    println!("value:");
    for line in val.lines() {
        println!("  {line}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn meta_set(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    scope: MetaScope,
    scope_id: Uuid,
    key: String,
    value: String,
    guest_visible: Option<bool>,
    guest_writable: bool,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let body = SetMetaRequest {
        value: parse_meta_value(&value),
        guest_visible,
        guest_writable: Some(guest_writable),
    };
    let response = client
        .set_meta()
        .scope(scope.clone())
        .scope_id(scope_id)
        .key(&key)
        .body(body)
        .send()
        .await
        .context("set_meta")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(());
    }
    println!(
        "set {} at {} {}",
        response.entry.key,
        meta_scope_label(&scope),
        scope_id
    );
    println!("  generation:     {}", response.generation);
    println!("  guest_visible:  {}", response.entry.guest_visible);
    println!("  guest_writable: {}", response.entry.guest_writable);
    Ok(())
}

pub async fn meta_unset(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    scope: MetaScope,
    scope_id: Uuid,
    key: String,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_meta()
        .scope(scope.clone())
        .scope_id(scope_id)
        .key(&key)
        .send()
        .await
        .context("delete_meta")?;
    println!(
        "deleted {} from {} {}",
        key,
        meta_scope_label(&scope),
        scope_id
    );
    Ok(())
}

pub async fn meta_realized(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let entries = client
        .get_instance_realized_meta()
        .instance_id(instance_id)
        .send()
        .await
        .context("get_instance_realized_meta")?
        .into_inner();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    if entries.is_empty() {
        println!("(no realized metadata for instance {})", instance_id);
        return Ok(());
    }
    println!(
        "realized metadata for instance {} ({} entries):",
        instance_id,
        entries.len()
    );
    println!(
        "  {:<40}  {:<10}  {:<8}  {:<8}  VALUE",
        "KEY", "FROM", "VISIBLE", "WRITABLE"
    );
    for RealizedMetaEntry { key, value, from } in entries {
        let val = serde_json::to_string(&value.value).unwrap_or_else(|_| "<bad-json>".into());
        let visible = if value.guest_visible { "yes" } else { "no" };
        let writable = if value.guest_writable { "yes" } else { "no" };
        println!(
            "  {:<40}  {:<10}  {:<8}  {:<8}  {}",
            key,
            provenance_label(&from),
            visible,
            writable,
            val,
        );
    }
    Ok(())
}

pub async fn instance_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant: Option<Uuid>,
    project: Option<Uuid>,
    image: Option<Uuid>,
    cn: Option<Uuid>,
    state: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_instances_v1();
    if let Some(t) = tenant {
        req = req.tenant(t);
    }
    if let Some(p) = project {
        req = req.project(p);
    }
    if let Some(i) = image {
        req = req.image(i);
    }
    if let Some(c) = cn {
        req = req.cn(c);
    }
    if let Some(s) = state {
        req = req.state(s);
    }
    let page = req.send().await.context("/v1/instances list")?.into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no instances)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  {:<10}  IMAGE", "ID", "NAME", "LIFECYCLE");
    for inst in &page.items {
        println!(
            "{:<36}  {:<24}  {:<10}  {}",
            inst.id,
            inst.name,
            format!("{:?}", inst.lifecycle),
            inst.image_id,
        );
    }
    Ok(())
}

pub async fn instance_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    instance_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let inst = client
        .get_instance_v1()
        .instance_id(instance_id)
        .send()
        .await
        .context("/v1/instances/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&inst)?);
        return Ok(());
    }
    print_instance(&inst);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn instance_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant: Uuid,
    project: Uuid,
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
    let inst = client
        .create_instance_v1()
        .tenant(tenant)
        .project(project)
        .body(tritond_client::types::NewInstance {
            name,
            description: Some(description),
            image_id,
            primary_subnet_id,
            ssh_key_ids,
            cpu,
            memory_bytes,
            // Multi-NIC is not yet surfaced on the CLI; build the
            // JSON body directly via curl if you need extra NICs.
            extra_nics: Vec::new(),
            mac: None,
        })
        .send()
        .await
        .context("/v1/instances create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&inst)?);
        return Ok(());
    }
    print_instance(&inst);
    Ok(())
}

/// `tcadm instance delete <instance_id> [--force]`.
pub async fn instance_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    instance_id: Uuid,
    force: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_instance_v1()
        .instance_id(instance_id)
        .force(force)
        .send()
        .await
        .context("/v1/instances/{id} delete")?;
    println!("Instance {instance_id} deleted.");
    Ok(())
}

/// `tcadm instance {start,stop,restart} <instance_id>`.
pub async fn instance_lifecycle_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    instance_id: Uuid,
    action: &str,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let inst = match action {
        "start" => client
            .start_instance_v1()
            .instance_id(instance_id)
            .send()
            .await
            .context("/v1/instances/{id}/start")?
            .into_inner(),
        "stop" => client
            .stop_instance_v1()
            .instance_id(instance_id)
            .send()
            .await
            .context("/v1/instances/{id}/stop")?
            .into_inner(),
        "restart" => client
            .restart_instance_v1()
            .instance_id(instance_id)
            .send()
            .await
            .context("/v1/instances/{id}/restart")?
            .into_inner(),
        other => anyhow::bail!("unknown lifecycle action: {other}"),
    };
    if json_output {
        println!("{}", serde_json::to_string_pretty(&inst)?);
        return Ok(());
    }
    print_instance(&inst);
    Ok(())
}

/// `tcadm system instances [--image=&cn=...]` -> fleet-wide search.
pub async fn system_instances_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    image: Option<Uuid>,
    cn: Option<Uuid>,
    silo: Option<Uuid>,
    tenant: Option<Uuid>,
    project: Option<Uuid>,
    state: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_system_instances_v1();
    if let Some(i) = image {
        req = req.image(i);
    }
    if let Some(c) = cn {
        req = req.cn(c);
    }
    if let Some(s) = silo {
        req = req.silo(s);
    }
    if let Some(t) = tenant {
        req = req.tenant(t);
    }
    if let Some(p) = project {
        req = req.project(p);
    }
    if let Some(st) = state {
        req = req.state(st);
    }
    let page = req
        .send()
        .await
        .context("/v1/system/instances list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no instances)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<10}  {:<36}  TENANT/PROJECT",
        "ID", "NAME", "LIFECYCLE", "IMAGE"
    );
    for inst in &page.items {
        println!(
            "{:<36}  {:<24}  {:<10}  {:<36}  {}/{}",
            inst.id,
            inst.name,
            format!("{:?}", inst.lifecycle),
            inst.image_id,
            inst.tenant_id,
            inst.project_id,
        );
    }
    Ok(())
}

/// `tcadm system nics [--ip=&subnet=&instance=]` -> fleet NIC search.
pub async fn system_nics_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    ip: Option<std::net::IpAddr>,
    subnet: Option<Uuid>,
    instance: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_system_nics_v1();
    if let Some(i) = ip {
        req = req.ip(i);
    }
    if let Some(s) = subnet {
        req = req.subnet(s);
    }
    if let Some(inst) = instance {
        req = req.instance(inst);
    }
    let page = req
        .send()
        .await
        .context("/v1/system/networking/nics list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no nics)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<17}  {:<15}  INSTANCE",
        "NIC_ID", "NAME", "MAC", "IPV4"
    );
    for nic in &page.items {
        println!(
            "{:<36}  {:<24}  {:<17}  {:<15}  {}",
            nic.id,
            nic.name,
            nic.mac,
            nic.primary_ipv4
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string()),
            nic.instance_id,
        );
    }
    Ok(())
}

/// `tcadm system cns [--state=...]` -> fleet CN inventory via the
/// `/v1/system/cns` operator endpoint. Capability: `SystemRead`.
pub async fn system_cns_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    state: Option<tritond_client::types::CnState>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_system_cns_v1();
    if let Some(s) = state {
        req = req.state(s);
    }
    let page = req
        .send()
        .await
        .context("/v1/system/cns list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no compute nodes)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<9}  ROLE",
        "SERVER_UUID", "HOSTNAME", "STATE"
    );
    for cn in &page.items {
        println!(
            "{:<36}  {:<24}  {:<9}  {:?}",
            cn.server_uuid, cn.hostname, cn.state, cn.role,
        );
    }
    Ok(())
}

/// Parse a CLI capability string into the wire enum. Accepts both
/// kebab-case (`system-read`) and the Rust variant name
/// (`SystemRead`) so operators don't have to remember which.
fn parse_capability(s: &str) -> Result<tritond_client::types::Capability> {
    use tritond_client::types::Capability;
    Ok(match s {
        "system-read" | "SystemRead" => Capability::SystemRead,
        "system-operate" | "SystemOperate" => Capability::SystemOperate,
        "system-config-write" | "SystemConfigWrite" => Capability::SystemConfigWrite,
        "storage-admin" | "StorageAdmin" => Capability::StorageAdmin,
        other => bail!(
            "unknown capability {other:?}; expected one of: system-read, \
             system-operate, system-config-write, storage-admin"
        ),
    })
}

/// `tcadm system user-grant <user_id> <capability>`.
pub async fn system_user_grant_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    user_id: Uuid,
    capability: String,
) -> Result<()> {
    let cap = parse_capability(&capability)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let view = client
        .grant_user_capability_v1()
        .user_id(user_id)
        .capability(cap)
        .send()
        .await
        .context("/v1/system/users/{user}/capabilities/{cap} grant")?
        .into_inner();
    println!(
        "Granted {capability} to {user_id}. User now carries {} capabilit{}.",
        view.capabilities.len(),
        if view.capabilities.len() == 1 {
            "y"
        } else {
            "ies"
        },
    );
    for c in &view.capabilities {
        println!("  - {c:?}");
    }
    Ok(())
}

/// Parses `what` in priority order (UUID > IP > MAC > name), fires
/// the matching /v1/system/* list queries in parallel, and renders
/// the union. No server-side `/v1/system/find` endpoint exists by
/// design.
pub async fn find_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    what: String,
    kind_hint: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;

    #[derive(serde::Serialize, Default)]
    struct FindResult {
        instances: Vec<tritond_client::types::Instance>,
        nics: Vec<tritond_client::types::Nic>,
    }
    let mut out = FindResult::default();

    // Priority dispatch: UUID, then IP, then explicit --kind=.
    // MAC parsing comes when /v1/system/networking/nics gains a
    // mac= selector (today's surface only accepts ip=).
    if let Ok(uuid) = Uuid::parse_str(&what) {
        // A UUID could be an instance id, image id, cn id, nic id,
        // etc. Try the common operator queries in parallel.
        let inst_by_image = client.list_system_instances_v1().image(uuid).send();
        let inst_by_cn = client.list_system_instances_v1().cn(uuid).send();
        let (by_image, by_cn) = tokio::join!(inst_by_image, inst_by_cn);
        if let Ok(r) = by_image {
            out.instances.extend(r.into_inner().items);
        }
        if let Ok(r) = by_cn {
            out.instances.extend(r.into_inner().items);
        }
        // Also try a direct instance lookup in case the UUID is
        // itself an instance id.
        if let Ok(inst) = client.get_instance_v1().instance_id(uuid).send().await {
            out.instances.push(inst.into_inner());
        }
        // Dedupe by id (an instance hit via image AND cn would
        // appear twice).
        let mut seen = std::collections::HashSet::new();
        out.instances.retain(|i| seen.insert(i.id));
    } else if let Ok(ip) = what.parse::<std::net::IpAddr>() {
        let r = client
            .list_system_nics_v1()
            .ip(ip)
            .send()
            .await
            .context("/v1/system/networking/nics?ip=")?;
        out.nics.extend(r.into_inner().items);
    } else {
        let _ = kind_hint;
        bail!(
            "freeform input {what:?} did not parse as UUID or IP; \
             name-based search requires `--kind=` and lands in a future slice"
        );
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!("== Instances ({}) ==", out.instances.len());
    for inst in &out.instances {
        println!(
            "  {}  {:<24}  {:?}  image={}",
            inst.id, inst.name, inst.lifecycle, inst.image_id,
        );
    }
    println!("== NICs ({}) ==", out.nics.len());
    for nic in &out.nics {
        println!(
            "  {}  {:<24}  {:<17}  {:<15}  instance={}",
            nic.id,
            nic.name,
            nic.mac,
            nic.primary_ipv4
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string()),
            nic.instance_id,
        );
    }
    Ok(())
}

fn parse_image_scope(s: &str) -> Result<tritond_client::types::ImageScopeSelector> {
    use tritond_client::types::ImageScopeSelector;
    Ok(match s {
        "public" | "Public" => ImageScopeSelector::Public,
        "silo" | "Silo" => ImageScopeSelector::Silo,
        "tenant" | "Tenant" => ImageScopeSelector::Tenant,
        "project" | "Project" => ImageScopeSelector::Project,
        "user" | "User" => ImageScopeSelector::User,
        other => {
            bail!("unknown scope {other:?}; expected one of: public, silo, tenant, project, user")
        }
    })
}

pub async fn image_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    scope: String,
    json_output: bool,
) -> Result<()> {
    let scope_sel = parse_image_scope(&scope)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_images_v1()
        .scope(scope_sel)
        .send()
        .await
        .context("/v1/images list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no images)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  {:<10}  OS", "ID", "NAME", "VERSION");
    for img in &page.items {
        println!(
            "{:<36}  {:<24}  {:<10}  {}",
            img.id, img.name, img.version, img.os
        );
    }
    Ok(())
}

pub async fn image_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    image_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let img = client
        .get_image_v1()
        .image_id(image_id)
        .send()
        .await
        .context("/v1/images/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&img)?);
        return Ok(());
    }
    println!("Image {}", img.id);
    println!("  name:    {}", img.name);
    println!("  os:      {}", img.os);
    println!("  version: {}", img.version);
    println!("  size:    {} bytes", img.size_bytes);
    println!("  sha256:  {}", img.sha256);
    Ok(())
}

pub async fn ssh_key_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    scope: String,
    json_output: bool,
) -> Result<()> {
    let scope_sel = parse_image_scope(&scope)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_ssh_keys_v1()
        .scope(scope_sel)
        .send()
        .await
        .context("/v1/ssh-keys list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no ssh keys)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  FINGERPRINT", "ID", "NAME");
    for k in &page.items {
        println!("{:<36}  {:<24}  {}", k.id, k.name, k.fingerprint);
    }
    Ok(())
}

pub async fn ssh_key_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    key_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let k = client
        .get_ssh_key_v1()
        .key_id(key_id)
        .send()
        .await
        .context("/v1/ssh-keys/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&k)?);
        return Ok(());
    }
    println!("SshKey {}", k.id);
    println!("  name:        {}", k.name);
    println!("  fingerprint: {}", k.fingerprint);
    Ok(())
}

pub async fn disk_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    instance: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_disks_v1()
        .instance(instance)
        .send()
        .await
        .context("/v1/disks list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no disks)");
        return Ok(());
    }
    println!("{:<36}  KIND  SIZE_BYTES", "ID");
    for d in &page.items {
        println!("{:<36}  {:?}  {}", d.id, d.kind, d.size_bytes);
    }
    Ok(())
}

pub async fn disk_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    disk_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let d = client
        .get_disk_v1()
        .disk_id(disk_id)
        .send()
        .await
        .context("/v1/disks/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&d)?);
        return Ok(());
    }
    println!("Disk {}", d.id);
    println!("  kind:       {:?}", d.kind);
    println!("  size_bytes: {}", d.size_bytes);
    println!("  instance:   {}", d.instance_id);
    Ok(())
}

pub async fn nic_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    ip: Option<std::net::IpAddr>,
    subnet: Option<Uuid>,
    instance: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_nics_v1();
    if let Some(i) = ip {
        req = req.ip(i);
    }
    if let Some(s) = subnet {
        req = req.subnet(s);
    }
    if let Some(inst) = instance {
        req = req.instance(inst);
    }
    let page = req.send().await.context("/v1/nics list")?.into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no nics)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  {:<17}  IP", "ID", "NAME", "MAC");
    for n in &page.items {
        println!(
            "{:<36}  {:<24}  {:<17}  {}",
            n.id,
            n.name,
            n.mac,
            n.primary_ipv4
                .map(|i| i.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
    }
    Ok(())
}

pub async fn nic_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    nic_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let n = client
        .get_nic_v1()
        .nic_id(nic_id)
        .send()
        .await
        .context("/v1/nics/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&n)?);
        return Ok(());
    }
    println!("Nic {}", n.id);
    println!("  name:     {}", n.name);
    println!("  mac:      {}", n.mac);
    println!("  ipv4:     {:?}", n.primary_ipv4);
    println!("  ipv6:     {:?}", n.primary_ipv6);
    println!("  instance: {}", n.instance_id);
    println!("  subnet:   {}", n.subnet_id);
    Ok(())
}

pub async fn vpc_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant: Uuid,
    project: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_vpcs_v1()
        .tenant(tenant)
        .project(project)
        .send()
        .await
        .context("/v1/vpcs list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no vpcs)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  VNI", "ID", "NAME");
    for v in &page.items {
        println!("{:<36}  {:<24}  {}", v.id, v.name, v.vni);
    }
    Ok(())
}

pub async fn vpc_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let v = client
        .get_vpc_v1()
        .vpc_id(vpc_id)
        .send()
        .await
        .context("/v1/vpcs/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("Vpc {}", v.id);
    println!("  name:    {}", v.name);
    println!("  vni:     {}", v.vni);
    println!("  tenant:  {}", v.tenant_id);
    println!("  project: {}", v.project_id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn vpc_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant: Uuid,
    project: Uuid,
    name: String,
    description: String,
    ipv4_block: Option<String>,
    ipv6_block: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let v = client
        .create_vpc_v1()
        .tenant(tenant)
        .project(project)
        .body(tritond_client::types::NewVpc {
            name,
            description: Some(description),
            ipv4_block,
            ipv6_block,
        })
        .send()
        .await
        .context("/v1/vpcs create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!("Vpc {}", v.id);
    println!("  name:    {}", v.name);
    println!("  vni:     {}", v.vni);
    println!("  tenant:  {}", v.tenant_id);
    println!("  project: {}", v.project_id);
    Ok(())
}

/// `tcadm vpc delete <vpc_id>`.
pub async fn vpc_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_vpc_v1()
        .vpc_id(vpc_id)
        .send()
        .await
        .context("/v1/vpcs/{id} delete")?;
    println!("Vpc {vpc_id} deleted.");
    Ok(())
}

pub async fn subnet_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_subnets_v1()
        .vpc(vpc)
        .send()
        .await
        .context("/v1/subnets list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no subnets)");
        return Ok(());
    }
    println!("{:<36}  {:<24}", "ID", "NAME");
    for s in &page.items {
        println!("{:<36}  {:<24}", s.id, s.name);
    }
    Ok(())
}

pub async fn subnet_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    subnet_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let s = client
        .get_subnet_v1()
        .subnet_id(subnet_id)
        .send()
        .await
        .context("/v1/subnets/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    println!("Subnet {}", s.id);
    println!("  name:   {}", s.name);
    println!("  vpc:    {}", s.vpc_id);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn subnet_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Uuid,
    name: String,
    description: String,
    ipv4_block: Option<String>,
    ipv6_block: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let s = client
        .create_subnet_v1()
        .vpc(vpc)
        .body(tritond_client::types::NewSubnet {
            name,
            description: Some(description),
            ipv4_block,
            ipv6_block,
        })
        .send()
        .await
        .context("/v1/subnets create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&s)?);
        return Ok(());
    }
    println!("Subnet {}", s.id);
    println!("  name: {}", s.name);
    println!("  vpc:  {}", s.vpc_id);
    Ok(())
}

/// `tcadm subnet delete <subnet_id>`.
pub async fn subnet_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    subnet_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_subnet_v1()
        .subnet_id(subnet_id)
        .send()
        .await
        .context("/v1/subnets/{id} delete")?;
    println!("Subnet {subnet_id} deleted.");
    Ok(())
}

pub async fn floating_ip_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant: Uuid,
    project: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_floating_ips_v1()
        .tenant(tenant)
        .project(project)
        .send()
        .await
        .context("/v1/floating-ips list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no floating-ips)");
        return Ok(());
    }
    println!("{:<36}  ADDRESS   ATTACHED_TO", "ID");
    for f in &page.items {
        println!(
            "{:<36}  {}  {}",
            f.id,
            f.address,
            f.attached_to
                .as_ref()
                .map(|a| format!("nic={}", a.nic_id))
                .unwrap_or_else(|| "-".to_string()),
        );
    }
    Ok(())
}

pub async fn floating_ip_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    floating_ip_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let f = client
        .get_floating_ip_v1()
        .floating_ip_id(floating_ip_id)
        .send()
        .await
        .context("/v1/floating-ips/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&f)?);
        return Ok(());
    }
    println!("FloatingIp {}", f.id);
    println!("  address:     {}", f.address);
    println!("  attached_to: {:?}", f.attached_to);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn floating_ip_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    tenant: Uuid,
    project: Uuid,
    name: String,
    description: String,
    family: String,
    json_output: bool,
) -> Result<()> {
    let family = match family.as_str() {
        "ipv4" | "v4" => tritond_client::types::AddressFamily::V4,
        "ipv6" | "v6" => tritond_client::types::AddressFamily::V6,
        other => anyhow::bail!("unknown family {other}; expected ipv4 or ipv6"),
    };
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let f = client
        .create_floating_ip_v1()
        .tenant(tenant)
        .project(project)
        .body(tritond_client::types::NewFloatingIp {
            name,
            description: Some(description),
            family,
        })
        .send()
        .await
        .context("/v1/floating-ips create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&f)?);
        return Ok(());
    }
    println!("FloatingIp {}", f.id);
    println!("  name:    {}", f.name);
    println!("  address: {}", f.address);
    Ok(())
}

/// `tcadm floating-ip delete <floating_ip_id>`.
pub async fn floating_ip_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    floating_ip_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_floating_ip_v1()
        .floating_ip_id(floating_ip_id)
        .send()
        .await
        .context("/v1/floating-ips/{id} delete")?;
    println!("Floating IP {floating_ip_id} deleted.");
    Ok(())
}

pub async fn floating_ip_attach_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    floating_ip_id: Uuid,
    nic_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let f = client
        .attach_floating_ip_v1()
        .floating_ip_id(floating_ip_id)
        .body(tritond_client::types::AttachFloatingIpRequest { nic_id })
        .send()
        .await
        .context("/v1/floating-ips/{id}/attach")?
        .into_inner();
    println!(
        "Floating IP {} attached to NIC {} (address {}).",
        f.id, nic_id, f.address
    );
    Ok(())
}

pub async fn floating_ip_detach_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    floating_ip_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let f = client
        .detach_floating_ip_v1()
        .floating_ip_id(floating_ip_id)
        .send()
        .await
        .context("/v1/floating-ips/{id}/detach")?
        .into_inner();
    println!("Floating IP {} detached (address {}).", f.id, f.address);
    Ok(())
}

pub async fn system_images_using_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    image_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_system_image_instances_v1()
        .image_id(image_id)
        .send()
        .await
        .context("/v1/system/images/{image}/instances list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    println!("Instances using image {image_id} ({}):", page.items.len());
    for inst in &page.items {
        println!(
            "  {}  {:<24}  {:?}  tenant={} project={}",
            inst.id, inst.name, inst.lifecycle, inst.tenant_id, inst.project_id
        );
    }
    Ok(())
}

/// `tcadm system cn-instances <cn_id>` -> `/v1/system/cns/{cn}/instances`.
pub async fn system_cn_instances_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    cn_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let page = client
        .list_system_cn_instances_v1()
        .cn_id(cn_id)
        .send()
        .await
        .context("/v1/system/cns/{cn}/instances list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    println!("Instances on CN {cn_id} ({}):", page.items.len());
    for inst in &page.items {
        println!(
            "  {}  {:<24}  {:?}  image={}",
            inst.id, inst.name, inst.lifecycle, inst.image_id
        );
    }
    Ok(())
}

/// `tcadm system utilization` -> `/v1/system/utilization/silos`. Returns
/// the locked 501 UtilizationUnavailable today; the surface is
/// reserved for the future implementation.
pub async fn system_utilization_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    match client.get_system_utilization_silos_v1().send().await {
        Ok(r) => {
            let silos = r.into_inner();
            if json_output {
                println!("{}", serde_json::to_string_pretty(&silos)?);
            } else {
                println!("Utilization (silos: {}):", silos.len());
                for s in &silos {
                    println!("  {}  {}", s.id, s.name);
                }
            }
        }
        Err(e) => {
            // Show the operator the 501 message verbatim so they
            // see the "not implemented yet" body the server returns.
            println!("{e}");
        }
    }
    Ok(())
}

/// Bare-MAC lease lookup across every VPC. Auth recovers the owning
/// tenant from the lease row.
pub async fn dhcp_lease_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    mac: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let lease = client
        .get_dhcp_lease_v1()
        .mac(mac)
        .send()
        .await
        .context("/v1/vpc-dhcp-leases/{mac} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&lease)?);
        return Ok(());
    }
    println!("DhcpLease {}", lease.mac);
    println!("  vpc:        {}", lease.vpc_id);
    println!("  ipv4:       {}", lease.ipv4);
    println!("  instance:   {}", lease.instance_id);
    println!("  nic:        {}", lease.nic_id);
    println!("  created_at: {}", lease.created_at);
    Ok(())
}

/// `tcadm system user-revoke <user_id> <capability>`.
pub async fn system_user_revoke_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    user_id: Uuid,
    capability: String,
) -> Result<()> {
    let cap = parse_capability(&capability)?;
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .revoke_user_capability_v1()
        .user_id(user_id)
        .capability(cap)
        .send()
        .await
        .context("/v1/system/users/{user}/capabilities/{cap} revoke")?;
    println!("Revoked {capability} from {user_id}.");
    Ok(())
}

// List endpoints accept silo/tenant/project/vpc as optional scope
// selectors; the server enforces that at least one is present.

pub async fn firewall_rule_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Option<Uuid>,
    project: Option<Uuid>,
    tenant: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_firewall_rules_v1();
    if let Some(v) = vpc {
        req = req.vpc(v);
    }
    if let Some(p) = project {
        req = req.project(p);
    }
    if let Some(t) = tenant {
        req = req.tenant(t);
    }
    let page = req
        .send()
        .await
        .context("/v1/firewall-rules list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no firewall rules)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<24}  {:<8}  {:<6}  PRIO  DEST",
        "ID", "NAME", "DIR", "ACTION"
    );
    for r in &page.items {
        println!(
            "{:<36}  {:<24}  {:<8?}  {:<6?}  {:>4}  {}",
            r.id,
            r.name,
            r.direction,
            r.action,
            r.priority,
            r.destination_cidr.as_deref().unwrap_or("any"),
        );
    }
    Ok(())
}

/// `tcadm firewall-rule show <id>`.
pub async fn firewall_rule_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    firewall_rule_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let r = client
        .get_firewall_rule_v1()
        .firewall_rule_id(firewall_rule_id)
        .send()
        .await
        .context("/v1/firewall-rules/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("FirewallRule {}", r.id);
    println!("  name:        {}", r.name);
    println!("  description: {}", r.description);
    println!("  vpc:         {}", r.vpc_id);
    println!("  project:     {}", r.project_id);
    println!("  direction:   {:?}", r.direction);
    println!("  action:      {:?}", r.action);
    println!("  priority:    {}", r.priority);
    println!("  protocol:    {:?}", r.protocol);
    println!(
        "  source:      {}",
        r.source_cidr.as_deref().unwrap_or("any")
    );
    println!(
        "  destination: {}",
        r.destination_cidr.as_deref().unwrap_or("any")
    );
    Ok(())
}

/// `tcadm nat-gateway list --vpc=<uuid> [--project=<uuid>] [--tenant=<uuid>]`.
pub async fn nat_gateway_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Option<Uuid>,
    project: Option<Uuid>,
    tenant: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_nat_gateways_v1();
    if let Some(v) = vpc {
        req = req.vpc(v);
    }
    if let Some(p) = project {
        req = req.project(p);
    }
    if let Some(t) = tenant {
        req = req.tenant(t);
    }
    let page = req
        .send()
        .await
        .context("/v1/nat-gateways list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no nat gateways)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  PUBLIC_ADDR", "ID", "NAME");
    for g in &page.items {
        println!("{:<36}  {:<24}  {}", g.id, g.name, g.public_address);
    }
    Ok(())
}

/// `tcadm nat-gateway show <id>`.
pub async fn nat_gateway_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    nat_gateway_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let g = client
        .get_nat_gateway_v1()
        .nat_gateway_id(nat_gateway_id)
        .send()
        .await
        .context("/v1/nat-gateways/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&g)?);
        return Ok(());
    }
    println!("NatGateway {}", g.id);
    println!("  name:           {}", g.name);
    println!("  vpc:            {}", g.vpc_id);
    println!("  project:        {}", g.project_id);
    println!("  public_address: {}", g.public_address);
    println!("  family:         {:?}", g.family);
    println!("  desired_gen:    {}", g.desired_generation);
    Ok(())
}

/// `tcadm route-table list --vpc=<uuid> [--project=<uuid>] [--tenant=<uuid>]`.
pub async fn route_table_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Option<Uuid>,
    project: Option<Uuid>,
    tenant: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_route_tables_v1();
    if let Some(v) = vpc {
        req = req.vpc(v);
    }
    if let Some(p) = project {
        req = req.project(p);
    }
    if let Some(t) = tenant {
        req = req.tenant(t);
    }
    let page = req
        .send()
        .await
        .context("/v1/route-tables list")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no route tables)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  MAIN", "ID", "NAME");
    for rt in &page.items {
        println!(
            "{:<36}  {:<24}  {}",
            rt.id,
            rt.name,
            if rt.is_main { "yes" } else { "no" }
        );
    }
    Ok(())
}

/// `tcadm route-table show <id>`.
pub async fn route_table_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    route_table_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let rt = client
        .get_route_table_v1()
        .route_table_id(route_table_id)
        .send()
        .await
        .context("/v1/route-tables/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&rt)?);
        return Ok(());
    }
    println!("RouteTable {}", rt.id);
    println!("  name:    {}", rt.name);
    println!("  vpc:     {}", rt.vpc_id);
    println!("  project: {}", rt.project_id);
    println!("  is_main: {}", rt.is_main);
    Ok(())
}

/// `tcadm route list --route-table=<uuid> [--project=<uuid>] [--tenant=<uuid>]`.
pub async fn route_list_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    route_table: Option<Uuid>,
    project: Option<Uuid>,
    tenant: Option<Uuid>,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let mut req = client.list_routes_v1();
    if let Some(rt) = route_table {
        req = req.route_table(rt);
    }
    if let Some(p) = project {
        req = req.project(p);
    }
    if let Some(t) = tenant {
        req = req.tenant(t);
    }
    let page = req.send().await.context("/v1/routes list")?.into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no routes)");
        return Ok(());
    }
    println!("{:<36}  {:<24}  {:<18}  TARGET", "ID", "NAME", "DEST");
    for r in &page.items {
        println!(
            "{:<36}  {:<24}  {:<18}  {:?}",
            r.id, r.name, r.destination, r.target
        );
    }
    Ok(())
}

/// `tcadm route show <id>`.
pub async fn route_show_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    route_id: Uuid,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let r = client
        .get_route_v1()
        .route_id(route_id)
        .send()
        .await
        .context("/v1/routes/{id} get")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("Route {}", r.id);
    println!("  name:        {}", r.name);
    println!("  description: {}", r.description);
    println!("  vpc:         {}", r.vpc_id);
    println!("  route_table: {}", r.route_table_id);
    println!("  destination: {}", r.destination);
    println!("  target:      {:?}", r.target);
    Ok(())
}

fn parse_port_range(s: &str) -> Result<tritond_client::types::FirewallPortRange> {
    // Accept "low-high" or a single "n" (treated as low=high=n).
    let (low_s, high_s) = match s.split_once('-') {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (s.trim(), s.trim()),
    };
    let low: u16 = low_s
        .parse()
        .with_context(|| format!("port range low `{low_s}` is not a u16"))?;
    let high: u16 = high_s
        .parse()
        .with_context(|| format!("port range high `{high_s}` is not a u16"))?;
    if low > high {
        anyhow::bail!("port range low ({low}) > high ({high})");
    }
    Ok(tritond_client::types::FirewallPortRange { low, high })
}

/// `tcadm firewall-rule create --vpc=<uuid> --name=X --action=allow ...`.
#[allow(clippy::too_many_arguments)]
pub async fn firewall_rule_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Uuid,
    name: String,
    description: String,
    action: String,
    direction: String,
    protocol: String,
    priority: u16,
    source_cidr: Option<String>,
    destination_cidr: Option<String>,
    source_ports: Option<String>,
    destination_ports: Option<String>,
    json_output: bool,
) -> Result<()> {
    use tritond_client::types::{
        FirewallAction, FirewallDirection, FirewallProtocol, NewFirewallRule,
    };

    let action = match action.to_ascii_lowercase().as_str() {
        "allow" => FirewallAction::Allow,
        "deny" => FirewallAction::Deny,
        other => anyhow::bail!("unknown action `{other}`; expected allow or deny"),
    };
    let direction = match direction.to_ascii_lowercase().as_str() {
        "inbound" | "in" => FirewallDirection::Inbound,
        "outbound" | "out" => FirewallDirection::Outbound,
        other => anyhow::bail!("unknown direction `{other}`; expected inbound or outbound"),
    };
    let protocol = match protocol.to_ascii_lowercase().as_str() {
        "any" => FirewallProtocol::Any,
        "tcp" => FirewallProtocol::Tcp,
        "udp" => FirewallProtocol::Udp,
        "icmp4" | "icmp" => FirewallProtocol::Icmp4,
        "icmp6" => FirewallProtocol::Icmp6,
        other => anyhow::bail!("unknown protocol `{other}`"),
    };

    let source_ports = source_ports.as_deref().map(parse_port_range).transpose()?;
    let destination_ports = destination_ports
        .as_deref()
        .map(parse_port_range)
        .transpose()?;

    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let r = client
        .create_firewall_rule_v1()
        .vpc(vpc)
        .body(NewFirewallRule {
            name,
            description: Some(description),
            action,
            direction,
            protocol,
            priority,
            source_cidr,
            destination_cidr,
            source_ports,
            destination_ports,
            icmp_type_code: None,
        })
        .send()
        .await
        .context("/v1/firewall-rules create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("FirewallRule {}", r.id);
    println!("  name:      {}", r.name);
    println!("  vpc:       {}", r.vpc_id);
    println!("  action:    {}", r.action);
    println!("  direction: {}", r.direction);
    println!("  protocol:  {}", r.protocol);
    println!("  priority:  {}", r.priority);
    Ok(())
}

/// `tcadm firewall-rule delete <id>`.
pub async fn firewall_rule_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    firewall_rule_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_firewall_rule_v1()
        .firewall_rule_id(firewall_rule_id)
        .send()
        .await
        .context("/v1/firewall-rules/{id} delete")?;
    println!("FirewallRule {firewall_rule_id} deleted.");
    Ok(())
}

/// `tcadm nat-gateway create --vpc=<uuid> --name=X --family=ipv4`.
pub async fn nat_gateway_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Uuid,
    name: String,
    description: String,
    family: String,
    json_output: bool,
) -> Result<()> {
    let family = match family.to_ascii_lowercase().as_str() {
        "ipv4" | "v4" => tritond_client::types::AddressFamily::V4,
        "ipv6" | "v6" => tritond_client::types::AddressFamily::V6,
        other => anyhow::bail!("unknown family `{other}`; expected ipv4 or ipv6"),
    };
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let g = client
        .create_nat_gateway_v1()
        .vpc(vpc)
        .body(tritond_client::types::NewNatGateway {
            name,
            description: Some(description),
            family,
        })
        .send()
        .await
        .context("/v1/nat-gateways create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&g)?);
        return Ok(());
    }
    println!("NatGateway {}", g.id);
    println!("  name:           {}", g.name);
    println!("  vpc:            {}", g.vpc_id);
    println!("  public_address: {}", g.public_address);
    Ok(())
}

/// `tcadm nat-gateway delete <id>`.
pub async fn nat_gateway_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    nat_gateway_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_nat_gateway_v1()
        .nat_gateway_id(nat_gateway_id)
        .send()
        .await
        .context("/v1/nat-gateways/{id} delete")?;
    println!("NatGateway {nat_gateway_id} deleted.");
    Ok(())
}

/// `tcadm route-table create --vpc=<uuid> --name=X`.
pub async fn route_table_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    vpc: Uuid,
    name: String,
    description: String,
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let rt = client
        .create_route_table_v1()
        .vpc(vpc)
        .body(tritond_client::types::NewRouteTable {
            name,
            description: Some(description),
        })
        .send()
        .await
        .context("/v1/route-tables create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&rt)?);
        return Ok(());
    }
    println!("RouteTable {}", rt.id);
    println!("  name: {}", rt.name);
    println!("  vpc:  {}", rt.vpc_id);
    Ok(())
}

/// `tcadm route-table delete <id>`.
pub async fn route_table_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    route_table_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_route_table_v1()
        .route_table_id(route_table_id)
        .send()
        .await
        .context("/v1/route-tables/{id} delete")?;
    println!("RouteTable {route_table_id} deleted.");
    Ok(())
}

/// `tcadm route create --route-table=<uuid> --name=X --destination=...`.
#[allow(clippy::too_many_arguments)]
pub async fn route_create_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    route_table: Uuid,
    name: String,
    description: String,
    destination: String,
    target_nat_gateway: Option<Uuid>,
    target_blackhole: bool,
    target_reject: bool,
    target_virtual_gateway: bool,
    json_output: bool,
) -> Result<()> {
    use tritond_client::types::{NewRoute, RouteTarget};

    let chosen = [
        target_nat_gateway.is_some(),
        target_blackhole,
        target_reject,
        target_virtual_gateway,
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if chosen != 1 {
        anyhow::bail!(
            "exactly one of --target-nat-gateway, --target-blackhole, --target-reject, --target-virtual-gateway must be provided ({chosen} given)"
        );
    }

    let target = if let Some(id) = target_nat_gateway {
        RouteTarget::NatGateway { nat_gateway_id: id }
    } else if target_blackhole {
        RouteTarget::Blackhole
    } else if target_reject {
        RouteTarget::Reject
    } else {
        RouteTarget::VirtualGateway
    };

    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let r = client
        .create_route_v1()
        .route_table(route_table)
        .body(NewRoute {
            name,
            description: Some(description),
            destination,
            target,
        })
        .send()
        .await
        .context("/v1/routes create")?
        .into_inner();
    if json_output {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("Route {}", r.id);
    println!("  name:        {}", r.name);
    println!("  destination: {}", r.destination);
    println!("  target:      {:?}", r.target);
    Ok(())
}

/// `tcadm route delete <id>`.
pub async fn route_delete_v1(
    endpoint_override: Option<String>,
    api_key_override: Option<String>,
    route_id: Uuid,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    client
        .delete_route_v1()
        .route_id(route_id)
        .send()
        .await
        .context("/v1/routes/{id} delete")?;
    println!("Route {route_id} deleted.");
    Ok(())
}
