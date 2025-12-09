<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Bugview API

API trait definition for Bugview, a public read-only interface to JIRA issues.

## Overview

Bugview exposes JIRA issues that have been explicitly marked as public through labels. It provides both JSON and HTML endpoints for viewing issue lists and details.

## Endpoints

### JSON Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/bugview/index.json` | Paginated list of public issues |
| GET | `/bugview/json/{key}` | Simplified issue details |
| GET | `/bugview/fulljson/{key}` | Full issue details including all fields |

### HTML Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/bugview/index.html` | HTML index of public issues |
| GET | `/bugview/label/{key}` | HTML index filtered by label |
| GET | `/bugview/issue/{key}` | HTML view of a single issue |

### Redirects

| Method | Path | Description |
|--------|------|-------------|
| GET | `/bugview` | Redirects to `/bugview/index.html` |

## Pagination

List endpoints support token-based pagination via query parameters:

- `next_page_token` - Token from previous response to fetch next page
- `sort` - Sort field (`key`, `created`, or `updated`)

## Related Crates

- `bugview-service` - Implementation of this API trait
- `bugview-client` - Generated client library (via Progenitor)
- `bugview-cli` - Command-line interface
