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

func TestUnionTypes(t *testing.T) {
	t.Run("DiskAction", func(t *testing.T) {
		var u cloudapi.DiskAction

		// Test From* variant 0
		if err := u.FromDiskAction0(cloudapi.DiskAction0Resize); err != nil {
			t.Fatalf("FromDiskAction0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.DiskAction
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsDiskAction0()
		if err != nil {
			t.Fatalf("AsDiskAction0: %v", err)
		}
		if v != cloudapi.DiskAction0Resize {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.DiskAction0Resize)
		}

		// Test Merge* variant 0
		if err := u.MergeDiskAction0(cloudapi.DiskAction0Resize); err != nil {
			t.Fatalf("MergeDiskAction0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromDiskAction1(cloudapi.DiskAction1Unknown); err != nil {
			t.Fatalf("FromDiskAction1: %v", err)
		}
		v1, err := u.AsDiskAction1()
		if err != nil {
			t.Fatalf("AsDiskAction1: %v", err)
		}
		if v1 != cloudapi.DiskAction1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.DiskAction1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeDiskAction1(cloudapi.DiskAction1Unknown); err != nil {
			t.Fatalf("MergeDiskAction1: %v", err)
		}
	})

	t.Run("DiskSize", func(t *testing.T) {
		var u cloudapi.DiskSize

		// DiskSize0 is uint64
		var val0 cloudapi.DiskSize0 = 10240

		// Test From* variant 0
		if err := u.FromDiskSize0(val0); err != nil {
			t.Fatalf("FromDiskSize0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.DiskSize
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsDiskSize0()
		if err != nil {
			t.Fatalf("AsDiskSize0: %v", err)
		}
		if v != val0 {
			t.Fatalf("round-trip: got %v, want %v", v, val0)
		}

		// Test Merge* variant 0
		if err := u.MergeDiskSize0(val0); err != nil {
			t.Fatalf("MergeDiskSize0: %v", err)
		}

		// DiskSize1 is a string type alias
		var val1 cloudapi.DiskSize1 = "remaining"

		// Test From/As for variant 1
		if err := u.FromDiskSize1(val1); err != nil {
			t.Fatalf("FromDiskSize1: %v", err)
		}
		v1, err := u.AsDiskSize1()
		if err != nil {
			t.Fatalf("AsDiskSize1: %v", err)
		}
		if v1 != val1 {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, val1)
		}

		// Test Merge variant 1
		if err := u.MergeDiskSize1(val1); err != nil {
			t.Fatalf("MergeDiskSize1: %v", err)
		}
	})

	t.Run("ImageAction", func(t *testing.T) {
		var u cloudapi.ImageAction

		// Test From* variant 0
		if err := u.FromImageAction0(cloudapi.ImageAction0Clone); err != nil {
			t.Fatalf("FromImageAction0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.ImageAction
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsImageAction0()
		if err != nil {
			t.Fatalf("AsImageAction0: %v", err)
		}
		if v != cloudapi.ImageAction0Clone {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.ImageAction0Clone)
		}

		// Test Merge* variant 0
		if err := u.MergeImageAction0(cloudapi.ImageAction0Clone); err != nil {
			t.Fatalf("MergeImageAction0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromImageAction1(cloudapi.ImageAction1Unknown); err != nil {
			t.Fatalf("FromImageAction1: %v", err)
		}
		v1, err := u.AsImageAction1()
		if err != nil {
			t.Fatalf("AsImageAction1: %v", err)
		}
		if v1 != cloudapi.ImageAction1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.ImageAction1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeImageAction1(cloudapi.ImageAction1Unknown); err != nil {
			t.Fatalf("MergeImageAction1: %v", err)
		}
	})

	t.Run("ImageState", func(t *testing.T) {
		var u cloudapi.ImageState

		// Test From* variant 0
		if err := u.FromImageState0(cloudapi.ImageState0Active); err != nil {
			t.Fatalf("FromImageState0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.ImageState
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsImageState0()
		if err != nil {
			t.Fatalf("AsImageState0: %v", err)
		}
		if v != cloudapi.ImageState0Active {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.ImageState0Active)
		}

		// Test Merge* variant 0
		if err := u.MergeImageState0(cloudapi.ImageState0Active); err != nil {
			t.Fatalf("MergeImageState0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromImageState1(cloudapi.All); err != nil {
			t.Fatalf("FromImageState1: %v", err)
		}
		v1, err := u.AsImageState1()
		if err != nil {
			t.Fatalf("AsImageState1: %v", err)
		}
		if v1 != cloudapi.All {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.All)
		}

		// Test Merge variant 1
		if err := u.MergeImageState1(cloudapi.All); err != nil {
			t.Fatalf("MergeImageState1: %v", err)
		}
	})

	t.Run("ImageType", func(t *testing.T) {
		var u cloudapi.ImageType

		// Test From* variant 0
		if err := u.FromImageType0(cloudapi.Docker); err != nil {
			t.Fatalf("FromImageType0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.ImageType
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsImageType0()
		if err != nil {
			t.Fatalf("AsImageType0: %v", err)
		}
		if v != cloudapi.Docker {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.Docker)
		}

		// Test Merge* variant 0
		if err := u.MergeImageType0(cloudapi.Docker); err != nil {
			t.Fatalf("MergeImageType0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromImageType1(cloudapi.ImageType1Unknown); err != nil {
			t.Fatalf("FromImageType1: %v", err)
		}
		v1, err := u.AsImageType1()
		if err != nil {
			t.Fatalf("AsImageType1: %v", err)
		}
		if v1 != cloudapi.ImageType1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.ImageType1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeImageType1(cloudapi.ImageType1Unknown); err != nil {
			t.Fatalf("MergeImageType1: %v", err)
		}
	})

	t.Run("MachineAction", func(t *testing.T) {
		var u cloudapi.MachineAction

		// Test From* variant 0
		if err := u.FromMachineAction0(cloudapi.MachineAction0DisableDeletionProtection); err != nil {
			t.Fatalf("FromMachineAction0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.MachineAction
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsMachineAction0()
		if err != nil {
			t.Fatalf("AsMachineAction0: %v", err)
		}
		if v != cloudapi.MachineAction0DisableDeletionProtection {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.MachineAction0DisableDeletionProtection)
		}

		// Test Merge* variant 0
		if err := u.MergeMachineAction0(cloudapi.MachineAction0DisableDeletionProtection); err != nil {
			t.Fatalf("MergeMachineAction0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromMachineAction1(cloudapi.MachineAction1Unknown); err != nil {
			t.Fatalf("FromMachineAction1: %v", err)
		}
		v1, err := u.AsMachineAction1()
		if err != nil {
			t.Fatalf("AsMachineAction1: %v", err)
		}
		if v1 != cloudapi.MachineAction1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.MachineAction1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeMachineAction1(cloudapi.MachineAction1Unknown); err != nil {
			t.Fatalf("MergeMachineAction1: %v", err)
		}
	})

	t.Run("MachineType", func(t *testing.T) {
		var u cloudapi.MachineType

		// Test From* variant 0
		if err := u.FromMachineType0(cloudapi.Smartmachine); err != nil {
			t.Fatalf("FromMachineType0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.MachineType
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsMachineType0()
		if err != nil {
			t.Fatalf("AsMachineType0: %v", err)
		}
		if v != cloudapi.Smartmachine {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.Smartmachine)
		}

		// Test Merge* variant 0
		if err := u.MergeMachineType0(cloudapi.Smartmachine); err != nil {
			t.Fatalf("MergeMachineType0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromMachineType1(cloudapi.Virtualmachine); err != nil {
			t.Fatalf("FromMachineType1: %v", err)
		}
		v1, err := u.AsMachineType1()
		if err != nil {
			t.Fatalf("AsMachineType1: %v", err)
		}
		if v1 != cloudapi.Virtualmachine {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.Virtualmachine)
		}

		// Test Merge variant 1
		if err := u.MergeMachineType1(cloudapi.Virtualmachine); err != nil {
			t.Fatalf("MergeMachineType1: %v", err)
		}

		// Test From/As for variant 2
		if err := u.FromMachineType2(cloudapi.MachineType2Unknown); err != nil {
			t.Fatalf("FromMachineType2: %v", err)
		}
		v2, err := u.AsMachineType2()
		if err != nil {
			t.Fatalf("AsMachineType2: %v", err)
		}
		if v2 != cloudapi.MachineType2Unknown {
			t.Fatalf("variant2 round-trip: got %v, want %v", v2, cloudapi.MachineType2Unknown)
		}

		// Test Merge variant 2
		if err := u.MergeMachineType2(cloudapi.MachineType2Unknown); err != nil {
			t.Fatalf("MergeMachineType2: %v", err)
		}
	})

	t.Run("MemberType", func(t *testing.T) {
		var u cloudapi.MemberType

		// Test From* variant 0
		if err := u.FromMemberType0(cloudapi.MemberType0Account); err != nil {
			t.Fatalf("FromMemberType0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.MemberType
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsMemberType0()
		if err != nil {
			t.Fatalf("AsMemberType0: %v", err)
		}
		if v != cloudapi.MemberType0Account {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.MemberType0Account)
		}

		// Test Merge* variant 0
		if err := u.MergeMemberType0(cloudapi.MemberType0Account); err != nil {
			t.Fatalf("MergeMemberType0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromMemberType1(cloudapi.MemberType1Unknown); err != nil {
			t.Fatalf("FromMemberType1: %v", err)
		}
		v1, err := u.AsMemberType1()
		if err != nil {
			t.Fatalf("AsMemberType1: %v", err)
		}
		if v1 != cloudapi.MemberType1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.MemberType1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeMemberType1(cloudapi.MemberType1Unknown); err != nil {
			t.Fatalf("MergeMemberType1: %v", err)
		}
	})

	t.Run("MigrationAction", func(t *testing.T) {
		var u cloudapi.MigrationAction

		// Test From/As variant 0
		if err := u.FromMigrationAction0(cloudapi.MigrationAction0Begin); err != nil {
			t.Fatalf("FromMigrationAction0: %v", err)
		}
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}
		var u2 cloudapi.MigrationAction
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}
		v0, err := u2.AsMigrationAction0()
		if err != nil {
			t.Fatalf("AsMigrationAction0: %v", err)
		}
		if v0 != cloudapi.MigrationAction0Begin {
			t.Fatalf("variant0 round-trip: got %v, want %v", v0, cloudapi.MigrationAction0Begin)
		}
		if err := u.MergeMigrationAction0(cloudapi.MigrationAction0Begin); err != nil {
			t.Fatalf("MergeMigrationAction0: %v", err)
		}

		// Test From/As variant 1
		if err := u.FromMigrationAction1(cloudapi.Sync); err != nil {
			t.Fatalf("FromMigrationAction1: %v", err)
		}
		v1, err := u.AsMigrationAction1()
		if err != nil {
			t.Fatalf("AsMigrationAction1: %v", err)
		}
		if v1 != cloudapi.Sync {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.Sync)
		}
		if err := u.MergeMigrationAction1(cloudapi.Sync); err != nil {
			t.Fatalf("MergeMigrationAction1: %v", err)
		}

		// Test From/As variant 2
		if err := u.FromMigrationAction2(cloudapi.Switch); err != nil {
			t.Fatalf("FromMigrationAction2: %v", err)
		}
		v2, err := u.AsMigrationAction2()
		if err != nil {
			t.Fatalf("AsMigrationAction2: %v", err)
		}
		if v2 != cloudapi.Switch {
			t.Fatalf("variant2 round-trip: got %v, want %v", v2, cloudapi.Switch)
		}
		if err := u.MergeMigrationAction2(cloudapi.Switch); err != nil {
			t.Fatalf("MergeMigrationAction2: %v", err)
		}

		// Test From/As variant 3
		if err := u.FromMigrationAction3(cloudapi.Automatic); err != nil {
			t.Fatalf("FromMigrationAction3: %v", err)
		}
		v3, err := u.AsMigrationAction3()
		if err != nil {
			t.Fatalf("AsMigrationAction3: %v", err)
		}
		if v3 != cloudapi.Automatic {
			t.Fatalf("variant3 round-trip: got %v, want %v", v3, cloudapi.Automatic)
		}
		if err := u.MergeMigrationAction3(cloudapi.Automatic); err != nil {
			t.Fatalf("MergeMigrationAction3: %v", err)
		}

		// Test From/As variant 4
		if err := u.FromMigrationAction4(cloudapi.Abort); err != nil {
			t.Fatalf("FromMigrationAction4: %v", err)
		}
		v4, err := u.AsMigrationAction4()
		if err != nil {
			t.Fatalf("AsMigrationAction4: %v", err)
		}
		if v4 != cloudapi.Abort {
			t.Fatalf("variant4 round-trip: got %v, want %v", v4, cloudapi.Abort)
		}
		if err := u.MergeMigrationAction4(cloudapi.Abort); err != nil {
			t.Fatalf("MergeMigrationAction4: %v", err)
		}

		// Test From/As variant 5
		if err := u.FromMigrationAction5(cloudapi.Pause); err != nil {
			t.Fatalf("FromMigrationAction5: %v", err)
		}
		v5, err := u.AsMigrationAction5()
		if err != nil {
			t.Fatalf("AsMigrationAction5: %v", err)
		}
		if v5 != cloudapi.Pause {
			t.Fatalf("variant5 round-trip: got %v, want %v", v5, cloudapi.Pause)
		}
		if err := u.MergeMigrationAction5(cloudapi.Pause); err != nil {
			t.Fatalf("MergeMigrationAction5: %v", err)
		}

		// Test From/As variant 6
		if err := u.FromMigrationAction6(cloudapi.Finalize); err != nil {
			t.Fatalf("FromMigrationAction6: %v", err)
		}
		v6, err := u.AsMigrationAction6()
		if err != nil {
			t.Fatalf("AsMigrationAction6: %v", err)
		}
		if v6 != cloudapi.Finalize {
			t.Fatalf("variant6 round-trip: got %v, want %v", v6, cloudapi.Finalize)
		}
		if err := u.MergeMigrationAction6(cloudapi.Finalize); err != nil {
			t.Fatalf("MergeMigrationAction6: %v", err)
		}
	})

	t.Run("MigrationPhase", func(t *testing.T) {
		var u cloudapi.MigrationPhase

		// Test From* variant 0
		if err := u.FromMigrationPhase0(cloudapi.MigrationPhase0Begin); err != nil {
			t.Fatalf("FromMigrationPhase0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.MigrationPhase
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsMigrationPhase0()
		if err != nil {
			t.Fatalf("AsMigrationPhase0: %v", err)
		}
		if v != cloudapi.MigrationPhase0Begin {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.MigrationPhase0Begin)
		}

		// Test Merge* variant 0
		if err := u.MergeMigrationPhase0(cloudapi.MigrationPhase0Begin); err != nil {
			t.Fatalf("MergeMigrationPhase0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromMigrationPhase1(cloudapi.MigrationPhase1Unknown); err != nil {
			t.Fatalf("FromMigrationPhase1: %v", err)
		}
		v1, err := u.AsMigrationPhase1()
		if err != nil {
			t.Fatalf("AsMigrationPhase1: %v", err)
		}
		if v1 != cloudapi.MigrationPhase1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.MigrationPhase1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeMigrationPhase1(cloudapi.MigrationPhase1Unknown); err != nil {
			t.Fatalf("MergeMigrationPhase1: %v", err)
		}
	})

	t.Run("MountMode", func(t *testing.T) {
		var u cloudapi.MountMode

		// Test From* variant 0
		if err := u.FromMountMode0(cloudapi.Rw); err != nil {
			t.Fatalf("FromMountMode0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.MountMode
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsMountMode0()
		if err != nil {
			t.Fatalf("AsMountMode0: %v", err)
		}
		if v != cloudapi.Rw {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.Rw)
		}

		// Test Merge* variant 0
		if err := u.MergeMountMode0(cloudapi.Rw); err != nil {
			t.Fatalf("MergeMountMode0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromMountMode1(cloudapi.Ro); err != nil {
			t.Fatalf("FromMountMode1: %v", err)
		}
		v1, err := u.AsMountMode1()
		if err != nil {
			t.Fatalf("AsMountMode1: %v", err)
		}
		if v1 != cloudapi.Ro {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.Ro)
		}

		// Test Merge variant 1
		if err := u.MergeMountMode1(cloudapi.Ro); err != nil {
			t.Fatalf("MergeMountMode1: %v", err)
		}

		// Test From/As for variant 2
		if err := u.FromMountMode2(cloudapi.MountMode2Unknown); err != nil {
			t.Fatalf("FromMountMode2: %v", err)
		}
		v2, err := u.AsMountMode2()
		if err != nil {
			t.Fatalf("AsMountMode2: %v", err)
		}
		if v2 != cloudapi.MountMode2Unknown {
			t.Fatalf("variant2 round-trip: got %v, want %v", v2, cloudapi.MountMode2Unknown)
		}

		// Test Merge variant 2
		if err := u.MergeMountMode2(cloudapi.MountMode2Unknown); err != nil {
			t.Fatalf("MergeMountMode2: %v", err)
		}
	})

	t.Run("SnapshotState", func(t *testing.T) {
		var u cloudapi.SnapshotState

		// Test From* variant 0
		if err := u.FromSnapshotState0(cloudapi.SnapshotState0Created); err != nil {
			t.Fatalf("FromSnapshotState0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.SnapshotState
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsSnapshotState0()
		if err != nil {
			t.Fatalf("AsSnapshotState0: %v", err)
		}
		if v != cloudapi.SnapshotState0Created {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.SnapshotState0Created)
		}

		// Test Merge* variant 0
		if err := u.MergeSnapshotState0(cloudapi.SnapshotState0Created); err != nil {
			t.Fatalf("MergeSnapshotState0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromSnapshotState1(cloudapi.SnapshotState1Unknown); err != nil {
			t.Fatalf("FromSnapshotState1: %v", err)
		}
		v1, err := u.AsSnapshotState1()
		if err != nil {
			t.Fatalf("AsSnapshotState1: %v", err)
		}
		if v1 != cloudapi.SnapshotState1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.SnapshotState1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeSnapshotState1(cloudapi.SnapshotState1Unknown); err != nil {
			t.Fatalf("MergeSnapshotState1: %v", err)
		}
	})

	t.Run("VMBrand", func(t *testing.T) {
		var u cloudapi.VMBrand

		// Test From* variant 0
		if err := u.FromVMBrand0(cloudapi.VMBrand0Bhyve); err != nil {
			t.Fatalf("FromVMBrand0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.VMBrand
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsVMBrand0()
		if err != nil {
			t.Fatalf("AsVMBrand0: %v", err)
		}
		if v != cloudapi.VMBrand0Bhyve {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.VMBrand0Bhyve)
		}

		// Test Merge* variant 0
		if err := u.MergeVMBrand0(cloudapi.VMBrand0Bhyve); err != nil {
			t.Fatalf("MergeVMBrand0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromVMBrand1(cloudapi.Builder); err != nil {
			t.Fatalf("FromVMBrand1: %v", err)
		}
		v1, err := u.AsVMBrand1()
		if err != nil {
			t.Fatalf("AsVMBrand1: %v", err)
		}
		if v1 != cloudapi.Builder {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.Builder)
		}

		// Test Merge variant 1
		if err := u.MergeVMBrand1(cloudapi.Builder); err != nil {
			t.Fatalf("MergeVMBrand1: %v", err)
		}

		// Test From/As for variant 2
		if err := u.FromVMBrand2(cloudapi.VMBrand2Unknown); err != nil {
			t.Fatalf("FromVMBrand2: %v", err)
		}
		v2, err := u.AsVMBrand2()
		if err != nil {
			t.Fatalf("AsVMBrand2: %v", err)
		}
		if v2 != cloudapi.VMBrand2Unknown {
			t.Fatalf("variant2 round-trip: got %v, want %v", v2, cloudapi.VMBrand2Unknown)
		}

		// Test Merge variant 2
		if err := u.MergeVMBrand2(cloudapi.VMBrand2Unknown); err != nil {
			t.Fatalf("MergeVMBrand2: %v", err)
		}
	})

	t.Run("VolumeAction", func(t *testing.T) {
		var u cloudapi.VolumeAction

		// Test From* variant 0
		if err := u.FromVolumeAction0(cloudapi.VolumeAction0Update); err != nil {
			t.Fatalf("FromVolumeAction0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.VolumeAction
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsVolumeAction0()
		if err != nil {
			t.Fatalf("AsVolumeAction0: %v", err)
		}
		if v != cloudapi.VolumeAction0Update {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.VolumeAction0Update)
		}

		// Test Merge* variant 0
		if err := u.MergeVolumeAction0(cloudapi.VolumeAction0Update); err != nil {
			t.Fatalf("MergeVolumeAction0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromVolumeAction1(cloudapi.VolumeAction1Unknown); err != nil {
			t.Fatalf("FromVolumeAction1: %v", err)
		}
		v1, err := u.AsVolumeAction1()
		if err != nil {
			t.Fatalf("AsVolumeAction1: %v", err)
		}
		if v1 != cloudapi.VolumeAction1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.VolumeAction1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeVolumeAction1(cloudapi.VolumeAction1Unknown); err != nil {
			t.Fatalf("MergeVolumeAction1: %v", err)
		}
	})

	t.Run("VolumeType", func(t *testing.T) {
		var u cloudapi.VolumeType

		// Test From* variant 0
		if err := u.FromVolumeType0(cloudapi.Tritonnfs); err != nil {
			t.Fatalf("FromVolumeType0: %v", err)
		}

		// Test MarshalJSON
		data, err := json.Marshal(u)
		if err != nil {
			t.Fatalf("MarshalJSON: %v", err)
		}

		// Test UnmarshalJSON
		var u2 cloudapi.VolumeType
		if err := json.Unmarshal(data, &u2); err != nil {
			t.Fatalf("UnmarshalJSON: %v", err)
		}

		// Test As* variant 0 (round-trip)
		v, err := u2.AsVolumeType0()
		if err != nil {
			t.Fatalf("AsVolumeType0: %v", err)
		}
		if v != cloudapi.Tritonnfs {
			t.Fatalf("round-trip: got %v, want %v", v, cloudapi.Tritonnfs)
		}

		// Test Merge* variant 0
		if err := u.MergeVolumeType0(cloudapi.Tritonnfs); err != nil {
			t.Fatalf("MergeVolumeType0: %v", err)
		}

		// Test From/As for variant 1
		if err := u.FromVolumeType1(cloudapi.VolumeType1Unknown); err != nil {
			t.Fatalf("FromVolumeType1: %v", err)
		}
		v1, err := u.AsVolumeType1()
		if err != nil {
			t.Fatalf("AsVolumeType1: %v", err)
		}
		if v1 != cloudapi.VolumeType1Unknown {
			t.Fatalf("variant1 round-trip: got %v, want %v", v1, cloudapi.VolumeType1Unknown)
		}

		// Test Merge variant 1
		if err := u.MergeVolumeType1(cloudapi.VolumeType1Unknown); err != nil {
			t.Fatalf("MergeVolumeType1: %v", err)
		}
	})
}
