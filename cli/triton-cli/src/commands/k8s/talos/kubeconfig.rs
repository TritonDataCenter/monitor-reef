/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use std::io::Read;
use tokio_stream::StreamExt;

use super::client;
use super::proto::machine;

pub async fn run(
    endpoint: &str,
    path: &str,
    talosconfig: Option<&str>,
    verbose: bool,
) -> Result<()> {
    let channel = client::connect(endpoint, talosconfig, verbose).await?;
    let mut client = machine::machine_service_client::MachineServiceClient::new(channel);

    if verbose {
        eprintln!("requesting kubeconfig from {}", endpoint);
    }

    let mut stream = client.kubeconfig(()).await?.into_inner();

    // Collect all streamed bytes.
    let mut all_bytes: Vec<u8> = Vec::new();
    while let Some(msg) = stream.next().await {
        let data = msg.context("kubeconfig stream error")?;

        if let Some(ref meta) = data.metadata
            && !meta.error.is_empty()
        {
            bail!("kubeconfig error: {}", meta.error);
        }

        all_bytes.extend_from_slice(&data.bytes);
    }

    if all_bytes.is_empty() {
        bail!("kubeconfig response was empty");
    }

    if verbose {
        eprintln!("received {} bytes of gzipped tar data", all_bytes.len());
    }

    // Decompress gzip and extract the kubeconfig from the tar archive.
    let gz = GzDecoder::new(&all_bytes[..]);
    let mut archive = tar::Archive::new(gz);

    let mut kubeconfig_data: Option<Vec<u8>> = None;

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let entry_path = entry
            .path()
            .context("reading tar entry path")?
            .to_string_lossy()
            .to_string();

        if verbose {
            eprintln!("tar entry: {}", entry_path);
        }

        if entry_path == "kubeconfig" || entry_path.ends_with("/kubeconfig") {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("reading kubeconfig from tar")?;
            kubeconfig_data = Some(buf);
            break;
        }
    }

    let kubeconfig_data = kubeconfig_data.context("'kubeconfig' entry not found in tar archive")?;

    // Ensure parent directory exists.
    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent directory '{}'", parent.display()))?;
    }

    std::fs::write(path, &kubeconfig_data)
        .with_context(|| format!("writing kubeconfig to '{}'", path))?;

    // Set file permissions to 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("setting permissions on '{}'", path))?;
    }

    if verbose {
        eprintln!("wrote kubeconfig to {}", path);
    }

    Ok(())
}
