//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

package cloudapi

import (
	"context"
	"crypto"
	"crypto/ed25519"
	"crypto/md5"
	"crypto/rand"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	auth "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang/authentication"
	"golang.org/x/crypto/ssh"
)

type recordingDoer struct {
	req *http.Request
}

func (d *recordingDoer) Do(req *http.Request) (*http.Response, error) {
	d.req = req.Clone(req.Context())
	d.req.Header = req.Header.Clone()

	return &http.Response{
		StatusCode: http.StatusOK,
		Header:     make(http.Header),
		Body:       io.NopCloser(strings.NewReader(`{}`)),
	}, nil
}

func TestNewAuthenticatedClientAddsSignatureHeaders(t *testing.T) {
	_, pemBytes, fingerprint := generateEd25519Key(t)

	signer, err := auth.NewPrivateKeySigner(auth.PrivateKeySignerInput{
		KeyID:              fingerprint,
		PrivateKeyMaterial: pemBytes,
		AccountName:        "testaccount",
		Username:           "testuser",
	})
	if err != nil {
		t.Fatalf("NewPrivateKeySigner: %v", err)
	}

	doer := &recordingDoer{}
	fixedNow := time.Date(2025, time.January, 1, 0, 0, 0, 0, time.UTC)

	client, err := NewAuthenticatedClient(
		"https://cloudapi.example.com",
		SignatureAuthOptions{
			Signer:        signer,
			AcceptVersion: "~9",
			UserAgent:     "cloudapi-go-test",
			ActAs:         "operator",
			Now: func() time.Time {
				return fixedNow
			},
		},
		WithHTTPClient(doer),
	)
	if err != nil {
		t.Fatalf("NewAuthenticatedClient: %v", err)
	}

	resp, err := client.GetAccount(context.Background(), "testaccount")
	if err != nil {
		t.Fatalf("GetAccount: %v", err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Fatalf("resp.Body.Close: %v", err)
	}

	if doer.req == nil {
		t.Fatal("expected request to be recorded")
	}

	if got, want := doer.req.Header.Get("Date"), fixedNow.Format(http.TimeFormat); got != want {
		t.Fatalf("Date header = %q, want %q", got, want)
	}

	authHeader := doer.req.Header.Get("Authorization")
	if !strings.Contains(authHeader, `headers="date"`) {
		t.Fatalf("Authorization header missing headers field: %q", authHeader)
	}
	if !strings.Contains(authHeader, `algorithm="ed25519-sha512"`) {
		t.Fatalf("Authorization header missing algorithm: %q", authHeader)
	}
	if !strings.Contains(authHeader, `/testaccount/users/testuser/keys/`) {
		t.Fatalf("Authorization header missing keyId path: %q", authHeader)
	}

	if got := doer.req.Header.Get("Accept-Version"); got != "~9" {
		t.Fatalf("Accept-Version = %q, want %q", got, "~9")
	}
	if got := doer.req.Header.Get("User-Agent"); got != "cloudapi-go-test" {
		t.Fatalf("User-Agent = %q, want %q", got, "cloudapi-go-test")
	}
	if got := doer.req.Header.Get("X-Act-As"); got != "operator" {
		t.Fatalf("X-Act-As = %q, want %q", got, "operator")
	}
}

func TestLoadSignerFromEnvPrefersTritonVars(t *testing.T) {
	_, pemBytes, fingerprint := generateEd25519Key(t)

	t.Setenv("TRITON_ACCOUNT", "testaccount")
	t.Setenv("TRITON_KEY_ID", fingerprint)
	t.Setenv("TRITON_KEY_MATERIAL", string(pemBytes))

	t.Setenv("SDC_ACCOUNT", "wrongaccount")
	t.Setenv("SDC_KEY_ID", "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00")
	t.Setenv("SDC_KEY_MATERIAL", "not a key")

	signer, err := LoadSignerFromEnv()
	if err != nil {
		t.Fatalf("LoadSignerFromEnv: %v", err)
	}

	privateKeySigner, ok := signer.(*auth.PrivateKeySigner)
	if !ok {
		t.Fatalf("signer type = %T, want *authentication.PrivateKeySigner", signer)
	}

	if got := privateKeySigner.KeyFingerprint(); got != fingerprint {
		t.Fatalf("KeyFingerprint = %q, want %q", got, fingerprint)
	}
}

func TestLoadSignerFromEnvWithOptionsOverridesEnv(t *testing.T) {
	_, pemBytes, fingerprint := generateEd25519Key(t)

	t.Setenv("TRITON_ACCOUNT", "wrongaccount")
	t.Setenv("TRITON_KEY_ID", "00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00")
	t.Setenv("TRITON_KEY_MATERIAL", "not a key")

	signer, err := LoadSignerFromEnvWithOptions(SignerFromEnvOptions{
		AccountName: "testaccount",
		KeyID:       fingerprint,
		KeyMaterial: string(pemBytes),
	})
	if err != nil {
		t.Fatalf("LoadSignerFromEnvWithOptions: %v", err)
	}

	if _, ok := signer.(*auth.PrivateKeySigner); !ok {
		t.Fatalf("signer type = %T, want *authentication.PrivateKeySigner", signer)
	}
}

func TestLoadSignerFromEnvEd25519OpenSSH(t *testing.T) {
	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ed25519.GenerateKey: %v", err)
	}

	block, err := ssh.MarshalPrivateKey(priv, "")
	if err != nil {
		t.Fatalf("ssh.MarshalPrivateKey: %v", err)
	}
	pemBytes := pem.EncodeToMemory(block)
	fingerprint := computeFingerprint(t, priv.Public())

	// Write to a temp file — OpenSSH PEM is too long for inline key material
	// (os.ReadFile returns ENAMETOOLONG instead of ENOENT).
	tmpFile := t.TempDir() + "/testkey.pem"
	if err := os.WriteFile(tmpFile, pemBytes, 0600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	signer, err := LoadSignerFromEnvWithOptions(SignerFromEnvOptions{
		AccountName: "testaccount",
		KeyID:       fingerprint,
		KeyMaterial: tmpFile,
	})
	if err != nil {
		t.Fatalf("LoadSignerFromEnvWithOptions: %v", err)
	}

	if _, ok := signer.(*auth.PrivateKeySigner); !ok {
		t.Fatalf("signer type = %T, want *authentication.PrivateKeySigner", signer)
	}

	if signer.DefaultAlgorithm() != "ed25519-sha512" {
		t.Errorf("DefaultAlgorithm() = %q, want %q", signer.DefaultAlgorithm(), "ed25519-sha512")
	}
}

func TestLoadSignerFromEnvMissingAccount(t *testing.T) {
	t.Setenv("TRITON_ACCOUNT", "")
	t.Setenv("SDC_ACCOUNT", "")
	t.Setenv("TRITON_KEY_ID", "")
	t.Setenv("SDC_KEY_ID", "")

	_, err := LoadSignerFromEnv()
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if err != ErrMissingAccountName {
		t.Fatalf("error = %v, want %v", err, ErrMissingAccountName)
	}
}

func TestLoadSignerFromEnvMissingKeyID(t *testing.T) {
	t.Setenv("TRITON_ACCOUNT", "testaccount")
	t.Setenv("SDC_ACCOUNT", "")
	t.Setenv("TRITON_KEY_ID", "")
	t.Setenv("SDC_KEY_ID", "")

	_, err := LoadSignerFromEnv()
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if err != ErrMissingKeyID {
		t.Fatalf("error = %v, want %v", err, ErrMissingKeyID)
	}
}

func TestWithSignatureAuthAddsHeaders(t *testing.T) {
	_, pemBytes, fingerprint := generateEd25519Key(t)

	signer, err := auth.NewPrivateKeySigner(auth.PrivateKeySignerInput{
		KeyID:              fingerprint,
		PrivateKeyMaterial: pemBytes,
		AccountName:        "testaccount",
	})
	if err != nil {
		t.Fatalf("NewPrivateKeySigner: %v", err)
	}

	doer := &recordingDoer{}
	client, err := NewClient(
		"https://cloudapi.example.com",
		WithSignatureAuth(signer),
		WithHTTPClient(doer),
	)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}

	resp, err := client.GetAccount(context.Background(), "testaccount")
	if err != nil {
		t.Fatalf("GetAccount: %v", err)
	}
	if err := resp.Body.Close(); err != nil {
		t.Fatalf("resp.Body.Close: %v", err)
	}

	if doer.req == nil {
		t.Fatal("expected request to be recorded")
	}
	if doer.req.Header.Get("Date") == "" {
		t.Fatal("expected Date header to be set")
	}
	if doer.req.Header.Get("Authorization") == "" {
		t.Fatal("expected Authorization header to be set")
	}
}

func TestWithSignatureAuthOptionsNilSigner(t *testing.T) {
	_, err := NewClient(
		"https://cloudapi.example.com",
		WithSignatureAuthOptions(SignatureAuthOptions{Signer: nil}),
	)
	if err == nil {
		t.Fatal("expected error for nil signer, got nil")
	}
	if !strings.Contains(err.Error(), "nil signer") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestNewAuthenticatedClientWithResponsesAddsHeaders(t *testing.T) {
	_, pemBytes, fingerprint := generateEd25519Key(t)

	signer, err := auth.NewPrivateKeySigner(auth.PrivateKeySignerInput{
		KeyID:              fingerprint,
		PrivateKeyMaterial: pemBytes,
		AccountName:        "testaccount",
	})
	if err != nil {
		t.Fatalf("NewPrivateKeySigner: %v", err)
	}

	doer := &recordingDoer{}
	client, err := NewAuthenticatedClientWithResponses(
		"https://cloudapi.example.com",
		SignatureAuthOptions{
			Signer:        signer,
			AcceptVersion: "~9",
		},
		WithHTTPClient(doer),
	)
	if err != nil {
		t.Fatalf("NewAuthenticatedClientWithResponses: %v", err)
	}

	_, err = client.GetAccountWithResponse(context.Background(), "testaccount")
	if err != nil {
		t.Fatalf("GetAccountWithResponse: %v", err)
	}

	if doer.req == nil {
		t.Fatal("expected request to be recorded")
	}
	if doer.req.Header.Get("Authorization") == "" {
		t.Fatal("expected Authorization header to be set")
	}
	if got := doer.req.Header.Get("Accept-Version"); got != "~9" {
		t.Fatalf("Accept-Version = %q, want %q", got, "~9")
	}
}

func TestLoadSignerFromEnvSSHAgentFallback(t *testing.T) {
	// When key material is empty, LoadSignerFromEnvWithOptions should try
	// the SSH agent. Without SSH_AUTH_SOCK, it returns ErrUnsetEnvVar.
	// We must fully unset the variable; LookupEnv treats "" as "set".
	prev, hadPrev := os.LookupEnv("SSH_AUTH_SOCK")
	if err := os.Unsetenv("SSH_AUTH_SOCK"); err != nil {
		t.Fatalf("os.Unsetenv: %v", err)
	}
	t.Cleanup(func() {
		if hadPrev {
			if err := os.Setenv("SSH_AUTH_SOCK", prev); err != nil {
				t.Errorf("os.Setenv: %v", err)
			}
		}
	})

	t.Setenv("TRITON_ACCOUNT", "testaccount")
	t.Setenv("TRITON_KEY_ID", "aa:bb:cc")
	t.Setenv("TRITON_KEY_MATERIAL", "")

	_, err := LoadSignerFromEnv()
	if err == nil {
		t.Fatal("expected error, got nil")
	}
	if err != auth.ErrUnsetEnvVar {
		t.Fatalf("error = %v, want %v", err, auth.ErrUnsetEnvVar)
	}
}

func TestGetKeyMaterialContentsInvalidPEM(t *testing.T) {
	t.Setenv("TRITON_ACCOUNT", "testaccount")
	t.Setenv("TRITON_KEY_ID", "aa:bb:cc")
	t.Setenv("TRITON_KEY_MATERIAL", "not-a-valid-pem")

	_, err := LoadSignerFromEnv()
	if err == nil {
		t.Fatal("expected error for invalid PEM, got nil")
	}
	if !strings.Contains(err.Error(), "no key found") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestGetKeyMaterialContentsFromFile(t *testing.T) {
	_, pemBytes, fingerprint := generateEd25519Key(t)

	tmpFile := t.TempDir() + "/testkey.pem"
	if err := os.WriteFile(tmpFile, pemBytes, 0600); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	signer, err := LoadSignerFromEnvWithOptions(SignerFromEnvOptions{
		AccountName: "testaccount",
		KeyID:       fingerprint,
		KeyMaterial: tmpFile,
	})
	if err != nil {
		t.Fatalf("LoadSignerFromEnvWithOptions: %v", err)
	}

	if _, ok := signer.(*auth.PrivateKeySigner); !ok {
		t.Fatalf("signer type = %T, want *authentication.PrivateKeySigner", signer)
	}
}

func TestGetKeyMaterialContentsEncryptedKey(t *testing.T) {
	// Simulate an encrypted PEM block.
	encryptedPEM := pem.EncodeToMemory(&pem.Block{
		Type: "RSA PRIVATE KEY",
		Headers: map[string]string{
			"Proc-Type": "4,ENCRYPTED",
		},
		Bytes: []byte("fake-encrypted-data"),
	})

	t.Setenv("TRITON_ACCOUNT", "testaccount")
	t.Setenv("TRITON_KEY_ID", "aa:bb:cc")
	t.Setenv("TRITON_KEY_MATERIAL", string(encryptedPEM))

	_, err := LoadSignerFromEnv()
	if err == nil {
		t.Fatal("expected error for encrypted key, got nil")
	}
	if !strings.Contains(err.Error(), "password protected") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func generateEd25519Key(t *testing.T) (ed25519.PrivateKey, []byte, string) {
	t.Helper()

	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("ed25519.GenerateKey: %v", err)
	}

	pkcs8, err := x509.MarshalPKCS8PrivateKey(priv)
	if err != nil {
		t.Fatalf("MarshalPKCS8PrivateKey: %v", err)
	}

	pemBytes := pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: pkcs8})
	fingerprint := computeFingerprint(t, priv.Public())

	return priv, pemBytes, fingerprint
}

func computeFingerprint(t *testing.T, pub crypto.PublicKey) string {
	t.Helper()

	sshPub, err := ssh.NewPublicKey(pub)
	if err != nil {
		t.Fatalf("ssh.NewPublicKey: %v", err)
	}

	hash := md5.New()
	hash.Write(sshPub.Marshal())
	hex := fmt.Sprintf("%x", hash.Sum(nil))

	parts := make([]string, 0, len(hex)/2)
	for i := 0; i < len(hex); i += 2 {
		parts = append(parts, hex[i:i+2])
	}

	return strings.Join(parts, ":")
}
