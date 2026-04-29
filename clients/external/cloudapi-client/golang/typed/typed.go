//
// Copyright 2026 Edgecast Cloud LLC.
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.
//

// Package typed provides a thin, strongly-typed wrapper around the
// generated cloudapi client for the CloudAPI "action-dispatch" endpoints.
//
// The generated endpoints (UpdateMachine, UpdateImage, UpdateVolume,
// ResizeMachineDisk, CreateOrImportImage) accept an `interface{}` body
// because a single HTTP POST route dispatches multiple unrelated
// operations distinguished by an `action` parameter. This package wraps
// that surface with one method per action, taking the typed request
// struct (StartMachineRequest, ResizeMachineRequest, ExportImageRequest,
// ...) that the generator now emits from the patched OpenAPI spec.
//
// Each method:
//
//  1. Marshals the typed request into a map.
//  2. Sets the `action` field (body-first precedence, matching node-cloudapi
//     Restify behavior).
//  3. Calls the underlying generated Update* / ResizeMachineDisk /
//     CreateOrImportImage method.
//  4. Returns a descriptive error for non-2xx responses.
//
// See docs/design/action-dispatch-openapi.md for context on why the
// underlying endpoints accept a free-form body.
package typed

import (
	"context"
	"encoding/json"
	"fmt"

	openapi_types "github.com/oapi-codegen/runtime/types"

	cloudapi "github.com/TritonDataCenter/monitor-reef/clients/external/cloudapi-client/golang"
)

// Client wraps the generated cloudapi client with per-action typed methods.
type Client struct {
	inner cloudapi.ClientWithResponsesInterface
}

// New returns a typed wrapper around the given generated client.
func New(inner cloudapi.ClientWithResponsesInterface) *Client {
	return &Client{inner: inner}
}

// Inner exposes the underlying generated client for operations this
// wrapper does not cover (list, get, create, delete, etc.).
func (c *Client) Inner() cloudapi.ClientWithResponsesInterface {
	return c.inner
}

// actionBody marshals a typed request into a map and stamps the action field.
// `req` may be nil or a struct value; nil/empty produces `{"action": action}`.
func actionBody(action string, req any) (map[string]any, error) {
	m := map[string]any{}
	if req != nil {
		data, err := json.Marshal(req)
		if err != nil {
			return nil, fmt.Errorf("marshal %s request: %w", action, err)
		}
		if len(data) > 0 && string(data) != "null" {
			if err := json.Unmarshal(data, &m); err != nil {
				return nil, fmt.Errorf("unmarshal %s request into map: %w", action, err)
			}
		}
	}
	m["action"] = action
	return m, nil
}

// checkOK returns nil when status is 2xx, a formatted error otherwise.
func checkOK(status int, body []byte) error {
	if status >= 200 && status < 300 {
		return nil
	}
	return fmt.Errorf("cloudapi status %d: %s", status, string(body))
}

// -----------------------------------------------------------------------------
// Machine actions (POST /{account}/machines/{machine})
// -----------------------------------------------------------------------------

// StartMachine dispatches `action=start` on the given machine.
func (c *Client) StartMachine(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.StartMachineRequest) error {
	return c.updateMachine(ctx, account, machineID, "start", req)
}

// StopMachine dispatches `action=stop` on the given machine.
func (c *Client) StopMachine(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.StopMachineRequest) error {
	return c.updateMachine(ctx, account, machineID, "stop", req)
}

// RebootMachine dispatches `action=reboot` on the given machine.
func (c *Client) RebootMachine(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.RebootMachineRequest) error {
	return c.updateMachine(ctx, account, machineID, "reboot", req)
}

// ResizeMachine dispatches `action=resize` on the given machine. `req.Package` is required.
func (c *Client) ResizeMachine(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.ResizeMachineRequest) error {
	return c.updateMachine(ctx, account, machineID, "resize", req)
}

// RenameMachine dispatches `action=rename` on the given machine. `req.Name` is required.
func (c *Client) RenameMachine(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.RenameMachineRequest) error {
	return c.updateMachine(ctx, account, machineID, "rename", req)
}

// EnableFirewall dispatches `action=enable_firewall` on the given machine.
func (c *Client) EnableFirewall(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.EnableFirewallRequest) error {
	return c.updateMachine(ctx, account, machineID, "enable_firewall", req)
}

// DisableFirewall dispatches `action=disable_firewall` on the given machine.
func (c *Client) DisableFirewall(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.DisableFirewallRequest) error {
	return c.updateMachine(ctx, account, machineID, "disable_firewall", req)
}

// EnableDeletionProtection dispatches `action=enable_deletion_protection` on the given machine.
func (c *Client) EnableDeletionProtection(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.EnableDeletionProtectionRequest) error {
	return c.updateMachine(ctx, account, machineID, "enable_deletion_protection", req)
}

// DisableDeletionProtection dispatches `action=disable_deletion_protection` on the given machine.
func (c *Client) DisableDeletionProtection(ctx context.Context, account string, machineID openapi_types.UUID, req cloudapi.DisableDeletionProtectionRequest) error {
	return c.updateMachine(ctx, account, machineID, "disable_deletion_protection", req)
}

func (c *Client) updateMachine(ctx context.Context, account string, machineID openapi_types.UUID, action string, req any) error {
	body, err := actionBody(action, req)
	if err != nil {
		return err
	}
	resp, err := c.inner.UpdateMachineWithResponse(ctx, account, machineID, &cloudapi.UpdateMachineParams{}, body)
	if err != nil {
		return fmt.Errorf("%s machine %s: %w", action, machineID, err)
	}
	return checkOK(resp.StatusCode(), resp.Body)
}

// -----------------------------------------------------------------------------
// Image actions (POST /{account}/images/{dataset})
// -----------------------------------------------------------------------------

// UpdateImageMetadata dispatches `action=update` on the given image.
func (c *Client) UpdateImageMetadata(ctx context.Context, account string, imageID openapi_types.UUID, req cloudapi.UpdateImageRequest) error {
	return c.updateImage(ctx, account, imageID, "update", req)
}

// ExportImage dispatches `action=export` on the given image. `req.MantaPath` is required.
func (c *Client) ExportImage(ctx context.Context, account string, imageID openapi_types.UUID, req cloudapi.ExportImageRequest) error {
	return c.updateImage(ctx, account, imageID, "export", req)
}

// CloneImage dispatches `action=clone` on the given image.
func (c *Client) CloneImage(ctx context.Context, account string, imageID openapi_types.UUID) error {
	return c.updateImage(ctx, account, imageID, "clone", nil)
}

// ImportImage dispatches `action=import-from-datacenter` on the given image.
// `req.Datacenter` and `req.Id` are required.
func (c *Client) ImportImage(ctx context.Context, account string, imageID openapi_types.UUID, req cloudapi.ImportImageRequest) error {
	return c.updateImage(ctx, account, imageID, "import-from-datacenter", req)
}

func (c *Client) updateImage(ctx context.Context, account string, imageID openapi_types.UUID, action string, req any) error {
	body, err := actionBody(action, req)
	if err != nil {
		return err
	}
	resp, err := c.inner.UpdateImageWithResponse(ctx, account, imageID, &cloudapi.UpdateImageParams{}, body)
	if err != nil {
		return fmt.Errorf("%s image %s: %w", action, imageID, err)
	}
	return checkOK(resp.StatusCode(), resp.Body)
}

// -----------------------------------------------------------------------------
// Volume action (POST /{account}/volumes/{id})
// -----------------------------------------------------------------------------

// UpdateVolume dispatches `action=update` on the given volume.
func (c *Client) UpdateVolume(ctx context.Context, account string, volumeID openapi_types.UUID, req cloudapi.UpdateVolumeRequest) error {
	body, err := actionBody("update", req)
	if err != nil {
		return err
	}
	resp, err := c.inner.UpdateVolumeWithResponse(ctx, account, volumeID, &cloudapi.UpdateVolumeParams{}, body)
	if err != nil {
		return fmt.Errorf("update volume %s: %w", volumeID, err)
	}
	return checkOK(resp.StatusCode(), resp.Body)
}

// -----------------------------------------------------------------------------
// Disk action (POST /{account}/machines/{machine}/disks/{disk})
// -----------------------------------------------------------------------------

// ResizeDisk dispatches `action=resize` on the given disk. `req.Size` is required.
func (c *Client) ResizeDisk(ctx context.Context, account string, machineID openapi_types.UUID, diskID openapi_types.UUID, req cloudapi.ResizeDiskRequest) error {
	body, err := actionBody("resize", req)
	if err != nil {
		return err
	}
	resp, err := c.inner.ResizeMachineDiskWithResponse(ctx, account, machineID, diskID, &cloudapi.ResizeMachineDiskParams{}, body)
	if err != nil {
		return fmt.Errorf("resize disk %s: %w", diskID, err)
	}
	return checkOK(resp.StatusCode(), resp.Body)
}
