//
// Copyright (c) 2018, Joyent, Inc. All rights reserved.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package authentication

import (
	"fmt"
	"strings"
)

type httpAuthSignature interface {
	SignatureType() string
	String() string
}

func keyFormatToKeyType(keyFormat string) (string, error) {
	switch keyFormat {
	case "ssh-rsa", "rsa-sha2-256", "rsa-sha2-512":
		return "rsa", nil
	case "ssh-ed25519":
		return "ed25519", nil
	default:
		if strings.HasPrefix(keyFormat, "ecdsa-sha2-") {
			return "ecdsa", nil
		}
		return "", fmt.Errorf("unknown key format: %s", keyFormat)
	}
}
