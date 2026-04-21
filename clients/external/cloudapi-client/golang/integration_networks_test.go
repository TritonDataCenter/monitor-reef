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
)

func TestIntegration_ListNetworks(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListNetworksWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListNetworks: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatal("expected at least one network")
	}
}

func TestIntegration_GetNetwork(t *testing.T) {
	ctx := context.Background()

	// List networks to find one.
	listResp, err := testClient.ListNetworksWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListNetworks: %v", err)
	}
	if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
		t.Skip("no networks available")
	}

	net := (*listResp.JSON200)[0]
	resp, err := testClient.GetNetworkWithResponse(ctx, testAccount, net.ID)
	if err != nil {
		t.Fatalf("GetNetwork(%s): %v", net.ID, err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if resp.JSON200.ID != net.ID {
		t.Errorf("expected network ID %s, got %s", net.ID, resp.JSON200.ID)
	}
}
