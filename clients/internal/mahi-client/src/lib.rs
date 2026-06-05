// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Mahi (Manta Auth Cache) Client Library
//!
//! This client provides typed access to the Mahi auth cache service. Mahi is a
//! Redis-backed auth cache that mirrors account/user/role/policy data from UFDS
//! and exposes lookup and AWS STS/IAM management endpoints. Used by CloudAPI,
//! Manta, and node-mahi clients.
//!
//! ## Usage
//!
//! ```ignore
//! use mahi_client::Client;
//!
//! let client = Client::new("http://mahi.my-dc.my-cloud.local");
//!
//! // Look up an account by login
//! let auth = client.get_account().login("bob").send().await?;
//!
//! // Look up an account by UUID
//! let auth = client.get_account_by_uuid().accountid(account_id).send().await?;
//! ```
//!
//! ## Endpoints with special handling
//!
//! - `POST /sts/get-caller-identity` returns XML; the generated client surfaces
//!   this as `ResponseValue<ByteStream>` (since the spec declares `text/xml` /
//!   `type: string`). Callers must collect the stream themselves.
//! - `GET /uuids` and `GET /names` accept comma-separated string query
//!   parameters (documented as arrays in the spec) — the generated builder
//!   method `.name(...)` / `.uuid(...)` takes a `String` that callers must
//!   build themselves.

// Allow unwrap in generated code - Progenitor uses it in Client::new()
#[allow(clippy::unwrap_used)]
mod generated;
pub use generated::*;

// Re-export types from the API crate for convenience.
pub use mahi_api::{
    // Path parameter types
    AccessKeyIdPath,
    AccessKeySecret,
    // Common / principal types
    Account,
    AccountIdPath,
    AccountUuidQuery,
    ArnPartition,
    AssumeRoleRequest,
    AssumeRoleResponse,
    AssumeRoleResponseInner,
    AssumeRoleResult,
    AssumedRole,
    AssumedRoleUser,
    AuthInfo,
    AwsAuthResult,
    // STS types
    Caller,
    CallerAccount,
    CallerUser,
    CreateRoleRequest,
    CreateRoleResponse,
    CredentialType,
    DeleteRolePolicyQuery,
    DeleteRolePolicyResponse,
    DeleteRoleResponse,
    GetAccountQuery,
    GetCallerIdentityRequest,
    GetRolePolicyResponse,
    GetRoleResponse,
    GetRolesQuery,
    GetSessionTokenRequest,
    GetSessionTokenResponse,
    GetSessionTokenResponseInner,
    GetSessionTokenResult,
    GetUserQuery,
    // IAM types
    IamRole,
    LegacyAccountPath,
    LegacyUserPath,
    ListRolePoliciesQuery,
    ListRolePoliciesResponse,
    ListRolesQuery,
    ListRolesResponse,
    // Lookup types
    LookupEntry,
    MantaPolicy,
    NameToUuidBody,
    NameToUuidQuery,
    NameToUuidResponse,
    ObjectType,
    ObjectTypeTag,
    Policy,
    PolicyEntry,
    PutRolePolicyRequest,
    PutRolePolicyResponse,
    Role,
    RoleNamePath,
    RolePolicyPath,
    SigV4VerifyQuery,
    SigV4VerifyResult,
    StringOrVec,
    StsCredentials,
    User,
    UserIdPath,
    // Type alias
    Uuid,
    UuidToNameBody,
    UuidToNameQuery,
};
