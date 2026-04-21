//
// Copyright (c) 2018, Joyent, Inc. All rights reserved.
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package authentication

import (
	"encoding/asn1"
	"encoding/base64"
	"fmt"
	"math/big"

	"golang.org/x/crypto/ssh"
)

type ecdsaSignature struct {
	signatureType string
	encoded       string
}

func (s *ecdsaSignature) SignatureType() string {
	return s.signatureType
}

func (s *ecdsaSignature) String() string {
	return s.encoded
}

func newECDSASignature(signatureBlob []byte) (*ecdsaSignature, error) {
	var ecSig struct {
		R *big.Int
		S *big.Int
	}

	if err := ssh.Unmarshal(signatureBlob, &ecSig); err != nil {
		return nil, fmt.Errorf("unable to unmarshal ecdsa signature: %w", err)
	}

	rValue := ecSig.R.Bytes()
	var hashAlgorithm string
	switch len(rValue) {
	case 31, 32:
		hashAlgorithm = "sha256"
	case 65, 66:
		hashAlgorithm = "sha512"
	default:
		return nil, fmt.Errorf("unsupported ecdsa key length: %d", len(rValue))
	}

	toEncode := struct {
		R *big.Int
		S *big.Int
	}{R: ecSig.R, S: ecSig.S}

	signatureBytes, err := asn1.Marshal(toEncode)
	if err != nil {
		return nil, fmt.Errorf("unable to marshal ecdsa signature: %w", err)
	}

	return &ecdsaSignature{
		signatureType: fmt.Sprintf("ecdsa-%s", hashAlgorithm),
		encoded:       base64.StdEncoding.EncodeToString(signatureBytes),
	}, nil
}
