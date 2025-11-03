use anyhow::Result;
use camino::Utf8PathBuf;
use clap::Parser;
use dropshot_api_manager::{Environment, ManagedApiConfig};
use dropshot_api_manager_types::{ManagedApiMetadata, Versions};
use std::process::ExitCode;

fn environment() -> Result<Environment> {
    let workspace_root = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();

    let env = Environment::new(
        "cargo openapi".to_string(),
        workspace_root,
        "openapi-specs/generated",
    )?;
    Ok(env)
}

fn all_apis() -> Result<dropshot_api_manager::ManagedApis> {
    let apis = vec![
        ManagedApiConfig {
            ident: "bugview-api",
            versions: Versions::Lockstep {
                version: "0.1.0".parse().unwrap(),
            },
            title: "Bugview API",
            metadata: ManagedApiMetadata {
                description: Some("Public JIRA issue viewer API"),
                ..ManagedApiMetadata::default()
            },
            api_description: bugview_api::bugview_api_mod::stub_api_description,
            extra_validation: None,
        },
        ManagedApiConfig {
            ident: "jira-api",
            versions: Versions::Lockstep {
                version: "0.1.0".parse().unwrap(),
            },
            title: "JIRA API (Subset)",
            metadata: ManagedApiMetadata {
                description: Some("Subset of JIRA REST API v3 used by bugview-service. This is NOT a complete JIRA API - only the specific endpoints we consume."),
                ..ManagedApiMetadata::default()
            },
            api_description: jira_api::jira_api_mod::stub_api_description,
            extra_validation: None,
        },
    ];
    let managed_apis = dropshot_api_manager::ManagedApis::new(apis)?;
    Ok(managed_apis)
}

fn main() -> Result<ExitCode> {
    let app = dropshot_api_manager::App::parse();
    let env = environment()?;
    let apis = all_apis()?;

    Ok(app.exec(&env, &apis))
}
