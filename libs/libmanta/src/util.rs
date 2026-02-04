// Copyright 2019 Joyent, Inc.

use quickcheck::Gen;
use rand::distributions::Alphanumeric;
use rand::Rng;
use std::iter;

pub fn random_string<G: Gen>(g: &mut G, len: usize) -> String {
    iter::repeat(())
        .map(|()| g.sample(Alphanumeric))
        .take(len)
        .collect()
}
