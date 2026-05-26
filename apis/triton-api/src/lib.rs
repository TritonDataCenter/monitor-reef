// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Triton API trait definition
//!
//! This crate defines the API trait for the Triton API service.
//! It serves as the public-facing HTTP API for the Triton datacenter.

use dropshot::{
    HttpError, HttpResponseAccepted, HttpResponseCreated, HttpResponseDeleted, HttpResponseHeaders,
    HttpResponseOk, Path, RequestContext, TypedBody, WebsocketChannelResult, WebsocketConnection,
};

pub mod types;
pub use types::*;

/// Triton API
#[dropshot::api_description]
pub trait TritonApi {
    type Context: Send + Sync + 'static;

    /// Ping
    #[endpoint {
        method = GET,
        path = "/v1/ping",
        tags = ["system"],
    }]
    async fn ping(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<PingResponse>, HttpError>;

    /// Authenticate an LDAP user and either issue tokens directly or
    /// require a second factor.
    ///
    /// The response body is a tagged [`LoginOutcome`]:
    ///
    ///   * `complete` — password was correct and the user has no 2FA
    ///     enrolment; the embedded fields are identical to the
    ///     historical [`LoginResponse`] shape and a `Set-Cookie`
    ///     header carries the access token for browser clients.
    ///   * `challenge_required` — password was correct but the user
    ///     has a second factor enrolled; the embedded
    ///     [`LoginChallenge`] carries a `challenge_token` and the
    ///     list of methods the client may use. The client must POST
    ///     the `challenge_token` plus a code to
    ///     `/v1/auth/login/verify`. No cookie is set on this branch
    ///     since the session has not been established yet.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login",
        tags = ["auth"],
    }]
    async fn auth_login(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginRequest>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginOutcome>>, HttpError>;

    /// Complete a 2FA login by presenting the challenge token and a
    /// second-factor code.
    ///
    /// Called only when `/v1/auth/login` returned a
    /// `challenge_required` outcome. The server re-reads the user's
    /// TOTP secret from UFDS (it is never carried in the challenge),
    /// verifies the code, and returns the standard [`LoginResponse`]
    /// — same shape, same `Set-Cookie` semantics — that
    /// `/v1/auth/login` issues for non-2FA users.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login/verify",
        tags = ["auth"],
    }]
    async fn auth_login_verify(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<LoginVerifyRequest>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError>;

    /// Exchange a proof-of-SSH-key-ownership for an access + refresh
    /// token pair. The caller presents an `Authorization: Signature …`
    /// header (draft-cavage HTTP Signature, same dialect cloudapi
    /// accepts). The server resolves the key via mahi, verifies the
    /// signature, and issues tokens via the same path the password
    /// login uses.
    ///
    /// Request body is empty — all auth material is in the headers.
    /// Response mirrors `POST /v1/auth/login`.
    #[endpoint {
        method = POST,
        path = "/v1/auth/login-ssh",
        tags = ["auth"],
    }]
    async fn auth_login_ssh(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LoginResponse>>, HttpError>;

    /// Revoke all outstanding refresh tokens for the caller and clear
    /// the auth cookie. Accepts an expired access token so that callers
    /// whose session has already expired can still log out cleanly.
    #[endpoint {
        method = POST,
        path = "/v1/auth/logout",
        tags = ["auth"],
    }]
    async fn auth_logout(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseHeaders<HttpResponseOk<LogoutResponse>>, HttpError>;

    /// Rotate a refresh token: consume the caller's refresh token and
    /// return a new `(access, refresh)` pair. The old refresh token is
    /// single-use and is invalidated on success.
    #[endpoint {
        method = POST,
        path = "/v1/auth/refresh",
        tags = ["auth"],
    }]
    async fn auth_refresh(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<RefreshRequest>,
    ) -> Result<HttpResponseOk<RefreshResponse>, HttpError>;

    /// Return the authenticated caller's identity, derived from the JWT
    /// claims. Useful for web UIs to check login state on page load.
    #[endpoint {
        method = GET,
        path = "/v1/auth/session",
        tags = ["auth"],
    }]
    async fn auth_session(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<SessionResponse>, HttpError>;

    /// RFC 7517 JWKS document containing the public key(s) used to sign
    /// access tokens. Consumed by external JWT verifiers -- the gateway
    /// today, any future adminui proxy or DC component tomorrow. No auth
    /// required.
    #[endpoint {
        method = GET,
        path = "/v1/auth/jwks.json",
        tags = ["auth"],
    }]
    async fn auth_jwks(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<JwkSet>, HttpError>;

    /// Create a Kelp-managed Kubernetes cluster record.
    ///
    /// Allocates the cluster identifier and stores a record owned by
    /// the authenticated caller. No Triton VMs are provisioned by this
    /// endpoint — bootstrap (a future endpoint) is responsible for
    /// allocating the fabric, provisioning control plane / worker VMs,
    /// and delivering Talos machine configs.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = POST,
        path = "/v1/k8s/clusters",
        tags = ["k8s"],
    }]
    async fn k8s_clusters_create(
        rqctx: RequestContext<Self::Context>,
        body: TypedBody<CreateClusterRequest>,
    ) -> Result<HttpResponseCreated<Cluster>, HttpError>;

    /// List the authenticated caller's cluster records.
    ///
    /// Filtered server-side to records owned by the caller's account;
    /// no cross-account leakage.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = GET,
        path = "/v1/k8s/clusters",
        tags = ["k8s"],
    }]
    async fn k8s_clusters_list(
        rqctx: RequestContext<Self::Context>,
    ) -> Result<HttpResponseOk<ClusterList>, HttpError>;

    /// Fetch a single cluster record by identifier.
    ///
    /// Returns 404 if the cluster does not exist or is owned by a
    /// different account (the two cases are intentionally
    /// indistinguishable to avoid leaking the existence of other
    /// accounts' clusters).
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = GET,
        path = "/v1/k8s/clusters/{cluster}",
        tags = ["k8s"],
    }]
    async fn k8s_clusters_get(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseOk<Cluster>, HttpError>;

    /// Delete a cluster record.
    ///
    /// In Phase 1 this only removes the server-side record; once
    /// bootstrap exists, deletion will also tear down provisioned VMs
    /// and fabric resources. Returns 404 with the same
    /// indistinguishable-not-found semantics as
    /// [`Self::k8s_clusters_get`].
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = DELETE,
        path = "/v1/k8s/clusters/{cluster}",
        tags = ["k8s"],
    }]
    async fn k8s_clusters_delete(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Retrieve the kubeconfig for a running cluster.
    ///
    /// Returns 404 if the cluster does not exist, is owned by a different
    /// account, or has not yet completed bootstrap (kubeconfig not yet
    /// available — the cluster is still `provisioning`).
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = GET,
        path = "/v1/k8s/clusters/{cluster}/kubeconfig",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_kubeconfig(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseOk<KubeconfigResponse>, HttpError>;

    /// Add nodes to a running cluster.
    ///
    /// Each node must already have the relay agent active. The server
    /// applies the supplied Talos machine config in maintenance mode and
    /// triggers a reboot; the node then joins the existing cluster
    /// automatically. Control-plane joiner configs and worker configs are
    /// both accepted — role assignment is determined by the config content,
    /// not enforced server-side.
    ///
    /// Returns 202 Accepted immediately. The node inventory in the cluster
    /// record is updated once configs have been applied.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = POST,
        path = "/v1/k8s/clusters/{cluster}/nodes",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_nodes_add(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<AddNodesRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError>;

    /// Provision and join new worker nodes to a running cluster.
    ///
    /// The server provisions `count` VMs on the cluster's fabric network,
    /// applies the Talos worker config to each in maintenance mode, and
    /// registers them in the cluster record. Returns 202 immediately.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = POST,
        path = "/v1/k8s/clusters/{cluster}/workers",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_workers_add(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<AddWorkersRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError>;

    /// Trigger a rolling Talos OS upgrade across all cluster nodes.
    ///
    /// The cluster must be in `running` state. Control-plane nodes are
    /// upgraded first (sequentially to preserve etcd quorum), then worker
    /// nodes. The `talos_version` field in the cluster record is updated
    /// once all nodes have been upgraded.
    ///
    /// Returns 202 Accepted immediately. Poll
    /// `GET /v1/k8s/clusters/{cluster}` to observe the updated
    /// `talos_version` when the upgrade completes.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = POST,
        path = "/v1/k8s/clusters/{cluster}/upgrade",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_upgrade(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<UpgradeClusterRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError>;

    /// Bootstrap a cluster: apply Talos configs, bootstrap etcd, retrieve
    /// kubeconfig.
    ///
    /// The cluster must be in `created` state. Nodes listed in the request
    /// must already be running the relay agent so the server can reach them
    /// through the registered relay tunnel.
    ///
    /// Returns 202 Accepted with the cluster record in `provisioning` state.
    /// Poll `GET /v1/k8s/clusters/{cluster}` until `state == "running"`.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = POST,
        path = "/v1/k8s/clusters/{cluster}/bootstrap",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_bootstrap(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<BootstrapClusterRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError>;

    /// Install the Triton LB controller into a cluster.
    ///
    /// Discovers CloudAPI configuration server-side, applies RBAC, a
    /// `triton-credentials` Secret, a `triton-lb-controller-config` ConfigMap,
    /// and the controller Deployment to the cluster via the relay tunnel. Polls
    /// until the Deployment is available (180 s timeout).
    ///
    /// Returns 202 Accepted immediately. Poll `GET .../lb` to check readiness.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = POST,
        path = "/v1/k8s/clusters/{cluster}/lb",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_lb_install(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
        body: TypedBody<InstallLbRequest>,
    ) -> Result<HttpResponseAccepted<Cluster>, HttpError>;

    /// Return LB controller status from the cluster.
    ///
    /// Connects to the cluster via relay and reads the `triton-lb-controller`
    /// Deployment in `kube-system`.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = GET,
        path = "/v1/k8s/clusters/{cluster}/lb",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_lb_status(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseOk<LbStatus>, HttpError>;

    /// Remove the LB controller from a cluster.
    ///
    /// Deletes the Deployment, ConfigMap, Secret, ClusterRoleBinding,
    /// ClusterRole, and ServiceAccount from `kube-system`.
    ///
    /// Accepts Bearer JWT or HTTP Signature authentication.
    #[endpoint {
        method = DELETE,
        path = "/v1/k8s/clusters/{cluster}/lb",
        tags = ["k8s"],
    }]
    async fn k8s_cluster_lb_remove(
        rqctx: RequestContext<Self::Context>,
        path: Path<ClusterPath>,
    ) -> Result<HttpResponseDeleted, HttpError>;

    /// Register a relay agent tunnel.
    ///
    /// Called by the gateway-zone agent. The connection is upgraded to
    /// WebSocket and then to a yamux session (server = agent, client = API
    /// server). The API server opens yamux streams when bridge clients request
    /// connections; the agent dials the target on the fabric network and
    /// bridges bytes.
    ///
    /// No authentication in the POC — this is intentional.
    #[channel {
        protocol = WEBSOCKETS,
        path = "/v1/k8s/relay",
        tags = ["k8s-relay"],
    }]
    async fn k8s_relay_register(
        rqctx: RequestContext<Self::Context>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;

    /// Connect to the relay as a bridge client.
    ///
    /// Called by `triton-relay-bridge`. The connection is upgraded to
    /// WebSocket and then to a yamux session (server = API server, client =
    /// bridge). For each inbound stream from the bridge the API server opens
    /// a yamux stream to the registered agent and splices the two together.
    ///
    /// No authentication in the POC — this is intentional.
    #[channel {
        protocol = WEBSOCKETS,
        path = "/v1/k8s/relay/connect",
        tags = ["k8s-relay"],
    }]
    async fn k8s_relay_connect(
        rqctx: RequestContext<Self::Context>,
        upgraded: WebsocketConnection,
    ) -> WebsocketChannelResult;
}
