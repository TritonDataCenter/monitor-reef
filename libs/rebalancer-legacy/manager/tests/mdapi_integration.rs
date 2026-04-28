/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2025 Edgecast Cloud LLC.
 */

//! Integration tests for mdapi evacuation functionality.
//!
//! This file contains two categories of tests:
//!
//! ## Mock-based tests (run in CI)
//!
//! These use an in-process mock Fast RPC server to exercise the full
//! RPC codepath through `MdapiClient` → TCP → Fast RPC protocol →
//! mock handler, without requiring external infrastructure.
//!
//! ```bash
//! cargo test -p manager --test mdapi_integration
//! ```
//!
//! ## Live integration tests (`#[ignore]`)
//!
//! These require a running mdapi server and test infrastructure.
//! They validate behavior against a real deployed instance.
//!
//! ```bash
//! cargo test -p manager --test mdapi_integration -- --ignored
//! ```
//!
//! ### Environment Variables (for live tests)
//!
//! - `MDAPI_TEST_ENDPOINT`: Mdapi server endpoint (e.g., "mdapi.test.domain:2030")
//! - `MDAPI_TEST_OWNER`: Test owner UUID
//! - `MDAPI_TEST_BUCKET`: Test bucket UUID
//! - `MORAY_TEST_DOMAIN`: Moray domain for hybrid tests
//! - `TEST_SHARK`: Storage ID of test shark to evacuate

mod common;

use std::env;

use common::mock_mdapi::{
    test_bucket_uuid, test_owner_uuid, MockMdapiServer,
};
use libmanta::mdapi::MdapiClient;
use uuid::Uuid;

// =============================================================================
// Mock-based tests (run in CI without external infrastructure)
// =============================================================================

/// Test mdapi client creation and basic connectivity via mock server
#[test]
fn test_mdapi_client_connectivity() {
    let server = MockMdapiServer::start();
    let client =
        MdapiClient::new(&server.endpoint()).expect("create client");

    // Make a simple RPC call to verify the connection works end-to-end.
    let vnodes = manager::mdapi_client::list_vnodes(&client);
    assert!(
        vnodes.is_ok(),
        "Failed to communicate with mock: {:?}",
        vnodes.err()
    );
}

/// Test listing objects from mdapi via mock
#[test]
fn test_mdapi_list_objects() {
    let server = MockMdapiServer::start();
    let client =
        MdapiClient::new(&server.endpoint()).expect("create client");

    let objects = manager::mdapi_client::find_objects(
        &client,
        test_owner_uuid(),
        test_bucket_uuid(),
        None,
        100,
    );

    assert!(
        objects.is_ok(),
        "Failed to list objects: {:?}",
        objects.err()
    );

    let objs = objects.unwrap();
    assert_eq!(objs.len(), 3, "Expected 3 test objects from mock");

    for obj in &objs {
        assert!(!obj.object_id.is_empty(), "object_id should not be empty");
        assert!(!obj.name.is_empty(), "name should not be empty");
    }
}

/// Test listing buckets for an owner via mock
#[test]
fn test_mdapi_list_buckets() {
    let server = MockMdapiServer::start();
    let client =
        MdapiClient::new(&server.endpoint()).expect("create client");

    let buckets =
        manager::mdapi_client::list_buckets(&client, test_owner_uuid());

    assert!(
        buckets.is_ok(),
        "Failed to list buckets: {:?}",
        buckets.err()
    );

    let b = buckets.unwrap();
    assert!(!b.is_empty(), "Expected at least one bucket");
    assert_eq!(b[0].name, "test-bucket");
}

/// Test updating a single object's metadata via mock
#[test]
fn test_mdapi_put_object() {
    let server = MockMdapiServer::start();
    let client =
        MdapiClient::new(&server.endpoint()).expect("create client");

    let objects = manager::mdapi_client::find_objects(
        &client,
        test_owner_uuid(),
        test_bucket_uuid(),
        None,
        1,
    )
    .expect("list objects");

    assert!(!objects.is_empty(), "Need at least one object for update test");

    let obj = &objects[0];
    let result = manager::mdapi_client::put_object(
        &client,
        obj,
        test_bucket_uuid(),
        Some(&obj.etag),
    );

    assert!(
        result.is_ok(),
        "Failed to update object: {:?}",
        result.err()
    );
}

/// Test batch update with chunking via mock
#[test]
fn test_mdapi_batch_update() {
    let server = MockMdapiServer::start();
    let client =
        MdapiClient::new(&server.endpoint()).expect("create client");

    let objects = manager::mdapi_client::find_objects(
        &client,
        test_owner_uuid(),
        test_bucket_uuid(),
        None,
        50,
    )
    .expect("list objects");

    assert!(!objects.is_empty(), "Need objects for batch test");

    let batch: Vec<_> = objects
        .iter()
        .map(|obj| (obj, test_bucket_uuid(), Some(obj.etag.as_str())))
        .collect();

    let result = manager::mdapi_client::batch_update_with_config(
        &client,
        batch,
        Some(10),
    );

    assert!(
        result.is_ok(),
        "Batch update failed: {:?}",
        result.err()
    );

    let batch_result = result.unwrap();
    assert!(
        batch_result.successful > 0,
        "Expected some successful updates"
    );
}

/// Test behavior when mdapi endpoint is unreachable.
///
/// Uses `MdapiClient::new` directly (bypassing DNS SRV
/// resolution in `create_client`) so the test exercises
/// the RPC-level connection failure path.
#[test]
fn test_mdapi_connection_failure() {
    // Create client pointing to an unreachable endpoint.
    // MdapiClient::new is lazy — it stores the address
    // without connecting.
    let client = MdapiClient::new("127.0.0.1:9999")
        .expect("client creation is lazy");

    // But operations should fail
    let owner = Uuid::new_v4();
    let bucket_id = Uuid::new_v4();

    let list_result = manager::mdapi_client::find_objects(
        &client,
        owner,
        bucket_id,
        None,
        10,
    );

    assert!(
        list_result.is_err(),
        "Expected error when connecting to invalid endpoint"
    );
}

/// Test behavior with invalid bucket ID via mock
#[test]
fn test_mdapi_invalid_bucket() {
    let server = MockMdapiServer::start();
    let client =
        MdapiClient::new(&server.endpoint()).expect("create client");

    // Use a random bucket ID that the mock won't recognize
    let invalid_bucket = Uuid::new_v4();

    let result = manager::mdapi_client::find_objects(
        &client,
        test_owner_uuid(),
        invalid_bucket,
        None,
        10,
    );

    // Should return empty list — mock returns [] for unknown bucket_id
    match result {
        Ok(objects) => {
            assert!(
                objects.is_empty(),
                "Expected empty list for invalid bucket"
            );
        }
        Err(e) => {
            panic!("Expected empty list, got error: {}", e);
        }
    }
}

/// Test configuration for hybrid mode.
///
/// Pure config test — constructs MdapiConfig directly without
/// requiring a running server.
#[test]
fn test_hybrid_mode_config() {
    let mdapi_config = manager::config::MdapiConfig {
        shards: vec![manager::config::MdapiShard {
            host: "127.0.0.1:2030".to_string(),
        }],
        connection_timeout_ms: 5000,
        max_batch_size: 100,
        operation_timeout_ms: 30000,
        max_retries: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 5000,
    };

    let use_mdapi = manager::mdapi_client::should_use_mdapi(&mdapi_config);
    assert!(use_mdapi, "should_use_mdapi should return true");
}

// =============================================================================
// Live integration tests (require real mdapi infrastructure)
// =============================================================================

/// Test configuration loaded from environment variables for live tests.
struct TestConfig {
    mdapi_endpoint: Option<String>,
    mdapi_owner: Option<Uuid>,
    mdapi_bucket: Option<Uuid>,
    moray_domain: Option<String>,
    #[allow(dead_code)]
    test_shark: Option<String>,
}

impl TestConfig {
    fn from_env() -> Self {
        TestConfig {
            mdapi_endpoint: env::var("MDAPI_TEST_ENDPOINT").ok(),
            mdapi_owner: env::var("MDAPI_TEST_OWNER")
                .ok()
                .and_then(|s| Uuid::parse_str(&s).ok()),
            mdapi_bucket: env::var("MDAPI_TEST_BUCKET")
                .ok()
                .and_then(|s| Uuid::parse_str(&s).ok()),
            moray_domain: env::var("MORAY_TEST_DOMAIN").ok(),
            test_shark: env::var("TEST_SHARK").ok(),
        }
    }

    fn has_mdapi(&self) -> bool {
        self.mdapi_endpoint.is_some()
            && self.mdapi_owner.is_some()
            && self.mdapi_bucket.is_some()
    }

    fn has_moray(&self) -> bool {
        self.moray_domain.is_some()
    }
}

/// Test mdapi client creation and basic connectivity against live server
#[test]
#[ignore]
fn test_live_mdapi_client_connectivity() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_ENDPOINT not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let result = manager::mdapi_client::create_client(&endpoint);

    assert!(
        result.is_ok(),
        "Failed to create mdapi client: {:?}",
        result.err()
    );

    println!("Successfully connected to mdapi at {}", endpoint);
}

/// Test listing objects from a live mdapi server
#[test]
#[ignore]
fn test_live_mdapi_list_objects() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();
    let bucket_id = config.mdapi_bucket.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    let objects = manager::mdapi_client::find_objects(
        &client,
        owner,
        bucket_id,
        None,  // no prefix
        100,   // limit
    );

    assert!(
        objects.is_ok(),
        "Failed to list objects: {:?}",
        objects.err()
    );

    let objs = objects.unwrap();
    println!("Found {} objects in bucket {}", objs.len(), bucket_id);

    // Verify object structure
    for obj in objs.iter().take(5) {
        println!(
            "  - {} (owner={}, vnode={})",
            obj.name, obj.owner, obj.vnode
        );
        assert!(!obj.object_id.is_empty(), "object_id should not be empty");
        assert!(!obj.name.is_empty(), "name should not be empty");
    }
}

/// Test listing buckets for an owner on a live mdapi server
#[test]
#[ignore]
fn test_live_mdapi_list_buckets() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    let buckets = manager::mdapi_client::list_buckets(&client, owner);

    match buckets {
        Ok(b) => {
            println!("Found {} buckets for owner {}", b.len(), owner);
            for bucket in &b {
                println!("  - {} (id={})", bucket.name, bucket.id);
            }
        }
        Err(e) => {
            // list_buckets may not be supported on older mdapi versions
            println!("list_buckets returned error (may be expected): {}", e);
        }
    }
}

/// Test updating a single object's metadata on a live mdapi server
#[test]
#[ignore]
fn test_live_mdapi_put_object() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();
    let bucket_id = config.mdapi_bucket.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    // First, get an existing object to update
    let objects = manager::mdapi_client::find_objects(
        &client,
        owner,
        bucket_id,
        None,
        1,
    )
    .expect("list objects");

    if objects.is_empty() {
        eprintln!("No objects found in test bucket, skipping update test");
        return;
    }

    let obj = &objects[0];
    println!("Testing update on object: {}", obj.name);

    // Update the object (this is a no-op update - just validates the path)
    let result = manager::mdapi_client::put_object(
        &client,
        obj,
        bucket_id,
        Some(&obj.etag),
    );

    assert!(
        result.is_ok(),
        "Failed to update object: {:?}",
        result.err()
    );

    println!("Successfully updated object metadata");
}

/// Test batch update with chunking on a live mdapi server
#[test]
#[ignore]
fn test_live_mdapi_batch_update() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();
    let bucket_id = config.mdapi_bucket.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    // Get some objects to update
    let objects = manager::mdapi_client::find_objects(
        &client,
        owner,
        bucket_id,
        None,
        50,  // Get up to 50 objects
    )
    .expect("list objects");

    if objects.is_empty() {
        eprintln!("No objects found in test bucket, skipping batch test");
        return;
    }

    println!("Testing batch update on {} objects", objects.len());

    // Build batch update tuples
    let batch: Vec<_> = objects
        .iter()
        .map(|obj| (obj, bucket_id, Some(obj.etag.as_str())))
        .collect();

    // Test with small batch size to force chunking
    let result = manager::mdapi_client::batch_update_with_config(
        &client,
        batch,
        Some(10),  // Force chunking at 10 objects
    );

    assert!(
        result.is_ok(),
        "Batch update failed: {:?}",
        result.err()
    );

    let batch_result = result.unwrap();
    println!(
        "Batch update complete: {} successful, {} failed",
        batch_result.successful, batch_result.failed
    );

    // Log any failures
    for (name, err) in &batch_result.errors {
        eprintln!("  Failed: {} - {}", name, err);
    }
}

/// Test behavior with invalid bucket ID on a live mdapi server
#[test]
#[ignore]
fn test_live_mdapi_invalid_bucket() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    // Use a random bucket ID that doesn't exist
    let invalid_bucket = Uuid::new_v4();

    let result = manager::mdapi_client::find_objects(
        &client,
        owner,
        invalid_bucket,
        None,
        10,
    );

    // Should return empty list or error, not panic
    match result {
        Ok(objects) => {
            assert!(objects.is_empty(), "Expected empty list for invalid bucket");
            println!("Got empty list for non-existent bucket (expected)");
        }
        Err(e) => {
            println!("Got error for non-existent bucket (expected): {}", e);
        }
    }
}

/// Test configuration for hybrid mode against live infrastructure
#[test]
#[ignore]
fn test_live_hybrid_mode_config() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() || !config.has_moray() {
        eprintln!("Skipping: Both MDAPI and MORAY env vars required");
        return;
    }

    let mdapi_config = manager::config::MdapiConfig {
        shards: vec![manager::config::MdapiShard {
            host: config.mdapi_endpoint.unwrap(),
        }],
        connection_timeout_ms: 5000,
        max_batch_size: 100,
        operation_timeout_ms: 30000,
        max_retries: 3,
        initial_backoff_ms: 100,
        max_backoff_ms: 5000,
    };

    // Verify hybrid mode detection
    let use_mdapi = manager::mdapi_client::should_use_mdapi(&mdapi_config);
    assert!(use_mdapi, "should_use_mdapi should return true");

    let use_moray = !config.moray_domain.unwrap().is_empty();
    assert!(use_moray, "moray domain should be set");

    println!("Hybrid mode configuration validated");
}

// =============================================================================
// Performance Benchmarks (require live infrastructure)
// =============================================================================

/// Benchmark object listing performance against live server
#[test]
#[ignore]
fn benchmark_mdapi_list_objects() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();
    let bucket_id = config.mdapi_bucket.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    // Warm up
    let _ = manager::mdapi_client::find_objects(&client, owner, bucket_id, None, 10);

    // Benchmark
    let iterations = 10;
    let start = std::time::Instant::now();

    for _ in 0..iterations {
        let _ = manager::mdapi_client::find_objects(
            &client,
            owner,
            bucket_id,
            None,
            100,
        );
    }

    let elapsed = start.elapsed();
    let avg_ms = elapsed.as_millis() as f64 / iterations as f64;

    println!(
        "Benchmark: {} iterations, avg {:.2}ms per list_objects(100)",
        iterations, avg_ms
    );
}

/// Benchmark batch update performance against live server
#[test]
#[ignore]
fn benchmark_mdapi_batch_update() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() {
        eprintln!("Skipping: MDAPI_TEST_* env vars not set");
        return;
    }

    let endpoint = config.mdapi_endpoint.unwrap();
    let owner = config.mdapi_owner.unwrap();
    let bucket_id = config.mdapi_bucket.unwrap();

    let client = manager::mdapi_client::create_client(&endpoint)
        .expect("create client");

    // Get objects for benchmark
    let objects = manager::mdapi_client::find_objects(
        &client,
        owner,
        bucket_id,
        None,
        100,
    )
    .expect("list objects");

    if objects.is_empty() {
        eprintln!("No objects found, skipping benchmark");
        return;
    }

    let batch: Vec<_> = objects
        .iter()
        .map(|obj| (obj, bucket_id, Some(obj.etag.as_str())))
        .collect();

    // Benchmark different batch sizes
    for batch_size in [10, 25, 50, 100] {
        let start = std::time::Instant::now();

        let result = manager::mdapi_client::batch_update_with_config(
            &client,
            batch.clone(),
            Some(batch_size),
        )
        .expect("batch update");

        let elapsed = start.elapsed();
        let objs_per_sec = if elapsed.as_secs_f64() > 0.0 {
            result.successful as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        println!(
            "Batch size {}: {} objects in {:.2}ms ({:.1} objs/sec)",
            batch_size,
            result.successful,
            elapsed.as_millis(),
            objs_per_sec
        );
    }
}
