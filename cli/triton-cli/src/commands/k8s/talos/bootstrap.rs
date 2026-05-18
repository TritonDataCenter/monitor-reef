/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::{Result, bail};

use super::client;
use super::proto::machine;
use super::retry;

pub async fn run(
    endpoint: &str,
    do_retry: bool,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<()> {
    if do_retry {
        retry::with_retry(verbose, || bootstrap_once(endpoint, talosconfig, verbose)).await
    } else {
        bootstrap_once(endpoint, talosconfig, verbose).await
    }
}

async fn bootstrap_once(endpoint: &str, talosconfig: Option<&str>, verbose: bool) -> Result<()> {
    let channel = client::connect(endpoint, talosconfig, verbose).await?;
    let mut client = machine::machine_service_client::MachineServiceClient::new(channel);

    let req = machine::BootstrapRequest {
        recover_etcd: false,
        recover_skip_hash_check: false,
    };

    let resp = client.bootstrap(req).await?.into_inner();

    for msg in &resp.messages {
        if let Some(ref meta) = msg.metadata
            && !meta.error.is_empty()
        {
            bail!("bootstrap error: {}", meta.error);
        }
    }

    Ok(())
}
