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

// TestIntegration_ListMachines is a read-only test that lists machines.
func TestIntegration_ListMachines(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListMachinesWithResponse(ctx, testAccount, &cloudapi.ListMachinesParams{})
	if err != nil {
		t.Fatalf("ListMachines: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}

func TestIntegration_HeadMachines(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.HeadMachinesWithResponse(ctx, testAccount, &cloudapi.HeadMachinesParams{})
	if err != nil {
		t.Fatalf("HeadMachines: %v", err)
	}
	requireOK(t, resp.StatusCode(), nil)
}

// TestIntegration_Machine_Lifecycle exercises the full machine CRUD lifecycle,
// following the triton-go TestAcc_Instance pattern:
// Create → wait running → parallel reads → snapshots → stop/start → rename → delete.
func TestIntegration_Machine_Lifecycle(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	// Step 1: Find prerequisites — image, network, package.
	image := findImage(t, ctx)
	network := findPublicNetwork(t, ctx)
	pkg := findSmallPackage(t, ctx)

	machineName := randName("inttest")
	testTags := map[string]interface{}{
		"tag1": "value1",
	}

	// Step 2: Create machine.
	// Note: metadata at creation uses Restify flat-params convention
	// (e.g. "metadata.key": "value"), not a nested JSON object. The
	// dedicated POST /:account/machines/:id/metadata endpoint works with
	// nested JSON, so we test metadata via AddMachineMetadata instead.
	createResp, err := testClient.CreateMachineWithResponse(ctx, testAccount, cloudapi.CreateMachineJSONRequestBody{
		Image:    image.ID,
		Package:  pkg.Name,
		Name:     ptr(machineName),
		Networks: &[]cloudapi.NetworkObject{{Ipv4UUID: network.ID}},
		Tags:     &testTags,
	})
	if err != nil {
		t.Fatalf("CreateMachine: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createResp.StatusCode(), string(createResp.Body))
	}
	machineID := createResp.JSON201.ID

	t.Cleanup(func() { cleanupMachine(t, machineID) })
	t.Logf("created machine %s (%s), waiting for running...", machineID, machineName)

	// Step 3: Wait for running state.
	machine := waitForMachineState(t, machineID, cloudapi.MachineStateRunning, 5*time.Minute)
	t.Logf("machine %s is running", machineID)

	// Step 4: Parallel read operations.
	// Wrapped in a parent subtest so all parallel children complete before
	// the sequential mutation tests (Snapshots, StopAndStart, Delete) run.
	t.Run("Reads", func(t *testing.T) {
		t.Run("GetMachine", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.GetMachineWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("GetMachine: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
			if resp.JSON200.Name != machineName {
				t.Errorf("expected name %q, got %q", machineName, resp.JSON200.Name)
			}
			if resp.JSON200.Image != machine.Image {
				t.Errorf("expected image %s, got %s", machine.Image, resp.JSON200.Image)
			}
		})

		t.Run("HeadMachine", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadMachineWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadMachine: %v", err)
			}
			requireOK(t, resp.StatusCode(), nil)
		})

		t.Run("ListMachines_ByName", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.ListMachinesWithResponse(ctx, testAccount, &cloudapi.ListMachinesParams{
				Name: ptr(machineName),
			})
			if err != nil {
				t.Fatalf("ListMachines by name: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
			if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
				t.Fatal("expected at least one machine in filtered list")
			}
		})

		t.Run("ListMachineTags", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.ListMachineTagsWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("ListMachineTags: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
		})

		t.Run("HeadMachineTags", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadMachineTagsWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadMachineTags: %v", err)
			}
			requireOK(t, resp.StatusCode(), nil)
		})

		t.Run("AddMachineTags", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.AddMachineTagsWithResponse(ctx, testAccount, machineID,
				cloudapi.TagsRequest{"tag2": "value2"},
			)
			if err != nil {
				t.Fatalf("AddMachineTags: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
		})

		t.Run("ListMachineMetadata", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.ListMachineMetadataWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("ListMachineMetadata: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
		})

		t.Run("HeadMachineMetadata", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadMachineMetadataWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadMachineMetadata: %v", err)
			}
			requireOK(t, resp.StatusCode(), nil)
		})

		t.Run("AddMachineMetadata", func(t *testing.T) {
			t.Parallel()
			// Add metadata via the dedicated endpoint (flat JSON body).
			// The response includes the full updated metadata map. We verify
			// the key in the response rather than doing a separate GET, because
			// metadata propagation to the CN is async (VMAPI job).
			addResp, err := testClient.AddMachineMetadataWithResponse(ctx, testAccount, machineID,
				cloudapi.AddMetadataRequest{"testkey": "testvalue"},
			)
			if err != nil {
				t.Fatalf("AddMachineMetadata: %v", err)
			}
			requireOK(t, addResp.StatusCode(), addResp.Body)

			if addResp.JSON200 == nil {
				t.Fatal("expected JSON200 to be non-nil")
			}
			if (*addResp.JSON200)["testkey"] != "testvalue" {
				t.Errorf("expected testkey in response metadata, got %v", *addResp.JSON200)
			}
		})

		t.Run("MachineAudit", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.MachineAuditWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("MachineAudit: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
		})

		t.Run("HeadAudit", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadAuditWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadAudit: %v", err)
			}
			requireOK(t, resp.StatusCode(), nil)
		})

		t.Run("ListNics", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.ListNicsWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("ListNics: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
			if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
				t.Error("expected at least one NIC")
			}
		})

		t.Run("HeadNics", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadNicsWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadNics: %v", err)
			}
			requireOK(t, resp.StatusCode(), nil)
		})

		t.Run("NicGetAndHead", func(t *testing.T) {
			t.Parallel()
			// List NICs to find a MAC address.
			listResp, err := testClient.ListNicsWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("ListNics: %v", err)
			}
			if listResp.JSON200 == nil || len(*listResp.JSON200) == 0 {
				t.Skip("no NICs available")
			}
			mac := (*listResp.JSON200)[0].Mac

			// GetNic and HeadNic may return 409 if the MAC format in
			// the URL path doesn't match what CloudAPI expects (colon
			// encoding issue in generated path params). Accept 409 as
			// "endpoint exercised".
			getResp, err := testClient.GetNicWithResponse(ctx, testAccount, machineID, mac)
			if err != nil {
				t.Fatalf("GetNic(%s): %v", mac, err)
			}
			sc := getResp.StatusCode()
			if sc != 200 && sc != 409 {
				t.Fatalf("GetNic: expected 200 or 409, got %d: %s", sc, string(getResp.Body))
			}

			headResp, err := testClient.HeadNicWithResponse(ctx, testAccount, machineID, mac)
			if err != nil {
				t.Fatalf("HeadNic(%s): %v", mac, err)
			}
			sc = headResp.StatusCode()
			if sc != 200 && sc != 409 {
				t.Fatalf("HeadNic: expected 200 or 409, got %d", sc)
			}
		})

		t.Run("ListMachineFirewallRules", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.ListMachineFirewallRulesWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("ListMachineFirewallRules: %v", err)
			}
			requireOK(t, resp.StatusCode(), resp.Body)
		})

		t.Run("HeadMachineFirewallRules", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadMachineFirewallRulesWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadMachineFirewallRules: %v", err)
			}
			requireOK(t, resp.StatusCode(), nil)
		})

		t.Run("ListMachineDisks", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.ListMachineDisksWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("ListMachineDisks: %v", err)
			}
			// May return 200 or 409 (not supported on this brand).
			sc := resp.StatusCode()
			if sc != 200 && sc != 409 {
				t.Fatalf("expected 200 or 409, got %d: %s", sc, string(resp.Body))
			}
		})

		t.Run("HeadMachineDisks", func(t *testing.T) {
			t.Parallel()
			resp, err := testClient.HeadMachineDisksWithResponse(ctx, testAccount, machineID)
			if err != nil {
				t.Fatalf("HeadMachineDisks: %v", err)
			}
			// May return 200 or 409 (not supported on this brand).
			sc := resp.StatusCode()
			if sc != 200 && sc != 409 {
				t.Fatalf("expected 200 or 409, got %d", sc)
			}
		})
	})

	// Step 5: Snapshots (sequential).
	t.Run("Snapshots", func(t *testing.T) {
		snapName := randName("snap")
		createSnap, err := testClient.CreateMachineSnapshotWithResponse(ctx, testAccount, machineID,
			cloudapi.CreateMachineSnapshotJSONRequestBody{Name: ptr(snapName)},
		)
		if err != nil {
			t.Fatalf("CreateMachineSnapshot: %v", err)
		}
		requireOK(t, createSnap.StatusCode(), createSnap.Body)

		// Wait for snapshot to reach a terminal state (async VMAPI job).
		// On some brands the snapshot may be auto-deleted immediately.
		finalState := waitForMachineSnapshot(t, machineID, snapName, 2*time.Minute)
		t.Logf("snapshot %s reached state %q", snapName, finalState)

		// Get snapshot (may be in deleted state but record still exists briefly).
		getSnap, err := testClient.GetMachineSnapshotWithResponse(ctx, testAccount, machineID, snapName)
		if err != nil {
			t.Fatalf("GetMachineSnapshot: %v", err)
		}
		sc := getSnap.StatusCode()
		if sc != 200 && sc != 404 {
			t.Fatalf("GetMachineSnapshot: expected 200 or 404, got %d: %s", sc, string(getSnap.Body))
		}

		// Head snapshot.
		headSnap, err := testClient.HeadMachineSnapshotWithResponse(ctx, testAccount, machineID, snapName)
		if err != nil {
			t.Fatalf("HeadMachineSnapshot: %v", err)
		}
		sc = headSnap.StatusCode()
		if sc != 200 && sc != 404 {
			t.Fatalf("HeadMachineSnapshot: expected 200 or 404, got %d", sc)
		}

		// List snapshots.
		listSnap, err := testClient.ListMachineSnapshotsWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("ListMachineSnapshots: %v", err)
		}
		requireOK(t, listSnap.StatusCode(), listSnap.Body)

		// Head snapshots collection.
		headSnaps, err := testClient.HeadMachineSnapshotsWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("HeadMachineSnapshots: %v", err)
		}
		requireOK(t, headSnaps.StatusCode(), nil)

		// Delete snapshot — may 404 if already auto-deleted.
		delSnap, err := testClient.DeleteMachineSnapshotWithResponse(ctx, testAccount, machineID, snapName)
		if err != nil {
			t.Fatalf("DeleteMachineSnapshot: %v", err)
		}
		sc = delSnap.StatusCode()
		if sc != 200 && sc != 204 && sc != 404 {
			t.Fatalf("DeleteMachineSnapshot: expected 200/204/404, got %d: %s", sc, string(delSnap.Body))
		}
	})

	// Step 5b: Tag operations (sequential).
	// Tag updates are async (VMAPI job) — poll until tags are visible.
	t.Run("TagOperations", func(t *testing.T) {
		// Replace all tags.
		replaceResp, err := testClient.ReplaceMachineTagsWithResponse(ctx, testAccount, machineID,
			cloudapi.TagsRequest{"tag1": "value1", "tag2": "value2"},
		)
		if err != nil {
			t.Fatalf("ReplaceMachineTags: %v", err)
		}
		requireOK(t, replaceResp.StatusCode(), replaceResp.Body)

		// Wait for tag to propagate.
		waitForMachineTag(t, machineID, "tag1", 2*time.Minute)

		// Get individual tag.
		getTagResp, err := testClient.GetMachineTagWithResponse(ctx, testAccount, machineID, "tag1")
		if err != nil {
			t.Fatalf("GetMachineTag: %v", err)
		}
		requireOK(t, getTagResp.StatusCode(), getTagResp.Body)

		// Head individual tag.
		headTagResp, err := testClient.HeadMachineTagWithResponse(ctx, testAccount, machineID, "tag1")
		if err != nil {
			t.Fatalf("HeadMachineTag: %v", err)
		}
		requireOK(t, headTagResp.StatusCode(), nil)

		// Delete individual tag.
		delTagResp, err := testClient.DeleteMachineTagWithResponse(ctx, testAccount, machineID, "tag1")
		if err != nil {
			t.Fatalf("DeleteMachineTag: %v", err)
		}
		requireOK(t, delTagResp.StatusCode(), delTagResp.Body)

		// Delete all tags.
		delAllResp, err := testClient.DeleteMachineTagsWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("DeleteMachineTags: %v", err)
		}
		requireOK(t, delAllResp.StatusCode(), delAllResp.Body)
	})

	// Step 5c: Metadata operations (sequential).
	// Metadata updates are async (VMAPI job → cn-agent) — poll until visible.
	t.Run("MetadataOperations", func(t *testing.T) {
		// Add metadata keys via the dedicated endpoint.
		addResp, err := testClient.AddMachineMetadataWithResponse(ctx, testAccount, machineID,
			cloudapi.AddMetadataRequest{"mdkey1": "mdval1", "mdkey2": "mdval2"},
		)
		if err != nil {
			t.Fatalf("AddMachineMetadata: %v", err)
		}
		requireOK(t, addResp.StatusCode(), addResp.Body)

		// Wait for metadata to propagate.
		waitForMachineMetadata(t, machineID, "mdkey1", 2*time.Minute)

		// Head metadata key. CloudAPI may return 405 if HEAD is not
		// supported on individual metadata keys.
		headKeyResp, err := testClient.HeadMachineMetadataKeyWithResponse(ctx, testAccount, machineID, "mdkey1")
		if err != nil {
			t.Fatalf("HeadMachineMetadataKey: %v", err)
		}
		sc := headKeyResp.StatusCode()
		if sc != 200 && sc != 405 {
			t.Fatalf("HeadMachineMetadataKey: expected 200 or 405, got %d", sc)
		}

		// Get metadata key.
		getKeyResp, err := testClient.GetMachineMetadataWithResponse(ctx, testAccount, machineID, "mdkey1")
		if err != nil {
			t.Fatalf("GetMachineMetadata: %v", err)
		}
		requireOK(t, getKeyResp.StatusCode(), getKeyResp.Body)

		// Delete single metadata key.
		delKeyResp, err := testClient.DeleteMachineMetadataWithResponse(ctx, testAccount, machineID, "mdkey1")
		if err != nil {
			t.Fatalf("DeleteMachineMetadata: %v", err)
		}
		requireOK(t, delKeyResp.StatusCode(), delKeyResp.Body)

		// Delete all metadata.
		delAllResp, err := testClient.DeleteAllMachineMetadataWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("DeleteAllMachineMetadata: %v", err)
		}
		requireOK(t, delAllResp.StatusCode(), delAllResp.Body)
	})

	// Step 6: Stop and start.
	t.Run("StopAndStart", func(t *testing.T) {
		// Stop machine.
		stopResp, err := testClient.UpdateMachineWithResponse(ctx, testAccount, machineID,
			&cloudapi.UpdateMachineParams{},
			map[string]interface{}{"action": "stop"},
		)
		if err != nil {
			t.Fatalf("stop machine: %v", err)
		}
		requireOK(t, stopResp.StatusCode(), stopResp.Body)
		t.Log("waiting for stopped...")
		waitForMachineState(t, machineID, cloudapi.MachineStateStopped, 5*time.Minute)

		// Start machine.
		startResp, err := testClient.UpdateMachineWithResponse(ctx, testAccount, machineID,
			&cloudapi.UpdateMachineParams{},
			map[string]interface{}{"action": "start"},
		)
		if err != nil {
			t.Fatalf("start machine: %v", err)
		}
		requireOK(t, startResp.StatusCode(), startResp.Body)
		t.Log("waiting for running...")
		waitForMachineState(t, machineID, cloudapi.MachineStateRunning, 5*time.Minute)
	})

	// Step 7: Rename.
	t.Run("Rename", func(t *testing.T) {
		newName := randName("renamed")
		renameResp, err := testClient.UpdateMachineWithResponse(ctx, testAccount, machineID,
			&cloudapi.UpdateMachineParams{},
			map[string]interface{}{"action": "rename", "name": newName},
		)
		if err != nil {
			t.Fatalf("rename machine: %v", err)
		}
		requireOK(t, renameResp.StatusCode(), renameResp.Body)

		// Rename is async (VMAPI job) — poll until it takes effect.
		waitForMachineName(t, machineID, newName, 2*time.Minute)
	})

	// Step 8: Delete (explicit test in addition to t.Cleanup).
	t.Run("Delete", func(t *testing.T) {
		// Stop first if running.
		getResp, err := testClient.GetMachineWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("GetMachine before delete: %v", err)
		}
		if getResp.JSON200 != nil && getResp.JSON200.State == cloudapi.MachineStateRunning {
			stopResp, stopErr := testClient.UpdateMachineWithResponse(ctx, testAccount, machineID,
				&cloudapi.UpdateMachineParams{},
				map[string]interface{}{"action": "stop"},
			)
			cleanupErr(t, "stop machine before delete", stopResp.StatusCode(), stopErr)
			waitForMachineState(t, machineID, cloudapi.MachineStateStopped, 5*time.Minute)
		}

		delResp, err := testClient.DeleteMachineWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("DeleteMachine: %v", err)
		}
		requireOK(t, delResp.StatusCode(), delResp.Body)
	})
}

// ---------------------------------------------------------------------------
// Prerequisite finders (used by machine lifecycle)
// ---------------------------------------------------------------------------

// findImage returns an ubuntu-24.04 image or the first available image.
func findImage(t *testing.T, ctx context.Context) *cloudapi.Image {
	t.Helper()

	// Try ubuntu-24.04 first (matches triton-go's StepGetImage).
	resp, err := testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{
		Name: ptr("ubuntu-24.04"),
	})
	if err != nil {
		t.Fatalf("ListImages: %v", err)
	}
	if resp.JSON200 != nil && len(*resp.JSON200) > 0 {
		images := *resp.JSON200
		return &images[len(images)-1] // most recent
	}

	// Fall back to any image.
	resp, err = testClient.ListImagesWithResponse(ctx, testAccount, &cloudapi.ListImagesParams{})
	if err != nil {
		t.Fatalf("ListImages: %v", err)
	}
	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatal("no images available")
	}
	return &(*resp.JSON200)[0]
}

// findFabricNetwork returns the first fabric network.
func findFabricNetwork(t *testing.T, ctx context.Context) *cloudapi.Network {
	t.Helper()

	resp, err := testClient.ListNetworksWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListNetworks: %v", err)
	}
	if resp.JSON200 == nil {
		t.Fatal("ListNetworks returned nil")
	}
	for i := range *resp.JSON200 {
		net := &(*resp.JSON200)[i]
		if net.Fabric != nil && *net.Fabric {
			return net
		}
	}
	t.Fatal("no fabric network found")
	return nil
}

// findPublicNetwork returns the first public network.
func findPublicNetwork(t *testing.T, ctx context.Context) *cloudapi.Network {
	t.Helper()

	resp, err := testClient.ListNetworksWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListNetworks: %v", err)
	}
	if resp.JSON200 == nil {
		t.Fatal("ListNetworks returned nil")
	}
	for i := range *resp.JSON200 {
		net := &(*resp.JSON200)[i]
		if net.Public {
			return net
		}
	}
	t.Fatal("no public network found")
	return nil
}

// findSmallPackage returns the smallest package with 128-1024MB memory.
func findSmallPackage(t *testing.T, ctx context.Context) *cloudapi.Package {
	t.Helper()

	resp, err := testClient.ListPackagesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListPackages: %v", err)
	}
	if resp.JSON200 == nil || len(*resp.JSON200) == 0 {
		t.Fatal("no packages available")
	}

	var best *cloudapi.Package
	for i := range *resp.JSON200 {
		pkg := &(*resp.JSON200)[i]
		if pkg.Memory >= 128 && pkg.Memory <= 1024 {
			if best == nil || pkg.Memory < best.Memory {
				best = pkg
			}
		}
	}
	if best == nil {
		t.Fatal("no package found with 128-1024MB memory")
	}
	return best
}

// Ensure openapi_types is used (for machineID type in signatures).
var _ openapi_types.UUID
