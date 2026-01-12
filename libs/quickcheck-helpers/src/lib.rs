// Copyright 2019 Joyent, Inc.

/// This module provides helper functions that generate pseudorandom output.
pub mod random {
    use std::iter;

    use quickcheck::Gen;
    use rand::distributions::Alphanumeric;
    use rand::Rng;

    /// Generate a random
    /// [`String`](https://doc.rust-lang.org/std/string/struct.String.html) of
    /// size `len` using the provided generator `g`.
    pub fn string<G: Gen>(g: &mut G, len: usize) -> String {
        iter::repeat(())
            .map(|()| g.sample(Alphanumeric))
            .take(len)
            .collect()
    }
}
