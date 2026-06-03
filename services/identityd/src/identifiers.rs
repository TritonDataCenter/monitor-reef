// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Pinned bootstrap identifiers. These are the zero-config "workbench
//! demo" values from the wire contract shared with the BFF lane; both
//! sides hardcode the same constants so the two daemons integrate with
//! no configuration.

use uuid::Uuid;

/// identityd's loopback bind address.
pub const BIND_ADDRESS: &str = "127.0.0.1:8090";

/// Base of every realm issuer URL: `{ISSUER_BASE}/realms/{realm}`.
pub const ISSUER_BASE: &str = "http://127.0.0.1:8090";

/// The one signing key id this dev provider publishes and signs with.
pub const SIGNING_KID: &str = "wb-rsa-1";

/// Access-token TTL (seconds).
pub const ACCESS_TTL_SECS: i64 = 3600;
/// Refresh-token TTL (seconds).
pub const REFRESH_TTL_SECS: i64 = 86_400;

/// Tenant realm id.
pub const TENANT_REALM_ID: Uuid = Uuid::from_u128(0x11111111_1111_4111_8111_111111111111);
/// System realm id.
pub const SYSTEM_REALM_ID: Uuid = Uuid::from_u128(0x00000000_0000_4000_8000_000000000000);

/// Demo tenant id.
pub const TENANT_ID: Uuid = Uuid::from_u128(0x22222222_2222_4222_8222_222222222222);
/// Demo silo id.
pub const SILO_ID: Uuid = Uuid::from_u128(0x33333333_3333_4333_8333_333333333333);

/// Demo user id from the wire contract. The MemStore assigns its own
/// user id at seed time (its `create_user` does not accept an id), so
/// the live `sub` is the store-assigned value, not this. Kept as the
/// documented contract reference; userinfo resolves `sub` back to the
/// same store user, so the token surface stays internally consistent.
#[allow(dead_code)]
pub const DEMO_USER_ID: Uuid = Uuid::from_u128(0x44444444_4444_4444_8444_444444444444);
/// Demo user login.
pub const DEMO_USERNAME: &str = "nwilkens";
/// Demo user password (plaintext; bcrypt-hashed at seed time).
pub const DEMO_PASSWORD: &str = "workbench-demo";
/// Demo user email.
pub const DEMO_EMAIL: &str = "nick@mnx.example";
/// Demo user display name.
pub const DEMO_DISPLAY_NAME: &str = "Nick Wilkens";

/// The Workbench OAuth client id.
pub const CLIENT_ID: &str = "triton-workbench";
/// The Workbench OAuth client secret (plaintext; bcrypt-hashed at seed
/// time). Dev-only.
pub const CLIENT_SECRET: &str = "dev-secret";

/// System-realm operator login (fleet_admin). Lets the admin surface be
/// exercised with a fleet token via the password grant. Dev-only.
pub const OPERATOR_USERNAME: &str = "operator";
/// Operator password (plaintext; bcrypt-hashed at seed time). Dev-only.
pub const OPERATOR_PASSWORD: &str = "operator-demo";
/// Operator email.
pub const OPERATOR_EMAIL: &str = "operator@mnx.example";
/// Operator display name.
pub const OPERATOR_DISPLAY_NAME: &str = "Fleet Operator";

/// The System-realm operator OAuth client id (confidential; password
/// grant), so a fleet token is obtainable in dev.
pub const SYSTEM_CLIENT_ID: &str = "triton-operator";
/// The System-realm operator client secret (plaintext; bcrypt-hashed at
/// seed time). Dev-only.
pub const SYSTEM_CLIENT_SECRET: &str = "operator-secret";

/// Issuer URL for the tenant realm.
#[must_use]
pub fn tenant_issuer_url() -> String {
    realm_issuer_url(TENANT_REALM_ID)
}

/// Issuer URL for an arbitrary realm id.
#[must_use]
pub fn realm_issuer_url(realm: Uuid) -> String {
    format!("{ISSUER_BASE}/realms/{realm}")
}
