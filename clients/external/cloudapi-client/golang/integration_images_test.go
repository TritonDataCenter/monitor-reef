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

	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
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
	listResp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
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

func TestIntegration_ListPackages_FilterByName(t *testing.T) {
	ctx := context.Background()

	// First list all packages to get a known name.
	allResp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
	if err != nil {
		t.Fatalf("ListPackages (unfiltered): %v", err)
	}
	if allResp.JSON200 == nil || len(*allResp.JSON200) == 0 {
		t.Skip("no packages available")
	}

	targetName := (*allResp.JSON200)[0].Name

	// Now filter by that name.
	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount, &cloudapi.ListPackagesParams{
		Name: ptr(targetName),
	})
	if err != nil {
		t.Fatalf("ListPackages filtered by name: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatalf("expected at least one package with name %q", targetName)
	}
	for _, pkg := range *resp.JSON200 {
		if pkg.Name != targetName {
			t.Errorf("expected package name %q, got %q", targetName, pkg.Name)
		}
	}
}

func TestIntegration_ListPackages_FilterByMemory(t *testing.T) {
	ctx := context.Background()

	// List all packages to find a known memory value.
	allResp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
	if err != nil {
		t.Fatalf("ListPackages (unfiltered): %v", err)
	}
	if allResp.JSON200 == nil || len(*allResp.JSON200) == 0 {
		t.Skip("no packages available")
	}

	targetMemory := (*allResp.JSON200)[0].Memory

	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount, &cloudapi.ListPackagesParams{
		Memory: ptr(targetMemory),
	})
	if err != nil {
		t.Fatalf("ListPackages filtered by memory: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatalf("expected at least one package with memory %d", targetMemory)
	}
	for _, pkg := range *resp.JSON200 {
		if pkg.Memory != targetMemory {
			t.Errorf("expected memory %d, got %d", targetMemory, pkg.Memory)
		}
	}
}

func TestIntegration_ListPackages_FilterByBrand(t *testing.T) {
	ctx := context.Background()

	// List all packages to find one with a non-nil brand.
	allResp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
	if err != nil {
		t.Fatalf("ListPackages (unfiltered): %v", err)
	}
	if allResp.JSON200 == nil || len(*allResp.JSON200) == 0 {
		t.Skip("no packages available")
	}

	// Find a package with brand set.
	var targetBrand string
	for _, pkg := range *allResp.JSON200 {
		if pkg.Brand != nil {
			brandBytes, err := pkg.Brand.MarshalJSON()
			if err != nil {
				continue
			}
			s := string(brandBytes)
			if len(s) >= 2 && s[0] == '"' {
				s = s[1 : len(s)-1]
			}
			if s != "" && s != "null" {
				targetBrand = s
				break
			}
		}
	}
	if targetBrand == "" {
		t.Skip("no packages with brand set")
	}

	t.Logf("filtering by brand=%q (unfiltered count: %d)", targetBrand, len(*allResp.JSON200))

	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount, &cloudapi.ListPackagesParams{
		Brand: ptr(targetBrand),
	})
	if err != nil {
		t.Fatalf("ListPackages filtered by brand: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatalf("expected at least one package with brand %q, got empty result", targetBrand)
	}

	t.Logf("filtered result: %d packages (vs %d unfiltered)", len(*resp.JSON200), len(*allResp.JSON200))
	for _, pkg := range *resp.JSON200 {
		t.Logf("  %s (brand=%v)", pkg.Name, pkg.Brand)
	}

	// Verify the filtered count is less than the unfiltered count
	// (proves server-side filtering, not just returning everything).
	if len(*resp.JSON200) >= len(*allResp.JSON200) {
		t.Errorf("brand filter did not reduce results: filtered=%d, unfiltered=%d",
			len(*resp.JSON200), len(*allResp.JSON200))
	}
}

func TestIntegration_ListPackages_FilterNoMatch(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount, &cloudapi.ListPackagesParams{
		Name: ptr("nonexistent-package-name-zzz"),
	})
	if err != nil {
		t.Fatalf("ListPackages filtered (no match): %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if len(*resp.JSON200) != 0 {
		t.Errorf("expected empty result for bogus filter, got %d packages", len(*resp.JSON200))
	}
}

func TestIntegration_HeadPackages_Filtered(t *testing.T) {
	ctx := context.Background()

	// List all packages to get a known name.
	allResp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
	if err != nil {
		t.Fatalf("ListPackages: %v", err)
	}
	if allResp.JSON200 == nil || len(*allResp.JSON200) == 0 {
		t.Skip("no packages available")
	}

	targetName := (*allResp.JSON200)[0].Name

	resp, err := testClient.HeadPackagesWithResponse(ctx, testAccount, &cloudapi.HeadPackagesParams{
		Name: ptr(targetName),
	})
	if err != nil {
		t.Fatalf("HeadPackages filtered: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_HeadImages(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadImagesWithResponse(ctx, testAccount, &cloudapi.HeadImagesParams{})
	if err != nil {
		t.Fatalf("HeadImages: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_HeadImage(t *testing.T) {
	ctx := context.Background()

	listResp, err := testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{})
	if err != nil {
		t.Fatalf("ListImages: %v", err)
	}
	if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
		t.Skip("no images available")
	}

	img := (*listResp.JSON200)[0]
	resp, err := testClient.HeadImageWithResponse(ctx, testAccount, img.ID)
	if err != nil {
		t.Fatalf("HeadImage(%s): %v", img.ID, err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_HeadPackages(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadPackagesWithResponse(ctx, testAccount, nil)
	if err != nil {
		t.Fatalf("HeadPackages: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

func TestIntegration_HeadPackage(t *testing.T) {
	ctx := context.Background()

	listResp, err := testClient.ListPackagesWithResponse(ctx, testAccount, nil)
	if err != nil {
		t.Fatalf("ListPackages: %v", err)
	}
	if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
		t.Skip("no packages available")
	}

	pkg := (*listResp.JSON200)[0]
	resp, err := testClient.HeadPackageWithResponse(ctx, testAccount, pkg.Name)
	if err != nil {
		t.Fatalf("HeadPackage(%s): %v", pkg.Name, err)
	}
	requireOK(t, resp.StatusCode(), nil)
}
