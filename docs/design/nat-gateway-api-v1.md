# NAT Gateway API v1

This note records the first MVP slice for north/south egress: exposing the
already-modeled `NatGateway` resource through tritond and `tcadm`.

## Scope

`NatGateway` is a tenant/project/VPC child resource:

```text
/v2/tenants/{tenant_id}/projects/{project_id}/vpcs/{vpc_id}/nat-gateways
```

The v1 API supports:

- `GET` collection: list NAT gateways in a VPC.
- `POST` collection: create a NAT gateway with `name`, optional
  `description`, and `family`.
- `GET` item: read one NAT gateway by id.
- `DELETE` item: delete one NAT gateway and release its public address.

The `tcadm` shorthand is:

```text
tcadm net nat-gw list <tenant_id> <project_id> <vpc_id>
tcadm net nat-gw create <tenant_id> <project_id> <vpc_id> --name <name> [--family v4|v6]
tcadm net nat-gw get <tenant_id> <project_id> <vpc_id> <nat_gateway_id>
tcadm net nat-gw delete <tenant_id> <project_id> <vpc_id> <nat_gateway_id>
```

## Contract

Create-time parentage comes only from the URL path. The response includes:

- `public_address`: the reserved public source address for egress.
- `desired_generation`: the generation tritond wants realized.
- `realized`: the current realization roll-up, initially unapplied.
- `edge_cluster_id`: `null` until edge placement lands.

The handler verifies every parent id on read and delete. A NAT gateway that
exists in another tenant, project, or VPC is returned as `404`.

Tenant members may manage NAT gateways only inside their own tenant. Anonymous
callers are denied through the same masked `404` behavior used by VPCs and
subnets.

## MVP Notes

The v1 dataplane backend remains `backend: "nftables"` in the edge manifest
contract. The `NatGateway` API intentionally does not expose nftables-specific
fields, so a later empirical move to `backend: "afxdp"` can be added without
changing this resource shape.

Tritond renders edge manifests through a pure function:

```text
render_edge_manifest(NatGateway, EdgeManifestBindings, EdgeManifestPlacement)
    -> edge_manifest::Manifest
```

`EdgeManifestBindings` carries the route-derived SNAT source CIDRs plus
resolved Floating IP bindings. Cn-terminated Floating IPs are accepted by the
renderer but do not emit `dataplane.fips` rules; edge-terminated bindings emit
the fhrun `external -> internal` mapping. `EdgeManifestPlacement` supplies the
edge instance id, firehyve/fhrun paths, explicit north/south NIC coordinates,
and the host Unix control socket path that fhrun bridges into the guest as
`/dev/hvc0` using `triton.edge.control.v1`.
