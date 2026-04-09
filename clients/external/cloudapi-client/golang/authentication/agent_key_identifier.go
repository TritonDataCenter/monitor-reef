//
// Copyright (c) 2018, Joyent, Inc. All rights reserved.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package authentication

import "path"

type KeyID struct {
	UserName    string
	AccountName string
	Fingerprint string
}

func (input *KeyID) generate() string {
	if input.UserName != "" {
		return path.Join("/", input.AccountName, "users", input.UserName, "keys", input.Fingerprint)
	}

	return path.Join("/", input.AccountName, "keys", input.Fingerprint)
}
