//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package cloudapi_test

import (
	"encoding/json"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

// Real CloudAPI ListRoles response shape (captured from production).
// Members are structured objects with id, login, type — not plain strings.
func TestRole_UnmarshalStructuredMembers(t *testing.T) {
	raw := `{
		"id": "d906f945-9f88-43b8-9b79-559321bb9b2d",
		"name": "MantaWriter",
		"members": [
			{
				"id": "4f7afff0-a651-c643-ee01-ae536b30b59d",
				"login": "manta.www",
				"type": "subuser"
			}
		],
		"policies": [
			{
				"id": "968a8ee4-6a08-4e0d-b490-f68ff6c3f385",
				"name": "MantaObjRW"
			}
		]
	}`

	var role cloudapi.Role
	if err := json.Unmarshal([]byte(raw), &role); err != nil {
		t.Fatalf("unmarshal Role with structured members: %v", err)
	}

	if role.Name != "MantaWriter" {
		t.Errorf("Name = %q, want %q", role.Name, "MantaWriter")
	}
	if role.Members == nil || len(*role.Members) != 1 {
		t.Fatalf("expected 1 member, got %v", role.Members)
	}
	member := (*role.Members)[0]
	if member.Login == nil || *member.Login != "manta.www" {
		t.Errorf("member.Login = %v, want %q", member.Login, "manta.www")
	}
	memberType, err := member.Type.AsMemberType0()
	if err != nil {
		t.Fatalf("AsMemberType0: %v", err)
	}
	if memberType != cloudapi.MemberType0Subuser {
		t.Errorf("member.Type = %v, want %v", memberType, cloudapi.MemberType0Subuser)
	}
}

func TestRole_UnmarshalEmptyMembers(t *testing.T) {
	raw := `{
		"id": "d7e6ec33-26af-e2e3-c47a-e35b747ed764",
		"name": "PortalUser",
		"members": [],
		"policies": [
			{
				"id": "11279a34-5be5-4990-814a-c64f016b76c3",
				"name": "PortalLogin"
			}
		]
	}`

	var role cloudapi.Role
	if err := json.Unmarshal([]byte(raw), &role); err != nil {
		t.Fatalf("unmarshal Role with empty members: %v", err)
	}

	if role.Name != "PortalUser" {
		t.Errorf("Name = %q, want %q", role.Name, "PortalUser")
	}
	if role.Members == nil || len(*role.Members) != 0 {
		t.Errorf("expected empty members slice, got %v", role.Members)
	}
}

func TestRole_UnmarshalMissingOptionalFields(t *testing.T) {
	raw := `{
		"id": "e581f508-9f24-c038-dff9-ae255fda2a6a",
		"name": "ReadOnly"
	}`

	var role cloudapi.Role
	if err := json.Unmarshal([]byte(raw), &role); err != nil {
		t.Fatalf("unmarshal minimal Role: %v", err)
	}

	if role.Name != "ReadOnly" {
		t.Errorf("Name = %q, want %q", role.Name, "ReadOnly")
	}
	if role.Members != nil {
		t.Errorf("Members should be nil when absent, got %v", role.Members)
	}
	if role.DefaultMembers != nil {
		t.Errorf("DefaultMembers should be nil when absent, got %v", role.DefaultMembers)
	}
}

// Verify that plain string members fail to unmarshal into MemberRef.
// This documents the wire format contract and would have caught the
// original bug.
func TestRole_UnmarshalStringMembersRejected(t *testing.T) {
	raw := `{
		"id": "d906f945-9f88-43b8-9b79-559321bb9b2d",
		"name": "BadFormat",
		"members": ["plainstring"]
	}`

	var role cloudapi.Role
	err := json.Unmarshal([]byte(raw), &role)
	if err == nil {
		t.Fatal("expected error when unmarshaling plain string members, got nil")
	}
}

func TestRole_RoundTrip(t *testing.T) {
	raw := `{
		"id": "d906f945-9f88-43b8-9b79-559321bb9b2d",
		"name": "MantaWriter",
		"members": [
			{
				"id": "4f7afff0-a651-c643-ee01-ae536b30b59d",
				"login": "manta.www",
				"type": "subuser"
			}
		],
		"default_members": [
			{
				"id": "7b2c1e3a-8d4f-4a6b-9c5e-1234567890ab",
				"login": "ops.admin",
				"type": "account",
				"default": true
			}
		],
		"policies": [
			{
				"id": "968a8ee4-6a08-4e0d-b490-f68ff6c3f385",
				"name": "MantaObjRW"
			}
		]
	}`

	var role cloudapi.Role
	if err := json.Unmarshal([]byte(raw), &role); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}

	data, err := json.Marshal(role)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	var role2 cloudapi.Role
	if err := json.Unmarshal(data, &role2); err != nil {
		t.Fatalf("round-trip unmarshal: %v", err)
	}

	if role.Name != role2.Name {
		t.Errorf("Name mismatch: %q vs %q", role.Name, role2.Name)
	}
	if len(*role.Members) != len(*role2.Members) {
		t.Errorf("Members length mismatch: %d vs %d", len(*role.Members), len(*role2.Members))
	}
	if len(*role.DefaultMembers) != len(*role2.DefaultMembers) {
		t.Errorf("DefaultMembers length mismatch: %d vs %d", len(*role.DefaultMembers), len(*role2.DefaultMembers))
	}
}

// ListRoles response is an array of Role objects.
func TestListRoles_UnmarshalArray(t *testing.T) {
	raw := `[
		{
			"id": "d906f945-9f88-43b8-9b79-559321bb9b2d",
			"name": "MantaWriter",
			"members": [
				{"id": "4f7afff0-a651-c643-ee01-ae536b30b59d", "login": "manta.www", "type": "subuser"}
			],
			"policies": [
				{"id": "968a8ee4-6a08-4e0d-b490-f68ff6c3f385", "name": "MantaObjRW"}
			]
		},
		{
			"id": "d7e6ec33-26af-e2e3-c47a-e35b747ed764",
			"name": "PortalUser",
			"members": [],
			"policies": [
				{"id": "11279a34-5be5-4990-814a-c64f016b76c3", "name": "PortalLogin"}
			]
		}
	]`

	var roles []cloudapi.Role
	if err := json.Unmarshal([]byte(raw), &roles); err != nil {
		t.Fatalf("unmarshal []Role: %v", err)
	}

	if len(roles) != 2 {
		t.Fatalf("expected 2 roles, got %d", len(roles))
	}
	if roles[0].Name != "MantaWriter" {
		t.Errorf("roles[0].Name = %q, want %q", roles[0].Name, "MantaWriter")
	}
	if roles[0].Members == nil || len(*roles[0].Members) != 1 {
		t.Fatalf("expected 1 member in roles[0]")
	}
	if roles[1].Name != "PortalUser" {
		t.Errorf("roles[1].Name = %q, want %q", roles[1].Name, "PortalUser")
	}
	if roles[1].Members == nil || len(*roles[1].Members) != 0 {
		t.Errorf("expected empty members in roles[1]")
	}
}
