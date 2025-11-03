use anyhow::Result;
use camino::Utf8PathBuf;
use clap::Parser;
use dropshot_api_manager::Environment;
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
    // No APIs registered yet - they will be added as we develop them
    let apis = vec![];
    let managed_apis = dropshot_api_manager::ManagedApis::new(apis)?;
    Ok(managed_apis)
}

fn main() -> Result<ExitCode> {
    let app = dropshot_api_manager::App::parse();
    let env = environment()?;
    let apis = all_apis()?;

    Ok(app.exec(&env, &apis))
}
