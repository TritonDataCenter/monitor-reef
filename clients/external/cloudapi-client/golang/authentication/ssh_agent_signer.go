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
	"crypto/md5"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"net"
	"os"
	"strings"

	"errors"

	"golang.org/x/crypto/ssh"
	"golang.org/x/crypto/ssh/agent"
)

var (
	ErrUnsetEnvVar = errors.New("environment variable SSH_AUTH_SOCK not set")
)

type SSHAgentSigner struct {
	formattedKeyFingerprint string
	keyFingerprint          string
	algorithm               string
	accountName             string
	userName                string

	conn net.Conn
	agent agent.Agent
	key   ssh.PublicKey
}

type SSHAgentSignerInput struct {
	KeyID       string
	AccountName string
	Username    string
}

func NewSSHAgentSigner(input SSHAgentSignerInput) (*SSHAgentSigner, error) {
	sshAgentAddress, agentOk := os.LookupEnv("SSH_AUTH_SOCK")
	if !agentOk {
		return nil, ErrUnsetEnvVar
	}

	conn, err := net.Dial("unix", sshAgentAddress)
	if err != nil {
		return nil, fmt.Errorf("unable to dial SSH agent: %w", err)
	}

	// Close the connection if construction fails at any subsequent step.
	// The caller never receives the SSHAgentSigner on error, so they
	// cannot call Close() themselves.
	success := false
	defer func() {
		if !success {
			conn.Close()
		}
	}()

	ag := agent.NewClient(conn)

	signer := &SSHAgentSigner{
		keyFingerprint: input.KeyID,
		accountName:    input.AccountName,
		conn:           conn,
		agent:          ag,
	}

	if input.Username != "" {
		signer.userName = input.Username
	}

	matchingKey, err := signer.MatchKey()
	if err != nil {
		return nil, err
	}
	signer.key = matchingKey
	signer.formattedKeyFingerprint, err = formatPublicKeyFingerprint(signer.key, true)
	if err != nil {
		return nil, fmt.Errorf("unable to format match public key: %w", err)
	}

	_, algorithm, err := signer.SignRaw("HelloWorld")
	if err != nil {
		return nil, fmt.Errorf("cannot sign using ssh agent: %w", err)
	}
	signer.algorithm = algorithm

	success = true
	return signer, nil
}

func (s *SSHAgentSigner) MatchKey() (ssh.PublicKey, error) {
	keys, err := s.agent.List()
	if err != nil {
		return nil, fmt.Errorf("unable to list keys in SSH Agent: %w", err)
	}

	keyFingerprintStripped := strings.TrimPrefix(s.keyFingerprint, "MD5:")
	keyFingerprintStripped = strings.TrimPrefix(keyFingerprintStripped, "SHA256:")
	keyFingerprintStripped = strings.Replace(keyFingerprintStripped, ":", "", -1)

	var matchingKey ssh.PublicKey
	for _, key := range keys {
		keyMD5 := md5.New()
		keyMD5.Write(key.Marshal())
		finalizedMD5 := fmt.Sprintf("%x", keyMD5.Sum(nil))

		keySHA256 := sha256.New()
		keySHA256.Write(key.Marshal())
		finalizedSHA256 := base64.RawStdEncoding.EncodeToString(keySHA256.Sum(nil))

		if keyFingerprintStripped == finalizedMD5 || keyFingerprintStripped == finalizedSHA256 {
			matchingKey = key
			break
		}
	}

	if matchingKey == nil {
		return nil, fmt.Errorf("no key in the SSH agent matches fingerprint: %s", s.keyFingerprint)
	}

	return matchingKey, nil
}

func (s *SSHAgentSigner) Sign(dateHeader string) (string, error) {
	const headerName = "date"

	message := fmt.Sprintf("%s: %s", headerName, dateHeader)
	signedBase64, algoName, err := s.SignRaw(message)
	if err != nil {
		return "", err
	}

	key := &KeyID{
		UserName:    s.userName,
		AccountName: s.accountName,
		Fingerprint: s.formattedKeyFingerprint,
	}

	return fmt.Sprintf(authorizationHeaderFormat, key.generate(), algoName, headerName, signedBase64), nil
}

func (s *SSHAgentSigner) SignRaw(toSign string) (string, string, error) {
	signature, err := s.agent.Sign(s.key, []byte(toSign))
	if err != nil {
		return "", "", fmt.Errorf("unable to sign string: %w", err)
	}

	authSig, err := parseAgentSignature(signature.Format, signature.Blob)
	if err != nil {
		return "", "", err
	}

	return authSig.String(), authSig.SignatureType(), nil
}

// parseAgentSignature maps an SSH agent signature to an httpAuthSignature.
func parseAgentSignature(format string, blob []byte) (httpAuthSignature, error) {
	keyFormat, err := keyFormatToKeyType(format)
	if err != nil {
		return nil, fmt.Errorf("unable to format key: %w", err)
	}

	switch keyFormat {
	case "rsa":
		return newRSASignature(blob, rsaFormatToAlgorithm(format))
	case "ecdsa":
		return newECDSASignature(blob)
	case "ed25519":
		return newEd25519Signature(blob)
	default:
		return nil, fmt.Errorf("unsupported algorithm from SSH agent: %s", format)
	}
}

// rsaFormatToAlgorithm maps SSH agent RSA signature formats to HTTP Signature
// algorithm names.
func rsaFormatToAlgorithm(format string) string {
	switch format {
	case "rsa-sha2-256":
		return "rsa-sha256"
	case "rsa-sha2-512":
		return "rsa-sha512"
	default:
		return "rsa-sha1"
	}
}

func (s *SSHAgentSigner) KeyFingerprint() string {
	return s.formattedKeyFingerprint
}

func (s *SSHAgentSigner) DefaultAlgorithm() string {
	return s.algorithm
}

// Close closes the underlying SSH agent connection.
func (s *SSHAgentSigner) Close() error {
	return s.conn.Close()
}
