//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package typed

import (
	"context"
	"errors"
	"net/http"
	"testing"

	openapi_types "github.com/oapi-codegen/runtime/types"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

// mockClient implements the 4 interface methods the typed client calls.
// Embedding the interface as nil means any other method panics, which is
// fine — those paths are never exercised by the typed wrapper.
type mockClient struct {
	cloudapi.ClientWithResponsesInterface

	// Captured from the most recent call
	gotAccount   string
	gotMachineID openapi_types.UUID
	gotImageID   openapi_types.UUID
	gotVolumeID  openapi_types.UUID
	gotDiskID    openapi_types.UUID
	gotBody      interface{}

	// Canned response
	statusCode int
	body       []byte
	err        error
}

func (m *mockClient) UpdateMachineWithResponse(_ context.Context, account string, machine openapi_types.UUID, _ *cloudapi.UpdateMachineParams, body cloudapi.UpdateMachineJSONRequestBody, _ ...cloudapi.RequestEditorFn) (*cloudapi.UpdateMachineResponse, error) {
	m.gotAccount = account
	m.gotMachineID = machine
	m.gotBody = body
	if m.err != nil {
		return nil, m.err
	}
	return &cloudapi.UpdateMachineResponse{
		Body:         m.body,
		HTTPResponse: &http.Response{StatusCode: m.statusCode},
	}, nil
}

func (m *mockClient) UpdateImageWithResponse(_ context.Context, account string, dataset openapi_types.UUID, _ *cloudapi.UpdateImageParams, body cloudapi.UpdateImageJSONRequestBody, _ ...cloudapi.RequestEditorFn) (*cloudapi.UpdateImageResponse, error) {
	m.gotAccount = account
	m.gotImageID = dataset
	m.gotBody = body
	if m.err != nil {
		return nil, m.err
	}
	return &cloudapi.UpdateImageResponse{
		Body:         m.body,
		HTTPResponse: &http.Response{StatusCode: m.statusCode},
	}, nil
}

func (m *mockClient) UpdateVolumeWithResponse(_ context.Context, account string, id openapi_types.UUID, _ *cloudapi.UpdateVolumeParams, body cloudapi.UpdateVolumeJSONRequestBody, _ ...cloudapi.RequestEditorFn) (*cloudapi.UpdateVolumeResponse, error) {
	m.gotAccount = account
	m.gotVolumeID = id
	m.gotBody = body
	if m.err != nil {
		return nil, m.err
	}
	return &cloudapi.UpdateVolumeResponse{
		Body:         m.body,
		HTTPResponse: &http.Response{StatusCode: m.statusCode},
	}, nil
}

func (m *mockClient) ResizeMachineDiskWithResponse(_ context.Context, account string, machine openapi_types.UUID, disk openapi_types.UUID, _ *cloudapi.ResizeMachineDiskParams, body cloudapi.ResizeMachineDiskJSONRequestBody, _ ...cloudapi.RequestEditorFn) (*cloudapi.ResizeMachineDiskResponse, error) {
	m.gotAccount = account
	m.gotMachineID = machine
	m.gotDiskID = disk
	m.gotBody = body
	if m.err != nil {
		return nil, m.err
	}
	return &cloudapi.ResizeMachineDiskResponse{
		Body:         m.body,
		HTTPResponse: &http.Response{StatusCode: m.statusCode},
	}, nil
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

func ptr(s string) *string { return &s }

func bodyMap(t *testing.T, raw interface{}) map[string]interface{} {
	t.Helper()
	m, ok := raw.(map[string]interface{})
	if !ok {
		t.Fatalf("expected map[string]interface{}, got %T", raw)
	}
	return m
}

// ---------------------------------------------------------------------------
// Tests: actionBody helper
// ---------------------------------------------------------------------------

func TestActionBody_NilRequest(t *testing.T) {
	m, err := actionBody("clone", nil)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if m["action"] != "clone" {
		t.Errorf("action = %q, want %q", m["action"], "clone")
	}
	if len(m) != 1 {
		t.Errorf("expected 1 key, got %d: %v", len(m), m)
	}
}

func TestActionBody_WithFields(t *testing.T) {
	req := cloudapi.ResizeMachineRequest{Package: "g4-highcpu-8G"}
	m, err := actionBody("resize", req)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if m["action"] != "resize" {
		t.Errorf("action = %q, want %q", m["action"], "resize")
	}
	if m["package"] != "g4-highcpu-8G" {
		t.Errorf("package = %q, want %q", m["package"], "g4-highcpu-8G")
	}
}

func TestActionBody_OmitsEmptyOptional(t *testing.T) {
	req := cloudapi.StartMachineRequest{} // Origin is omitempty
	m, err := actionBody("start", req)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if _, ok := m["origin"]; ok {
		t.Error("origin should be omitted when nil")
	}
}

func TestActionBody_IncludesOptionalWhenSet(t *testing.T) {
	req := cloudapi.StartMachineRequest{Origin: ptr("operator-portal")}
	m, err := actionBody("start", req)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if m["origin"] != "operator-portal" {
		t.Errorf("origin = %v, want %q", m["origin"], "operator-portal")
	}
}

// ---------------------------------------------------------------------------
// Tests: checkOK helper
// ---------------------------------------------------------------------------

func TestCheckOK(t *testing.T) {
	cases := []struct {
		status int
		wantOK bool
	}{
		{200, true},
		{202, true},
		{204, true},
		{299, true},
		{199, false},
		{300, false},
		{404, false},
		{500, false},
	}
	for _, tc := range cases {
		err := checkOK(tc.status, []byte("body"))
		if tc.wantOK && err != nil {
			t.Errorf("checkOK(%d) = %v, want nil", tc.status, err)
		}
		if !tc.wantOK && err == nil {
			t.Errorf("checkOK(%d) = nil, want error", tc.status)
		}
	}
}

func TestCheckOK_ErrorIncludesBody(t *testing.T) {
	err := checkOK(422, []byte(`{"code":"InvalidArgument","message":"bad"}`))
	if err == nil {
		t.Fatal("expected error")
	}
	if got := err.Error(); got != `cloudapi status 422: {"code":"InvalidArgument","message":"bad"}` {
		t.Errorf("unexpected error text: %s", got)
	}
}

// ---------------------------------------------------------------------------
// Tests: New / Inner
// ---------------------------------------------------------------------------

func TestNewAndInner(t *testing.T) {
	m := &mockClient{}
	c := New(m)
	if c.Inner() != m {
		t.Error("Inner() should return the same client passed to New()")
	}
}

// ---------------------------------------------------------------------------
// Tests: Machine actions
// ---------------------------------------------------------------------------

func TestStartMachine(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)
	id := openapi_types.UUID{0x01}

	err := c.StartMachine(context.Background(), "acct", id, cloudapi.StartMachineRequest{})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if m.gotAccount != "acct" {
		t.Errorf("account = %q, want %q", m.gotAccount, "acct")
	}
	if m.gotMachineID != id {
		t.Error("machine ID not forwarded")
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "start" {
		t.Errorf("action = %q, want %q", b["action"], "start")
	}
}

func TestStopMachine(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)
	id := openapi_types.UUID{0x02}

	err := c.StopMachine(context.Background(), "acct", id, cloudapi.StopMachineRequest{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "stop" {
		t.Errorf("action = %q, want %q", b["action"], "stop")
	}
}

func TestRebootMachine(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.RebootMachine(context.Background(), "a", openapi_types.UUID{}, cloudapi.RebootMachineRequest{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "reboot" {
		t.Errorf("action = %q, want %q", b["action"], "reboot")
	}
}

func TestResizeMachine(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.ResizeMachine(context.Background(), "a", openapi_types.UUID{}, cloudapi.ResizeMachineRequest{Package: "g4-highcpu-8G"})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "resize" {
		t.Errorf("action = %q, want %q", b["action"], "resize")
	}
	if b["package"] != "g4-highcpu-8G" {
		t.Errorf("package = %v, want %q", b["package"], "g4-highcpu-8G")
	}
}

func TestRenameMachine(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.RenameMachine(context.Background(), "a", openapi_types.UUID{}, cloudapi.RenameMachineRequest{Name: "new-name"})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "rename" {
		t.Errorf("action = %q, want %q", b["action"], "rename")
	}
	if b["name"] != "new-name" {
		t.Errorf("name = %v, want %q", b["name"], "new-name")
	}
}

func TestEnableFirewall(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.EnableFirewall(context.Background(), "a", openapi_types.UUID{}, cloudapi.EnableFirewallRequest{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "enable_firewall" {
		t.Errorf("action = %q, want %q", b["action"], "enable_firewall")
	}
}

func TestDisableFirewall(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.DisableFirewall(context.Background(), "a", openapi_types.UUID{}, cloudapi.DisableFirewallRequest{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "disable_firewall" {
		t.Errorf("action = %q, want %q", b["action"], "disable_firewall")
	}
}

func TestEnableDeletionProtection(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.EnableDeletionProtection(context.Background(), "a", openapi_types.UUID{}, cloudapi.EnableDeletionProtectionRequest{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "enable_deletion_protection" {
		t.Errorf("action = %q, want %q", b["action"], "enable_deletion_protection")
	}
}

func TestDisableDeletionProtection(t *testing.T) {
	m := &mockClient{statusCode: 202}
	c := New(m)

	err := c.DisableDeletionProtection(context.Background(), "a", openapi_types.UUID{}, cloudapi.DisableDeletionProtectionRequest{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "disable_deletion_protection" {
		t.Errorf("action = %q, want %q", b["action"], "disable_deletion_protection")
	}
}

// ---------------------------------------------------------------------------
// Tests: Image actions
// ---------------------------------------------------------------------------

func TestUpdateImageMetadata(t *testing.T) {
	m := &mockClient{statusCode: 200}
	c := New(m)
	id := openapi_types.UUID{0x10}

	desc := "new description"
	err := c.UpdateImageMetadata(context.Background(), "acct", id, cloudapi.UpdateImageRequest{Description: &desc})
	if err != nil {
		t.Fatal(err)
	}
	if m.gotImageID != id {
		t.Error("image ID not forwarded")
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "update" {
		t.Errorf("action = %q, want %q", b["action"], "update")
	}
	if b["description"] != "new description" {
		t.Errorf("description = %v, want %q", b["description"], "new description")
	}
}

func TestExportImage(t *testing.T) {
	m := &mockClient{statusCode: 200}
	c := New(m)

	err := c.ExportImage(context.Background(), "a", openapi_types.UUID{}, cloudapi.ExportImageRequest{MantaPath: "/user/stor/img"})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "export" {
		t.Errorf("action = %q, want %q", b["action"], "export")
	}
	if b["manta_path"] != "/user/stor/img" {
		t.Errorf("manta_path = %v, want %q", b["manta_path"], "/user/stor/img")
	}
}

func TestCloneImage(t *testing.T) {
	m := &mockClient{statusCode: 200}
	c := New(m)

	err := c.CloneImage(context.Background(), "a", openapi_types.UUID{})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "clone" {
		t.Errorf("action = %q, want %q", b["action"], "clone")
	}
	if len(b) != 1 {
		t.Errorf("expected only action key, got %v", b)
	}
}

func TestImportImage(t *testing.T) {
	m := &mockClient{statusCode: 200}
	c := New(m)
	imgID := openapi_types.UUID{0x20}

	err := c.ImportImage(context.Background(), "a", imgID, cloudapi.ImportImageRequest{
		Datacenter: "us-east-1",
		ID:         openapi_types.UUID{0x30},
	})
	if err != nil {
		t.Fatal(err)
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "import-from-datacenter" {
		t.Errorf("action = %q, want %q", b["action"], "import-from-datacenter")
	}
	if b["datacenter"] != "us-east-1" {
		t.Errorf("datacenter = %v, want %q", b["datacenter"], "us-east-1")
	}
}

// ---------------------------------------------------------------------------
// Tests: Volume action
// ---------------------------------------------------------------------------

func TestUpdateVolume(t *testing.T) {
	m := &mockClient{statusCode: 200}
	c := New(m)
	volID := openapi_types.UUID{0x40}
	name := "new-vol-name"

	err := c.UpdateVolume(context.Background(), "acct", volID, cloudapi.UpdateVolumeRequest{Name: &name})
	if err != nil {
		t.Fatal(err)
	}
	if m.gotVolumeID != volID {
		t.Error("volume ID not forwarded")
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "update" {
		t.Errorf("action = %q, want %q", b["action"], "update")
	}
	if b["name"] != "new-vol-name" {
		t.Errorf("name = %v, want %q", b["name"], "new-vol-name")
	}
}

// ---------------------------------------------------------------------------
// Tests: Disk action
// ---------------------------------------------------------------------------

func TestResizeDisk(t *testing.T) {
	m := &mockClient{statusCode: 200}
	c := New(m)
	machineID := openapi_types.UUID{0x50}
	diskID := openapi_types.UUID{0x60}

	err := c.ResizeDisk(context.Background(), "acct", machineID, diskID, cloudapi.ResizeDiskRequest{Size: 10240})
	if err != nil {
		t.Fatal(err)
	}
	if m.gotMachineID != machineID {
		t.Error("machine ID not forwarded")
	}
	if m.gotDiskID != diskID {
		t.Error("disk ID not forwarded")
	}
	b := bodyMap(t, m.gotBody)
	if b["action"] != "resize" {
		t.Errorf("action = %q, want %q", b["action"], "resize")
	}
	// JSON numbers decode as float64
	if b["size"] != float64(10240) {
		t.Errorf("size = %v, want %v", b["size"], float64(10240))
	}
}

// ---------------------------------------------------------------------------
// Tests: Error propagation
// ---------------------------------------------------------------------------

func TestMachineAction_InnerError(t *testing.T) {
	m := &mockClient{err: errors.New("connection refused")}
	c := New(m)

	err := c.StartMachine(context.Background(), "a", openapi_types.UUID{}, cloudapi.StartMachineRequest{})
	if err == nil {
		t.Fatal("expected error")
	}
	if got := err.Error(); got != "start machine 00000000-0000-0000-0000-000000000000: connection refused" {
		t.Errorf("unexpected error: %s", got)
	}
}

func TestMachineAction_Non2xx(t *testing.T) {
	m := &mockClient{statusCode: 404, body: []byte("not found")}
	c := New(m)

	err := c.StopMachine(context.Background(), "a", openapi_types.UUID{}, cloudapi.StopMachineRequest{})
	if err == nil {
		t.Fatal("expected error")
	}
	if got := err.Error(); got != "cloudapi status 404: not found" {
		t.Errorf("unexpected error: %s", got)
	}
}

func TestImageAction_InnerError(t *testing.T) {
	m := &mockClient{err: errors.New("timeout")}
	c := New(m)

	err := c.ExportImage(context.Background(), "a", openapi_types.UUID{}, cloudapi.ExportImageRequest{MantaPath: "/x"})
	if err == nil {
		t.Fatal("expected error")
	}
	if got := err.Error(); got != "export image 00000000-0000-0000-0000-000000000000: timeout" {
		t.Errorf("unexpected error: %s", got)
	}
}

func TestVolumeAction_Non2xx(t *testing.T) {
	m := &mockClient{statusCode: 500, body: []byte("internal error")}
	c := New(m)

	err := c.UpdateVolume(context.Background(), "a", openapi_types.UUID{}, cloudapi.UpdateVolumeRequest{})
	if err == nil {
		t.Fatal("expected error")
	}
	if got := err.Error(); got != "cloudapi status 500: internal error" {
		t.Errorf("unexpected error: %s", got)
	}
}

func TestDiskAction_InnerError(t *testing.T) {
	m := &mockClient{err: errors.New("refused")}
	c := New(m)
	diskID := openapi_types.UUID{0x60}

	err := c.ResizeDisk(context.Background(), "a", openapi_types.UUID{}, diskID, cloudapi.ResizeDiskRequest{Size: 1024})
	if err == nil {
		t.Fatal("expected error")
	}
	if got := err.Error(); got != "resize disk 60000000-0000-0000-0000-000000000000: refused" {
		t.Errorf("unexpected error: %s", got)
	}
}
