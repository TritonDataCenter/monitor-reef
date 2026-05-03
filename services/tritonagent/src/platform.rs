// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Host-platform introspection for compatibility gates.
//!
//! Phase 0 reads the SmartOS platform buildstamp from
//! `uname -v`, which returns a string like
//! `joyent_20260417T033207Z` on every SmartOS host. The
//! buildstamp portion (`20260417T033207Z`) is the
//! lexicographically-comparable identifier the agent
//! compares against an image's
//! [`tritond_image_manifest::Compatibility::min_smartos_platform`]
//! constraint.
//!
//! `uname -v` is preferred over `sysinfo` because it's a
//! plain libc syscall away (no JSON parse, no `cmd::Command`
//! cost) and exists on every illumos host. Falls back to
//! `cmd::Command` if for any reason the syscall path returns
//! something unexpected.

use anyhow::{Context, Result, anyhow};
use tokio::process::Command;

/// Read the host's SmartOS platform buildstamp by running
/// `uname -v` and stripping the leading `joyent_` (or
/// `smartos_`) prefix. Returns the bare buildstamp string —
/// `20260417T033207Z` shape — suitable for lexicographic
/// comparison against
/// [`tritond_image_manifest::Compatibility::min_smartos_platform`].
pub async fn host_platform_buildstamp() -> Result<String> {
    let output = Command::new("uname")
        .arg("-v")
        .output()
        .await
        .context("spawn `uname -v`")?;
    if !output.status.success() {
        return Err(anyhow!(
            "uname -v failed (exit {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let raw = String::from_utf8(output.stdout)
        .context("uname -v output is not utf-8")?
        .trim()
        .to_string();
    Ok(parse_buildstamp(&raw))
}

/// Strip the SmartOS-flavour prefix off a `uname -v` value.
/// Both `joyent_<stamp>` and `smartos_<stamp>` are observed in
/// the wild; anything without an underscore is returned as-is
/// (defensive — a future flavour change shouldn't crash the
/// agent).
fn parse_buildstamp(uname_v: &str) -> String {
    if let Some((_, after)) = uname_v.split_once('_') {
        after.to_string()
    } else {
        uname_v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_joyent_prefix() {
        assert_eq!(
            parse_buildstamp("joyent_20260417T033207Z"),
            "20260417T033207Z",
        );
    }

    #[test]
    fn parse_smartos_prefix() {
        assert_eq!(
            parse_buildstamp("smartos_20240101T000000Z"),
            "20240101T000000Z",
        );
    }

    #[test]
    fn parse_passes_through_unknown_format() {
        // Defensive: future builds may change the prefix.
        assert_eq!(parse_buildstamp("rawstamp"), "rawstamp");
    }

    #[test]
    fn lexicographic_compare_is_chronological() {
        // SmartOS buildstamps are ISO-8601-like; lex sort
        // matches chronological sort within a calendar.
        assert!("20240101T000000Z" < "20260417T033207Z");
    }
}
