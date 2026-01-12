/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2019, Joyent, Inc.
 */

use std::net::IpAddr;
use trust_dns_resolver::Resolver;

///
/// ```
/// use joyent_rust_utils::net::lookup_ip;
/// let ip = lookup_ip("localhost").unwrap();
/// assert_eq!(ip.to_string(), "127.0.0.1");
/// ```
///
pub fn lookup_ip(host: &str) -> Result<IpAddr, String> {
    let resolver = Resolver::from_system_conf().map_err(|e| {
        e.to_string()
    }).unwrap();
    let response = resolver.lookup_ip(host).map_err(|e| {
        e.to_string()
    }).unwrap();
    let ip: Vec<IpAddr> = response.iter().collect();

    Ok(ip[0])
}
