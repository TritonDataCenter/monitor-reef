# API Template

This is a template for creating new API trait definitions.

## Usage

1. Copy this directory to `apis/your-service-api`
2. Update `Cargo.toml` with your service name
3. Define your API types (request/response structs) in `lib.rs`
4. Define your API endpoints as methods on the trait
5. Add the API to `openapi-manager/src/main.rs` for spec generation

## Structure

- All types should be public and derive `Serialize`, `Deserialize`, and `JsonSchema`
- Each endpoint method needs an `#[endpoint]` attribute
- The trait must have `#[dropshot::api_description]` attribute
- The trait must define an associated `Context` type

## Example

```rust
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MyRequest {
    pub field: String,
}

#[endpoint {
    method = POST,
    path = "/my-endpoint",
    tags = ["my-tag"],
}]
async fn my_endpoint(
    rqctx: RequestContext<Self::Context>,
    body: TypedBody<MyRequest>,
) -> Result<HttpResponseOk<MyResponse>, HttpError>;
```
