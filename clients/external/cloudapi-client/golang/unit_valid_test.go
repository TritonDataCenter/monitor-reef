//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package cloudapi_test

import (
	"testing"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

func TestEnumValid(t *testing.T) {
	tests := []struct {
		name    string
		valid   func() bool
		invalid func() bool
	}{
		{"AccessKeyStatus", func() bool { return cloudapi.AccessKeyStatusActive.Valid() }, func() bool { return cloudapi.AccessKeyStatus("__invalid__").Valid() }},
		{"AuditSuccess", func() bool { return cloudapi.AuditSuccessNo.Valid() }, func() bool { return cloudapi.AuditSuccess("__invalid__").Valid() }},
		{"Brand", func() bool { return cloudapi.BrandBhyve.Valid() }, func() bool { return cloudapi.Brand("__invalid__").Valid() }},
		{"CredentialType", func() bool { return cloudapi.CredentialTypePermanent.Valid() }, func() bool { return cloudapi.CredentialType("__invalid__").Valid() }},
		{"DiskAction0", func() bool { return cloudapi.DiskAction0Resize.Valid() }, func() bool { return cloudapi.DiskAction0("__invalid__").Valid() }},
		{"DiskAction1", func() bool { return cloudapi.DiskAction1Unknown.Valid() }, func() bool { return cloudapi.DiskAction1("__invalid__").Valid() }},
		{"DiskState", func() bool { return cloudapi.DiskStateCreating.Valid() }, func() bool { return cloudapi.DiskState("__invalid__").Valid() }},
		{"ImageAction0", func() bool { return cloudapi.ImageAction0Clone.Valid() }, func() bool { return cloudapi.ImageAction0("__invalid__").Valid() }},
		{"ImageAction1", func() bool { return cloudapi.ImageAction1Unknown.Valid() }, func() bool { return cloudapi.ImageAction1("__invalid__").Valid() }},
		{"ImageState0", func() bool { return cloudapi.ImageState0Active.Valid() }, func() bool { return cloudapi.ImageState0("__invalid__").Valid() }},
		{"ImageState1", func() bool { return cloudapi.All.Valid() }, func() bool { return cloudapi.ImageState1("__invalid__").Valid() }},
		{"ImageType0", func() bool { return cloudapi.Docker.Valid() }, func() bool { return cloudapi.ImageType0("__invalid__").Valid() }},
		{"ImageType1", func() bool { return cloudapi.ImageType1Unknown.Valid() }, func() bool { return cloudapi.ImageType1("__invalid__").Valid() }},
		{"MachineAction0", func() bool { return cloudapi.MachineAction0DisableDeletionProtection.Valid() }, func() bool { return cloudapi.MachineAction0("__invalid__").Valid() }},
		{"MachineAction1", func() bool { return cloudapi.MachineAction1Unknown.Valid() }, func() bool { return cloudapi.MachineAction1("__invalid__").Valid() }},
		{"MachineState", func() bool { return cloudapi.MachineStateDeleted.Valid() }, func() bool { return cloudapi.MachineState("__invalid__").Valid() }},
		{"MachineType0", func() bool { return cloudapi.Smartmachine.Valid() }, func() bool { return cloudapi.MachineType0("__invalid__").Valid() }},
		{"MachineType1", func() bool { return cloudapi.Virtualmachine.Valid() }, func() bool { return cloudapi.MachineType1("__invalid__").Valid() }},
		{"MachineType2", func() bool { return cloudapi.MachineType2Unknown.Valid() }, func() bool { return cloudapi.MachineType2("__invalid__").Valid() }},
		{"MemberType0", func() bool { return cloudapi.MemberType0Account.Valid() }, func() bool { return cloudapi.MemberType0("__invalid__").Valid() }},
		{"MemberType1", func() bool { return cloudapi.MemberType1Unknown.Valid() }, func() bool { return cloudapi.MemberType1("__invalid__").Valid() }},
		{"MigrationAction0", func() bool { return cloudapi.MigrationAction0Begin.Valid() }, func() bool { return cloudapi.MigrationAction0("__invalid__").Valid() }},
		{"MigrationAction1", func() bool { return cloudapi.Sync.Valid() }, func() bool { return cloudapi.MigrationAction1("__invalid__").Valid() }},
		{"MigrationAction2", func() bool { return cloudapi.Switch.Valid() }, func() bool { return cloudapi.MigrationAction2("__invalid__").Valid() }},
		{"MigrationAction3", func() bool { return cloudapi.Automatic.Valid() }, func() bool { return cloudapi.MigrationAction3("__invalid__").Valid() }},
		{"MigrationAction4", func() bool { return cloudapi.Abort.Valid() }, func() bool { return cloudapi.MigrationAction4("__invalid__").Valid() }},
		{"MigrationAction5", func() bool { return cloudapi.Pause.Valid() }, func() bool { return cloudapi.MigrationAction5("__invalid__").Valid() }},
		{"MigrationAction6", func() bool { return cloudapi.Finalize.Valid() }, func() bool { return cloudapi.MigrationAction6("__invalid__").Valid() }},
		{"MigrationPhase0", func() bool { return cloudapi.MigrationPhase0Begin.Valid() }, func() bool { return cloudapi.MigrationPhase0("__invalid__").Valid() }},
		{"MigrationPhase1", func() bool { return cloudapi.MigrationPhase1Unknown.Valid() }, func() bool { return cloudapi.MigrationPhase1("__invalid__").Valid() }},
		{"MigrationState", func() bool { return cloudapi.MigrationStateAborted.Valid() }, func() bool { return cloudapi.MigrationState("__invalid__").Valid() }},
		{"MountMode0", func() bool { return cloudapi.Rw.Valid() }, func() bool { return cloudapi.MountMode0("__invalid__").Valid() }},
		{"MountMode1", func() bool { return cloudapi.Ro.Valid() }, func() bool { return cloudapi.MountMode1("__invalid__").Valid() }},
		{"MountMode2", func() bool { return cloudapi.MountMode2Unknown.Valid() }, func() bool { return cloudapi.MountMode2("__invalid__").Valid() }},
		{"NicState", func() bool { return cloudapi.NicStateProvisioning.Valid() }, func() bool { return cloudapi.NicState("__invalid__").Valid() }},
		{"SnapshotState0", func() bool { return cloudapi.SnapshotState0Created.Valid() }, func() bool { return cloudapi.SnapshotState0("__invalid__").Valid() }},
		{"SnapshotState1", func() bool { return cloudapi.SnapshotState1Unknown.Valid() }, func() bool { return cloudapi.SnapshotState1("__invalid__").Valid() }},
		{"VMBrand0", func() bool { return cloudapi.VMBrand0Bhyve.Valid() }, func() bool { return cloudapi.VMBrand0("__invalid__").Valid() }},
		{"VMBrand1", func() bool { return cloudapi.Builder.Valid() }, func() bool { return cloudapi.VMBrand1("__invalid__").Valid() }},
		{"VMBrand2", func() bool { return cloudapi.VMBrand2Unknown.Valid() }, func() bool { return cloudapi.VMBrand2("__invalid__").Valid() }},
		{"VolumeAction0", func() bool { return cloudapi.VolumeAction0Update.Valid() }, func() bool { return cloudapi.VolumeAction0("__invalid__").Valid() }},
		{"VolumeAction1", func() bool { return cloudapi.VolumeAction1Unknown.Valid() }, func() bool { return cloudapi.VolumeAction1("__invalid__").Valid() }},
		{"VolumeState", func() bool { return cloudapi.VolumeStateCreating.Valid() }, func() bool { return cloudapi.VolumeState("__invalid__").Valid() }},
		{"VolumeType0", func() bool { return cloudapi.Tritonnfs.Valid() }, func() bool { return cloudapi.VolumeType0("__invalid__").Valid() }},
		{"VolumeType1", func() bool { return cloudapi.VolumeType1Unknown.Valid() }, func() bool { return cloudapi.VolumeType1("__invalid__").Valid() }},
	}

	for _, tt := range tests {
		t.Run(tt.name+"/valid", func(t *testing.T) {
			if !tt.valid() {
				t.Errorf("%s: expected known constant to be valid", tt.name)
			}
		})
		t.Run(tt.name+"/invalid", func(t *testing.T) {
			if tt.invalid() {
				t.Errorf("%s: expected bogus value to be invalid", tt.name)
			}
		})
	}
}
