// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Runtime-dispatch client wrapper for the triton CLI.
//!
//! A `triton` invocation either talks to cloudapi directly (SSH profile,
//! HTTP Signature auth) via `cloudapi_client::TypedClient`, or goes through
//! the triton-gateway (tritonapi profile, Bearer JWT auth) via
//! `triton_gateway_client::TypedClient`. The two clients are
//! separately-generated Progenitor crates, so their builder types are
//! structurally identical but distinct — a trait abstraction can't unify
//! them without Opts-struct indirection, and a runtime enum is cheaper to
//! grow. Command handlers match on the variant and dispatch the builder
//! chain inside each arm; every handler-visible value type
//! (`Vec<Machine>`, `HashMap<String, String>`, ...) is a canonical
//! re-exported API type or `std`, so the handler's post-call logic stays
//! variant-agnostic.
//!
//! This module is the common entrypoint Phase 4 grows. Today only
//! `commands::datacenters` consumes it; other commands still take
//! `&cloudapi_client::TypedClient` directly.

use cloudapi_client::TypedClient as CloudApiTyped;
use triton_gateway_client::TypedClient as GatewayTyped;

/// Either a cloudapi-direct client or a gateway (Bearer JWT) client.
///
/// Both variants carry the profile's `insecure` flag so non-HTTP
/// consumers (WebSocket upgrades, for example) can set up their own
/// TLS stack without reaching back to the profile.
pub enum AnyClient {
    /// SSH profile — talks straight to cloudapi, signs with an SSH key.
    CloudApi {
        client: CloudApiTyped,
        insecure: bool,
    },
    /// Tritonapi profile — talks to the gateway with a Bearer JWT. The
    /// `account` is captured at construction time because the gateway's
    /// Progenitor client doesn't carry it the way cloudapi's `AuthConfig`
    /// does.
    Gateway {
        client: GatewayTyped,
        account: String,
        insecure: bool,
    },
}

impl AnyClient {
    /// Account name the CLI should use in `/{account}/...` path parameters.
    ///
    /// For the gateway variant, callers may alternatively pass `"my"` which
    /// the gateway rewrites to the authenticated user; we prefer the
    /// explicit account since it works for both gateway and cloudapi paths
    /// and makes the wire traffic readable.
    pub fn effective_account(&self) -> &str {
        match self {
            Self::CloudApi { client, .. } => client.effective_account(),
            Self::Gateway { account, .. } => account,
        }
    }

    /// Base URL the HTTP client is talking to (cloudapi URL for SSH
    /// profiles, gateway URL for tritonapi).
    pub fn baseurl(&self) -> &str {
        match self {
            Self::CloudApi { client, .. } => {
                use cloudapi_client::ClientInfo;
                client.inner().baseurl()
            }
            Self::Gateway { client, .. } => client.baseurl(),
        }
    }

    /// Whether this client was built with TLS verification disabled.
    /// Used by out-of-band consumers (WebSocket upgrades) that need to
    /// construct their own TLS stack.
    pub fn insecure(&self) -> bool {
        match self {
            Self::CloudApi { insecure, .. } | Self::Gateway { insecure, .. } => *insecure,
        }
    }
}

/// Dispatch a block across both variants of [`AnyClient`], binding the
/// inner typed client to an identifier.
///
/// The block is textually substituted into each match arm, so the bound
/// identifier refers to a *different Rust type* in each arm (the cloudapi
/// vs gateway Progenitor `TypedClient`). Both arms must typecheck
/// independently; this macro isn't polymorphism, it's duplication with a
/// single caller-visible body. It exists because the two generated clients
/// have structurally identical but nominally distinct builder types that
/// no trait signature can unify without Opts-struct indirection.
///
/// # Example
///
/// ```ignore
/// use crate::dispatch;
///
/// let dcs = dispatch!(client, |c| {
///     c.inner()
///         .list_datacenters()
///         .account(account)
///         .send()
///         .await?
///         .into_inner()
/// });
/// ```
#[macro_export]
macro_rules! dispatch {
    ($client:expr, |$c:ident| $body:block) => {
        match $client {
            $crate::client::AnyClient::CloudApi { client: $c, .. } => $body,
            $crate::client::AnyClient::Gateway { client: $c, .. } => $body,
        }
    };
}

/// Variant of [`dispatch!`] that takes a second `types:` path token,
/// substituted literally into each arm. Lets the body name per-client
/// Progenitor types (e.g. `$t::MigrateRequest`) without needing a single
/// textually shared type path.
///
/// # Example
///
/// ```ignore
/// dispatch_with_types!(client, |c, t| {
///     let body = t::MigrateRequestBuilder::default()
///         .action(t::MigrationAction::Begin)
///         .try_into()?;
///     c.inner().migrate().body(body).send().await?.into_inner()
/// });
/// ```
#[macro_export]
macro_rules! dispatch_with_types {
    ($client:expr, |$c:ident, $t:ident| $body:block) => {
        match $client {
            $crate::client::AnyClient::CloudApi { client: $c, .. } => {
                #[allow(unused_imports)]
                use cloudapi_client::types as $t;
                $body
            }
            $crate::client::AnyClient::Gateway { client: $c, .. } => {
                #[allow(unused_imports)]
                use triton_gateway_client::types as $t;
                $body
            }
        }
    };
}
