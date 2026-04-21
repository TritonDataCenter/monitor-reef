//go:build integration

//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package cloudapi_test

import (
	"context"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

func TestIntegration_ListPolicies(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListPoliciesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListPolicies: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)
}

func TestIntegration_HeadPolicies(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadPoliciesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadPolicies: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_ListRoles(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListRolesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListRoles: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)
}

func TestIntegration_HeadRoles(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadRolesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadRoles: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_RBAC_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	// --- Create Policy ---
	policyName := randName("intpol")
	createPolResp, err := testClient.CreatePolicyWithResponse(ctx, testAccount, cloudapi.CreatePolicyJSONRequestBody{
		Name:  policyName,
		Rules: []string{"CAN listmachines"},
	})
	if err != nil {
		t.Fatalf("CreatePolicy: %v", err)
	}
	requireOK(t, createPolResp.StatusCode(), createPolResp.Body)

	if createPolResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createPolResp.StatusCode(), string(createPolResp.Body))
	}
	policyID := createPolResp.JSON201.ID.String()

	t.Cleanup(func() {
		_, _ = testClient.DeletePolicyWithResponse(context.Background(), testAccount, policyID)
	})

	// Get policy.
	getPolResp, err := testClient.GetPolicyWithResponse(ctx, testAccount, policyID)
	if err != nil {
		t.Fatalf("GetPolicy: %v", err)
	}
	requireOK(t, getPolResp.StatusCode(), getPolResp.Body)
	if getPolResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}

	// Head policy.
	headPolResp, err := testClient.HeadPolicyWithResponse(ctx, testAccount, policyID)
	if err != nil {
		t.Fatalf("HeadPolicy: %v", err)
	}
	requireOK(t, headPolResp.StatusCode(), nil)

	// Update policy description.
	polDesc := "updated policy"
	updatePolResp, err := testClient.UpdatePolicyWithResponse(ctx, testAccount, policyID, cloudapi.UpdatePolicyJSONRequestBody{
		Description: &polDesc,
	})
	if err != nil {
		t.Fatalf("UpdatePolicy: %v", err)
	}
	requireOK(t, updatePolResp.StatusCode(), updatePolResp.Body)

	// --- Create Role ---
	roleName := randName("introle")
	createRoleResp, err := testClient.CreateRoleWithResponse(ctx, testAccount, cloudapi.CreateRoleJSONRequestBody{
		Name: roleName,
		Policies: &[]cloudapi.PolicyRef{
			{Name: &policyName},
		},
	})
	if err != nil {
		t.Fatalf("CreateRole: %v", err)
	}
	requireOK(t, createRoleResp.StatusCode(), createRoleResp.Body)

	if createRoleResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createRoleResp.StatusCode(), string(createRoleResp.Body))
	}
	roleID := createRoleResp.JSON201.ID.String()

	t.Cleanup(func() {
		_, _ = testClient.DeleteRoleWithResponse(context.Background(), testAccount, roleID)
	})

	// Get role.
	getRoleResp, err := testClient.GetRoleWithResponse(ctx, testAccount, roleID)
	if err != nil {
		t.Fatalf("GetRole: %v", err)
	}
	requireOK(t, getRoleResp.StatusCode(), getRoleResp.Body)
	if getRoleResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}

	// Head role.
	headRoleResp, err := testClient.HeadRoleWithResponse(ctx, testAccount, roleID)
	if err != nil {
		t.Fatalf("HeadRole: %v", err)
	}
	requireOK(t, headRoleResp.StatusCode(), nil)

	// Update role.
	roleDesc := "updated role"
	_ = roleDesc // UpdateRoleRequest doesn't have description, update name instead.
	newRoleName := randName("introle2")
	updateRoleResp, err := testClient.UpdateRoleWithResponse(ctx, testAccount, roleID, cloudapi.UpdateRoleJSONRequestBody{
		Name: &newRoleName,
	})
	if err != nil {
		t.Fatalf("UpdateRole: %v", err)
	}
	requireOK(t, updateRoleResp.StatusCode(), updateRoleResp.Body)
	// Use updated name for role-tags.
	roleName = newRoleName

	// --- RoleTags on collection endpoints ---
	t.Run("RoleTags_Collections", func(t *testing.T) {
		emptyTags := cloudapi.ReplaceRoleTagsRequest{RoleTag: &[]string{}}
		roleTags := cloudapi.ReplaceRoleTagsRequest{RoleTag: &[]string{roleName}}

		// Account.
		resp, err := testClient.ReplaceAccountRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceAccountRoleTags: %v", err)
		}
		requireOK(t, resp.StatusCode(), resp.Body)
		// Reset.
		_, _ = testClient.ReplaceAccountRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Datacenters collection.
		resp2, err := testClient.ReplaceDatacentersCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceDatacentersCollectionRoleTags: %v", err)
		}
		requireOK(t, resp2.StatusCode(), resp2.Body)
		_, _ = testClient.ReplaceDatacentersCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Firewall rules collection.
		resp3, err := testClient.ReplaceFwrulesCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceFwrulesCollectionRoleTags: %v", err)
		}
		requireOK(t, resp3.StatusCode(), resp3.Body)
		_, _ = testClient.ReplaceFwrulesCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Images collection.
		resp4, err := testClient.ReplaceImagesCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceImagesCollectionRoleTags: %v", err)
		}
		requireOK(t, resp4.StatusCode(), resp4.Body)
		_, _ = testClient.ReplaceImagesCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Keys collection.
		resp5, err := testClient.ReplaceKeysCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceKeysCollectionRoleTags: %v", err)
		}
		requireOK(t, resp5.StatusCode(), resp5.Body)
		_, _ = testClient.ReplaceKeysCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Networks collection.
		resp6, err := testClient.ReplaceNetworksCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceNetworksCollectionRoleTags: %v", err)
		}
		requireOK(t, resp6.StatusCode(), resp6.Body)
		_, _ = testClient.ReplaceNetworksCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Packages collection.
		resp7, err := testClient.ReplacePackagesCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplacePackagesCollectionRoleTags: %v", err)
		}
		requireOK(t, resp7.StatusCode(), resp7.Body)
		_, _ = testClient.ReplacePackagesCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Policies collection.
		resp8, err := testClient.ReplacePoliciesCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplacePoliciesCollectionRoleTags: %v", err)
		}
		requireOK(t, resp8.StatusCode(), resp8.Body)
		_, _ = testClient.ReplacePoliciesCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Roles collection.
		resp9, err := testClient.ReplaceRolesCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceRolesCollectionRoleTags: %v", err)
		}
		requireOK(t, resp9.StatusCode(), resp9.Body)
		_, _ = testClient.ReplaceRolesCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Services collection.
		resp10, err := testClient.ReplaceServicesCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceServicesCollectionRoleTags: %v", err)
		}
		requireOK(t, resp10.StatusCode(), resp10.Body)
		_, _ = testClient.ReplaceServicesCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)

		// Users collection.
		resp11, err := testClient.ReplaceUsersCollectionRoleTagsWithResponse(ctx, testAccount, roleTags)
		if err != nil {
			t.Fatalf("ReplaceUsersCollectionRoleTags: %v", err)
		}
		requireOK(t, resp11.StatusCode(), resp11.Body)
		_, _ = testClient.ReplaceUsersCollectionRoleTagsWithResponse(ctx, testAccount, emptyTags)
	})

	// --- RoleTags on individual resources ---
	t.Run("RoleTags_Individual", func(t *testing.T) {
		emptyTags := cloudapi.ReplaceRoleTagsRequest{RoleTag: &[]string{}}
		roleTags := cloudapi.ReplaceRoleTagsRequest{RoleTag: &[]string{roleName}}

		// Policy role-tags.
		resp, err := testClient.ReplacePolicyRoleTagsWithResponse(ctx, testAccount, policyID, roleTags)
		if err != nil {
			t.Fatalf("ReplacePolicyRoleTags: %v", err)
		}
		requireOK(t, resp.StatusCode(), resp.Body)
		_, _ = testClient.ReplacePolicyRoleTagsWithResponse(ctx, testAccount, policyID, emptyTags)

		// Role role-tags.
		resp2, err := testClient.ReplaceRoleRoleTagsWithResponse(ctx, testAccount, roleID, roleTags)
		if err != nil {
			t.Fatalf("ReplaceRoleRoleTags: %v", err)
		}
		requireOK(t, resp2.StatusCode(), resp2.Body)
		_, _ = testClient.ReplaceRoleRoleTagsWithResponse(ctx, testAccount, roleID, emptyTags)

		// Network role-tags (pick first network).
		netResp, err := testClient.ListNetworksWithResponse(ctx, testAccount)
		if err != nil {
			t.Fatalf("ListNetworks: %v", err)
		}
		if netResp.JSON200 != nil && len(*netResp.JSON200) > 0 {
			netID := (*netResp.JSON200)[0].ID
			resp3, err := testClient.ReplaceNetworkRoleTagsWithResponse(ctx, testAccount, netID, roleTags)
			if err != nil {
				t.Fatalf("ReplaceNetworkRoleTags: %v", err)
			}
			requireOK(t, resp3.StatusCode(), resp3.Body)
			_, _ = testClient.ReplaceNetworkRoleTagsWithResponse(ctx, testAccount, netID, emptyTags)
		}

		// Package role-tags (pick first package).
		pkgResp, err := testClient.ListPackagesWithResponse(ctx, testAccount)
		if err != nil {
			t.Fatalf("ListPackages: %v", err)
		}
		if pkgResp.JSON200 != nil && len(*pkgResp.JSON200) > 0 {
			pkgName := (*pkgResp.JSON200)[0].Name
			resp4, err := testClient.ReplacePackageRoleTagsWithResponse(ctx, testAccount, pkgName, roleTags)
			if err != nil {
				t.Fatalf("ReplacePackageRoleTags: %v", err)
			}
			requireOK(t, resp4.StatusCode(), resp4.Body)
			_, _ = testClient.ReplacePackageRoleTagsWithResponse(ctx, testAccount, pkgName, emptyTags)
		}

		// Image role-tags (pick first image).
		imgResp, err := testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{})
		if err != nil {
			t.Fatalf("ListImages: %v", err)
		}
		if imgResp.JSON200 != nil && len(*imgResp.JSON200) > 0 {
			imgID := (*imgResp.JSON200)[0].ID
			resp5, err := testClient.ReplaceImageRoleTagsWithResponse(ctx, testAccount, imgID, roleTags)
			if err != nil {
				t.Fatalf("ReplaceImageRoleTags: %v", err)
			}
			requireOK(t, resp5.StatusCode(), resp5.Body)
			_, _ = testClient.ReplaceImageRoleTagsWithResponse(ctx, testAccount, imgID, emptyTags)
		}

		// Firewall rule role-tags (pick first rule).
		fwResp, err := testClient.ListFirewallRulesWithResponse(ctx, testAccount)
		if err != nil {
			t.Fatalf("ListFirewallRules: %v", err)
		}
		if fwResp.JSON200 != nil && len(*fwResp.JSON200) > 0 {
			fwID := (*fwResp.JSON200)[0].ID
			resp6, err := testClient.ReplaceFwruleRoleTagsWithResponse(ctx, testAccount, fwID, roleTags)
			if err != nil {
				t.Fatalf("ReplaceFwruleRoleTags: %v", err)
			}
			requireOK(t, resp6.StatusCode(), resp6.Body)
			_, _ = testClient.ReplaceFwruleRoleTagsWithResponse(ctx, testAccount, fwID, emptyTags)
		}

		// Key role-tags (pick first key).
		keyResp, err := testClient.ListKeysWithResponse(ctx, testAccount)
		if err != nil {
			t.Fatalf("ListKeys: %v", err)
		}
		if keyResp.JSON200 != nil && len(*keyResp.JSON200) > 0 {
			keyName := (*keyResp.JSON200)[0].Name
			resp7, err := testClient.ReplaceKeyRoleTagsWithResponse(ctx, testAccount, keyName, roleTags)
			if err != nil {
				t.Fatalf("ReplaceKeyRoleTags: %v", err)
			}
			requireOK(t, resp7.StatusCode(), resp7.Body)
			_, _ = testClient.ReplaceKeyRoleTagsWithResponse(ctx, testAccount, keyName, emptyTags)
		}

		// Machine role-tags (pick first machine).
		machResp, err := testClient.ListMachinesWithResponse(ctx, testAccount, &cloudapi.ListMachinesParams{})
		if err != nil {
			t.Fatalf("ListMachines: %v", err)
		}
		if machResp.JSON200 != nil && len(*machResp.JSON200) > 0 {
			machID := (*machResp.JSON200)[0].ID
			resp8, err := testClient.ReplaceMachineRoleTagsWithResponse(ctx, testAccount, machID, roleTags)
			if err != nil {
				t.Fatalf("ReplaceMachineRoleTags: %v", err)
			}
			requireOK(t, resp8.StatusCode(), resp8.Body)
			_, _ = testClient.ReplaceMachineRoleTagsWithResponse(ctx, testAccount, machID, emptyTags)
		}

		// User role-tags (use account owner — the testAccount itself acts as a user).
		// Note: ReplaceUserRoleTags takes a UUID string. Get it from ListUsers.
		usersResp, err := testClient.ListUsersWithResponse(ctx, testAccount)
		if err != nil {
			t.Fatalf("ListUsers: %v", err)
		}
		if usersResp.JSON200 != nil && len(*usersResp.JSON200) > 0 {
			userUUID := (*usersResp.JSON200)[0].ID.String()
			resp9, err := testClient.ReplaceUserRoleTagsWithResponse(ctx, testAccount, userUUID, roleTags)
			if err != nil {
				t.Fatalf("ReplaceUserRoleTags: %v", err)
			}
			requireOK(t, resp9.StatusCode(), resp9.Body)
			_, _ = testClient.ReplaceUserRoleTagsWithResponse(ctx, testAccount, userUUID, emptyTags)
		}
	})

	// --- Delete role and policy ---
	delRoleResp, err := testClient.DeleteRoleWithResponse(ctx, testAccount, roleID)
	if err != nil {
		t.Fatalf("DeleteRole: %v", err)
	}
	requireOK(t, delRoleResp.StatusCode(), delRoleResp.Body)

	delPolResp, err := testClient.DeletePolicyWithResponse(ctx, testAccount, policyID)
	if err != nil {
		t.Fatalf("DeletePolicy: %v", err)
	}
	requireOK(t, delPolResp.StatusCode(), delPolResp.Body)
}
