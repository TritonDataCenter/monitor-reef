/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

// Allow clippy warnings in generated protobuf code
#![allow(clippy::enum_variant_names)]
#![allow(clippy::derive_partial_eq_without_eq)]
#![allow(dead_code)]

pub mod google {
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

pub mod common {
    tonic::include_proto!("common");
}

pub mod machine {
    tonic::include_proto!("machine");
}

pub mod cluster {
    tonic::include_proto!("cluster");
}
