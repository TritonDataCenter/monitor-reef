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

func TestIntegration_ListFirewallRules(t *testing.T) {
	ctx := context.Background()

	resp, err := testClient.ListFirewallRulesWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListFirewallRules: %v", err)
	}
	requireOK(t, resp.StatusCode(), resp.Body)

	if resp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
}

func TestIntegration_FirewallRules_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	ruleText := "FROM any TO all vms ALLOW tcp PORT 22"

	// Create rule (disabled).
	createResp, err := testClient.CreateFirewallRuleWithResponse(ctx, testAccount, cloudapi.CreateFirewallRuleJSONRequestBody{
		Rule:        ruleText,
		Enabled:     ptr(false),
		Description: ptr("integration test rule"),
	})
	if err != nil {
		t.Fatalf("CreateFirewallRule: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatal("expected JSON201 to be non-nil")
	}
	ruleID := createResp.JSON201.ID

	t.Cleanup(func() {
		_, err := testClient.DeleteFirewallRuleWithResponse(context.Background(), testAccount, ruleID)
		if err != nil {
			t.Logf("cleanup: DeleteFirewallRule %s: %v", ruleID, err)
		}
	})

	// Get rule.
	getResp, err := testClient.GetFirewallRuleWithResponse(ctx, testAccount, ruleID)
	if err != nil {
		t.Fatalf("GetFirewallRule: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	if getResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if getResp.JSON200.Rule != ruleText {
		t.Errorf("expected rule text %q, got %q", ruleText, getResp.JSON200.Rule)
	}

	// Update rule description.
	updateResp, err := testClient.UpdateFirewallRuleWithResponse(ctx, testAccount, ruleID, cloudapi.UpdateFirewallRuleJSONRequestBody{
		Description: ptr("updated integration test rule"),
	})
	if err != nil {
		t.Fatalf("UpdateFirewallRule: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// Enable rule.
	enableResp, err := testClient.EnableFirewallRuleWithResponse(ctx, testAccount, ruleID)
	if err != nil {
		t.Fatalf("EnableFirewallRule: %v", err)
	}
	requireOK(t, enableResp.StatusCode(), enableResp.Body)

	// Disable rule.
	disableResp, err := testClient.DisableFirewallRuleWithResponse(ctx, testAccount, ruleID)
	if err != nil {
		t.Fatalf("DisableFirewallRule: %v", err)
	}
	requireOK(t, disableResp.StatusCode(), disableResp.Body)

	// Delete rule.
	delResp, err := testClient.DeleteFirewallRuleWithResponse(ctx, testAccount, ruleID)
	if err != nil {
		t.Fatalf("DeleteFirewallRule: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)

	// Verify deleted.
	getResp2, err := testClient.GetFirewallRuleWithResponse(ctx, testAccount, ruleID)
	if err != nil {
		t.Fatalf("GetFirewallRule after delete: %v", err)
	}
	if getResp2.StatusCode() != 404 {
		t.Errorf("expected 404 after delete, got %d", getResp2.StatusCode())
	}
}
