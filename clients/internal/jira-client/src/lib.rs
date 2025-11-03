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

// Include the Progenitor-generated client code
include!(concat!(env!("OUT_DIR"), "/client.rs"));
