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
	"fmt"
	"math/rand"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

func TestIntegration_Users_CRUD(t *testing.T) {
	skipUnlessWriteActions(t)
	ctx := context.Background()

	userLogin := fmt.Sprintf("iuser%05d", rand.Intn(99999))
	userEmail := fmt.Sprintf("%s@example.com", userLogin)
	userPassword := fmt.Sprintf("Pass1!%d", rand.Intn(99999))

	// Create user.
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

	// Get user.
	getResp, err := testClient.GetUserWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("GetUser: %v", err)
	}
	requireOK(t, getResp.StatusCode(), getResp.Body)
	if getResp.JSON200 == nil {
		t.Fatal("expected JSON200 to be non-nil")
	}
	if getResp.JSON200.Login != userLogin {
		t.Errorf("expected login %q, got %q", userLogin, getResp.JSON200.Login)
	}

	// Head user.
	headUserResp, err := testClient.HeadUserWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("HeadUser: %v", err)
	}
	requireOK(t, headUserResp.StatusCode(), nil)

	// Head users collection.
	headUsersResp, err := testClient.HeadUsersWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("HeadUsers: %v", err)
	}
	requireOK(t, headUsersResp.StatusCode(), nil)

	// Update user.
	newCompany := "inttest-company"
	updateResp, err := testClient.UpdateUserWithResponse(ctx, testAccount, userID, cloudapi.UpdateUserJSONRequestBody{
		CompanyName: &newCompany,
	})
	if err != nil {
		t.Fatalf("UpdateUser: %v", err)
	}
	requireOK(t, updateResp.StatusCode(), updateResp.Body)

	// List users and verify our user is present.
	listResp, err := testClient.ListUsersWithResponse(ctx, testAccount)
	if err != nil {
		t.Fatalf("ListUsers: %v", err)
	}
	requireOK(t, listResp.StatusCode(), listResp.Body)
	found := false
	if listResp.JSON200 != nil {
		for _, u := range *listResp.JSON200 {
			if u.Login == userLogin {
				found = true
				break
			}
		}
	}
	if !found {
		t.Errorf("expected user %q in ListUsers response", userLogin)
	}

	// Change password.
	newPassword := randName("NewPass1!")
	chgResp, err := testClient.ChangeUserPasswordWithResponse(ctx, testAccount, userID, cloudapi.ChangeUserPasswordJSONRequestBody{
		Password:             newPassword,
		PasswordConfirmation: newPassword,
	})
	if err != nil {
		t.Fatalf("ChangeUserPassword: %v", err)
	}
	requireOK(t, chgResp.StatusCode(), chgResp.Body)

	// Delete user.
	delResp, err := testClient.DeleteUserWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("DeleteUser: %v", err)
	}
	requireOK(t, delResp.StatusCode(), delResp.Body)

	// Verify deleted. CloudAPI may return 500 wrapping a UFDS "not found" error
	// instead of a clean 404, so we accept that too.
	getResp2, err := testClient.GetUserWithResponse(ctx, testAccount, userID)
	if err != nil {
		t.Fatalf("GetUser after delete: %v", err)
	}
	sc := getResp2.StatusCode()
	if sc != 404 && sc != 410 && sc != 500 {
		t.Errorf("expected 404/410/500 after delete, got %d", sc)
	}
}
