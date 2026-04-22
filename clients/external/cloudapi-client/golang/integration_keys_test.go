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
	"crypto/ed25519"
	"crypto/rand"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
	"golang.org/x/crypto/ssh"
)

func TestIntegration_Keys_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	// Generate an ephemeral ed25519 keypair.
	pub, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ed25519.GenerateKey: %v", err)
	}
	sshPub, err := ssh.NewPublicKey(pub)
	if err != nil {
		t.Fatalf("ssh.NewPublicKey: %v", err)
	}
	pubKeyStr := string(ssh.MarshalAuthorizedKey(sshPub))
	keyName := randName("inttest-key")

	// Create key.
	createResp, err := testClient.CreateKeyWithResponse(ctx, testAccount, cloudapi.CreateKeyJSONRequestBody{
		Name: keyName,
		Key:  pubKeyStr,
	})
	if err != nil {
		t.Fatalf("CreateKey: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	t.Cleanup(func() {
		resp, err := testClient.DeleteKeyWithResponse(context.Background(), testAccount, keyName)
		cleanupErr(t, "delete key", resp.StatusCode(), err)
	})

	if createResp.JSON201 == nil {
		t.Fatal("expected JSON201 to be non-nil")
	}
	if createResp.JSON201.Name != keyName {
		t.Errorf("expected key name %q, got %q", keyName, createResp.JSON201.Name)
	}

	// Get key.
	getResp, err := testClient.GetKeyWithResponse(ctx, testAccount, keyName)
	if err != nil {
		t.Fatalf("GetKey: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	if getResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if getResp.JSON200.Name != keyName {
		t.Errorf("GetKey: expected name %q, got %q", keyName, getResp.JSON200.Name)
	}

	// Head key.
	headKeyResp, err := testClient.HeadKeyWithResponse(ctx, testAccount, keyName)
	if err != nil {
		t.Fatalf("HeadKey: %v", err)
	}
	requireOK(t, headKeyResp.StatusCode(), nil)

	// Head keys collection.
	headKeysResp, err := testClient.HeadKeysWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadKeys: %v", err)
	}
	requireOK(t, headKeysResp.StatusCode(), nil)

	// List keys and verify our key is present.
	listResp, err := testClient.ListKeysWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListKeys: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)
	if listResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	found := false
	for _, k := range *listResp.JSON200 {
		if k.Name == keyName {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("expected key %q in ListKeys response", keyName)
	}

	// Delete key.
	delResp, err := testClient.DeleteKeyWithResponse(ctx, testAccount, keyName)
	if err != nil {
		t.Fatalf("DeleteKey: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)

	// Verify deleted — key deletion is synchronous, so 404 is expected.
	getResp2, err := testClient.GetKeyWithResponse(ctx, testAccount, keyName)
	if err != nil {
		t.Fatalf("GetKey after delete: %v", err)
	}
	requireStatus(t, 404, getResp2.StatusCode(), getResp2.Body)
}
