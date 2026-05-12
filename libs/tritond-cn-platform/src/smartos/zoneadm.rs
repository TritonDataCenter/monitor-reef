// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! `/usr/sbin/zoneadm` wrapper — just enough to resolve a zone's
//! canonical zonename, zonepath, brand, and run state.
//!
//! Unlike `vmadm`, `zoneadm` is core illumos and is not part of the
//! legacy Triton agent stack that vnext is replacing. It is the stable
//! way for the per-CN agent's console listener to find:
//!
//! * `<zonepath>/root/tmp/vm.vnc` / `vm.console` — the in-zonepath UDS
//!   that bhyve / KVM expose their framebuffer / serial line on; and
//! * `/var/run/zones/<zonename>.console_sock` — zoneadmd's zone-console
//!   socket (the one `zlogin -C` / `vmadm console` attach to).
//!
//! and to confirm the zone is actually `running` before attempting a
//! connection. Callers inject the binary path so tests can swap in a
//! mock script; production uses the default `/usr/sbin/zoneadm`.

use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;
use uuid::Uuid;

/// illumos ships `zoneadm` at `/usr/sbin/zoneadm`.
pub const DEFAULT_ZONEADM_BIN: &str = "/usr/sbin/zoneadm";

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ZoneadmError {
    #[error("failed to spawn {path}: {source}")]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("zoneadm exited with status {status}: {stderr}")]
    NonZeroExit { status: ExitStatus, stderr: String },
    /// No zone with the requested name / uuid is configured on this host.
    #[error("zone {zone} not found")]
    NotFound { zone: String },
    /// `zoneadm list -p` produced output we could not parse into a
    /// [`ZoneInfo`].
    #[error("could not parse `zoneadm list -p` output: {0:?}")]
    Parse(String),
}

/// One zone as reported by `zoneadm list -p`.
///
/// The parseable line format (illumos `zoneadm(8)`) is colon-separated
/// with backslash-escaped `:` / `\` inside fields:
///
/// ```text
/// zoneid:zonename:state:zonepath:uuid:brand:ip-type[:r/w[:file-mac-profile]]
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneInfo {
    /// Numeric zone id, or `None` for a configured-but-not-running zone
    /// (`zoneadm` prints `-1` there).
    pub zoneid: Option<i64>,
    /// Canonical zonename. For SmartOS VMs this equals the zone UUID,
    /// but the console-socket path keys off this value verbatim.
    pub zonename: String,
    /// Run state: `running`, `installed`, `configured`, ...
    pub state: String,
    /// Filesystem root of the zone (its `zonecfg zonepath`).
    pub zonepath: PathBuf,
    /// The `zonecfg uuid`, when present.
    pub uuid: Option<Uuid>,
    /// Zone brand: `bhyve`, `kvm`, `joyent-minimal`, `lx`, ...
    pub brand: String,
}

impl ZoneInfo {
    /// Whether the zone is in the `running` state.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }

    /// Path to zoneadmd's zone-console socket for this zone
    /// (`/var/run/zones/<zonename>.console_sock`). This is the socket
    /// `zlogin -C` uses; consumers must send the `IDENT C 0\n` zlogin
    /// handshake before bytes flow.
    #[must_use]
    pub fn zone_console_socket(&self) -> PathBuf {
        PathBuf::from(format!("/var/run/zones/{}.console_sock", self.zonename))
    }

    /// Path to the bhyve / KVM framebuffer (VNC) UDS inside the zone:
    /// `<zonepath>/root/tmp/vm.vnc`.
    #[must_use]
    pub fn vnc_socket(&self) -> PathBuf {
        self.zonepath.join("root/tmp/vm.vnc")
    }

    /// Path to the KVM serial-line UDS inside the zone:
    /// `<zonepath>/root/tmp/vm.console`. (For non-KVM brands the serial
    /// line is the zone console — see [`Self::zone_console_socket`].)
    #[must_use]
    pub fn kvm_serial_socket(&self) -> PathBuf {
        self.zonepath.join("root/tmp/vm.console")
    }
}

/// Wrapper around `zoneadm`.
#[derive(Debug, Clone)]
pub struct ZoneadmTool {
    bin: PathBuf,
}

impl Default for ZoneadmTool {
    fn default() -> Self {
        Self {
            bin: PathBuf::from(DEFAULT_ZONEADM_BIN),
        }
    }
}

impl ZoneadmTool {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_bin(bin: impl Into<PathBuf>) -> Self {
        Self { bin: bin.into() }
    }

    /// `zoneadm -z <zone> list -p` — look up a single zone by name or
    /// uuid. Returns [`ZoneadmError::NotFound`] when no such zone is
    /// configured.
    pub async fn lookup(&self, zone: &str) -> Result<ZoneInfo, ZoneadmError> {
        let output = tokio::process::Command::new(&self.bin)
            .args(["-z", zone, "list", "-p"])
            .output()
            .await
            .map_err(|source| ZoneadmError::Spawn {
                path: self.bin.clone(),
                source,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr_matches_no_such_zone(&stderr) {
                return Err(ZoneadmError::NotFound {
                    zone: zone.to_string(),
                });
            }
            return Err(ZoneadmError::NonZeroExit {
                status: output.status,
                stderr: stderr.into_owned(),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // `-z` constrains to one zone, but `list` still prints a line
        // per zone matched; take the first non-empty line.
        let line = stdout
            .lines()
            .find(|l| !l.trim().is_empty())
            .ok_or_else(|| ZoneadmError::NotFound {
                zone: zone.to_string(),
            })?;
        parse_zoneadm_line(line).ok_or_else(|| ZoneadmError::Parse(line.to_string()))
    }
}

/// Recognise the `zoneadm` "no such zone" error so callers can map it
/// to a 404 without leaking raw stderr.
fn stderr_matches_no_such_zone(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("no such zone configured") || s.contains("could not find zone")
}

/// Parse one `zoneadm list -p` line into a [`ZoneInfo`].
///
/// Handles the backslash escaping `zoneadm` applies inside fields
/// (`\:` for a literal colon, `\\` for a literal backslash). Returns
/// `None` if the line has fewer than the six load-bearing fields.
#[must_use]
pub fn parse_zoneadm_line(line: &str) -> Option<ZoneInfo> {
    let fields = split_escaped_colon(line.trim_end_matches(['\n', '\r']));
    if fields.len() < 6 {
        return None;
    }
    let zoneid = match fields[0].as_str() {
        "-" | "" => None,
        other => match other.parse::<i64>() {
            Ok(-1) => None,
            Ok(n) => Some(n),
            Err(_) => None,
        },
    };
    let zonename = fields[1].clone();
    if zonename.is_empty() {
        return None;
    }
    let state = fields[2].clone();
    let zonepath = PathBuf::from(&fields[3]);
    let uuid = match fields[4].as_str() {
        "" => None,
        other => Uuid::parse_str(other).ok(),
    };
    let brand = fields[5].clone();
    Some(ZoneInfo {
        zoneid,
        zonename,
        state,
        zonepath,
        uuid,
        brand,
    })
}

/// Split on unescaped `:`, then un-escape `\:` → `:` and `\\` → `\`
/// within each field.
fn split_escaped_colon(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(next) => cur.push(next), // \: -> :, \\ -> \, \x -> x
                None => cur.push('\\'),
            },
            ':' => {
                out.push(std::mem::take(&mut cur));
            }
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_running_bhyve_zone() {
        let line = "12:7e0a5e3c-1111-2222-3333-444455556666:running:/zones/7e0a5e3c-1111-2222-3333-444455556666:7e0a5e3c-1111-2222-3333-444455556666:bhyve:exclusive:-:-";
        let z = parse_zoneadm_line(line).expect("parses");
        assert_eq!(z.zoneid, Some(12));
        assert_eq!(z.zonename, "7e0a5e3c-1111-2222-3333-444455556666");
        assert_eq!(z.state, "running");
        assert_eq!(
            z.zonepath,
            PathBuf::from("/zones/7e0a5e3c-1111-2222-3333-444455556666")
        );
        assert_eq!(
            z.uuid,
            Some(Uuid::parse_str("7e0a5e3c-1111-2222-3333-444455556666").unwrap())
        );
        assert_eq!(z.brand, "bhyve");
        assert!(z.is_running());
        assert_eq!(
            z.zone_console_socket(),
            PathBuf::from("/var/run/zones/7e0a5e3c-1111-2222-3333-444455556666.console_sock")
        );
        assert_eq!(
            z.vnc_socket(),
            PathBuf::from("/zones/7e0a5e3c-1111-2222-3333-444455556666/root/tmp/vm.vnc")
        );
        assert_eq!(
            z.kvm_serial_socket(),
            PathBuf::from("/zones/7e0a5e3c-1111-2222-3333-444455556666/root/tmp/vm.console")
        );
    }

    #[test]
    fn not_running_zone_has_no_zoneid() {
        let line = "-:abc:installed:/zones/abc::joyent-minimal:excl";
        let z = parse_zoneadm_line(line).expect("parses");
        assert_eq!(z.zoneid, None);
        assert_eq!(z.uuid, None);
        assert!(!z.is_running());
        assert_eq!(z.brand, "joyent-minimal");
    }

    #[test]
    fn handles_escaped_colon_in_zonepath() {
        // Contrived, but exercises the un-escaper.
        let line = r"3:weird:running:/zones/a\:b:11111111-1111-1111-1111-111111111111:lx:excl";
        let z = parse_zoneadm_line(line).expect("parses");
        assert_eq!(z.zonepath, PathBuf::from("/zones/a:b"));
        assert_eq!(z.brand, "lx");
    }

    #[test]
    fn too_few_fields_is_none() {
        assert!(parse_zoneadm_line("1:abc:running").is_none());
        assert!(parse_zoneadm_line("").is_none());
        // empty zonename rejected
        assert!(parse_zoneadm_line("1::running:/z::brand:x").is_none());
    }

    #[test]
    fn stderr_no_such_zone_detected() {
        assert!(stderr_matches_no_such_zone(
            "zoneadm: zone 'nope': No such zone configured\n"
        ));
        assert!(!stderr_matches_no_such_zone("zoneadm: permission denied\n"));
    }

    #[tokio::test]
    async fn lookup_against_stub_binary() {
        // Write a fake `zoneadm` that echoes a fixture line, point the
        // tool at it, and verify end-to-end parsing.
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("zoneadm");
        std::fs::write(
            &bin,
            "#!/bin/sh\necho '7:zn:running:/zones/zn:22222222-2222-2222-2222-222222222222:bhyve:excl:-:-'\n",
        )
        .expect("write stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }
        let tool = ZoneadmTool::with_bin(&bin);
        let z = tool.lookup("zn").await.expect("lookup ok");
        assert_eq!(z.zonename, "zn");
        assert_eq!(z.brand, "bhyve");
        assert!(z.is_running());
    }

    #[tokio::test]
    async fn lookup_missing_zone_is_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin = dir.path().join("zoneadm");
        std::fs::write(
            &bin,
            "#!/bin/sh\necho \"zoneadm: zone 'nope': No such zone configured\" 1>&2\nexit 1\n",
        )
        .expect("write stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&bin, perms).unwrap();
        }
        let tool = ZoneadmTool::with_bin(&bin);
        match tool.lookup("nope").await {
            Err(ZoneadmError::NotFound { zone }) => assert_eq!(zone, "nope"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
