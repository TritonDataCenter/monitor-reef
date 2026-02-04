// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! CloudAPI Changefeed types for real-time VM state notifications.
//!
//! The changefeed WebSocket endpoint (`/{account}/changefeed`) provides real-time
//! notifications of VM state transitions. Clients connect via WebSocket, send a
//! subscription message, and receive change notifications as they occur.
//!
//! # Protocol
//!
//! 1. Client connects to `wss://{cloudapi}/{account}/changefeed`
//! 2. Client sends a [`ChangefeedSubscription`] message to register interest
//! 3. Server sends [`ChangefeedMessage`] notifications for matching changes
//!
//! # Example
//!
//! ```ignore
//! // Subscribe to state changes for specific VMs
//! let subscription = ChangefeedSubscription {
//!     resource: ChangefeedResource::Vm,
//!     sub_resources: vec![ChangefeedSubResource::State],
//!     vms: Some(vec![machine_uuid]),
//! };
//!
//! ws.send(serde_json::to_string(&subscription)?).await?;
//!
//! // Receive change notifications
//! while let Some(msg) = ws.next().await {
//!     let change: ChangefeedMessage = serde_json::from_str(&msg)?;
//!     println!("VM {} is now {}", change.changed_resource_id, change.resource_state);
//! }
//! ```
//!
//! # References
//!
//! - CloudAPI docs: `GET /{account}/changefeed`
//! - Source: `sdc-cloudapi/lib/changefeed.js`

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Machine;

/// Resource types supported by the changefeed.
///
/// Currently only VM resources are supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChangefeedResource {
    /// Virtual machine resources
    Vm,
}

/// Sub-resources that can be monitored for changes.
///
/// These correspond to specific properties of a VM that can change.
/// Based on `VM_SUB_RESOURCES` in `sdc-cloudapi/lib/changefeed.js`.
///
/// Note: VMAPI may send additional sub-resources not in CloudAPI's subscription list.
/// Unknown variants are captured by the `Other` variant to prevent deserialization failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangefeedSubResource {
    /// VM alias/name changed
    Alias,
    /// Autoboot setting changed (from VMAPI, not in CloudAPI subscription list)
    Autoboot,
    /// Customer metadata changed
    CustomerMetadata,
    /// VM was destroyed
    Destroyed,
    /// Last modified timestamp changed
    LastModified,
    /// Network interfaces changed
    Nics,
    /// Owner UUID changed
    OwnerUuid,
    /// Server UUID changed (migration)
    ServerUuid,
    /// VM state changed (running, stopped, etc.)
    State,
    /// Tags changed
    Tags,
    /// Zone state changed (from VMAPI, internal state representation)
    ZoneState,
    /// Unknown sub-resource (catch-all for forward compatibility)
    #[serde(other)]
    Other,
}

/// Subscription message sent by clients to register for change notifications.
///
/// Clients send this message after connecting to the changefeed WebSocket
/// to specify which resources and changes they want to monitor.
///
/// # Example
///
/// ```
/// use cloudapi_api::types::{ChangefeedSubscription, ChangefeedResource, ChangefeedSubResource};
///
/// // Subscribe to state changes for all VMs
/// let subscription = ChangefeedSubscription {
///     resource: ChangefeedResource::Vm,
///     sub_resources: vec![ChangefeedSubResource::State],
///     vms: None,
/// };
///
/// // Subscribe to state and tag changes for specific VMs
/// let filtered = ChangefeedSubscription {
///     resource: ChangefeedResource::Vm,
///     sub_resources: vec![ChangefeedSubResource::State, ChangefeedSubResource::Tags],
///     vms: Some(vec!["uuid-1".to_string(), "uuid-2".to_string()]),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangefeedSubscription {
    /// Resource type to monitor (currently only "vm" is supported)
    pub resource: ChangefeedResource,

    /// Sub-resources to monitor for changes
    pub sub_resources: Vec<ChangefeedSubResource>,

    /// Optional list of VM UUIDs to filter notifications.
    /// If not provided or empty, notifications for all VMs are received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vms: Option<Vec<String>>,
}

/// Describes what kind of change occurred.
///
/// Included in each [`ChangefeedMessage`] to indicate what properties changed.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangefeedChangeKind {
    /// The resource type that changed
    pub resource: ChangefeedResource,

    /// Which sub-resources/properties changed
    pub sub_resources: Vec<ChangefeedSubResource>,
}

/// Change notification message received from the changefeed.
///
/// The server sends these messages when a subscribed resource changes.
/// The `resource_object` field contains the full current state of the resource.
///
/// # Fields
///
/// Per CloudAPI documentation:
/// - `changedResourceId`: UUID of the modified resource (machine UUID)
/// - `published`: Timestamp when the change was published (as a string)
/// - `resourceState`: Internal machine state from VMAPI
/// - `changeKind`: Object with `resource` and `subResources` describing the change
/// - `resourceObject`: Complete machine object with current state
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangefeedMessage {
    /// UUID of the changed resource (the machine UUID)
    pub changed_resource_id: String,

    /// Timestamp when the change was published.
    ///
    /// This is a Unix timestamp in milliseconds, serialized as a string.
    /// CloudAPI documentation specifies this as `String(timestamp)`.
    pub published: String,

    /// The current state of the resource after the change.
    ///
    /// This is the internal VMAPI state, which may differ from CloudAPI's
    /// translated states. Common values: "running", "stopped", "failed",
    /// "provisioning", "stopping", "shutting_down".
    pub resource_state: String,

    /// Describes what kind of change occurred
    pub change_kind: ChangefeedChangeKind,

    /// The complete resource object with current state.
    ///
    /// For VM resources, this is the full Machine object.
    /// May be absent in some edge cases.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_object: Option<Machine>,
}

impl ChangefeedMessage {
    /// Get the resource state as a typed `VmState` enum.
    ///
    /// Returns `None` if the state string cannot be parsed as a known `VmState`.
    /// This can happen for unknown or future states not yet in our enum.
    pub fn resource_state_typed(&self) -> Option<vmapi_api::VmState> {
        self.resource_state.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_serialization() {
        let sub = ChangefeedSubscription {
            resource: ChangefeedResource::Vm,
            sub_resources: vec![ChangefeedSubResource::State],
            vms: Some(vec!["test-uuid".to_string()]),
        };

        let json = serde_json::to_string(&sub).expect("serialize");
        assert!(json.contains("\"resource\":\"vm\""));
        assert!(json.contains("\"subResources\":[\"state\"]"));
        assert!(json.contains("\"vms\":[\"test-uuid\"]"));
    }

    #[test]
    fn test_subscription_without_vms() {
        let sub = ChangefeedSubscription {
            resource: ChangefeedResource::Vm,
            sub_resources: vec![ChangefeedSubResource::State, ChangefeedSubResource::Tags],
            vms: None,
        };

        let json = serde_json::to_string(&sub).expect("serialize");
        assert!(!json.contains("\"vms\""));
    }

    #[test]
    fn test_changefeed_message_deserialization() {
        // Minimal message without resourceObject
        let json = r#"{
            "published": "1706367000000",
            "changeKind": {
                "resource": "vm",
                "subResources": ["state"]
            },
            "resourceState": "running",
            "changedResourceId": "28faa36c-2031-4632-a819-f7defa1299a3"
        }"#;

        let msg: ChangefeedMessage = serde_json::from_str(json).expect("deserialize");
        assert_eq!(msg.published, "1706367000000");
        assert_eq!(msg.change_kind.resource, ChangefeedResource::Vm);
        assert_eq!(
            msg.change_kind.sub_resources,
            vec![ChangefeedSubResource::State]
        );
        assert_eq!(msg.resource_state, "running");
        assert_eq!(
            msg.changed_resource_id,
            "28faa36c-2031-4632-a819-f7defa1299a3"
        );
        assert!(msg.resource_object.is_none());
    }

    #[test]
    fn test_change_kind_multiple_subresources() {
        let json = r#"{
            "resource": "vm",
            "subResources": ["last_modified", "state"]
        }"#;

        let kind: ChangefeedChangeKind = serde_json::from_str(json).expect("deserialize");
        assert_eq!(kind.resource, ChangefeedResource::Vm);
        assert_eq!(kind.sub_resources.len(), 2);
    }
}
