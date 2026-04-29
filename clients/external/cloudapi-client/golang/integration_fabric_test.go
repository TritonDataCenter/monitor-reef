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
	"math/rand"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

func TestIntegration_ListFabricVlans(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListFabricVlansWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListFabricVlans: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)
}

func TestIntegration_HeadFabricVlans(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadFabricVlansWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadFabricVlans: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_FabricVlan_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	if !testConfig.AllowFabricTests {
		t.Skip("fabric tests disabled (set allowFabricTests in testconfig.json)")
	}
	ctx := context.Background()

	// Use a random VLAN ID in range 1000-3999 to avoid collisions.
	vlanID := uint16(1000 + rand.Intn(3000))
	vlanName := randName("inttest-vlan")

	// Create VLAN.
	createResp, err := testClient.CreateFabricVlanWithResponse(ctx, testAccount, cloudapi.CreateFabricVlanJSONRequestBody{
		VlanID: vlanID,
		Name:   vlanName,
	})
	if err != nil {
		t.Fatalf("CreateFabricVlan: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createResp.StatusCode(), string(createResp.Body))
	}

	t.Cleanup(func() {
		_, err := testClient.DeleteFabricVlanWithResponse(context.Background(), testAccount, vlanID)
		if err != nil {
			t.Logf("cleanup: DeleteFabricVlan %d: %v", vlanID, err)
		}
	})

	// Get VLAN.
	getResp, err := testClient.GetFabricVlanWithResponse(ctx, testAccount, vlanID)
	if err != nil {
		t.Fatalf("GetFabricVlan: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	if getResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if getResp.JSON200.VlanID != vlanID {
		t.Errorf("expected VLAN ID %d, got %d", vlanID, getResp.JSON200.VlanID)
	}

	// Head VLAN.
	headResp, err := testClient.HeadFabricVlanWithResponse(ctx, testAccount, vlanID)
	if err != nil {
		t.Fatalf("HeadFabricVlan: %v", err)
	}
	requireOK(t, headResp.StatusCode(), nil)

	// Update VLAN description.
	desc := "updated integration test vlan"
	updateResp, err := testClient.UpdateFabricVlanWithResponse(ctx, testAccount, vlanID, cloudapi.UpdateFabricVlanJSONRequestBody{
		Description: &desc,
	})
	if err != nil {
		t.Fatalf("UpdateFabricVlan: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// List VLANs and verify ours is present.
	listResp, err := testClient.ListFabricVlansWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListFabricVlans: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)
	found := false
	if listResp.JSON200 != nil {
		for _, v := range *listResp.JSON200 {
			if v.VlanID == vlanID {
				found = true
				break
			}
		}
	}
	if !found {
		t.Errorf("expected VLAN %d in ListFabricVlans response", vlanID)
	}

	// --- Fabric Network within this VLAN ---
	t.Run("FabricNetwork", func(t *testing.T) {
		netName := randName("inttest-fnet")
		createNetResp, err := testClient.CreateFabricNetworkWithResponse(ctx, testAccount, vlanID, cloudapi.CreateFabricNetworkJSONRequestBody{
			Name:             netName,
			Subnet:           "10.99.0.0/24",
			ProvisionStartIP: "10.99.0.10",
			ProvisionEndIP:   "10.99.0.250",
			Gateway:          ptr("10.99.0.1"),
			Resolvers:        &[]string{"8.8.8.8"},
			InternetNat:      ptr(false),
		})
		if err != nil {
			t.Fatalf("CreateFabricNetwork: %v", err)
		}
		requireOK(t, createNetResp.StatusCode(), createNetResp.Body)

		if createNetResp.JSON201 == nil {
			t.Fatalf("expected JSON201, got status %d: %s", createNetResp.StatusCode(), string(createNetResp.Body))
		}
		netID := createNetResp.JSON201.ID

		t.Cleanup(func() {
			_, err := testClient.DeleteFabricNetworkWithResponse(context.Background(), testAccount, vlanID, netID)
			if err != nil {
				t.Logf("cleanup: DeleteFabricNetwork %s: %v", netID, err)
			}
		})

		// Get network.
		getNetResp, err := testClient.GetFabricNetworkWithResponse(ctx, testAccount, vlanID, netID)
		if err != nil {
			t.Fatalf("GetFabricNetwork: %v", err)
		}
		requireOK(t, getNetResp.StatusCode(), getNetResp.Body)
		if getNetResp.JSON200 == nil {
			t.Fatal("expected JSON200 to be non-nil")
		}

		// Head network.
		headNetResp, err := testClient.HeadFabricNetworkWithResponse(ctx, testAccount, vlanID, netID)
		if err != nil {
			t.Fatalf("HeadFabricNetwork: %v", err)
		}
		requireOK(t, headNetResp.StatusCode(), nil)

		// Update network description.
		netDesc := "updated fabric network"
		updateNetResp, err := testClient.UpdateFabricNetworkWithResponse(ctx, testAccount, vlanID, netID, cloudapi.UpdateFabricNetworkJSONRequestBody{
			Description: &netDesc,
		})
		if err != nil {
			t.Fatalf("UpdateFabricNetwork: %v", err)
		}
		requireOK(t, updateNetResp.StatusCode(), updateNetResp.Body)

		// List networks in VLAN.
		listNetResp, err := testClient.ListFabricNetworksWithResponse(ctx, testAccount, vlanID)
		if err != nil {
			t.Fatalf("ListFabricNetworks: %v", err)
		}
		requireOK(t, listNetResp.StatusCode(), listNetResp.Body)

		// Head networks collection.
		headNetsResp, err := testClient.HeadFabricNetworksWithResponse(ctx, testAccount, vlanID)
		if err != nil {
			t.Fatalf("HeadFabricNetworks: %v", err)
		}
		requireOK(t, headNetsResp.StatusCode(), nil)

		// Delete network.
		delNetResp, err := testClient.DeleteFabricNetworkWithResponse(ctx, testAccount, vlanID, netID)
		if err != nil {
			t.Fatalf("DeleteFabricNetwork: %v", err)
		}
		requireOK(t, delNetResp.StatusCode(), delNetResp.Body)
	})

	// Delete VLAN.
	delResp, err := testClient.DeleteFabricVlanWithResponse(ctx, testAccount, vlanID)
	if err != nil {
		t.Fatalf("DeleteFabricVlan: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)
}
