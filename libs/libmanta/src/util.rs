// Copyright 2019 Joyent, Inc.

use quickcheck::Gen;
use rand::Rng;
use rand::distr::Alphanumeric;

pub fn random_string(_g: &mut Gen, len: usize) -> String {
    rand::rng()
        .sample_iter(Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}
