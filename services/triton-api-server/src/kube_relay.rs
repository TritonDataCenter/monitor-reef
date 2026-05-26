// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Build a [`kube::Client`] that routes connections through the relay tunnel.
//!
//! The Kubernetes API server lives on the cluster's fabric network.  The
//! server has no direct route there, but the relay agent does.  The strategy:
//!
//! 1. Spawn a local TCP listener on `127.0.0.1:0` (random free port).
//! 2. Each incoming connection opens a relay stream to the API server's
//!    fabric IP:6443 and splices bytes bidirectionally.
//! 3. Patch the stored kubeconfig's server URL to `https://127.0.0.1:<port>`
//!    and set `insecure-skip-tls-verify: true` so TLS verification passes
//!    (the API server cert is issued for the fabric IP, not 127.0.0.1).
//! 4. Build a [`kube::Client`] from the patched kubeconfig.
//!
//! The listener task runs for the lifetime of the current process; for lb
//! operations (a handful of apply/get calls) this is fine.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::compat::FuturesAsyncReadCompatExt as _;
use triton_relay_protocol::{bridge, write_connect_target};

use crate::cluster_store::ClusterRecord;
use crate::relay::RelayState;

/// Create a `kube::Client` whose connections are routed through the relay
/// tunnel registered for `relay`.
pub async fn kube_client_for_cluster(
    relay: &Arc<RelayState>,
    record: &ClusterRecord,
) -> Result<kube::Client> {
    let kubeconfig_yaml = record
        .kubeconfig_yaml
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("cluster {} has no kubeconfig stored", record.id))?;

    // Extract the API server target (host:port) from the kubeconfig.
    let raw_kc: kube::config::Kubeconfig =
        serde_yaml::from_str(kubeconfig_yaml).context("parse stored kubeconfig")?;
    let server_url = raw_kc
        .clusters
        .first()
        .and_then(|c| c.cluster.as_ref())
        .and_then(|c| c.server.as_deref())
        .ok_or_else(|| anyhow::anyhow!("kubeconfig missing server URL"))?
        .to_string();

    // "https://10.x.x.x:6443" → "10.x.x.x:6443"
    let target = server_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string();

    // Bind a local listener on a random port.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind local kube-relay proxy")?;
    let local_port = listener.local_addr()?.port();

    let relay_clone = Arc::clone(relay);
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let r = Arc::clone(&relay_clone);
            let t = target.clone();
            tokio::spawn(async move {
                if let Err(e) = proxy_connection(r, tcp, &t).await {
                    tracing::warn!(error = %e, "kube-relay proxy connection error");
                }
            });
        }
    });

    let patched = patch_server(kubeconfig_yaml, &format!("https://127.0.0.1:{local_port}"))
        .context("patch kubeconfig for relay")?;

    let kc: kube::config::Kubeconfig =
        serde_yaml::from_str(&patched).context("re-parse patched kubeconfig")?;
    let config =
        kube::Config::from_custom_kubeconfig(kc, &kube::config::KubeConfigOptions::default())
            .await
            .context("build kube Config from patched kubeconfig")?;

    kube::Client::try_from(config).context("build kube Client")
}

async fn proxy_connection(relay: Arc<RelayState>, mut tcp: TcpStream, target: &str) -> Result<()> {
    let mut stream = relay.open_stream().await.context("open relay stream")?;
    write_connect_target(&mut stream, target)
        .await
        .context("write relay connect target")?;
    let mut compat = stream.compat();
    bridge(&mut compat, &mut tcp)
        .await
        .context("bridge relay stream to kube TCP")?;
    Ok(())
}

/// Patch the kubeconfig YAML to use a different server URL and disable TLS
/// certificate verification (required when the server is behind a local proxy
/// that changes the hostname).
fn patch_server(kubeconfig_yaml: &str, new_server: &str) -> Result<String> {
    let mut value: serde_yaml::Value =
        serde_yaml::from_str(kubeconfig_yaml).context("parse kubeconfig YAML")?;

    let clusters = value
        .get_mut("clusters")
        .and_then(|v| v.as_sequence_mut())
        .ok_or_else(|| anyhow::anyhow!("kubeconfig has no 'clusters' sequence"))?;

    for entry in clusters.iter_mut() {
        let cluster = entry
            .get_mut("cluster")
            .ok_or_else(|| anyhow::anyhow!("cluster entry missing 'cluster' key"))?;

        cluster["server"] = serde_yaml::Value::String(new_server.to_string());
        cluster["insecure-skip-tls-verify"] = serde_yaml::Value::Bool(true);

        // Remove certificate-authority-data: incompatible with insecure-skip-tls-verify.
        if let serde_yaml::Value::Mapping(m) = cluster {
            m.remove("certificate-authority-data");
        }
    }

    serde_yaml::to_string(&value).context("serialize patched kubeconfig")
}
