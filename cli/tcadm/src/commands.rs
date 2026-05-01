// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Subcommand implementations for `tcadm`.

use anyhow::{Context, Result};
use tritond_client::Client;
use tritond_client::types::{LoginRequest, NewApiKey, TokenResponse};
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
    json_output: bool,
) -> Result<()> {
    let session = Session::resolve(endpoint_override, api_key_override).await?;
    let client = session.client()?;
    let response = client
        .create_api_key()
        .body(NewApiKey { description })
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
        println!("{}  {}  {}", key.id, key.created_at, key.description);
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
