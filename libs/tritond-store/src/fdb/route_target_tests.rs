// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

use super::*;

fn nat_gateway_record(tenant_id: Uuid, project_id: Uuid, vpc_id: Uuid) -> NatGatewayRecord {
    let now = Utc::now();
    NatGatewayRecord {
        id: Uuid::new_v4(),
        tenant_id,
        project_id,
        vpc_id,
        name: "egress".to_string(),
        description: String::new(),
        family: AddressFamily::V4,
        public_address: "203.0.113.10".parse().unwrap(),
        edge_cluster_id: None,
        desired_generation: 1,
        created_at: now,
        updated_at: now,
    }
}

#[test]
fn nat_gateway_route_target_must_belong_to_requested_vpc_scope() {
    let tenant_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let vpc_id = Uuid::new_v4();
    let nat = nat_gateway_record(tenant_id, project_id, vpc_id);

    FdbStore::validate_nat_gateway_route_target(&nat, tenant_id, project_id, vpc_id)
        .expect("matching NAT gateway scope should be accepted");

    let err = FdbStore::validate_nat_gateway_route_target(
        &nat,
        tenant_id,
        project_id,
        Uuid::new_v4(),
    )
    .expect_err("cross-VPC NAT gateway target should be rejected");
    assert!(matches!(err, StoreError::Conflict(_)));
}
