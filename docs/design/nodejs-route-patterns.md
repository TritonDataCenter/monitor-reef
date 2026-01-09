<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2026 Edgecast Cloud LLC.
-->

# Node.js Restify Route Patterns in Triton Services

This document summarizes the route definition patterns found across Triton's Node.js services, ordered by complexity. This serves as a reference for the Restify-to-Dropshot conversion process.

## Services by Complexity

| Rank | Service | Route Location | Variable(s) | Complexity Notes |
|------|---------|---------------|-------------|------------------|
| 1 | volapi | `lib/endpoints/*.js` | `server` | Standard, flat directory |
| 2 | vmapi | `lib/endpoints/*.js` | `server` | Standard, flat directory |
| 3 | imgapi | `lib/*.js` | `server` | No endpoints/ dir, but flat structure |
| 4 | papi | `lib/papi.js` | `server` | Single file, uses HEAD method |
| 5 | cnapi | `lib/endpoints/*.js` | `http` | Different variable name, `attachTo()` pattern |
| 6 | sapi | `lib/server/endpoints/*.js` | `sapi` | Nested under server/, custom variable name |
| 7 | cloudapi | Both `lib/endpoints/*.js` and `lib/*.js` | `server` | Hybrid - routes in multiple locations |
| 8 | napi | `lib/endpoints/**/*.js` | `http` | Nested subdirs, `register()` pattern |
| 9 | fwapi | `lib/endpoints/**/*.js` | `server` AND `http` | Most complex: nested subdirs + mixed variable names |

## Complexity Factors

1. **Flat vs nested** - Nested subdirectories require recursive searching
2. **Standard vs custom variable** - `server` is common; `http`, `sapi` require broader pattern matching
3. **Single vs multiple locations** - Some services split routes across directories
4. **Mixed variable names** - fwapi uses both `server` and `http` in different files

## Route Definition Patterns

### Pattern 1: Standard (vmapi, volapi, imgapi, cloudapi, papi)

```javascript
server.get({ path: '/vms', name: 'ListVms' }, handler);
server.post({ path: '/vms', name: 'CreateVm' }, handler);
server.put({ path: '/vms/:uuid', name: 'UpdateVm' }, handler);
server.del({ path: '/vms/:uuid', name: 'DeleteVm' }, handler);
server.head({ path: '/vms/:uuid', name: 'HeadVm' }, handler);  // papi uses HEAD
```

### Pattern 2: attachTo/register (cnapi, napi)

Routes are defined in endpoint files but attached via a function:

```javascript
// In endpoint file
function attachTo(http, before) {
    http.get({ path: '/servers', name: 'ListServers' }, before, handler);
    http.post({ path: '/servers', name: 'CreateServer' }, before, handler);
}

// Called from main server setup
endpoints.attachTo(http, before);
```

### Pattern 3: Service name as variable (sapi)

```javascript
sapi.get({ path: '/services', name: 'ListServices' }, handler);
sapi.post({ path: '/services', name: 'CreateService' }, handler);
```

## HTTP Method Mapping

All patterns use the same method names:

| Restify Method | HTTP Method |
|----------------|-------------|
| `.get()` | GET |
| `.post()` | POST |
| `.put()` | PUT |
| `.del()` | DELETE |
| `.patch()` | PATCH |
| `.head()` | HEAD |

## Directory Structure Variations

### Flat (most common)
```
lib/
└── endpoints/
    ├── vms.js
    ├── servers.js
    └── images.js
```

### No endpoints/ directory (imgapi, papi)
```
lib/
├── images.js      (contains routes)
├── channels.js    (contains routes)
└── papi.js        (single file with all routes)
```

### Nested under server/ (sapi)
```
lib/
└── server/
    └── endpoints/
        ├── services.js
        └── instances.js
```

### Nested subdirectories (fwapi, napi)
```
lib/
└── endpoints/
    ├── rules/
    │   ├── list.js
    │   └── update.js
    ├── firewalls/
    │   └── index.js
    └── ping.js
```

## Conversion Recommendations

1. **Start simple**: Begin with volapi or vmapi as they follow the cleanest patterns
2. **Progress gradually**: Work through the list to build confidence before tackling complex services
3. **Search broadly**: Always search the entire `lib/` tree recursively; don't assume any specific structure
4. **Handle all variables**: Use regex pattern `\.(get|post|put|del|head|patch)\(` to catch all variations
5. **Check for mixed patterns**: Some services (fwapi) mix variable names within the same codebase
