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
	cryptorand "crypto/rand"
	"fmt"
	"math/rand"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
	"golang.org/x/crypto/ssh"
)

// createTestUser creates a sub-user for testing and returns its UUID string.
// It registers a cleanup function to delete the user.
func createTestUser(t *testing.T, ctx context.Context) string {
	t.Helper()

	userLogin := fmt.Sprintf("iusub%05d", rand.Intn(99999))
	userEmail := fmt.Sprintf("%s@example.com", userLogin)
	userPassword := fmt.Sprintf("Pass1!%d", rand.Intn(99999))

	createResp, err := testClient.CreateUserWithResponse(ctx, testAccount, cloudapi.CreateUserJSONRequestBody{
		Login:    userLogin,
		Email:    userEmail,
		Password: userPassword,
	})
	if err != nil {
		t.Fatalf("CreateUser: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createResp.StatusCode(), string(createResp.Body))
	}
	userID := createResp.JSON201.ID.String()

	t.Cleanup(func() {
		_, err := testClient.DeleteUserWithResponse(context.Background(), testAccount, userID)
		if err != nil {
			t.Logf("cleanup: DeleteUser %s: %v", userID, err)
		}
	})

	return userID
}

func TestIntegration_UserKeys_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	userID := createTestUser(t, ctx)

	// Generate an ephemeral ed25519 keypair.
	pub, _, err := ed25519.GenerateKey(cryptorand.Reader)
	if err != nil {
		t.Fatalf("ed25519.GenerateKey: %v", err)
	}
	sshPub, err := ssh.NewPublicKey(pub)
	if err != nil {
		t.Fatalf("ssh.NewPublicKey: %v", err)
	}
	pubKeyStr := string(ssh.MarshalAuthorizedKey(sshPub))
	keyName := randName("iukey")

	// Create user key.
	createResp, err := testClient.CreateUserKeyWithResponse(ctx, testAccount, userID, cloudapi.CreateUserKeyJSONRequestBody{
		Name: keyName,
		Key:  pubKeyStr,
	})
	if err != nil {
		t.Fatalf("CreateUserKey: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	t.Cleanup(func() {
		_, _ = testClient.DeleteUserKeyWithResponse(context.Background(), testAccount, userID, keyName)
	})

	// Get user key.
	getResp, err := testClient.GetUserKeyWithResponse(ctx, testAccount, userID, keyName)
	if err != nil {
		t.Fatalf("GetUserKey: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)

	// Head user key.
	headResp, err := testClient.HeadUserKeyWithResponse(ctx, testAccount, userID, keyName)
	if err != nil {
		t.Fatalf("HeadUserKey: %v", err)
	}
	requireOK(t, headResp.StatusCode(), nil)

	// List user keys.
	listResp, err := testClient.ListUserKeysWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("ListUserKeys: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)

	// Head user keys collection.
	headKeysResp, err := testClient.HeadUserKeysWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("HeadUserKeys: %v", err)
	}
	requireOK(t, headKeysResp.StatusCode(), nil)

	// Delete user key.
	delResp, err := testClient.DeleteUserKeyWithResponse(ctx, testAccount, userID, keyName)
	if err != nil {
		t.Fatalf("DeleteUserKey: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)
}

func TestIntegration_UserAccessKeys_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	userID := createTestUser(t, ctx)

	// Create user access key.
	desc := "integration test user access key"
	createResp, err := testClient.CreateUserAccessKeyWithResponse(ctx, testAccount, userID, cloudapi.CreateUserAccessKeyJSONRequestBody{
		Description: &desc,
	})
	if err != nil {
		t.Fatalf("CreateUserAccessKey: %v", err)
	}
	requireOK(t, createResp.StatusCode(), createResp.Body)

	if createResp.JSON201 == nil {
		t.Fatalf("expected JSON201, got status %d: %s", createResp.StatusCode(), string(createResp.Body))
	}
	akID := createResp.JSON201.Accesskeyid

	t.Cleanup(func() {
		_, _ = testClient.DeleteUserAccessKeyWithResponse(context.Background(), testAccount, userID, akID)
	})

	// Get user access key.
	getResp, err := testClient.GetUserAccessKeyWithResponse(ctx, testAccount, userID, akID)
	if err != nil {
		t.Fatalf("GetUserAccessKey: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)

	// Head user access key.
	headResp, err := testClient.HeadUserAccessKeyWithResponse(ctx, testAccount, userID, akID)
	if err != nil {
		t.Fatalf("HeadUserAccessKey: %v", err)
	}
	requireOK(t, headResp.StatusCode(), nil)

	// Update user access key.
	newDesc := "updated user access key"
	updateResp, err := testClient.UpdateUserAccessKeyWithResponse(ctx, testAccount, userID, akID, cloudapi.UpdateUserAccessKeyJSONRequestBody{
		Description: &newDesc,
	})
	if err != nil {
		t.Fatalf("UpdateUserAccessKey: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// List user access keys.
	listResp, err := testClient.ListUserAccessKeysWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("ListUserAccessKeys: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)

	// Head user access keys collection.
	headAKsResp, err := testClient.HeadUserAccessKeysWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("HeadUserAccessKeys: %v", err)
	}
	requireOK(t, headAKsResp.StatusCode(), nil)

	// Delete user access key.
	delResp, err := testClient.DeleteUserAccessKeyWithResponse(ctx, testAccount, userID, akID)
	if err != nil {
		t.Fatalf("DeleteUserAccessKey: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)
}
