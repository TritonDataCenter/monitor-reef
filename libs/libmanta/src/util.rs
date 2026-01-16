// Copyright 2019 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

use quickcheck::{Arbitrary, Gen};

pub fn random_string(g: &mut Gen, len: usize) -> String {
    (0..len)
        .map(|_| {
            let c = u8::arbitrary(g);
            (b'a' + (c % 26)) as char
        })
        .collect()
}
