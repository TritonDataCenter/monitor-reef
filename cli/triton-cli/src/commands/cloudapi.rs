// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! CloudAPI raw HTTP request command
//!
//! This hidden command allows making direct authenticated HTTP requests
//! to the CloudAPI, useful for debugging and accessing endpoints that
//! aren't exposed through the CLI.

use anyhow::Result;
use clap::{Args, ValueEnum};
use cloudapi_client::{ClientInfo, TypedClient};
use reqwest::header::{AUTHORIZATION, DATE};

#[derive(Clone, Copy, Debug, ValueEnum, Default)]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Delete,
    Head,
}

impl std::fmt::Display for HttpMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpMethod::Get => write!(f, "GET"),
            HttpMethod::Post => write!(f, "POST"),
            HttpMethod::Put => write!(f, "PUT"),
            HttpMethod::Delete => write!(f, "DELETE"),
            HttpMethod::Head => write!(f, "HEAD"),
        }
    }
}

#[derive(Args, Clone)]
pub struct CloudApiArgs {
    /// API path (e.g., /my/machines, /{account}/machines)
    ///
    /// Use {account} as a placeholder for the current account name.
    pub path: String,

    /// HTTP method
    #[arg(short, long, value_enum, default_value = "get")]
    pub method: HttpMethod,

    /// Request body (for POST/PUT)
    #[arg(short, long)]
    pub body: Option<String>,

    /// Read request body from stdin
    #[arg(long, conflicts_with = "body")]
    pub stdin: bool,

    /// Add custom header (can be repeated)
    #[arg(short = 'H', long = "header")]
    pub headers: Vec<String>,

    /// Show response headers
    #[arg(long)]
    pub show_headers: bool,

    /// Pretty print JSON response
    #[arg(long, default_value = "true")]
    pub pretty: bool,
}

pub async fn run(args: CloudApiArgs, client: &TypedClient) -> Result<()> {
    let auth_config = client.auth_config();
    let account = &auth_config.account;

    // Build the URL by substituting {account} placeholder
    let path = args.path.replace("{account}", account);

    // Ensure path starts with /
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{}", path)
    };

    // Build the full URL
    let base_url = client.inner().baseurl();
    let url = format!("{}{}", base_url, path);

    // Read body from stdin if requested
    let body = if args.stdin {
        use std::io::Read;
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        Some(buffer)
    } else {
        args.body.clone()
    };

    // Get auth headers using triton-auth
    let method_str = match args.method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
    };

    let (date_header, auth_header) =
        triton_auth::sign_request(auth_config, method_str, &path).await?;

    // Build request
    let http_client = reqwest::Client::new();
    let mut request = match args.method {
        HttpMethod::Get => http_client.get(&url),
        HttpMethod::Post => http_client.post(&url),
        HttpMethod::Put => http_client.put(&url),
        HttpMethod::Delete => http_client.delete(&url),
        HttpMethod::Head => http_client.head(&url),
    };

    // Add auth headers
    request = request
        .header(DATE, &date_header)
        .header(AUTHORIZATION, &auth_header);

    // Add body if provided
    if let Some(body_content) = &body {
        request = request
            .header("Content-Type", "application/json")
            .body(body_content.clone());
    }

    // Add custom headers
    for header in &args.headers {
        if let Some((key, value)) = header.split_once(':') {
            request = request.header(key.trim(), value.trim());
        } else {
            eprintln!(
                "Warning: Invalid header format '{}', expected 'Key: Value'",
                header
            );
        }
    }

    // Send request
    let response = request.send().await?;

    // Show headers if requested
    if args.show_headers {
        println!("HTTP/{:?} {}", response.version(), response.status());
        for (key, value) in response.headers() {
            println!("{}: {}", key, value.to_str().unwrap_or("<binary>"));
        }
        println!();
    }

    // Get response status for error handling
    let status = response.status();

    // Get response body
    let response_body = response.text().await?;

    // Try to pretty-print JSON
    if args.pretty && !response_body.is_empty() {
        match serde_json::from_str::<serde_json::Value>(&response_body) {
            Ok(json) => {
                println!("{}", serde_json::to_string_pretty(&json)?);
            }
            Err(_) => {
                // Not JSON, print as-is
                println!("{}", response_body);
            }
        }
    } else if !response_body.is_empty() {
        println!("{}", response_body);
    }

    // Return error if status was not successful
    if !status.is_success() {
        return Err(anyhow::anyhow!("Request failed with status: {}", status));
    }

    Ok(())
}
