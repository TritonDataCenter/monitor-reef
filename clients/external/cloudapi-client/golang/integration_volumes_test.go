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
	"time"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
	openapi_types "github.com/oapi-codegen/runtime/types"
)

func TestIntegration_ListVolumeSizes(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListVolumeSizesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListVolumeSizes: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}

func TestIntegration_Volumes_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	if !testConfig.AllowVolumesTests {
		t.Skip("volumes tests disabled (set allowVolumesTests in testconfig.json)")
	}
	ctx := context.Background()

	volName := randName("inttest-vol")

	// NFS volumes require a fabric network.
	network := findFabricNetwork(t, ctx)

	// Create volume (10 GiB = 10240 MiB).
	var volType cloudapi.VolumeType
	volType.FromVolumeType0(cloudapi.Tritonnfs)
	createResp, err := testClient.CreateVolumeWithResponse(ctx, testAccount, cloudapi.CreateVolumeJSONRequestBody{
		Name:     ptr(volName),
		Type:     &volType,
		Size:     10240,
		Networks: &[]openapi_types.UUID{network.ID},
	})
	if err != nil {
		t.Fatalf("CreateVolume: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createResp.StatusCode(), string(createResp.Body))
	}
	volID := createResp.JSON201.ID

	t.Cleanup(func() {
		// Wait briefly for volume to become ready before deleting.
		time.Sleep(5 * time.Second)
		_, err := testClient.DeleteVolumeWithResponse(context.Background(), testAccount, volID)
		if err != nil {
			t.Logf("cleanup: DeleteVolume %s: %v", volID, err)
		}
	})

	// Get volume.
	getResp, err := testClient.GetVolumeWithResponse(ctx, testAccount, volID)
	if err != nil {
		t.Fatalf("GetVolume: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	if getResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if getResp.JSON200.ID != volID {
		t.Errorf("expected volume ID %s, got %s", volID, getResp.JSON200.ID)
	}

	// List volumes.
	listResp, err := testClient.ListVolumesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListVolumes: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)
	if listResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	found := false
	for _, v := range *listResp.JSON200 {
		if v.ID == volID {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("expected volume %s in ListVolumes response", volID)
	}

	// Delete volume.
	delResp, err := testClient.DeleteVolumeWithResponse(ctx, testAccount, volID)
	if err != nil {
		t.Fatalf("DeleteVolume: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)
}
