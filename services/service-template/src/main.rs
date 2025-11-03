// Service Template - Trait-Based Dropshot Service
//
// This template demonstrates how to implement a service using Dropshot API traits.
//
// To use this template:
// 1. First, create your API trait in apis/your-api/src/lib.rs
// 2. Uncomment and update the API import below
// 3. Define your ApiContext struct with needed state
// 4. Implement the trait for your implementation type
// 5. Update the main function to use your API

use anyhow::Result;
use dropshot::{
    ConfigDropshot, ConfigLogging, ConfigLoggingLevel, HttpServerStarter,
};
use tracing::info;

// Uncomment and replace with your API crate:
// use your_api::{YourApi, YourApiTypes, ...};

/// Context for your API handlers
///
/// Add any shared state your handlers need here (database connections,
/// caches, configuration, etc.)
struct ApiContext {
    // Example: db_pool: sqlx::PgPool,
    // Example: cache: Arc<RwLock<HashMap<String, String>>>,
}

// Uncomment and implement your API trait:
//
// enum YourServiceImpl {}
//
// impl YourApi for YourServiceImpl {
//     type Context = ApiContext;
//
//     async fn your_endpoint(
//         rqctx: RequestContext<Self::Context>,
//         // ... parameters
//     ) -> Result<HttpResponseOk<YourResponse>, HttpError> {
//         let ctx = rqctx.context();
//         // Your implementation here
//         todo!()
//     }
// }

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "service_template=info,dropshot=info",
        ))
        .init();

    // Create your API context
    let api_context = ApiContext {
        // Initialize your state here
    };

    // Get API description from the trait implementation
    // IMPORTANT: Uncomment and replace with your API trait implementation:
    let api = your_api::your_api_mod::api_description::<YourServiceImpl>()
        .map_err(|e| anyhow::anyhow!("Failed to create API description: {}", e))?;

    // NOTE: Do NOT use ApiDescription::new() - that's the old function-based pattern!
    // This template uses the trait-based approach as shown above.

    // Configure the server
    let config_dropshot = ConfigDropshot {
        bind_address: "127.0.0.1:8080".parse()?,
        default_request_body_max_bytes: 1024 * 1024, // 1MB
        default_handler_task_mode: dropshot::HandlerTaskMode::Detached,
        ..Default::default()
    };

    let config_logging = ConfigLogging::StderrTerminal {
        level: ConfigLoggingLevel::Info,
    };

    let log = config_logging
        .to_logger("service-template")
        .map_err(|error| anyhow::anyhow!("failed to create logger: {}", error))?;

    // Start the server
    let server = HttpServerStarter::new(&config_dropshot, api, api_context, &log)
        .map_err(|error| anyhow::anyhow!("failed to create server: {}", error))?
        .start();

    info!("Server running on http://127.0.0.1:8080");
    info!("OpenAPI spec available at http://127.0.0.1:8080/api/v1/openapi.json");

    server
        .await
        .map_err(|error| anyhow::anyhow!("server failed: {}", error))
}
