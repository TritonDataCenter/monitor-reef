// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Storage-layer validation for user-controlled key components.
//!
//! Names, fingerprints, and MAC addresses all flow as terminal
//! components of FDB keys via `format!("…/{val}")`. Without
//! validation, a name containing `/`, NUL, or control bytes can
//! break range-scan parsing, collide with adjacent prefixes, or
//! exceed FDB's 10 KB key limit. Defense in depth at the storage
//! layer is non-negotiable: the API edge is one layer of validation,
//! this is the second.

use thiserror::Error;

use crate::StoreError;

/// Maximum byte length of a resource name. 63 follows the DNS label
/// convention and is a comfortable upper bound for everything that
/// becomes part of a key.
pub const MAX_NAME_BYTES: usize = 63;

/// Maximum byte length of an SSH key fingerprint. Generous: SHA256
/// base64 is ~50 bytes, MD5 colon-hex is 47, but operator tools
/// sometimes prefix labels.
pub const MAX_FINGERPRINT_BYTES: usize = 512;

/// Canonical lowercase MAC address (`aa:bb:cc:dd:ee:ff`): 17 chars exact.
pub const MAC_LEN: usize = 17;

/// Why a user-controlled string is unsafe to flow into a storage key.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum InvalidInput {
    #[error("{field} is empty")]
    Empty { field: &'static str },

    #[error("{field} exceeds {limit} bytes ({len} given)")]
    TooLong {
        field: &'static str,
        len: usize,
        limit: usize,
    },

    #[error("{field} contains {ch:?} which is reserved for key separators")]
    ReservedChar { field: &'static str, ch: char },

    #[error("{field} contains control byte {byte:#04x}")]
    ControlByte { field: &'static str, byte: u8 },

    #[error("{field} has leading or trailing whitespace")]
    EdgeWhitespace { field: &'static str },

    #[error("{field} {value:?} is not a canonical MAC address (aa:bb:cc:dd:ee:ff)")]
    BadMac { field: &'static str, value: String },
}

impl From<InvalidInput> for StoreError {
    fn from(e: InvalidInput) -> Self {
        StoreError::Conflict(e.to_string())
    }
}

/// Validate a resource name (silo/tenant/project/vpc/subnet/etc.).
///
/// Rules:
/// * 1..=63 bytes
/// * no `/` or `\\` (key-separator collision risk)
/// * no NUL or ASCII control bytes (0x00..=0x1F, 0x7F)
/// * no leading/trailing whitespace (silent typo trap)
///
/// Internal Unicode is fine: `"café-prod"` validates. ASCII control
/// bytes are rejected because they can't round-trip through `format!`
/// safely and have no business in a UX-visible identifier.
pub fn name(field: &'static str, s: &str) -> Result<(), InvalidInput> {
    check_basics(field, s, MAX_NAME_BYTES, /* allow_internal_slash */ false)
}

/// Validate an SSH-key fingerprint. Same rules as [`name`] but with
/// a much higher length cap and `/` allowed (base64 fingerprints can
/// contain `/`).
pub fn fingerprint(field: &'static str, s: &str) -> Result<(), InvalidInput> {
    check_basics(field, s, MAX_FINGERPRINT_BYTES, /* allow_internal_slash */ true)
}

/// Validate a MAC address in canonical lowercase form `aa:bb:cc:dd:ee:ff`.
/// Uppercase or dash-separated variants are rejected so writers
/// normalise once at the boundary.
pub fn mac(field: &'static str, s: &str) -> Result<(), InvalidInput> {
    if s.len() != MAC_LEN {
        return Err(InvalidInput::BadMac {
            field,
            value: s.to_string(),
        });
    }
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        let want_colon = i == 2 || i == 5 || i == 8 || i == 11 || i == 14;
        if want_colon {
            if *b != b':' {
                return Err(InvalidInput::BadMac {
                    field,
                    value: s.to_string(),
                });
            }
        } else {
            // Lowercase hex: 0-9 or a-f.
            let ok = b.is_ascii_digit() || (b'a'..=b'f').contains(b);
            if !ok {
                return Err(InvalidInput::BadMac {
                    field,
                    value: s.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn check_basics(
    field: &'static str,
    s: &str,
    limit: usize,
    allow_internal_slash: bool,
) -> Result<(), InvalidInput> {
    if s.is_empty() {
        return Err(InvalidInput::Empty { field });
    }
    if s.len() > limit {
        return Err(InvalidInput::TooLong {
            field,
            len: s.len(),
            limit,
        });
    }
    if s.starts_with(char::is_whitespace) || s.ends_with(char::is_whitespace) {
        return Err(InvalidInput::EdgeWhitespace { field });
    }
    for ch in s.chars() {
        if (ch == '/' && !allow_internal_slash) || ch == '\\' {
            return Err(InvalidInput::ReservedChar { field, ch });
        }
    }
    for byte in s.bytes() {
        if byte < 0x20 || byte == 0x7F {
            return Err(InvalidInput::ControlByte { field, byte });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_accepts_typical_inputs() {
        assert!(name("vpc", "prod-vpc").is_ok());
        assert!(name("vpc", "a").is_ok());
        assert!(name("vpc", "café-prod").is_ok()); // internal Unicode fine
        assert!(name("vpc", "v.1_2-3").is_ok());
    }

    #[test]
    fn name_rejects_empty() {
        assert_eq!(name("vpc", ""), Err(InvalidInput::Empty { field: "vpc" }));
    }

    #[test]
    fn name_rejects_slash() {
        assert!(matches!(
            name("vpc", "a/b"),
            Err(InvalidInput::ReservedChar { ch: '/', .. })
        ));
        assert!(matches!(
            name("vpc", "a\\b"),
            Err(InvalidInput::ReservedChar { ch: '\\', .. })
        ));
    }

    #[test]
    fn name_rejects_control_bytes() {
        // NUL byte
        assert!(matches!(
            name("vpc", "a\0b"),
            Err(InvalidInput::ControlByte { byte: 0, .. })
        ));
        // newline
        assert!(matches!(
            name("vpc", "a\nb"),
            Err(InvalidInput::ControlByte { byte: 0x0A, .. })
        ));
        // DEL
        assert!(matches!(
            name("vpc", "a\x7Fb"),
            Err(InvalidInput::ControlByte { byte: 0x7F, .. })
        ));
    }

    #[test]
    fn name_rejects_edge_whitespace() {
        assert!(matches!(
            name("vpc", " prod"),
            Err(InvalidInput::EdgeWhitespace { .. })
        ));
        assert!(matches!(
            name("vpc", "prod "),
            Err(InvalidInput::EdgeWhitespace { .. })
        ));
        // Internal whitespace is fine.
        assert!(name("vpc", "prod vpc").is_ok());
    }

    #[test]
    fn name_rejects_too_long() {
        let s = "a".repeat(MAX_NAME_BYTES + 1);
        assert!(matches!(
            name("vpc", &s),
            Err(InvalidInput::TooLong { limit: MAX_NAME_BYTES, .. })
        ));
        // Exactly at the limit is allowed.
        let s = "a".repeat(MAX_NAME_BYTES);
        assert!(name("vpc", &s).is_ok());
    }

    #[test]
    fn fingerprint_allows_internal_slash() {
        // SSH SHA256 fingerprint: `SHA256:base64...` which can contain `/`.
        assert!(fingerprint("fp", "SHA256:AbCd+/EfGh").is_ok());
    }

    #[test]
    fn mac_accepts_canonical_form() {
        assert!(mac("mac", "aa:bb:cc:dd:ee:ff").is_ok());
        assert!(mac("mac", "00:11:22:33:44:55").is_ok());
    }

    #[test]
    fn mac_rejects_uppercase_and_dashes() {
        assert!(mac("mac", "AA:BB:CC:DD:EE:FF").is_err());
        assert!(mac("mac", "aa-bb-cc-dd-ee-ff").is_err());
    }

    #[test]
    fn mac_rejects_wrong_length() {
        assert!(mac("mac", "aa:bb:cc:dd:ee").is_err());
        assert!(mac("mac", "aa:bb:cc:dd:ee:ff:gg").is_err());
    }

    #[test]
    fn store_error_conversion() {
        let e: StoreError = InvalidInput::Empty { field: "vpc" }.into();
        assert!(matches!(e, StoreError::Conflict(_)));
        // Conflict variant is the right shape: API edge maps to 409.
    }
}
