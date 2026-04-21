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
