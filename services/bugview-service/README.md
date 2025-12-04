<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Bugview Service

A public JIRA issue viewer that provides read-only access to issues marked with specific labels. Built with Rust using Dropshot and trait-based API design.

## Features

- **JSON API** - Programmatic access to issue data
- **HTML UI** - Bootstrap-styled web interface with pagination
- **Label filtering** - Browse issues by public label
- **JIRA markup rendering** - Full support via ADF (Atlassian Document Format) to HTML conversion
- **Security** - Label-based access control to show only public issues

## Configuration

The service is configured entirely through environment variables:

### Required Variables

```bash
# JIRA instance URL
JIRA_URL="https://your-jira-instance.atlassian.net"

# JIRA authentication (use API token, not password)
JIRA_USERNAME="your-username"
JIRA_PASSWORD="your-api-token"

# Label that marks issues as "public"
JIRA_DEFAULT_LABEL="public"
```

### Optional Variables

```bash
# Additional labels users can filter by (comma-separated)
JIRA_ALLOWED_LABELS="smartos,triton,illumos"

# Allowed domains for remote links (comma-separated)
# Filters external links to prevent exposing sensitive URLs (e.g., signed Manta URLs)
JIRA_ALLOWED_DOMAINS="cr.joyent.us,github.com,illumos.org"

# Server bind address (default: 127.0.0.1:8080)
BIND_ADDRESS="0.0.0.0:3000"

# Logging level (default: bugview_service=info,dropshot=info)
RUST_LOG="bugview_service=debug,dropshot=info"
```

## Running

### Development

```bash
# Set environment variables
export JIRA_URL="https://smartos.atlassian.net"
export JIRA_USERNAME="bugview-bot"
export JIRA_PASSWORD="your-api-token-here"
export JIRA_DEFAULT_LABEL="public"
export JIRA_ALLOWED_LABELS="smartos,triton,illumos"
export JIRA_ALLOWED_DOMAINS="cr.joyent.us,github.com,illumos.org"

# Run from workspace root
cargo run -p bugview-service
```

### Using a .env file

```bash
# Create .env file (don't commit it!)
cat > .env <<EOF
JIRA_URL=https://smartos.atlassian.net
JIRA_USERNAME=bugview-bot
JIRA_PASSWORD=your-api-token-here
JIRA_DEFAULT_LABEL=public
JIRA_ALLOWED_LABELS=smartos,triton,illumos
JIRA_ALLOWED_DOMAINS=cr.joyent.us,github.com,illumos.org
BIND_ADDRESS=0.0.0.0:8080
EOF

# Run with environment variables loaded
set -a; source .env; set +a
cargo run -p bugview-service
```

### Production

```bash
# Build release binary
cargo build --release -p bugview-service

# Run with environment variables
./target/release/bugview-service
```

## API Endpoints

### HTML Endpoints

- `GET /bugview` - Redirects (302) to `/bugview/index.html`
- `GET /bugview/` - Redirects (302) to `/bugview/index.html`
- `GET /bugview/index.html` - Paginated list of all public issues
  - Query params: `next_page_token` (for pagination), `sort` (default: updated)
  - Example: `/bugview/index.html?sort=created`
  - Pagination uses token-based "First Page" and "Next Page" links (not numeric pages)

- `GET /bugview/label/{key}` - Issues filtered by label
  - Example: `/bugview/label/smartos`
  - Query params: same as index

- `GET /bugview/issue/{key}` - Individual issue view
  - Example: `/bugview/issue/OS-1234`

### JSON Endpoints

- `GET /bugview/index.json` - Issue list (JSON)
  - Returns: `{ issues, next_page_token, is_last }`
  - Use `next_page_token` from response for pagination
- `GET /bugview/json/{key}` - Simple issue data (JSON)
- `GET /bugview/fulljson/{key}` - Complete issue data (JSON)

## Pagination

**Important**: Due to JIRA Cloud API v3 changes, pagination uses **tokens** instead of offsets:
- The first page requires no token parameter
- Each response includes a `next_page_token` for the next page
- You cannot jump to arbitrary pages (no page numbers)
- Navigation is sequential: First Page → Next Page → Next Page...
- This is a limitation of the JIRA Cloud REST API v3

**Security Note**: JIRA's pagination tokens contain the JQL query being executed. To prevent exposing query details in URLs, browser history, and logs, this service implements **server-side token mapping**:
- JIRA returns tokens like `Ck11cGRhdGVkJn...` (contains base64-encoded query)
- We store these in an in-memory cache (1-hour TTL)
- URLs contain short random IDs like `?next_page_token=a7F3mK9pQ2wX`
- **HTML endpoints**: Expired/invalid tokens gracefully fall back to the first page
- **JSON endpoints**: Expired/invalid tokens return a 400 error (proper API behavior)

## Usage Examples

```bash
# View HTML in browser
open http://localhost:8080/bugview/index.html

# Get JSON data with curl
curl http://localhost:8080/bugview/index.json | jq

# Filter by label
curl http://localhost:8080/bugview/label/smartos/index.json | jq

# Get specific issue
curl http://localhost:8080/bugview/json/OS-1234 | jq

# Get full issue details
curl http://localhost:8080/bugview/fulljson/OS-1234 | jq '.fields'
```

## Security

The service implements multiple security measures:

1. **Label-based access control**: Only issues with `JIRA_DEFAULT_LABEL` are visible
2. **Label filtering**: Users can filter by labels in `JIRA_ALLOWED_LABELS`
3. **Domain whitelisting**: Remote links are filtered by `JIRA_ALLOWED_DOMAINS` to prevent exposing sensitive URLs (e.g., signed Manta URLs)
4. **404 on unauthorized access**: Attempting to view an issue without the required label returns 404
5. **Public read-only**: No authentication is required (public read-only access)

## JIRA API Token

To create a JIRA API token:

1. Log into your JIRA instance
2. Go to Profile → Manage Account → Security
3. Click "Create and manage API tokens"
4. Create a new token and save it securely
5. Use it as `JIRA_PASSWORD` (not your actual password)

## Development

### API Definition

The API is defined in `../../apis/bugview-api/` as a Dropshot trait.

### Templates

HTML templates are in `templates/`:
- `primary.html` - Main page layout with Bootstrap
- `issue_index.html` - Issue list table

### JIRA Client

The minimal JIRA client in `src/jira_client.rs` implements only the endpoints needed:
- Search issues with JQL
- Get issue by key
- Get remote links

### Testing

```bash
# Build and run tests
cargo test -p bugview-service

# Check the service compiles
cargo check -p bugview-service

# Build optimized binary
cargo build --release -p bugview-service
```

## Migration from Node.js

This Rust implementation provides feature parity with the original Node.js bugview:

✅ Same JSON API structure
✅ Same HTML UI and styling
✅ Same label-based access control
✅ JIRA markup rendering (via ADF to HTML conversion)
✅ Pagination and sorting (now token-based for v3 API)

**URL Difference**: The HTML issue view endpoint is `/bugview/issue/{key}` instead of `/bugview/{key}`. This is required because Dropshot (unlike restify) doesn't allow mixing literal path segments (`/bugview/json/{key}`, `/bugview/label/{key}`) with variable segments (`/bugview/{key}`) at the same route level.

### Backward Compatibility with nginx

To maintain compatibility with existing `/bugview/{key}` URLs (e.g., from GitHub issues, documentation), add this nginx rewrite rule:

```nginx
location ^~ /bugview {
    # Rewrite legacy /bugview/{issue-key} URLs to /bugview/issue/{issue-key}
    # Issue keys are typically PROJECT-NUMBER (e.g., OS-1234, TRITON-2520)
    if ($request_uri ~ ^/bugview/([A-Z][A-Z0-9]+-[0-9]+)$) {
        rewrite ^/bugview/(.+)$ /bugview/issue/$1 break;
    }

    proxy_pass http://localhost:8080;
}
```

This transparently rewrites URLs like `/bugview/OS-1234` to `/bugview/issue/OS-1234` before they reach the Rust service, while leaving all other endpoints (`/bugview/json/*`, `/bugview/fulljson/*`, `/bugview/label/*`, etc.) untouched. The `break` flag ensures the rewritten URI is passed directly to the backend service.

## Troubleshooting

**Issue: "Failed to get issue"**
- Check JIRA_URL is correct
- Verify API token is valid
- Ensure issue exists and has the required label

**Issue: "Label 'xyz' is not public"**
- Add the label to `JIRA_ALLOWED_LABELS`
- Or use `JIRA_DEFAULT_LABEL` for general browsing

**Issue: Templates not found**
- Templates must be in `services/bugview-service/templates/`
- They're loaded at compile time using `env!("CARGO_MANIFEST_DIR")`

**Issue: HTML renders but no markup formatting**
- Check the issue description is in ADF (Atlassian Document Format) with a `content` field
- If the issue description is missing or not in ADF format, the service will not display markup
- Ensure JIRA issues have proper descriptions set
