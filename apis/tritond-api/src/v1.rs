// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Shared wire types for the `/v1/` API surface introduced by
//! RFD 00007.
//!
//! The first stable Triton Cloud API ships at `/v1/`. The legacy
//! `/v2/` paths in this trait stay during AP-3a..AP-3e (the
//! coordinated rollover) and flip to `410 Gone` with a
//! [`RedirectHint`] body at AP-3e.
//!
//! Per RFD 00007 the customer-facing surface is flat with selector
//! query parameters (`?tenant=&project=&image=&cn=` etc.); the
//! operator-cardinal surface lives at `/v1/system/` and is gated by
//! [`Capability`]. The types here are the shared primitives both
//! surfaces consume.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-export `Capability` from the store layer so handler and CLI
// code never has to drift on the wire type. Adding a new variant
// is a fail-loud compile error in the auth-layer match (per the
// types-module comment on the source enum).
pub use tritond_store::Capability;

/// Path parameters for `/v1/instances/{instance}` and its
/// action sub-paths (`/start`, `/stop`, etc.).
///
/// AP-2c ships UUID-only path segments (matches the existing
/// `tenant_id: Uuid` convention everywhere else); AP-3a swaps in a
/// `NameOrId` newtype with a custom `Deserialize` that lands the
/// name-or-uuid dispatch at the extractor edge. The handler then
/// resolves names via the principal's scope per RFD 00007 D-Ap-3.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InstancePath {
    pub instance_id: Uuid,
}

/// Path parameters for `/v1/disks/{disk_id}` (the flat disk-by-id
/// surface introduced in AP-2e).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiskPath {
    pub disk_id: Uuid,
}

/// Path parameters for `/v1/nics/{nic_id}` (AP-2f).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NicPath {
    pub nic_id: Uuid,
}

/// Path parameters for `/v1/vpcs/{vpc_id}` (AP-2g).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct VpcPath {
    pub vpc_id: Uuid,
}

/// Path parameters for `/v1/subnets/{subnet_id}` (AP-2g).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SubnetPath {
    pub subnet_id: Uuid,
}

/// Query parameters for `GET /v1/vpcs?tenant=&project=`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct VpcQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
}

/// Query parameters for `GET /v1/subnets?vpc=<uuid>`. AP-2g requires
/// a `vpc=` selector (subnets are sub-resources of a VPC; cross-VPC
/// subnet scans are deferred).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct SubnetQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to subnets in a given VPC. Required at AP-2g.
    #[serde(default)]
    pub vpc: Option<Uuid>,
}

/// Path parameters for `/v1/floating-ips/{floating_ip_id}` and its
/// `/attach` / `/detach` action sub-paths.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FloatingIpPath {
    pub floating_ip_id: Uuid,
}

/// Path parameters for `/v1/firewall-rules/{firewall_rule_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct FirewallRulePath {
    pub firewall_rule_id: Uuid,
}

/// Path parameters for `/v1/nat-gateways/{nat_gateway_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NatGatewayPath {
    pub nat_gateway_id: Uuid,
}

/// Path parameters for `/v1/route-tables/{route_table_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RouteTablePath {
    pub route_table_id: Uuid,
}

/// Path parameters for `/v1/routes/{route_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RoutePath {
    pub route_id: Uuid,
}

/// Query parameters for `GET /v1/firewall-rules?vpc=<uuid>`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct FirewallRuleQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to firewall rules attached to this VPC. Required.
    #[serde(default)]
    pub vpc: Option<Uuid>,
}

/// Query parameters for `GET /v1/nat-gateways?vpc=<uuid>`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct NatGatewayQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to NAT gateways in this VPC. Required.
    #[serde(default)]
    pub vpc: Option<Uuid>,
}

/// Query parameters for `GET /v1/route-tables?vpc=<uuid>`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct RouteTableQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to route tables in this VPC. Required.
    #[serde(default)]
    pub vpc: Option<Uuid>,
}

/// Query parameters for `GET /v1/routes?route_table=<uuid>`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct RouteQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to routes in this route table. Required.
    #[serde(default)]
    pub route_table: Option<Uuid>,
}

/// Query parameters for `GET /v1/floating-ips?tenant=&project=`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct FloatingIpQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
}

/// Image scope selector for `/v1/images?scope=...`. Per RFD 00007
/// D-Ap-1 + Locked Decision #33 the five legacy image URL surfaces
/// collapse into one path discriminated by this enum. AP-2h ships
/// the `public` variant only; the silo/tenant/project/user variants
/// land when the scope-resolution helper arrives in AP-3a.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ImageScopeSelector {
    Public,
    Silo,
    Tenant,
    Project,
    User,
}

/// Path parameters for `/v1/images/{image_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImagePath {
    pub image_id: Uuid,
}

/// Path parameters for `/v1/ssh-keys/{key_id}`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshKeyPath {
    pub key_id: Uuid,
}

/// Query parameters for `GET /v1/images?scope=&silo=&tenant=&project=`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ImageQuery {
    /// Required at AP-2h: only `public` is accepted. Other scopes
    /// land in AP-3a with the scope-resolution helper.
    pub scope: ImageScopeSelector,
    #[serde(flatten)]
    pub selectors: ScopeSelectors,
}

/// Query parameters for `GET /v1/ssh-keys?scope=&silo=&tenant=&project=`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SshKeyQuery {
    pub scope: ImageScopeSelector,
    #[serde(flatten)]
    pub selectors: ScopeSelectors,
}

/// Query parameters for `GET /v1/nics?...`.
///
/// AP-2f wires the three indexed selectors landed in AP-1c: `subnet=`,
/// `ip=`, and the existing `instance=` membership. Per RFD 00007 02 §1.2
/// `mac=` is bounded-scan within a VPC and is omitted from the v1
/// customer surface today (the operator surface
/// `/v1/system/networking/nics?mac=` will accept it once it ships).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct NicQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to NICs attached to a single instance. Backed by
    /// the existing `nic/in_instance/<instance>/<nic>` index.
    #[serde(default)]
    pub instance: Option<Uuid>,
    /// Restrict to NICs in a single subnet. Backed by AP-1c's
    /// `nic/in_subnet/<subnet>/<nic>` keyspace.
    #[serde(default)]
    pub subnet: Option<Uuid>,
    /// Resolve the unique NIC owning a given IP. Backed by AP-1c's
    /// `nic/by_ip/<ip>` keyspace; returns at most one row.
    #[serde(default)]
    pub ip: Option<std::net::IpAddr>,
}

/// Query parameters for `GET /v1/disks?tenant=&project=&instance=`.
///
/// AP-2e ships the `instance=` reference selector, which drives the
/// per-instance disk view. The `image=` selector lands when the
/// disk index keyspace is extended in a follow-up (the AP-1c
/// `idx/image/...` keyspace today only covers Instance rows).
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct DiskQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to disks attached to a single instance. Backed by
    /// the existing `disk/in_instance/<instance>/<disk>` membership
    /// index.
    #[serde(default)]
    pub instance: Option<Uuid>,
}

/// A resource selector that accepts either a UUID or a name.
///
/// Path segments in the `/v1/` surface (per RFD 00007 D-Ap-3)
/// accept either form transparently. UUID parsing is tried first
/// (a 36-character hyphenated hex string is unambiguous); name
/// lookup is the fallback, and on the customer surface requires
/// enough scope context (silo + tenant + project for tenant
/// resources) to disambiguate.
///
/// Operators type names; automation passes UUIDs; the same endpoint
/// serves both.
///
/// Serialised as a bare string on the wire (`"web-01"` or
/// `"0ab9...d2c4"`); deserialise dispatches by parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NameOrId {
    /// Parsed UUID. Wins on deserialise if the input parses as a UUID.
    Id(Uuid),
    /// Otherwise treat as a name; the handler resolves it against
    /// the principal's scope via
    /// `handlers::selectors::resolve_name_or_id`.
    Name(String),
}

impl NameOrId {
    /// Parse a single string into either an `Id` or a `Name`.
    /// Used by CLI front-ends that take a single positional
    /// argument and need to dispatch on its shape.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        match Uuid::parse_str(input) {
            Ok(u) => NameOrId::Id(u),
            Err(_) => NameOrId::Name(input.to_string()),
        }
    }

    /// Return the UUID if this is an `Id` variant. Useful when the
    /// caller has already resolved a name to a UUID via the store
    /// and wants to short-circuit further work.
    #[must_use]
    pub fn as_uuid(&self) -> Option<Uuid> {
        match self {
            NameOrId::Id(u) => Some(*u),
            NameOrId::Name(_) => None,
        }
    }

    /// Return the name if this is a `Name` variant.
    #[must_use]
    pub fn as_name(&self) -> Option<&str> {
        match self {
            NameOrId::Name(s) => Some(s.as_str()),
            NameOrId::Id(_) => None,
        }
    }
}

impl std::fmt::Display for NameOrId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NameOrId::Id(u) => write!(f, "{u}"),
            NameOrId::Name(s) => f.write_str(s),
        }
    }
}

/// Body of the `410 Gone` response served at every legacy `/v2/`
/// path after the AP-3e cutover commit.
///
/// The hint carries the *new* path a client should call, with the
/// path parameters from the *old* URL echoed into the query string.
/// A confused caller (someone replaying a stale `curl` from notes)
/// gets a clear next step without keeping the old handler alive.
///
/// The 410 body is intentionally machine-readable: `tcadm` and other
/// generated clients can surface a typed retry suggestion rather
/// than just a stringified error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RedirectHint {
    /// HTTP method the caller should use for the replacement.
    pub method: String,
    /// New `/v1/` path.
    pub path: String,
    /// Query parameters to set on the replacement call. Built from
    /// the `(tenant_id, project_id, ...)` segments of the old URL
    /// so a `GET /v2/tenants/T/projects/P/instances` hint carries
    /// `{"tenant": "T", "project": "P"}`.
    pub query_params: std::collections::BTreeMap<String, String>,
}

/// Scope-selector query parameters carried by every customer-facing
/// `/v1/` list endpoint. Per RFD 00007 D-Ap-1 + D-Ap-8.
///
/// Resolution rules (enforced in
/// `services/tritond/src/handlers/selectors.rs`):
///
/// * A principal without `Capability::SystemRead` who omits
///   `tenant` falls back to the principal's tenant. Setting
///   `tenant` to a value outside the principal's tenant returns
///   `404 NotFound` (cross-tenant probe invariant).
/// * A principal without `Capability::SystemRead` who omits
///   `project` returns resources across every project they can see
///   in the resolved tenant.
/// * A `SystemRead`-capable caller may omit any selector for a
///   fleet-wide read.
/// * Setting `silo` on a customer endpoint without `SystemRead`
///   returns `400 ScopeNotAccepted` (typed error, not silent
///   ignore).
///
/// AP-2b ships UUID-only selectors (Dropshot requires scalar query
/// param types). AP-3a swaps these to a `NameOrId` newtype with a
/// custom `Deserialize` that lands the name-or-uuid dispatch at the
/// extractor edge.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ScopeSelectors {
    /// Restrict to a single silo (fleet-admin only).
    #[serde(default)]
    pub silo: Option<Uuid>,
    /// Restrict to a single tenant.
    #[serde(default)]
    pub tenant: Option<Uuid>,
    /// Restrict to a single project.
    #[serde(default)]
    pub project: Option<Uuid>,
}

/// Reference-selector query parameters on `/v1/instances` (and the
/// equivalent fleet-admin `/v1/system/instances`).
///
/// Per RFD 00007 02 §1.2: each selector narrows by a reference the
/// `Instance` row carries to another resource. `image` and `cn` are
/// the two indexed axes (AP-1c keyspaces); `state` is bounded-scan
/// within scope.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct InstanceQuery {
    #[serde(flatten)]
    pub scope: ScopeSelectors,
    /// Restrict to instances whose `image_id` matches. Backed by
    /// `idx/image/<image>/<instance>` in FDB (AP-1c).
    #[serde(default)]
    pub image: Option<Uuid>,
    /// Restrict to instances placed on a single compute node.
    /// Backed by `instance/in_host_cn/<cn>/<instance>` (existing
    /// pre-RFD index; AP-1c added the matching read method).
    #[serde(default)]
    pub cn: Option<Uuid>,
    /// Restrict to instances in a given lifecycle state. Bounded-scan
    /// within the resolved scope (cap [`tritond_store::SCAN_CAP`]);
    /// over-cap returns `400 ScanLimitExceeded`.
    #[serde(default)]
    pub state: Option<String>,
}

/// Paginated wire envelope for `/v1/` list endpoints. Mirrors
/// Oxide's `ResultsPage<T>` (per RFD 00007 D-Ap-5) so a Progenitor
/// regen sees the same shape it's already used to across the Oxide
/// API.
///
/// `items` is the rows for this page; `next_page` is an opaque
/// cursor the caller passes back as `?page_token=...` to fetch the
/// next page, or `None` when the result set is exhausted.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResultsPage<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page: Option<String>,
}

impl<T> ResultsPage<T> {
    /// Construct a page with no continuation cursor. Used by
    /// handlers that don't yet paginate (the result set is bounded
    /// by [`tritond_store::SCAN_CAP`] regardless).
    pub fn single(items: Vec<T>) -> Self {
        Self {
            items,
            next_page: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_or_id_parses_uuid_first() {
        let u = Uuid::new_v4();
        let parsed = NameOrId::parse(&u.to_string());
        assert_eq!(parsed.as_uuid(), Some(u));
        assert!(parsed.as_name().is_none());

        let named = NameOrId::parse("web-01");
        assert!(named.as_uuid().is_none());
        assert_eq!(named.as_name(), Some("web-01"));
    }

    #[test]
    fn name_or_id_round_trips_through_json_untagged() {
        let u = Uuid::new_v4();
        let id_json = serde_json::to_string(&NameOrId::Id(u)).unwrap();
        // Untagged enum: a UUID round-trips as a bare string.
        assert_eq!(id_json, format!("\"{u}\""));
        let back: NameOrId = serde_json::from_str(&id_json).unwrap();
        assert_eq!(back.as_uuid(), Some(u));

        let name_json = serde_json::to_string(&NameOrId::Name("web-01".into())).unwrap();
        assert_eq!(name_json, "\"web-01\"");
        let back: NameOrId = serde_json::from_str(&name_json).unwrap();
        // Note: a plain string that happens not to parse as UUID
        // round-trips as a Name. A string that IS a valid UUID
        // round-trips as Id - that's the untagged dispatch rule.
        assert_eq!(back.as_name(), Some("web-01"));
    }

    #[test]
    fn results_page_omits_next_page_when_none() {
        let page = ResultsPage::single(vec![1u32, 2, 3]);
        let json = serde_json::to_string(&page).unwrap();
        assert!(!json.contains("next_page"), "next_page should be skipped when None: {json}");
        assert!(json.contains("\"items\":[1,2,3]"));
    }

    #[test]
    fn redirect_hint_serialises_v1_path_and_query_params() {
        let mut params = std::collections::BTreeMap::new();
        params.insert("tenant".to_string(), "acme".to_string());
        params.insert("project".to_string(), "prod".to_string());
        let hint = RedirectHint {
            method: "GET".to_string(),
            path: "/v1/instances".to_string(),
            query_params: params,
        };
        let json = serde_json::to_string(&hint).unwrap();
        assert!(json.contains("\"method\":\"GET\""));
        assert!(json.contains("\"path\":\"/v1/instances\""));
        assert!(json.contains("\"tenant\":\"acme\""));
        assert!(json.contains("\"project\":\"prod\""));
    }
}
