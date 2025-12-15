# Agent Prompt: Add Documentation Redirect Endpoints

## Objective

Add the 3 documentation redirect endpoints to the CloudAPI Dropshot API trait that were
previously marked as "intentionally omitted." These endpoints are important for:

1. **Complete behavioral parity**: A drop-in Rust replacement must behave exactly like
   the Node.js service, including documentation redirects
2. **Testing**: The CLI should be able to verify that both Node.js and Rust implementations
   return the expected 302 redirects with correct Location headers
3. **100% coverage**: All endpoints in production Node.js CloudAPI should be represented

## Background

The Node.js CloudAPI (lib/docs.js) has 3 documentation endpoints that return HTTP 302
redirects to external documentation URLs. These were previously omitted because they
are "not API functionality," but for complete testing and drop-in replacement verification,
they must be included.

## Source Materials

- **Node.js source**: `/Users/nshalman/Workspace/monitor-reef/target/sdc-cloudapi/lib/docs.js`
- **Current Rust API trait**: `apis/cloudapi-api/`
- **CLI**: `cli/cloudapi-cli/`
- **Missing features doc**: `conversion-plans/cloudapi/missing-features.md`
- **Validation report**: `conversion-plans/cloudapi/validation.md`

## Node.js Implementation Details

From `lib/docs.js`:

```javascript
function redirect(req, res, next) {
    res.set('Content-Length', 0);
    res.set('Connection', 'keep-alive');
    res.set('Date', new Date());
    res.header('Location', 'http://apidocs.tritondatacenter.com/cloudapi/');
    res.set('Server', 'Cloud API');
    res.send(302);
    return next(false);
}

function favicon(req, res, next) {
    res.set('Content-Length', 0);
    res.set('Connection', 'keep-alive');
    res.set('Date', new Date());
    res.header('Location', 'http://apidocs.tritondatacenter.com/favicon.ico');
    res.set('Server', 'Cloud API');
    res.send(302);
    return next(false);
}

function mount(server) {
    server.get('/', redirect);
    server.get(/^\/docs\/?/, redirect);
    server.get('/favicon.ico', favicon);
}
```

### Endpoint Summary

| Path | Method | Response | Location Header |
|------|--------|----------|-----------------|
| `/` | GET | 302 | `http://apidocs.tritondatacenter.com/cloudapi/` |
| `/docs` | GET | 302 | `http://apidocs.tritondatacenter.com/cloudapi/` |
| `/docs/` | GET | 302 | `http://apidocs.tritondatacenter.com/cloudapi/` |
| `/docs/*` | GET | 302 | `http://apidocs.tritondatacenter.com/cloudapi/` |
| `/favicon.ico` | GET | 302 | `http://apidocs.tritondatacenter.com/favicon.ico` |

Note: The Node.js uses a regex `/^\/docs\/?/` which matches `/docs`, `/docs/`, and
any path starting with `/docs/`. In Dropshot, we'll need to handle this with explicit
paths since Dropshot doesn't support regex routes.

## Tasks

### Task 1: Add Documentation Redirect Response Type

**File**: `apis/cloudapi-api/src/types/common.rs` or new `types/docs.rs`

Since Dropshot doesn't have a built-in 302 redirect response type, we need to create one.
Dropshot's `HttpResponseHeaders` can be used to add custom headers.

Research how Dropshot handles redirects. Options include:

1. Using `HttpResponseHeaders<HttpResponseUpdatedNoContent>` with Location header
2. Creating a custom response type that implements `HttpResponse`
3. Using `HttpError` with status code 302 (though this is semantically wrong)

The cleanest approach is likely to return a response with the appropriate status code
and Location header. Check Dropshot documentation for redirect patterns.

**Proposed Type**:
```rust
/// Response for documentation redirect endpoints
/// Returns HTTP 302 with Location header
#[derive(Debug, Serialize, JsonSchema)]
pub struct RedirectResponse {
    /// The URL to redirect to (returned in Location header)
    pub location: String,
}
```

### Task 2: Add Documentation Endpoints to API Trait

**File**: `apis/cloudapi-api/src/lib.rs`

Add endpoints in a new "Documentation" section:

```rust
// ========================================================================
// Documentation Redirects
// ========================================================================

/// Redirect root to API documentation
///
/// Returns HTTP 302 redirect to http://apidocs.tritondatacenter.com/cloudapi/
#[endpoint {
    method = GET,
    path = "/",
    tags = ["documentation"],
}]
async fn redirect_root(
    rqctx: RequestContext<Self::Context>,
) -> Result<HttpResponseFound, HttpError>;

/// Redirect /docs to API documentation
///
/// Returns HTTP 302 redirect to http://apidocs.tritondatacenter.com/cloudapi/
#[endpoint {
    method = GET,
    path = "/docs",
    tags = ["documentation"],
}]
async fn redirect_docs(
    rqctx: RequestContext<Self::Context>,
) -> Result<HttpResponseFound, HttpError>;

/// Redirect /docs/ to API documentation
///
/// Returns HTTP 302 redirect to http://apidocs.tritondatacenter.com/cloudapi/
#[endpoint {
    method = GET,
    path = "/docs/",
    tags = ["documentation"],
}]
async fn redirect_docs_slash(
    rqctx: RequestContext<Self::Context>,
) -> Result<HttpResponseFound, HttpError>;

/// Redirect favicon.ico to documentation site
///
/// Returns HTTP 302 redirect to http://apidocs.tritondatacenter.com/favicon.ico
#[endpoint {
    method = GET,
    path = "/favicon.ico",
    tags = ["documentation"],
}]
async fn redirect_favicon(
    rqctx: RequestContext<Self::Context>,
) -> Result<HttpResponseFound, HttpError>;
```

**Note on /docs/* handling**: The Node.js regex `/^\/docs\/?/` matches any path starting
with `/docs`. Since Dropshot doesn't support wildcard routes in the same way, we have
options:

1. Add explicit routes for `/docs` and `/docs/` only (simplest)
2. Research if Dropshot supports path wildcards like `/docs/{path:.*}`
3. Accept that `/docs/something` won't be handled (document this limitation)

For strict parity, investigate Dropshot's path matching capabilities. If wildcards
aren't supported, document the limitation clearly.

### Task 3: Research Dropshot Redirect Support

Before implementing, research:

1. Does Dropshot have `HttpResponseFound` (302) or similar?
2. How to set the `Location` header on a response?
3. How to return an empty body with status 302?

Check:
- Dropshot documentation: https://docs.rs/dropshot/latest/dropshot/
- Dropshot source code for response types
- Existing examples in the codebase

If Dropshot doesn't have a built-in redirect response, you may need to:

```rust
use dropshot::HttpResponseHeaders;
use http::StatusCode;

// Custom redirect response that sets Location header and returns 302
```

### Task 4: Add CLI Commands for Testing Documentation Endpoints

**File**: `cli/cloudapi-cli/src/main.rs`

Add new commands to test the documentation endpoints. These commands should:

1. Make HTTP requests to the endpoints
2. Check for 302 status code
3. Verify the Location header is correct
4. Report success/failure

```rust
/// Test documentation redirect endpoints
TestDocs {
    /// Test specific endpoint (root, docs, favicon) or all
    #[arg(long)]
    endpoint: Option<String>,
},
```

The CLI should NOT follow redirects automatically. Use reqwest with redirect policy
disabled to capture the 302 response.

**Expected Output**:
```
$ cloudapi test-docs
Testing documentation redirects...
  GET / -> 302 Location: http://apidocs.tritondatacenter.com/cloudapi/ [OK]
  GET /docs -> 302 Location: http://apidocs.tritondatacenter.com/cloudapi/ [OK]
  GET /docs/ -> 302 Location: http://apidocs.tritondatacenter.com/cloudapi/ [OK]
  GET /favicon.ico -> 302 Location: http://apidocs.tritondatacenter.com/favicon.ico [OK]
All documentation endpoints working correctly.
```

**Error Output Example**:
```
$ cloudapi test-docs
Testing documentation redirects...
  GET / -> 302 Location: http://apidocs.tritondatacenter.com/cloudapi/ [OK]
  GET /docs -> 200 [FAIL: expected 302]
  GET /favicon.ico -> 302 Location: http://wrong.url/favicon.ico [FAIL: wrong location]
2 of 4 tests failed.
```

### Task 5: Update Client Library

The generated client may need updates to handle 302 responses properly. Check if
Progenitor-generated clients can handle redirect responses or if custom handling
is needed.

Consider adding helper methods to the TypedClient:

```rust
impl TypedClient {
    /// Test if root redirects correctly
    pub async fn test_root_redirect(&self) -> Result<RedirectTestResult, Error> {
        // Use reqwest with redirect disabled
        // Check status code and Location header
    }
}
```

### Task 6: Update OpenAPI Spec

After adding endpoints:

```bash
cargo run -p openapi-manager -- generate
```

Verify the spec includes:
- All 4 documentation endpoints (or 3 if /docs and /docs/ are merged)
- Correct response types (302 with Location header)
- Documentation tag

### Task 7: Update Documentation

Update `conversion-plans/cloudapi/missing-features.md`:
- Remove documentation redirects from "Intentionally Omitted" section
- Mark as implemented

Update `conversion-plans/cloudapi/validation.md`:
- Update endpoint count (183 + 4 = 187, or similar)
- Update coverage to note documentation endpoints included
- Remove documentation redirects from gaps section

## Verification Steps

1. **Build succeeds**:
   ```bash
   cargo build -p cloudapi-api
   ```

2. **OpenAPI spec generates**:
   ```bash
   cargo run -p openapi-manager -- generate
   ```

3. **Client builds**:
   ```bash
   cargo build -p cloudapi-client
   ```

4. **CLI builds**:
   ```bash
   cargo build -p cloudapi-cli
   ```

5. **All checks pass**:
   ```bash
   make check
   ```

6. **Test against live Node.js CloudAPI** (if available):
   ```bash
   cloudapi --base-url https://your-cloudapi-server test-docs
   ```

## Commit Strategy

Make atomic commits for each logical unit:

1. `Add redirect response type for documentation endpoints`
2. `Add documentation redirect endpoints to CloudAPI trait`
3. `Add test-docs command to CloudAPI CLI`
4. `Update OpenAPI spec for documentation endpoints`
5. `Update documentation to reflect complete endpoint coverage`

## Implementation Notes

### Dropshot Response Types

Dropshot provides these response types (check current version):
- `HttpResponseOk<T>` - 200
- `HttpResponseCreated<T>` - 201
- `HttpResponseDeleted` - 204
- `HttpResponseUpdatedNoContent` - 204

For 302, you may need to look at:
- `HttpResponseHeaders` - wraps another response to add headers
- `HttpResponseSeeOther` - 303 (close but not exact)
- Custom implementation

### Redirect Policy for Testing

When testing redirects with reqwest:

```rust
use reqwest::redirect::Policy;

let client = reqwest::Client::builder()
    .redirect(Policy::none())  // Don't follow redirects
    .build()?;

let response = client.get(url).send().await?;
assert_eq!(response.status(), 302);
assert_eq!(
    response.headers().get("location").unwrap(),
    "http://apidocs.tritondatacenter.com/cloudapi/"
);
```

### No Authentication Required

These documentation endpoints do NOT require authentication in the Node.js implementation.
They are publicly accessible. Ensure the Rust trait and implementation reflect this.

## Success Criteria

- [ ] Documentation redirect response type created
- [ ] 4 documentation endpoints added to API trait (/, /docs, /docs/, /favicon.ico)
- [ ] OpenAPI spec regenerated with ~187 operations
- [ ] CLI `test-docs` command implemented
- [ ] CLI can verify redirects against live Node.js CloudAPI
- [ ] `make check` passes
- [ ] Documentation updated to reflect complete coverage

## Questions to Resolve

1. How does Dropshot handle 302 redirects? Is there a built-in type?
2. Should /docs/* wildcard be supported, or just /docs and /docs/?
3. Should the redirect URLs be configurable (for testing against different doc sites)?
4. Should the CLI test-docs command be in cloudapi-cli or a separate test utility?

## References

- Dropshot documentation: https://docs.rs/dropshot/latest/dropshot/
- Node.js CloudAPI source: `/Users/nshalman/Workspace/monitor-reef/target/sdc-cloudapi/`
- HTTP 302 specification: RFC 7231 Section 6.4.3
