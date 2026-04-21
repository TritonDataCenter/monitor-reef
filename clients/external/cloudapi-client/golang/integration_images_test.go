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

func TestIntegration_ListImages(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{})
	if err != nil {
		t.Fatalf("ListImages: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatal("expected at least one image")
	}
}

func TestIntegration_ListImages_Filtered(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{
		Name: ptr("ubuntu-24.04"),
	})
	if err != nil {
		t.Fatalf("ListImages filtered: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	for _, img := range *resp.JSON200 {
		if img.Name != "ubuntu-24.04" {
			t.Errorf("expected image name %q, got %q", "ubuntu-24.04", img.Name)
		}
	}
}

func TestIntegration_GetImage(t *testing.T) {
	ctx := context.Background()

	// List images to find one.
	listResp, err := testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{})
	if err != nil {
		t.Fatalf("ListImages: %v", err)
	}
	if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
		t.Skip("no images available")
	}

	img := (*listResp.JSON200)[0]
	resp, err := testClient.GetImageWithResponse(ctx, testAccount, img.ID)
	if err != nil {
		t.Fatalf("GetImage(%s): %v", img.ID, err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if resp.JSON200.ID != img.ID {
		t.Errorf("expected image ID %s, got %s", img.ID, resp.JSON200.ID)
	}
	if resp.JSON200.Name == "" {
		t.Error("expected image name to be non-empty")
	}
}

func TestIntegration_ListPackages(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListPackages: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatal("expected at least one package")
	}
}

func TestIntegration_GetPackage(t *testing.T) {
	ctx := context.Background()

	// List packages and pick one.
	listResp, err := testClient.ListPackagesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListPackages: %v", err)
	}
	if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
		t.Skip("no packages available")
	}

	pkg := (*listResp.JSON200)[0]
	resp, err := testClient.GetPackageWithResponse(ctx, testAccount, pkg.Name)
	if err != nil {
		t.Fatalf("GetPackage(%s): %v", pkg.Name, err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if resp.JSON200.Name != pkg.Name {
		t.Errorf("expected package name %q, got %q", pkg.Name, resp.JSON200.Name)
	}
}
