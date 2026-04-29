//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package cloudapi_test

import (
	"encoding/json"
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

// TestCreateVolumeRequest_SizeOmitEmpty verifies that CreateVolumeRequest
// omits the "size" field from JSON when Size is nil. This matches the actual
// CloudAPI behavior where size is optional -- when omitted, the server picks
// the smallest available volume size from the NFS billing packages. A previous
// bug had size as a required uint64, causing clients that didn't set a size
// to send "size": 0, which CloudAPI rejects with HTTP 409.
func TestCreateVolumeRequest_SizeOmitEmpty(t *testing.T) {
	t.Run("nil_size_omitted", func(t *testing.T) {
		name := "test-vol"
		req := cloudapi.CreateVolumeRequest{
			Name: &name,
		}
		data, err := json.Marshal(req)
		if err != nil {
			t.Fatalf("Marshal: %v", err)
		}

		var m map[string]interface{}
		if err := json.Unmarshal(data, &m); err != nil {
			t.Fatalf("Unmarshal: %v", err)
		}
		if _, ok := m["size"]; ok {
			t.Errorf("expected size to be absent from JSON, got %v", m["size"])
		}
	})

	t.Run("explicit_size_present", func(t *testing.T) {
		name := "test-vol"
		size := uint64(10240)
		req := cloudapi.CreateVolumeRequest{
			Name: &name,
			Size: &size,
		}
		data, err := json.Marshal(req)
		if err != nil {
			t.Fatalf("Marshal: %v", err)
		}

		var m map[string]interface{}
		if err := json.Unmarshal(data, &m); err != nil {
			t.Fatalf("Unmarshal: %v", err)
		}
		v, ok := m["size"]
		if !ok {
			t.Fatal("expected size to be present in JSON")
		}
		// JSON numbers unmarshal as float64.
		if v.(float64) != 10240 {
			t.Errorf("expected size 10240, got %v", v)
		}
	})
}
