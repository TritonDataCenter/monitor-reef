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

func TestIntegration_ListAccessKeys(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListAccessKeysWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListAccessKeys: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)
}

func TestIntegration_HeadAccessKeys(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadAccessKeysWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadAccessKeys: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_AccessKeys_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	// Create access key.
	desc := "integration test key"
	createResp, err := testClient.CreateAccessKeyWithResponse(ctx, testAccount, cloudapi.CreateAccessKeyJSONRequestBody{
		Description: &desc,
	})
	if err != nil {
		t.Fatalf("CreateAccessKey: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createResp.StatusCode(), string(createResp.Body))
	}
	keyID := createResp.JSON201.Accesskeyid

	t.Cleanup(func() {
		_, err := testClient.DeleteAccessKeyWithResponse(context.Background(), testAccount, keyID)
		if err != nil {
			t.Logf("cleanup: DeleteAccessKey %s: %v", keyID, err)
		}
	})

	// Get access key.
	getResp, err := testClient.GetAccessKeyWithResponse(ctx, testAccount, keyID)
	if err != nil {
		t.Fatalf("GetAccessKey: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	if getResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if getResp.JSON200.Accesskeyid != keyID {
		t.Errorf("expected key ID %q, got %q", keyID, getResp.JSON200.Accesskeyid)
	}

	// Head access key.
	headResp, err := testClient.HeadAccessKeyWithResponse(ctx, testAccount, keyID)
	if err != nil {
		t.Fatalf("HeadAccessKey: %v", err)
	}
	requireOK(t, headResp.StatusCode(), nil)

	// Update access key description.
	newDesc := "updated integration test key"
	updateResp, err := testClient.UpdateAccessKeyWithResponse(ctx, testAccount, keyID, cloudapi.UpdateAccessKeyJSONRequestBody{
		Description: &newDesc,
	})
	if err != nil {
		t.Fatalf("UpdateAccessKey: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// List and verify our key is present.
	listResp, err := testClient.ListAccessKeysWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListAccessKeys: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)
	found := false
	if listResp.JSON200 != nil {
		for _, k := range *listResp.JSON200 {
			if k.Accesskeyid == keyID {
				found = true
				break
			}
		}
	}
	if !found {
		t.Errorf("expected access key %q in ListAccessKeys response", keyID)
	}

	// Delete access key.
	delResp, err := testClient.DeleteAccessKeyWithResponse(ctx, testAccount, keyID)
	if err != nil {
		t.Fatalf("DeleteAccessKey: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)
}
