// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/usr/bin/sysinfo` wrapper.
//!
//! The SmartOS `sysinfo` binary writes a JSON blob describing the compute
//! node (UUID, CPU, memory, boot time, network interfaces, and so on).
//! cn-agent reads this on startup and on demand. The schema is large and
//! somewhat fluid, so we store the raw [`serde_json::Value`] verbatim and
//! expose typed accessors for the handful of fields we care about.

use std::net::Ipv4Addr;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Path to the SmartOS `sysinfo` binary on a real compute node.
///
/// Lives under `/usr/bin` on every current SmartOS platform image. A
/// different path (e.g., a mock script) can be substituted via
/// [`Sysinfo::collect_from_path`] in tests.
pub const SYSINFO_BIN: &str = "/usr/bin/sysinfo";

/// Parsed `/usr/bin/sysinfo` output, plus convenient accessors for the fields
/// cn-agent touches directly.
///
/// Callers should treat the `raw` value as opaque; only the accessors below
/// are part of the stable interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sysinfo {
    /// The full JSON as produced by `/usr/bin/sysinfo`.
    pub raw: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum SysinfoError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} exited with status {status}: {stderr}")]
    NonZeroExit {
        path: String,
        status: String,
        stderr: String,
    },
    #[error("failed to parse sysinfo JSON: {0}")]
    Parse(#[from] serde_json::Error),
}

impl Sysinfo {
    /// Run `/usr/bin/sysinfo` and parse its stdout as JSON.
    pub async fn collect() -> Result<Self, SysinfoError> {
        Self::collect_from_path(SYSINFO_BIN).await
    }

    /// Run an alternate `sysinfo` binary (primarily for tests).
    pub async fn collect_from_path(path: impl AsRef<Path>) -> Result<Self, SysinfoError> {
        let path_ref = path.as_ref();
        let output = tokio::process::Command::new(path_ref)
            .output()
            .await
            .map_err(|source| SysinfoError::Spawn {
                path: path_ref.display().to_string(),
                source,
            })?;

        if !output.status.success() {
            return Err(SysinfoError::NonZeroExit {
                path: path_ref.display().to_string(),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        Self::from_json(&output.stdout)
    }

    /// Parse pre-captured `sysinfo` output. Useful for tests and for replaying
    /// a sysinfo fixture on a non-illumos platform.
    pub fn from_json(bytes: &[u8]) -> Result<Self, SysinfoError> {
        let raw = serde_json::from_slice(bytes)?;
        Ok(Self { raw })
    }

    /// Compute-node UUID, as reported in the `UUID` field.
    pub fn uuid(&self) -> Option<uuid::Uuid> {
        self.raw
            .get("UUID")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
    }

    /// Hostname, as reported in the `Hostname` field.
    pub fn hostname(&self) -> Option<&str> {
        self.raw.get("Hostname").and_then(|v| v.as_str())
    }

    /// Boot time as a unix timestamp (seconds since epoch), from `Boot Time`.
    ///
    /// The legacy agent passed `parseInt(sysinfo['Boot Time'], 10)` so we
    /// replicate that tolerance and accept the value as either a string or
    /// a number.
    pub fn boot_time_unix(&self) -> Option<i64> {
        let v = self.raw.get("Boot Time")?;
        if let Some(n) = v.as_i64() {
            return Some(n);
        }
        v.as_str().and_then(|s| s.parse::<i64>().ok())
    }

    /// Name of the NIC tag used for the admin network.
    ///
    /// Defaults to `"admin"`; an operator may override per-CN via the
    /// `Admin NIC Tag` sysinfo field, and the legacy agent honors that.
    pub fn admin_nic_tag(&self) -> &str {
        self.raw
            .get("Admin NIC Tag")
            .and_then(|v| v.as_str())
            .unwrap_or("admin")
    }

    /// Extract the admin network IPv4 address.
    ///
    /// Two-step lookup matching `triton-netconfig.adminIpFromSysinfo`:
    ///
    /// 1. If `Admin IP` is set to a valid IPv4 address, return it.
    /// 2. Otherwise, walk `Network Interfaces`, find the NIC whose
    ///    `NIC Names` array contains the admin tag, and return its
    ///    `ip4addr`.
    ///
    /// An `Admin IP` value of `"dhcp"` intentionally falls through to the
    /// NIC scan so we read the actual assigned address.
    pub fn admin_ip(&self) -> Option<Ipv4Addr> {
        if let Some(ip) = self
            .raw
            .get("Admin IP")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Ipv4Addr>().ok())
        {
            return Some(ip);
        }

        let admin_tag = self.admin_nic_tag();
        let interfaces = self.raw.get("Network Interfaces")?.as_object()?;

        for (_, iface) in interfaces {
            let Some(tags) = iface.get("NIC Names").and_then(|v| v.as_array()) else {
                continue;
            };
            let matches = tags.iter().any(|t| t.as_str() == Some(admin_tag));
            if !matches {
                continue;
            }
            if let Some(ip) = iface
                .get("ip4addr")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<Ipv4Addr>().ok())
            {
                return Some(ip);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal sysinfo fixture modeled on real CN output.
    const FIXTURE: &str = r#"{
        "UUID": "564d1023-4ab1-4e9e-816f-6cf7f09eafd6",
        "Hostname": "headnode",
        "Boot Time": "1700000000",
        "Admin IP": "dhcp",
        "Network Interfaces": {
            "vmx0": {
                "MAC Address": "00:50:56:01:02:03",
                "ip4addr": "10.88.88.7",
                "NIC Names": ["admin"]
            },
            "vmx1": {
                "MAC Address": "00:50:56:01:02:04",
                "ip4addr": "192.168.99.5",
                "NIC Names": ["external"]
            }
        }
    }"#;

    #[test]
    fn extracts_uuid_and_hostname() {
        let si = Sysinfo::from_json(FIXTURE.as_bytes()).expect("parse");
        assert_eq!(
            si.uuid(),
            Some(
                uuid::Uuid::parse_str("564d1023-4ab1-4e9e-816f-6cf7f09eafd6").expect("valid uuid"),
            ),
        );
        assert_eq!(si.hostname(), Some("headnode"));
    }

    #[test]
    fn boot_time_parses_string() {
        let si = Sysinfo::from_json(FIXTURE.as_bytes()).expect("parse");
        assert_eq!(si.boot_time_unix(), Some(1_700_000_000));
    }

    #[test]
    fn admin_ip_falls_back_to_nic_scan_when_admin_ip_is_dhcp() {
        let si = Sysinfo::from_json(FIXTURE.as_bytes()).expect("parse");
        assert_eq!(
            si.admin_ip(),
            Some("10.88.88.7".parse::<Ipv4Addr>().expect("valid ipv4"))
        );
    }

    #[test]
    fn admin_ip_prefers_valid_admin_ip_field() {
        let mut raw: serde_json::Value = serde_json::from_str(FIXTURE).expect("parse fixture");
        raw["Admin IP"] = serde_json::json!("10.0.0.1");
        let si = Sysinfo { raw };
        assert_eq!(
            si.admin_ip(),
            Some("10.0.0.1".parse::<Ipv4Addr>().expect("valid ipv4"))
        );
    }

    #[test]
    fn admin_ip_honors_custom_admin_nic_tag() {
        let mut raw: serde_json::Value = serde_json::from_str(FIXTURE).expect("parse fixture");
        raw["Admin NIC Tag"] = serde_json::json!("custom_admin");
        raw["Network Interfaces"]["vmx0"]["NIC Names"] = serde_json::json!(["custom_admin"]);
        let si = Sysinfo { raw };
        assert_eq!(
            si.admin_ip(),
            Some("10.88.88.7".parse::<Ipv4Addr>().expect("valid ipv4"))
        );
    }

    #[test]
    fn admin_ip_none_when_no_admin_nic() {
        let raw = serde_json::json!({
            "UUID": "564d1023-4ab1-4e9e-816f-6cf7f09eafd6",
            "Admin IP": "dhcp",
            "Network Interfaces": {
                "vmx0": {
                    "ip4addr": "192.168.99.5",
                    "NIC Names": ["external"]
                }
            }
        });
        let si = Sysinfo { raw };
        assert_eq!(si.admin_ip(), None);
    }
}
