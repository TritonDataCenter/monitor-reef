/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::{Context, Result, bail};
use tokio_stream::StreamExt;

use super::client;
use super::proto::cluster;

pub async fn run(
    endpoint: &str,
    wait_timeout: &str,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let timeout_dur = parse_duration(wait_timeout)?;

    let channel = client::connect(endpoint, talosconfig, verbose).await?;
    let mut client = cluster::cluster_service_client::ClusterServiceClient::new(channel);

    let req = cluster::HealthCheckRequest {
        wait_timeout: Some(prost_types::Duration {
            seconds: timeout_dur.as_secs() as i64,
            nanos: 0,
        }),
        cluster_info: None,
    };

    let stream_result = tokio::time::timeout(timeout_dur, async {
        let mut stream = client
            .health_check(req)
            .await
            .context("starting health check stream")?
            .into_inner();

        while let Some(msg) = stream.next().await {
            let progress = msg.context("health check stream error")?;

            if let Some(ref meta) = progress.metadata
                && !meta.error.is_empty()
            {
                bail!("health check error: {}", meta.error);
            }

            if !progress.message.is_empty() {
                eprintln!("{}", progress.message);
            }
        }

        Ok::<(), anyhow::Error>(())
    })
    .await;

    match stream_result {
        Ok(inner) => inner?,
        Err(_) => bail!("health check timed out after {}", wait_timeout),
    }

    Ok(())
}

/// Parse a human-readable duration string like "10m", "1h", "30s", "1h30m".
fn parse_duration(s: &str) -> Result<std::time::Duration> {
    let mut total_secs: u64 = 0;
    let mut current_num = String::new();

    for ch in s.chars() {
        if ch.is_ascii_digit() {
            current_num.push(ch);
        } else {
            let n: u64 = current_num
                .parse()
                .with_context(|| format!("invalid duration '{}'", s))?;
            current_num.clear();

            match ch {
                'h' => total_secs += n * 3600,
                'm' => total_secs += n * 60,
                's' => total_secs += n,
                _ => bail!("unknown duration unit '{}' in '{}'", ch, s),
            }
        }
    }

    // Handle bare number (assume seconds).
    if !current_num.is_empty() {
        let n: u64 = current_num.parse()?;
        total_secs += n;
    }

    if total_secs == 0 {
        bail!("duration '{}' resolves to zero", s);
    }

    Ok(std::time::Duration::from_secs(total_secs))
}
