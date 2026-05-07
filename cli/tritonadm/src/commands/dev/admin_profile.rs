// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `tritonadm post-setup admin-profile` — generate a `triton` CLI profile
//! for the headnode admin account. Reads the SDC config for datacenter
//! name and admin login, derives the SSH-key fingerprint via `ssh-keygen
//! -E md5 -lf`, and probes the CloudAPI URL to decide whether to set
//! `insecure: true` (auto-true on TLS verification failure, e.g. COAL).
//!
//! Writes to `$HOME/.triton/profiles.d/<name>.json`. Profile schema
//! matches `cli/triton-cli/src/config/profile.rs`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::process::Command;

use crate::config::TritonConfig;

const ADMIN_PUB_KEY_PATH: &str = "/root/.ssh/sdc.id_rsa.pub";
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Serialize)]
struct ProfileFile<'a> {
    url: &'a str,
    account: &'a str,
    #[serde(rename = "keyId")]
    key_id: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    insecure: bool,
}

pub struct AdminProfileOpts {
    pub name: String,
    pub force: bool,
    pub print: bool,
}

pub async fn run(sdc_config: Option<TritonConfig>, opts: AdminProfileOpts) -> Result<()> {
    let cfg = sdc_config.context(
        "tritonadm post-setup admin-profile must run on a Triton headnode \
         (could not load /lib/sdc/config.sh)",
    )?;

    let account = cfg
        .get_str("ufds_admin_login")
        .unwrap_or("admin")
        .to_string();
    let url = cloudapi_url(&cfg);
    let key_id = key_fingerprint_md5(Path::new(ADMIN_PUB_KEY_PATH)).await?;

    eprintln!("==> Probing {url}");
    let insecure = needs_insecure(&url).await;
    if insecure {
        eprintln!(
            "warning: TLS verification of {url} failed; setting insecure=true \
             in the profile. This is expected on COAL (self-signed cert)."
        );
    }

    let profile = ProfileFile {
        url: &url,
        account: &account,
        key_id: &key_id,
        insecure,
    };
    let body = serde_json::to_string_pretty(&profile)?;

    if opts.print {
        println!("{body}");
        return Ok(());
    }

    let path = profile_path(&opts.name)?;
    if tokio::fs::try_exists(&path).await.unwrap_or(false) && !opts.force {
        bail!(
            "{} already exists; pass --force to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tokio::fs::write(&path, &body)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;

    eprintln!();
    eprintln!("==> Wrote {} (profile '{}')", path.display(), opts.name);
    eprintln!("    url:      {url}");
    eprintln!("    account:  {account}");
    eprintln!("    keyId:    {key_id}");
    eprintln!("    insecure: {insecure}");
    eprintln!();
    eprintln!("Use it with:");
    eprintln!("    triton -p {} <command>", opts.name);
    eprintln!("or set as default:");
    eprintln!("    triton profile set-current {}", opts.name);

    if !tokio::fs::try_exists("/root/.ssh/sdc.id_rsa")
        .await
        .unwrap_or(false)
    {
        eprintln!();
        eprintln!(
            "warning: /root/.ssh/sdc.id_rsa not found — `triton` won't be \
             able to sign requests until the matching private key is on \
             disk or loaded into ssh-agent."
        );
    }

    Ok(())
}

/// Construct the CloudAPI URL. Prefer `cloudapi_domain` from SDC config
/// if set; otherwise fall back to the canonical
/// `cloudapi.<datacenter>.<dns_domain>` form (which DNS resolves on a
/// fully set up headnode).
fn cloudapi_url(cfg: &TritonConfig) -> String {
    if let Some(domain) = cfg.get_str("cloudapi_domain").filter(|s| !s.is_empty()) {
        return format!("https://{domain}");
    }
    format!(
        "https://cloudapi.{}.{}",
        cfg.datacenter_name, cfg.dns_domain
    )
}

/// Derive the MD5 fingerprint (colon-separated hex, the keyId format
/// triton/CloudAPI use) of an OpenSSH public key by shelling out to
/// `ssh-keygen -E md5 -lf <path>`. ssh-keygen is universally present on
/// SmartOS GZ and is the same tool node-triton uses.
async fn key_fingerprint_md5(pub_key_path: &Path) -> Result<String> {
    if !tokio::fs::try_exists(pub_key_path).await.unwrap_or(false) {
        bail!(
            "admin SSH public key not found at {}; expected the standard \
             headnode location",
            pub_key_path.display()
        );
    }
    let output = Command::new("ssh-keygen")
        .args(["-E", "md5", "-lf"])
        .arg(pub_key_path)
        .output()
        .await
        .context("failed to invoke ssh-keygen")?;
    if !output.status.success() {
        bail!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // ssh-keygen output: "<bits> MD5:<aa:bb:...> <comment> (<TYPE>)"
    let line = String::from_utf8_lossy(&output.stdout);
    let fingerprint = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.strip_prefix("MD5:"))
        .with_context(|| format!("unexpected ssh-keygen output: {line:?}"))?;
    Ok(fingerprint.to_string())
}

/// Probe the CloudAPI URL with default TLS verification. Return `true`
/// iff the request fails specifically due to a TLS / certificate error,
/// indicating we should set `insecure: true` in the profile. Other
/// failures (DNS, connection refused, timeout) return `false` — the
/// profile is still written and the operator can fix connectivity later.
///
/// The crypto-provider install is required because the workspace builds
/// reqwest with `rustls-no-provider`; without it, `Client::build()`
/// panics with "No provider set" (see `libs/triton-tls/src/lib.rs`).
async fn needs_insecure(url: &str) -> bool {
    triton_tls::install_default_crypto_provider();
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(_) => false,
        Err(e) => is_tls_error(&e),
    }
}

/// Walk a reqwest error's source chain looking for a TLS-layer failure.
/// reqwest wraps errors from the underlying TLS stack; the chain
/// contains a `rustls::Error` (or native-tls equivalent) when the cert
/// chain didn't validate. Generic transport errors (DNS, refused) won't
/// match.
fn is_tls_error(err: &reqwest::Error) -> bool {
    // reqwest::Error::is_connect() is true for both "couldn't connect"
    // and TLS handshake failures, so we have to dig into the source
    // chain to disambiguate.
    let mut source: Option<&dyn std::error::Error> = Some(err);
    while let Some(e) = source {
        let s = format!("{e}").to_lowercase();
        if s.contains("certificate")
            || s.contains("self signed")
            || s.contains("self-signed")
            || s.contains("unknownissuer")
            || s.contains("invalid peer certificate")
            || s.contains("tls handshake")
        {
            return true;
        }
        source = e.source();
    }
    false
}

/// Resolve `$HOME/.triton/profiles.d/<name>.json`, honoring
/// `TRITON_CONFIG_DIR` and `XDG_CONFIG_HOME` so this matches what
/// `triton` itself will read (see `cli/triton-cli/src/config/paths.rs`).
fn profile_path(name: &str) -> Result<PathBuf> {
    let base = if let Ok(dir) = std::env::var("TRITON_CONFIG_DIR") {
        PathBuf::from(dir)
    } else if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("triton")
    } else {
        dirs::home_dir()
            .context("could not determine $HOME for profile path")?
            .join(".triton")
    };
    Ok(base.join("profiles.d").join(format!("{name}.json")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn cloudapi_url_uses_cloudapi_domain_when_set() {
        let cfg = TritonConfig::from_raw(serde_json::json!({
            "datacenter_name": "us-east-1",
            "dns_domain": "triton.zone",
            "cloudapi_domain": "us-east.api.example.com",
        }));
        assert_eq!(cloudapi_url(&cfg), "https://us-east.api.example.com");
    }

    #[test]
    fn cloudapi_url_falls_back_to_dc_dns() {
        let cfg = TritonConfig::from_raw(serde_json::json!({
            "datacenter_name": "coal",
            "dns_domain": "joyent.us",
        }));
        assert_eq!(cloudapi_url(&cfg), "https://cloudapi.coal.joyent.us");
    }

    #[test]
    fn cloudapi_url_treats_empty_domain_as_unset() {
        let cfg = TritonConfig::from_raw(serde_json::json!({
            "datacenter_name": "coal",
            "dns_domain": "joyent.us",
            "cloudapi_domain": "",
        }));
        assert_eq!(cloudapi_url(&cfg), "https://cloudapi.coal.joyent.us");
    }

    #[test]
    fn profile_path_honors_triton_config_dir() {
        unsafe {
            std::env::set_var("TRITON_CONFIG_DIR", "/tmp/tcfg-test");
        }
        let p = profile_path("admin").unwrap();
        unsafe {
            std::env::remove_var("TRITON_CONFIG_DIR");
        }
        assert_eq!(p, PathBuf::from("/tmp/tcfg-test/profiles.d/admin.json"));
    }
}
