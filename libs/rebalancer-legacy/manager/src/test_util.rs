/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

use crate::config::Config;
use rebalancer::error::Error;

use lazy_static::lazy_static;
use mustache::{Data, MapBuilder};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

// Legacy static path for backwards compatibility - tests should migrate to TestConfig
pub static TEST_CONFIG_FILE: &str = "config.test.json";

lazy_static! {
    pub static ref TEMPLATE_PATH: String = format!(
        "{}/{}",
        env!("CARGO_MANIFEST_DIR"),
        "../sapi_manifests/rebalancer/template"
    );
}

/// A test configuration that uses a unique temporary file.
/// This allows tests to run in parallel without race conditions.
/// The temp file is automatically cleaned up when TestConfig is dropped.
pub struct TestConfig {
    pub config: Config,
    pub config_path: PathBuf,
    // Keep the NamedTempFile alive - it deletes the file on drop
    _temp_file: NamedTempFile,
}

impl TestConfig {
    /// Create a new test config with default values.
    /// Uses a unique temp file that won't conflict with other parallel tests.
    pub fn new() -> Self {
        let vars = MapBuilder::new()
            .insert_str("DOMAIN_NAME", "fake.joyent.us")
            .insert_bool("SNAPLINK_CLEANUP_REQUIRED", true)
            .insert_vec("INDEX_MORAY_SHARDS", |builder| {
                builder.push_map(|bld| {
                    bld.insert_str("host", "1.fake.joyent.us")
                        .insert_bool("last", true)
                })
            })
            .build();

        Self::with_vars(&vars)
    }

    /// Create a test config with custom template variables.
    pub fn with_vars(vars: &Data) -> Self {
        let temp_file = NamedTempFile::new().expect("create temp file");
        let config_path = temp_file.path().to_path_buf();

        let template_str = std::fs::read_to_string(TEMPLATE_PATH.to_string())
            .expect("template string");

        let config_data = mustache::compile_str(&template_str)
            .and_then(|t| t.render_data_to_string(vars))
            .expect("render template");

        std::fs::write(&config_path, config_data.as_bytes())
            .expect("write config file");

        let config = Config::parse_config(&Some(config_path.to_string_lossy().to_string()))
            .expect("parse config");

        TestConfig {
            config,
            config_path,
            _temp_file: temp_file,
        }
    }

    /// Create a test config from raw file contents.
    pub fn from_contents(contents: &[u8]) -> Self {
        let temp_file = NamedTempFile::new().expect("create temp file");
        let config_path = temp_file.path().to_path_buf();

        std::fs::write(&config_path, contents).expect("write config file");

        let config = Config::parse_config(&Some(config_path.to_string_lossy().to_string()))
            .expect("parse config");

        TestConfig {
            config,
            config_path,
            _temp_file: temp_file,
        }
    }

    /// Update the config file with new template variables and reload.
    pub fn update_with_vars(&mut self, vars: &Data) {
        let template_str = std::fs::read_to_string(TEMPLATE_PATH.to_string())
            .expect("template string");

        let config_data = mustache::compile_str(&template_str)
            .and_then(|t| t.render_data_to_string(vars))
            .expect("render template");

        std::fs::write(&self.config_path, config_data.as_bytes())
            .expect("write config file");

        self.config = Config::parse_config(&Some(self.config_path.to_string_lossy().to_string()))
            .expect("parse config");
    }

    /// Get the config file path as a String.
    pub fn path_string(&self) -> String {
        self.config_path.to_string_lossy().to_string()
    }
}

// ============================================================================
// Legacy functions below - kept for backward compatibility
// Tests should migrate to TestConfig for parallel-safe execution
// ============================================================================

pub fn write_config_file(buf: &[u8]) -> Config {
    File::create(TEST_CONFIG_FILE)
        .and_then(|mut f| f.write_all(buf))
        .map_err(Error::from)
        .and_then(|_| Config::parse_config(&Some(TEST_CONFIG_FILE.to_string())))
        .expect("file write")
}

// Update our test config file with new variables
pub fn update_test_config_with_vars(vars: &Data) -> Config {
    let template_str = std::fs::read_to_string(TEMPLATE_PATH.to_string())
        .expect("template string");

    println!("{}", template_str);

    let config_data = mustache::compile_str(&template_str)
        .and_then(|t| t.render_data_to_string(vars))
        .expect("render template");

    println!("{}", &config_data);
    write_config_file(config_data.as_bytes())
}

// Initialize a test configuration file by parsing and rendering the
// same configuration template used in production.
pub fn config_init() -> Config {
    std::fs::remove_file(TEST_CONFIG_FILE).unwrap_or(());

    let vars = MapBuilder::new()
        .insert_str("DOMAIN_NAME", "fake.joyent.us")
        .insert_bool("SNAPLINK_CLEANUP_REQUIRED", true)
        .insert_vec("INDEX_MORAY_SHARDS", |builder| {
            builder.push_map(|bld| {
                bld.insert_str("host", "1.fake.joyent.us")
                    .insert_bool("last", true)
            })
        })
        .build();

    update_test_config_with_vars(&vars)
}

pub fn config_fini() {
    std::fs::remove_file(TEST_CONFIG_FILE)
        .expect("attempt to delete missing file")
}
