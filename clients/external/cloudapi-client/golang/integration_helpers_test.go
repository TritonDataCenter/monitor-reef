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
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"math/rand"
	"net/http"
	"net/http/httputil"
	"os"
	"testing"
	"time"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
	openapi_types "github.com/oapi-codegen/runtime/types"
)

// Package-level state shared by all integration tests.
var (
	testClient  *cloudapi.ClientWithResponses
	testAccount string
	testConfig  TestConfig
)

// TestConfig controls which integration test groups are enabled.
type TestConfig struct {
	AllowWriteActions bool `json:"allowWriteActions"`
	AllowVolumesTests bool `json:"allowVolumesTests"`
	AllowFabricTests  bool `json:"allowFabricTests"`
	SkipHvmTests      bool `json:"skipHvmTests"`
	SkipFlexDiskTests bool `json:"skipFlexDiskTests"`
}

// TestMain sets up the shared authenticated client for all integration tests.
// It is gated by the TRITON_TEST environment variable, matching the convention
// from triton-go's testutils.AccTest.
func TestMain(m *testing.M) {
	if os.Getenv("TRITON_TEST") == "" {
		fmt.Println("TRITON_TEST not set, skipping integration tests")
		os.Exit(0)
	}

	tritonURL := os.Getenv("TRITON_URL")
	if tritonURL == "" {
		fmt.Fprintln(os.Stderr, "TRITON_URL must be set")
		os.Exit(1)
	}

	testAccount = os.Getenv("TRITON_ACCOUNT")
	if testAccount == "" {
		fmt.Fprintln(os.Stderr, "TRITON_ACCOUNT must be set")
		os.Exit(1)
	}

	signer, err := cloudapi.LoadSignerFromEnv()
	if err != nil {
		fmt.Fprintf(os.Stderr, "LoadSignerFromEnv: %v\n", err)
		os.Exit(1)
	}

	// Build transport chain: TLS config → optional logging wrapper.
	transport := http.DefaultTransport.(*http.Transport).Clone()
	val := os.Getenv("TRITON_TLS_INSECURE")
	if val == "1" || val == "true" || val == "yes" {
		transport.TLSClientConfig = &tls.Config{
			InsecureSkipVerify: true, //nolint:gosec // intentional for dev/test
		}
	}

	var rt http.RoundTripper = transport
	if os.Getenv("TRITON_TEST_VERBOSE") != "" {
		rt = &loggingTransport{inner: transport}
	}

	extraOpts := []cloudapi.ClientOption{
		cloudapi.WithHTTPClient(&http.Client{Transport: rt}),
	}

	testClient, err = cloudapi.NewAuthenticatedClientWithResponses(
		tritonURL,
		cloudapi.SignatureAuthOptions{
			Signer:        signer,
			AcceptVersion: "~9",
		},
		extraOpts...,
	)
	if err != nil {
		fmt.Fprintf(os.Stderr, "NewAuthenticatedClientWithResponses: %v\n", err)
		os.Exit(1)
	}

	// Load optional test config.
	configPath := os.Getenv("TRITON_TEST_CONFIG")
	if configPath == "" {
		configPath = "testconfig.json"
	}
	if data, err := os.ReadFile(configPath); err == nil {
		if err := json.Unmarshal(data, &testConfig); err != nil {
			fmt.Fprintf(os.Stderr, "parse %s: %v\n", configPath, err)
			os.Exit(1)
		}
	}
	// If no config file, defaults are all zero-value (write actions disabled).

	os.Exit(m.Run())
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

// skipUnlessWriteActions skips the test if write actions are not enabled.
func skipUnlessWriteActions(t *testing.T) {
	t.Helper()
	if !testConfig.AllowWriteActions {
		t.Skip("write actions disabled (set allowWriteActions in testconfig.json)")
	}
}

// requireOK asserts an HTTP 2xx status code.
func requireOK(t *testing.T, statusCode int, body []byte) {
	t.Helper()
	if statusCode < 200 || statusCode >= 300 {
		t.Fatalf("expected HTTP 2xx, got %d: %s", statusCode, string(body))
	}
}

// requireStatus asserts a specific HTTP status code.
func requireStatus(t *testing.T, want, got int, body []byte) {
	t.Helper()
	if got != want {
		t.Fatalf("expected HTTP %d, got %d: %s", want, got, string(body))
	}
}

// randName returns a unique name with the given prefix.
func randName(prefix string) string {
	return fmt.Sprintf("%s-%d", prefix, rand.Int())
}

// ptr returns a pointer to the given value.
func ptr[T any](v T) *T {
	return &v
}

// waitForMachineName polls GetMachine until the machine's name matches the
// expected value or the timeout expires.
func waitForMachineName(t *testing.T, machineID openapi_types.UUID, wantName string, timeout time.Duration) {
	t.Helper()

	ctx := context.Background()
	deadline := time.Now().Add(timeout)

	for {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for machine %s to be renamed to %q", machineID, wantName)
		}

		resp, err := testClient.GetMachineWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("GetMachine while waiting for rename: %v", err)
		}
		if resp.JSON200 != nil && resp.JSON200.Name == wantName {
			return
		}

		time.Sleep(3 * time.Second)
	}
}

// waitForMachineState polls GetMachine until the machine reaches the target
// state or the timeout expires.
func waitForMachineState(t *testing.T, machineID openapi_types.UUID, targetState cloudapi.MachineState, timeout time.Duration) *cloudapi.Machine {
	t.Helper()

	ctx := context.Background()
	deadline := time.Now().Add(timeout)

	for {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for machine %s to reach state %q", machineID, targetState)
		}

		resp, err := testClient.GetMachineWithResponse(ctx, testAccount, machineID)
		if err != nil {
			t.Fatalf("GetMachine while waiting: %v", err)
		}
		if resp.JSON200 == nil {
			t.Fatalf("GetMachine returned %d while waiting for state %q", resp.StatusCode(), targetState)
		}

		if resp.JSON200.State == targetState {
			return resp.JSON200
		}
		if resp.JSON200.State == cloudapi.MachineStateFailed {
			t.Fatalf("machine %s entered failed state while waiting for %q", machineID, targetState)
		}

		time.Sleep(3 * time.Second)
	}
}

// cleanupMachine attempts to stop and delete a machine. It is intended to be
// registered with t.Cleanup() immediately after creation. Errors are logged
// but do not fail the test since cleanup is best-effort.
func cleanupMachine(t *testing.T, machineID openapi_types.UUID) {
	t.Helper()
	ctx := context.Background()

	// Check current state.
	resp, err := testClient.GetMachineWithResponse(ctx, testAccount, machineID)
	if err != nil {
		t.Logf("cleanup: GetMachine %s: %v", machineID, err)
		return
	}
	if resp.StatusCode() == 404 || resp.StatusCode() == 410 {
		return // already gone
	}

	// Stop if running.
	if resp.JSON200 != nil && resp.JSON200.State == cloudapi.MachineStateRunning {
		_, err := testClient.UpdateMachineWithResponse(ctx, testAccount, machineID,
			&cloudapi.UpdateMachineParams{},
			map[string]interface{}{"action": "stop"},
		)
		if err != nil {
			t.Logf("cleanup: stop machine %s: %v", machineID, err)
		}

		// Wait for stopped (best-effort, short timeout).
		deadline := time.Now().Add(2 * time.Minute)
		for time.Now().Before(deadline) {
			time.Sleep(3 * time.Second)
			r, err := testClient.GetMachineWithResponse(ctx, testAccount, machineID)
			if err != nil {
				break
			}
			if r.JSON200 != nil && r.JSON200.State == cloudapi.MachineStateStopped {
				break
			}
		}
	}

	// Delete.
	_, err = testClient.DeleteMachineWithResponse(ctx, testAccount, machineID)
	if err != nil {
		t.Logf("cleanup: delete machine %s: %v", machineID, err)
	}
}

// ---------------------------------------------------------------------------
// Logging transport (enabled via TRITON_TEST_VERBOSE=1)
// ---------------------------------------------------------------------------

// loggingTransport wraps an http.RoundTripper and logs request/response details
// to stderr. Enable with TRITON_TEST_VERBOSE=1.
type loggingTransport struct {
	inner http.RoundTripper
}

func (lt *loggingTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	// Log request.
	reqDump, _ := httputil.DumpRequestOut(req, false)
	var reqBody string
	if req.Body != nil && req.Body != http.NoBody {
		bodyBytes, err := io.ReadAll(req.Body)
		if err == nil {
			req.Body = io.NopCloser(bytes.NewReader(bodyBytes))
			if len(bodyBytes) > 0 {
				reqBody = string(bodyBytes)
			}
		}
	}
	log.Printf(">>> %s %s\n%s", req.Method, req.URL, reqDump)
	if reqBody != "" {
		log.Printf(">>> BODY: %s", reqBody)
	}

	// Execute request.
	resp, err := lt.inner.RoundTrip(req)
	if err != nil {
		log.Printf("<<< ERROR: %v", err)
		return resp, err
	}

	// Log response (read and re-buffer the body).
	respBody, _ := io.ReadAll(resp.Body)
	resp.Body = io.NopCloser(bytes.NewReader(respBody))

	log.Printf("<<< %d %s", resp.StatusCode, resp.Status)
	if len(respBody) > 0 {
		// Truncate very large bodies.
		body := string(respBody)
		if len(body) > 2048 {
			body = body[:2048] + "...(truncated)"
		}
		log.Printf("<<< BODY: %s", body)
	}

	return resp, nil
}
