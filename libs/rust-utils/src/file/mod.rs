/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2019, Joyent, Inc.
 */

use std::fs;
use md5::{Digest, Md5};

pub fn calculate_md5(file_path: &str) -> String {
    let mut file = match fs::File::open(&file_path) {
        Err(e) => panic!("Error opening file {}", e),
        Ok(file) => file,
    };

    let mut hasher = Md5::new();
    match std::io::copy(&mut file, &mut hasher) {
        Ok(_) => (),
        Err(e) => {
            panic!("Error hashing {}", e);
        }
    };

    base64::encode(&hasher.result())
}
