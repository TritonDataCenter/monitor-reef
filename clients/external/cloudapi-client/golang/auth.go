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
	"crypto/tls"
	"encoding/pem"
	"errors"
	"fmt"
	"net/http"
	"os"
	"time"

	auth "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang/authentication"
)

var (
	// ErrMissingAccountName indicates no CloudAPI account name was provided via
	// options or the TRITON_/SDC_ environment.
	ErrMissingAccountName = errors.New("cloudapi: missing account name; set TRITON_ACCOUNT or SDC_ACCOUNT")

	// ErrMissingKeyID indicates no SSH key fingerprint was provided via options
	// or the TRITON_/SDC_ environment.
	ErrMissingKeyID = errors.New("cloudapi: missing key id; set TRITON_KEY_ID or SDC_KEY_ID")
)

var authEnvPrefixes = []string{"TRITON", "SDC"}

// SignatureAuthOptions configures Date/Authorization header injection for
// CloudAPI requests.
type SignatureAuthOptions struct {
	Signer        auth.Signer
	AcceptVersion string
	UserAgent     string
	ActAs         string
	Now           func() time.Time
}

// SignerFromEnvOptions allows callers to override env-derived auth settings.
// Empty fields fall back to TRITON_* and then SDC_* environment variables.
type SignerFromEnvOptions struct {
	AccountName string
	Username    string
	KeyID       string
	KeyMaterial string
}

// WithTLSInsecure returns a ClientOption that disables TLS certificate
// verification. This is intended for development and test environments with
// self-signed certificates and should not be used in production.
func WithTLSInsecure() ClientOption {
	return WithHTTPClient(&http.Client{
		Transport: &http.Transport{
			TLSClientConfig: &tls.Config{
				InsecureSkipVerify: true, //nolint:gosec // intentional for dev/test
			},
		},
	})
}

// TLSInsecureFromEnv returns WithTLSInsecure if TRITON_TLS_INSECURE is set to
// a truthy value ("1", "true", "yes"). Otherwise it returns a no-op option.
// The result is always safe to pass to NewClient without nil-checking.
func TLSInsecureFromEnv() ClientOption {
	val := os.Getenv("TRITON_TLS_INSECURE")
	switch val {
	case "1", "true", "yes":
		return WithTLSInsecure()
	default:
		return func(_ *Client) error { return nil }
	}
}

// WithSignatureAuth adds CloudAPI signature authentication to generated
// requests using the provided signer.
func WithSignatureAuth(signer auth.Signer) ClientOption {
	return WithSignatureAuthOptions(SignatureAuthOptions{Signer: signer})
}

// WithSignatureAuthOptions adds CloudAPI Date and Authorization headers to
// generated requests. The wrapped signer is called with the finalized Date
// header value. Callers should avoid mutating Date or Authorization in per-call
// request editors after this option runs.
func WithSignatureAuthOptions(opts SignatureAuthOptions) ClientOption {
	if opts.Now == nil {
		opts.Now = time.Now
	}

	return func(c *Client) error {
		if opts.Signer == nil {
			return fmt.Errorf("cloudapi: nil signer")
		}

		c.RequestEditors = append(c.RequestEditors, func(_ context.Context, req *http.Request) error {

			date := opts.Now().UTC().Format(http.TimeFormat)
			req.Header.Set("Date", date)

			authHeader, err := opts.Signer.Sign(date)
			if err != nil {
				return fmt.Errorf("cloudapi: sign request: %w", err)
			}
			req.Header.Set("Authorization", authHeader)

			if opts.AcceptVersion != "" && req.Header.Get("Accept-Version") == "" {
				req.Header.Set("Accept-Version", opts.AcceptVersion)
			}
			if opts.UserAgent != "" && req.Header.Get("User-Agent") == "" {
				req.Header.Set("User-Agent", opts.UserAgent)
			}
			if opts.ActAs != "" && req.Header.Get("X-Act-As") == "" {
				req.Header.Set("X-Act-As", opts.ActAs)
			}

			return nil
		})

		return nil
	}
}

// NewAuthenticatedClient creates a generated CloudAPI client with signature
// authentication enabled.
func NewAuthenticatedClient(server string, authOpts SignatureAuthOptions, opts ...ClientOption) (*Client, error) {
	opts = append(opts, WithSignatureAuthOptions(authOpts))
	return NewClient(server, opts...)
}

// NewAuthenticatedClientWithResponses creates a generated CloudAPI client with
// typed response helpers and signature authentication enabled.
func NewAuthenticatedClientWithResponses(server string, authOpts SignatureAuthOptions, opts ...ClientOption) (*ClientWithResponses, error) {
	opts = append(opts, WithSignatureAuthOptions(authOpts))
	return NewClientWithResponses(server, opts...)
}

// LoadSignerFromEnv builds a signer from TRITON_* and SDC_* environment
// variables. Precedence is: explicit options > TRITON_* > SDC_*.
func LoadSignerFromEnv() (auth.Signer, error) {
	return LoadSignerFromEnvWithOptions(SignerFromEnvOptions{})
}

// LoadSignerFromEnvWithOptions builds a signer from explicit options and the
// TRITON_/SDC_ environment. If KeyMaterial is empty, SSH agent auth is used.
// Otherwise KeyMaterial is treated as a file path when it exists, or as inline
// PEM contents when it does not.
func LoadSignerFromEnvWithOptions(opts SignerFromEnvOptions) (auth.Signer, error) {
	accountName := firstNonEmpty(opts.AccountName, getAuthEnv("ACCOUNT"))
	if accountName == "" {
		return nil, ErrMissingAccountName
	}

	keyID := firstNonEmpty(opts.KeyID, getAuthEnv("KEY_ID"))
	if keyID == "" {
		return nil, ErrMissingKeyID
	}

	username := firstNonEmpty(opts.Username, getAuthEnv("USER"))
	keyMaterial := firstNonEmpty(opts.KeyMaterial, getAuthEnv("KEY_MATERIAL"))

	if keyMaterial == "" {
		return auth.NewSSHAgentSigner(auth.SSHAgentSignerInput{
			KeyID:       keyID,
			AccountName: accountName,
			Username:    username,
		})
	}

	keyBytes, err := getKeyMaterialContents(keyMaterial)
	if err != nil {
		return nil, err
	}

	return auth.NewPrivateKeySigner(auth.PrivateKeySignerInput{
		KeyID:              keyID,
		PrivateKeyMaterial: keyBytes,
		AccountName:        accountName,
		Username:           username,
	})
}

func getAuthEnv(name string) string {
	for _, prefix := range authEnvPrefixes {
		if val, found := os.LookupEnv(prefix + "_" + name); found && val != "" {
			return val
		}
	}

	return ""
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}

	return ""
}

func getKeyMaterialContents(keyMaterial string) ([]byte, error) {
	keyBytes, err := os.ReadFile(keyMaterial)
	if err != nil {
		if !errors.Is(err, os.ErrNotExist) {
			return nil, fmt.Errorf("cloudapi: read key material from %s: %w", keyMaterial, err)
		}
		// Not a file path — treat as inline PEM content.
		keyBytes = []byte(keyMaterial)
	}

	block, _ := pem.Decode(keyBytes)
	if block == nil {
		return nil, fmt.Errorf("cloudapi: failed to read key material: no key found")
	}

	if block.Headers["Proc-Type"] == "4,ENCRYPTED" {
		return nil, fmt.Errorf("cloudapi: password protected private keys are not supported")
	}

	return keyBytes, nil
}
