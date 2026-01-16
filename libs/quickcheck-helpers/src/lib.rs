// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

/// This module provides helper functions that generate pseudorandom output.
pub mod random {
    use quickcheck::{Arbitrary, Gen};

    /// Generate a random [`String`] of size `len` containing only lowercase
    /// alphanumeric characters (a-z, 0-9) using the provided generator `g`.
    pub fn string(g: &mut Gen, len: usize) -> String {
        (0..len)
            .map(|_| {
                let c = u8::arbitrary(g);
                match c % 36 {
                    n @ 0..=25 => (b'a' + n) as char,
                    n => (b'0' + (n - 26)) as char,
                }
            })
            .collect()
    }
}
