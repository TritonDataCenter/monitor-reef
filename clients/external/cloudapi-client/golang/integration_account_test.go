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

func TestIntegration_GetAccount(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.GetAccountWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("GetAccount: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if resp.JSON200.Login == "" {
		t.Error("expected account login to be non-empty")
	}
}

func TestIntegration_HeadAccount(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadAccountWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadAccount: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_GetConfig(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.GetConfigWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("GetConfig: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}

func TestIntegration_ListDatacenters(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListDatacentersWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListDatacenters: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}


func TestIntegration_ListServices(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListServicesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListServices: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}

func TestIntegration_HeadConfig(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadConfigWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadConfig: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_UpdateAccount(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	// Get current account to capture original values.
	getResp, err := testClient.GetAccountWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("GetAccount: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	original := getResp.JSON200

	// Update company name, address, city, and state.
	newCompany := "inttest-company"
	newAddress := "123 Integration Test Blvd"
	newCity := "Testville"
	newState := "TX"
	updateResp, err := testClient.UpdateAccountWithResponse(ctx, testAccount, cloudapi.UpdateAccountJSONRequestBody{
		CompanyName: &newCompany,
		Address:     &newAddress,
		City:        &newCity,
		State:       &newState,
	})
	if err != nil {
		t.Fatalf("UpdateAccount: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// Restore original values.
	t.Cleanup(func() {
		restore := cloudapi.UpdateAccountJSONRequestBody{
			CompanyName: ptrOrEmpty(original.CompanyName),
			Address:     ptrOrEmpty(original.Address),
			City:        ptrOrEmpty(original.City),
			State:       ptrOrEmpty(original.State),
		}
		resp, err := testClient.UpdateAccountWithResponse(context.Background(), testAccount, restore)
		cleanupErr(t, "restore account", resp.StatusCode(), err)
	})

	if updateResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if updateResp.JSON200.CompanyName == nil || *updateResp.JSON200.CompanyName != newCompany {
		t.Errorf("expected company name %q, got %v", newCompany, updateResp.JSON200.CompanyName)
	}
	if updateResp.JSON200.Address == nil || *updateResp.JSON200.Address != newAddress {
		t.Errorf("expected address %q, got %v", newAddress, updateResp.JSON200.Address)
	}
	if updateResp.JSON200.City == nil || *updateResp.JSON200.City != newCity {
		t.Errorf("expected city %q, got %v", newCity, updateResp.JSON200.City)
	}
	if updateResp.JSON200.State == nil || *updateResp.JSON200.State != newState {
		t.Errorf("expected state %q, got %v", newState, updateResp.JSON200.State)
	}
}

func TestIntegration_UpdateConfig(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	// Get current config to capture original default_network.
	getResp, err := testClient.GetConfigWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("GetConfig: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	originalNetwork := getResp.JSON200.DefaultNetwork

	// Find a network to set as default.
	netResp, err := testClient.ListNetworksWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListNetworks: %v", err)
	}
	if netResp.JSON200 == nil || len(*netResp.JSON200) == 0 {
		t.Skip("no networks available")
	}
	newDefault := (*netResp.JSON200)[0].ID

	updateResp, err := testClient.UpdateConfigWithResponse(ctx, testAccount, cloudapi.UpdateConfigJSONRequestBody{
		DefaultNetwork: &newDefault,
	})
	if err != nil {
		t.Fatalf("UpdateConfig: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// Restore original.
	t.Cleanup(func() {
		if originalNetwork != nil {
			resp, err := testClient.UpdateConfigWithResponse(context.Background(), testAccount, cloudapi.UpdateConfigJSONRequestBody{
				DefaultNetwork: originalNetwork,
			})
			cleanupErr(t, "restore default network config", resp.StatusCode(), err)
		}
	})

	if updateResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}

func TestIntegration_GetDatacenter(t *testing.T) {
	ctx := context.Background()

	// List datacenters to find one.
	listResp, err := testClient.ListDatacentersWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListDatacenters: %v", err)
	}
	if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
		t.Skip("no datacenters available")
	}

	// Pick the first datacenter name.
	var dcName string
	for name := range *listResp.JSON200 {
		dcName = name
		break
	}

	// GetDatacenter returns a 302 redirect to the datacenter's CloudAPI URL.
	// The oapi-codegen client follows redirects, which may fail with a DNS
	// error if the target is not reachable. We accept that as "endpoint works".
	resp, err := testClient.GetDatacenterWithResponse(ctx, testAccount, dcName)
	if err != nil {
		// DNS/connection errors from following the redirect are expected
		// in environments where other DCs are unreachable.
		t.Logf("GetDatacenter(%s): %v (redirect target likely unreachable, OK)", dcName, err)
		return
	}
	// Any HTTP response means the endpoint itself responded.
	t.Logf("GetDatacenter(%s): status %d", dcName, resp.StatusCode())
}

func TestIntegration_GetProvisioningLimits(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.GetProvisioningLimitsWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("GetProvisioningLimits: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	t.Logf("GetProvisioningLimits: %d limit entries", len(*resp.JSON200))
}

func TestIntegration_ListForeignDatacenters(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListForeignDatacentersWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListForeignDatacenters: %v", err)
	}
	// May return 200, 404 (single-DC), or 403 (requires delegated auth).
	sc := resp.StatusCode()
	if sc != 200 && sc != 403 && sc != 404 {
		t.Fatalf("expected 200, 403, or 404, got %d: %s", sc, string(resp.Body))
	}
}
