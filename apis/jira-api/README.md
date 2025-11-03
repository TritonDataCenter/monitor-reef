# JIRA API Trait (Subset)

**IMPORTANT**: This is a *subset* of the JIRA REST API v3, not a complete JIRA API definition.

## Purpose

This trait defines only the specific JIRA endpoints used by bugview-service:
- Search issues using JQL
- Get issue details
- Get remote links for issues

The actual JIRA API is implemented by Atlassian's JIRA servers. We define this trait to:
1. Document the exact JIRA API surface we depend on
2. Generate an OpenAPI specification for type-safe client generation
3. Enable mock implementations for testing
4. Serve as a real-world example of defining external API subsets

## Key Design Decisions

### Path Parameter Naming
The `get_remote_links` endpoint uses a path parameter named `key` (for Dropshot consistency) but expects a numeric issue ID, not an issue key like "PROJECT-123". This is documented in both the type definition and endpoint documentation.

### Dynamic Field Types
Issue fields are represented as `HashMap<String, serde_json::Value>` because JIRA's field structure varies by configuration. Clients must handle field extraction dynamically.

## Usage

This API is registered in `openapi-manager` and generates an OpenAPI spec used by Progenitor to create the `jira-client` library.

## Reference

- [JIRA REST API v3 Documentation](https://developer.atlassian.com/cloud/jira/platform/rest/v3/)
- [Dropshot API Traits (RFD 479)](https://rfd.shared.oxide.computer/rfd/0479)
