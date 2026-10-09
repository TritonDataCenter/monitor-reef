// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton datacenter configuration discovery.
//!
//! On a Triton headnode, `/lib/sdc/config.sh -json` provides the datacenter
//! name and DNS domain. Internal API URLs are constructed as:
//!
//!     http://{service}.{datacenter_name}.{dns_domain}
//!
//! This matches sdcadm's config loading from `/usbkey/config`.

use std::process::Command;

/// Configuration derived from the Triton headnode's SDC config.
pub struct TritonConfig {
    pub datacenter_name: String,
    pub dns_domain: String,
    /// Headnode server UUID (from sysinfo, if available)
    pub server_uuid: Option<String>,
    /// Full parsed config blob — for callers (admin-profile setup, ad-hoc
    /// scripts) that need keys we haven't promoted to typed fields yet.
    raw: serde_json::Value,
}

impl TritonConfig {
    /// Try to load configuration from `/lib/sdc/config.sh -json`.
    ///
    /// Returns `None` on non-Triton systems where the script doesn't exist
    /// or fails. This is expected during development on non-SmartOS hosts.
    pub fn load() -> Option<Self> {
        let output = Command::new("/usr/bin/bash")
            .args(["/lib/sdc/config.sh", "-json"])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

        let datacenter_name = parsed.get("datacenter_name")?.as_str()?.to_string();
        let dns_domain = parsed.get("dns_domain")?.as_str()?.to_string();

        // Try to get server UUID from sysinfo
        let server_uuid = Self::load_server_uuid();

        Some(Self {
            datacenter_name,
            dns_domain,
            server_uuid,
            raw: parsed,
        })
    }

    /// Look up an arbitrary string key from the SDC config (e.g.
    /// `ufds_admin_login`, `cloudapi_domain`, `ufds_admin_key_fingerprint`).
    /// Returns `None` if the key is missing or non-string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.raw.get(key)?.as_str()
    }

    /// Test-only constructor: build a TritonConfig from a raw JSON blob
    /// without invoking `/lib/sdc/config.sh`. Pulls `datacenter_name` and
    /// `dns_domain` out of the JSON; `server_uuid` is left None.
    #[cfg(test)]
    pub fn from_raw(raw: serde_json::Value) -> Self {
        Self {
            datacenter_name: raw
                .get("datacenter_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            dns_domain: raw
                .get("dns_domain")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            server_uuid: None,
            raw,
        }
    }

    /// Try to get the headnode's server UUID from `sysinfo`.
    fn load_server_uuid() -> Option<String> {
        let output = Command::new("/usr/bin/sysinfo").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        parsed.get("UUID")?.as_str().map(String::from)
    }

    /// Construct an internal service URL from the SDC config.
    ///
    /// Returns `http://{service}.{datacenter_name}.{dns_domain}`.
    pub fn service_url(&self, service: &str) -> String {
        format!(
            "http://{}.{}.{}",
            service, self.datacenter_name, self.dns_domain
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_url_format() {
        let cfg = TritonConfig {
            datacenter_name: "us-east-1".to_string(),
            dns_domain: "triton.zone".to_string(),
            server_uuid: None,
            raw: serde_json::json!({}),
        };
        assert_eq!(cfg.service_url("sapi"), "http://sapi.us-east-1.triton.zone");
        assert_eq!(
            cfg.service_url("vmapi"),
            "http://vmapi.us-east-1.triton.zone"
        );
    }
}
