//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package cloudapi_test

import (
	"net/http"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

func TestResponseStatus(t *testing.T) {
	resp := &http.Response{
		StatusCode: 200,
		Status:     "200 OK",
	}

	type testCase struct {
		name     string
		statusFn func(*http.Response) string
	}

	cases := []testCase{
		{name: "GetAccountResponse", statusFn: func(r *http.Response) string { return cloudapi.GetAccountResponse{HTTPResponse: r}.Status() }},
		{name: "HeadAccountResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadAccountResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateAccountResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateAccountResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceAccountRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceAccountRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListAccessKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.ListAccessKeysResponse{HTTPResponse: r}.Status() }},
		{name: "HeadAccessKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadAccessKeysResponse{HTTPResponse: r}.Status() }},
		{name: "CreateAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "GetAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.GetAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "HeadAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "GetChangefeedResponse", statusFn: func(r *http.Response) string { return cloudapi.GetChangefeedResponse{HTTPResponse: r}.Status() }},
		{name: "GetConfigResponse", statusFn: func(r *http.Response) string { return cloudapi.GetConfigResponse{HTTPResponse: r}.Status() }},
		{name: "HeadConfigResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadConfigResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateConfigResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateConfigResponse{HTTPResponse: r}.Status() }},
		{name: "ListDatacentersResponse", statusFn: func(r *http.Response) string { return cloudapi.ListDatacentersResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceDatacentersCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceDatacentersCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "GetDatacenterResponse", statusFn: func(r *http.Response) string { return cloudapi.GetDatacenterResponse{HTTPResponse: r}.Status() }},
		{name: "ListFabricVlansResponse", statusFn: func(r *http.Response) string { return cloudapi.ListFabricVlansResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFabricVlansResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFabricVlansResponse{HTTPResponse: r}.Status() }},
		{name: "CreateFabricVlanResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateFabricVlanResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteFabricVlanResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteFabricVlanResponse{HTTPResponse: r}.Status() }},
		{name: "GetFabricVlanResponse", statusFn: func(r *http.Response) string { return cloudapi.GetFabricVlanResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFabricVlanResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFabricVlanResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateFabricVlanResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateFabricVlanResponse{HTTPResponse: r}.Status() }},
		{name: "ListFabricNetworksResponse", statusFn: func(r *http.Response) string { return cloudapi.ListFabricNetworksResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFabricNetworksResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFabricNetworksResponse{HTTPResponse: r}.Status() }},
		{name: "CreateFabricNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateFabricNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteFabricNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteFabricNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "GetFabricNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.GetFabricNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFabricNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFabricNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateFabricNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateFabricNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "ListForeignDatacentersResponse", statusFn: func(r *http.Response) string { return cloudapi.ListForeignDatacentersResponse{HTTPResponse: r}.Status() }},
		{name: "AddForeignDatacenterResponse", statusFn: func(r *http.Response) string { return cloudapi.AddForeignDatacenterResponse{HTTPResponse: r}.Status() }},
		{name: "ListFirewallRulesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListFirewallRulesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFirewallRulesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFirewallRulesResponse{HTTPResponse: r}.Status() }},
		{name: "CreateFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceFwrulesCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceFwrulesCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "GetFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.GetFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceFwruleRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceFwruleRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DisableFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.DisableFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "EnableFirewallRuleResponse", statusFn: func(r *http.Response) string { return cloudapi.EnableFirewallRuleResponse{HTTPResponse: r}.Status() }},
		{name: "ListFirewallRuleMachinesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListFirewallRuleMachinesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadFirewallRuleMachinesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadFirewallRuleMachinesResponse{HTTPResponse: r}.Status() }},
		{name: "ListImagesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListImagesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadImagesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadImagesResponse{HTTPResponse: r}.Status() }},
		{name: "CreateOrImportImageResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateOrImportImageResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceImagesCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceImagesCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteImageResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteImageResponse{HTTPResponse: r}.Status() }},
		{name: "GetImageResponse", statusFn: func(r *http.Response) string { return cloudapi.GetImageResponse{HTTPResponse: r}.Status() }},
		{name: "HeadImageResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadImageResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateImageResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateImageResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceImageRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceImageRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.ListKeysResponse{HTTPResponse: r}.Status() }},
		{name: "HeadKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadKeysResponse{HTTPResponse: r}.Status() }},
		{name: "CreateKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateKeyResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceKeysCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceKeysCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteKeyResponse{HTTPResponse: r}.Status() }},
		{name: "GetKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.GetKeyResponse{HTTPResponse: r}.Status() }},
		{name: "HeadKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadKeyResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceKeyRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceKeyRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "GetProvisioningLimitsResponse", statusFn: func(r *http.Response) string { return cloudapi.GetProvisioningLimitsResponse{HTTPResponse: r}.Status() }},
		{name: "ListMachinesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMachinesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachinesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachinesResponse{HTTPResponse: r}.Status() }},
		{name: "CreateMachineResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateMachineResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteMachineResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteMachineResponse{HTTPResponse: r}.Status() }},
		{name: "GetMachineResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMachineResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateMachineResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateMachineResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceMachineRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceMachineRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "MachineAuditResponse", statusFn: func(r *http.Response) string { return cloudapi.MachineAuditResponse{HTTPResponse: r}.Status() }},
		{name: "HeadAuditResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadAuditResponse{HTTPResponse: r}.Status() }},
		{name: "ListMachineDisksResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMachineDisksResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineDisksResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineDisksResponse{HTTPResponse: r}.Status() }},
		{name: "CreateMachineDiskResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateMachineDiskResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteMachineDiskResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteMachineDiskResponse{HTTPResponse: r}.Status() }},
		{name: "GetMachineDiskResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMachineDiskResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineDiskResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineDiskResponse{HTTPResponse: r}.Status() }},
		{name: "ResizeMachineDiskResponse", statusFn: func(r *http.Response) string { return cloudapi.ResizeMachineDiskResponse{HTTPResponse: r}.Status() }},
		{name: "ListMachineFirewallRulesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMachineFirewallRulesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineFirewallRulesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineFirewallRulesResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteAllMachineMetadataResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteAllMachineMetadataResponse{HTTPResponse: r}.Status() }},
		{name: "ListMachineMetadataResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMachineMetadataResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineMetadataResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineMetadataResponse{HTTPResponse: r}.Status() }},
		{name: "AddMachineMetadataResponse", statusFn: func(r *http.Response) string { return cloudapi.AddMachineMetadataResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteMachineMetadataResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteMachineMetadataResponse{HTTPResponse: r}.Status() }},
		{name: "GetMachineMetadataResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMachineMetadataResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineMetadataKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineMetadataKeyResponse{HTTPResponse: r}.Status() }},
		{name: "MigrateMachineEstimateResponse", statusFn: func(r *http.Response) string { return cloudapi.MigrateMachineEstimateResponse{HTTPResponse: r}.Status() }},
		{name: "MigrateResponse", statusFn: func(r *http.Response) string { return cloudapi.MigrateResponse{HTTPResponse: r}.Status() }},
		{name: "ListNicsResponse", statusFn: func(r *http.Response) string { return cloudapi.ListNicsResponse{HTTPResponse: r}.Status() }},
		{name: "HeadNicsResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadNicsResponse{HTTPResponse: r}.Status() }},
		{name: "AddNicResponse", statusFn: func(r *http.Response) string { return cloudapi.AddNicResponse{HTTPResponse: r}.Status() }},
		{name: "RemoveNicResponse", statusFn: func(r *http.Response) string { return cloudapi.RemoveNicResponse{HTTPResponse: r}.Status() }},
		{name: "GetNicResponse", statusFn: func(r *http.Response) string { return cloudapi.GetNicResponse{HTTPResponse: r}.Status() }},
		{name: "HeadNicResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadNicResponse{HTTPResponse: r}.Status() }},
		{name: "ListMachineSnapshotsResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMachineSnapshotsResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineSnapshotsResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineSnapshotsResponse{HTTPResponse: r}.Status() }},
		{name: "CreateMachineSnapshotResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateMachineSnapshotResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteMachineSnapshotResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteMachineSnapshotResponse{HTTPResponse: r}.Status() }},
		{name: "GetMachineSnapshotResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMachineSnapshotResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineSnapshotResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineSnapshotResponse{HTTPResponse: r}.Status() }},
		{name: "StartMachineFromSnapshotResponse", statusFn: func(r *http.Response) string { return cloudapi.StartMachineFromSnapshotResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteMachineTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteMachineTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListMachineTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMachineTagsResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineTagsResponse{HTTPResponse: r}.Status() }},
		{name: "AddMachineTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.AddMachineTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceMachineTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceMachineTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteMachineTagResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteMachineTagResponse{HTTPResponse: r}.Status() }},
		{name: "GetMachineTagResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMachineTagResponse{HTTPResponse: r}.Status() }},
		{name: "HeadMachineTagResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadMachineTagResponse{HTTPResponse: r}.Status() }},
		{name: "GetMachineVncResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMachineVncResponse{HTTPResponse: r}.Status() }},
		{name: "ListMigrationsResponse", statusFn: func(r *http.Response) string { return cloudapi.ListMigrationsResponse{HTTPResponse: r}.Status() }},
		{name: "GetMigrationResponse", statusFn: func(r *http.Response) string { return cloudapi.GetMigrationResponse{HTTPResponse: r}.Status() }},
		{name: "WatchMigrationResponse", statusFn: func(r *http.Response) string { return cloudapi.WatchMigrationResponse{HTTPResponse: r}.Status() }},
		{name: "ListNetworksResponse", statusFn: func(r *http.Response) string { return cloudapi.ListNetworksResponse{HTTPResponse: r}.Status() }},
		{name: "HeadNetworksResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadNetworksResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceNetworksCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceNetworksCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "GetNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.GetNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "HeadNetworkResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadNetworkResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceNetworkRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceNetworkRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListNetworkIpsResponse", statusFn: func(r *http.Response) string { return cloudapi.ListNetworkIpsResponse{HTTPResponse: r}.Status() }},
		{name: "HeadNetworkIpsResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadNetworkIpsResponse{HTTPResponse: r}.Status() }},
		{name: "GetNetworkIPResponse", statusFn: func(r *http.Response) string { return cloudapi.GetNetworkIPResponse{HTTPResponse: r}.Status() }},
		{name: "HeadNetworkIPResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadNetworkIPResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateNetworkIPResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateNetworkIPResponse{HTTPResponse: r}.Status() }},
		{name: "ListPackagesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListPackagesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadPackagesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadPackagesResponse{HTTPResponse: r}.Status() }},
		{name: "ReplacePackagesCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplacePackagesCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "GetPackageResponse", statusFn: func(r *http.Response) string { return cloudapi.GetPackageResponse{HTTPResponse: r}.Status() }},
		{name: "HeadPackageResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadPackageResponse{HTTPResponse: r}.Status() }},
		{name: "ReplacePackageRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplacePackageRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListPoliciesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListPoliciesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadPoliciesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadPoliciesResponse{HTTPResponse: r}.Status() }},
		{name: "CreatePolicyResponse", statusFn: func(r *http.Response) string { return cloudapi.CreatePolicyResponse{HTTPResponse: r}.Status() }},
		{name: "ReplacePoliciesCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplacePoliciesCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeletePolicyResponse", statusFn: func(r *http.Response) string { return cloudapi.DeletePolicyResponse{HTTPResponse: r}.Status() }},
		{name: "GetPolicyResponse", statusFn: func(r *http.Response) string { return cloudapi.GetPolicyResponse{HTTPResponse: r}.Status() }},
		{name: "HeadPolicyResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadPolicyResponse{HTTPResponse: r}.Status() }},
		{name: "UpdatePolicyResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdatePolicyResponse{HTTPResponse: r}.Status() }},
		{name: "ReplacePolicyRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplacePolicyRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListRolesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListRolesResponse{HTTPResponse: r}.Status() }},
		{name: "HeadRolesResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadRolesResponse{HTTPResponse: r}.Status() }},
		{name: "CreateRoleResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateRoleResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceRolesCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceRolesCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteRoleResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteRoleResponse{HTTPResponse: r}.Status() }},
		{name: "GetRoleResponse", statusFn: func(r *http.Response) string { return cloudapi.GetRoleResponse{HTTPResponse: r}.Status() }},
		{name: "HeadRoleResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadRoleResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateRoleResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateRoleResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceRoleRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceRoleRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListServicesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListServicesResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceServicesCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceServicesCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListUsersResponse", statusFn: func(r *http.Response) string { return cloudapi.ListUsersResponse{HTTPResponse: r}.Status() }},
		{name: "HeadUsersResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadUsersResponse{HTTPResponse: r}.Status() }},
		{name: "CreateUserResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateUserResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceUsersCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceUsersCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteUserResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteUserResponse{HTTPResponse: r}.Status() }},
		{name: "GetUserResponse", statusFn: func(r *http.Response) string { return cloudapi.GetUserResponse{HTTPResponse: r}.Status() }},
		{name: "HeadUserResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadUserResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateUserResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateUserResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceUserRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceUserRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListUserAccessKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.ListUserAccessKeysResponse{HTTPResponse: r}.Status() }},
		{name: "HeadUserAccessKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadUserAccessKeysResponse{HTTPResponse: r}.Status() }},
		{name: "CreateUserAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateUserAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteUserAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteUserAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "GetUserAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.GetUserAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "HeadUserAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadUserAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateUserAccessKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateUserAccessKeyResponse{HTTPResponse: r}.Status() }},
		{name: "ChangeUserPasswordResponse", statusFn: func(r *http.Response) string { return cloudapi.ChangeUserPasswordResponse{HTTPResponse: r}.Status() }},
		{name: "ListUserKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.ListUserKeysResponse{HTTPResponse: r}.Status() }},
		{name: "HeadUserKeysResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadUserKeysResponse{HTTPResponse: r}.Status() }},
		{name: "CreateUserKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateUserKeyResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceUserKeysCollectionRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceUserKeysCollectionRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteUserKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteUserKeyResponse{HTTPResponse: r}.Status() }},
		{name: "GetUserKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.GetUserKeyResponse{HTTPResponse: r}.Status() }},
		{name: "HeadUserKeyResponse", statusFn: func(r *http.Response) string { return cloudapi.HeadUserKeyResponse{HTTPResponse: r}.Status() }},
		{name: "ReplaceUserKeyRoleTagsResponse", statusFn: func(r *http.Response) string { return cloudapi.ReplaceUserKeyRoleTagsResponse{HTTPResponse: r}.Status() }},
		{name: "ListVolumesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListVolumesResponse{HTTPResponse: r}.Status() }},
		{name: "CreateVolumeResponse", statusFn: func(r *http.Response) string { return cloudapi.CreateVolumeResponse{HTTPResponse: r}.Status() }},
		{name: "DeleteVolumeResponse", statusFn: func(r *http.Response) string { return cloudapi.DeleteVolumeResponse{HTTPResponse: r}.Status() }},
		{name: "GetVolumeResponse", statusFn: func(r *http.Response) string { return cloudapi.GetVolumeResponse{HTTPResponse: r}.Status() }},
		{name: "UpdateVolumeResponse", statusFn: func(r *http.Response) string { return cloudapi.UpdateVolumeResponse{HTTPResponse: r}.Status() }},
		{name: "ListVolumeSizesResponse", statusFn: func(r *http.Response) string { return cloudapi.ListVolumeSizesResponse{HTTPResponse: r}.Status() }},
	}

	if len(cases) != 186 {
		t.Fatalf("expected 186 response types, got %d", len(cases))
	}

	for _, tc := range cases {
		t.Run(tc.name+"/with_response", func(t *testing.T) {
			got := tc.statusFn(resp)
			if got != "200 OK" {
				t.Errorf("Status() with HTTPResponse = %q, want %q", got, "200 OK")
			}
		})

		t.Run(tc.name+"/nil_response", func(t *testing.T) {
			got := tc.statusFn(nil)
			want := http.StatusText(0)
			if got != want {
				t.Errorf("Status() with nil HTTPResponse = %q, want %q", got, want)
			}
		})
	}
}
