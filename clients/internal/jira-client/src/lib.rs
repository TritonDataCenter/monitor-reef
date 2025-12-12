// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

// Copyright 2025 Edgecast Cloud LLC.

//! JIRA API Client
//!
//! This is a Progenitor-generated client for the JIRA API subset used by bugview-service.
//! The client is generated from the OpenAPI spec defined in apis/jira-api.
//!
//! **IMPORTANT**: This client represents a *subset* of JIRA's API, not the complete API.
//! It only includes the specific endpoints needed by bugview-service:
//! - Search issues using JQL
//! - Get issue details
//! - Get remote links for an issue
//!
//! The generated client provides a type-safe, async interface to these endpoints.

// Re-export IssueKey from jira-api for type-safe usage
pub use jira_api::IssueKey;

// Include the Progenitor-generated client code
include!(concat!(env!("OUT_DIR"), "/client.rs"));

// Add a conversion from generated Issue (with String key) to jira_api::Issue (with IssueKey)
impl From<types::Issue> for jira_api::Issue {
    fn from(issue: types::Issue) -> Self {
        jira_api::Issue {
            key: IssueKey::new_unchecked(issue.key),
            id: issue.id,
            fields: issue.fields.into_iter().collect(),
            rendered_fields: issue.rendered_fields.map(|m| m.into_iter().collect()),
        }
    }
}

// Add a conversion from generated RemoteLink to jira_api::RemoteLink
impl From<types::RemoteLink> for jira_api::RemoteLink {
    fn from(link: types::RemoteLink) -> Self {
        jira_api::RemoteLink {
            id: link.id,
            object: link.object.map(|obj| jira_api::RemoteLinkObject {
                url: obj.url,
                title: obj.title,
            }),
        }
    }
}
