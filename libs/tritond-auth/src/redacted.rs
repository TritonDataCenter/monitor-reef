// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! [`RedactedString`] — wire-transparent newtype that hides its
//! plaintext from `Debug` and zeroes it on drop.
//!
//! Used for any field that carries a credential we don't want
//! accidentally logged (login passwords, bootstrap-banner password,
//! ad-hoc bearer tokens). Serde is `transparent` so the JSON wire
//! shape is identical to a plain string; `Debug` prints
//! `RedactedString(***)` so a stray `dbg!()` or `tracing::error!`
//! interpolation can't leak the value. `Drop` overwrites the
//! backing buffer via [`zeroize::Zeroize`] so a coredump taken
//! after the value has gone out of scope doesn't carry it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// String that redacts itself from `Debug` and zeroes its memory on
/// drop. Wire-transparent for serde and JsonSchema.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RedactedString(String);

impl RedactedString {
    /// Wrap an existing `String`. The original is moved in; nothing
    /// is copied.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Borrow the underlying plaintext. Callers should pass the
    /// returned `&str` directly into the consumer (bcrypt, the wire,
    /// stderr) rather than copying it into another `String`.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl From<String> for RedactedString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for RedactedString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl PartialEq for RedactedString {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for RedactedString {}

impl std::fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedactedString(***)")
    }
}

impl Drop for RedactedString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_print_value() {
        let secret = RedactedString::new("p@ssw0rd".to_string());
        let debugged = format!("{secret:?}");
        assert!(!debugged.contains("p@ssw0rd"));
        assert!(debugged.contains("***"));
    }

    #[test]
    fn serde_round_trip_is_transparent() {
        let secret = RedactedString::new("p@ssw0rd".to_string());
        let json = serde_json::to_string(&secret).unwrap();
        assert_eq!(json, "\"p@ssw0rd\"");
        let back: RedactedString = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expose(), "p@ssw0rd");
    }

    #[test]
    fn expose_returns_inner_str() {
        let secret = RedactedString::new("hello".to_string());
        assert_eq!(secret.expose(), "hello");
    }
}
