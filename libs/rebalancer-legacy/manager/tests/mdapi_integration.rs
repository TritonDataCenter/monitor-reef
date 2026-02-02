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
//! These tests require a running mdapi server and test infrastructure.
//! They are marked as `#[ignore]` by default and can be run with:
//!
//! ```bash
//! cargo test -p manager --test mdapi_integration -- --ignored
//! ```
//!
//! # Environment Variables
//!
//! - `MDAPI_TEST_ENDPOINT`: Mdapi server endpoint (e.g., "mdapi.test.domain:2030")
//! - `MDAPI_TEST_OWNER`: Test owner UUID
//! - `MDAPI_TEST_BUCKET`: Test bucket UUID
//! - `MORAY_TEST_DOMAIN`: Moray domain for hybrid tests
//! - `TEST_SHARK`: Storage ID of test shark to evacuate
//!
//! # Test Infrastructure Requirements
//!
//! 1. **Mdapi staging server** - A buckets-mdapi instance with test data
//! 2. **Moray test instance** - For hybrid mode testing
//! 3. **Test shark** - Storage node with 10K-100K test objects
//!
//! # Test Categories
//!
//! - Basic connectivity tests
//! - Object discovery tests
//! - Metadata update tests
//! - Batch processing tests
//! - Error handling tests
//! - Performance benchmarks

use std::env;
use uuid::Uuid;

/// Test configuration loaded from environment variables
struct TestConfig {
    mdapi_endpoint: Option<String>,
    mdapi_owner: Option<Uuid>,
    mdapi_bucket: Option<Uuid>,
    moray_domain: Option<String>,
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

    fn has_shark(&self) -> bool {
        self.test_shark.is_some()
    }
}

// =============================================================================
// Basic Connectivity Tests
// =============================================================================

/// Test mdapi client creation and basic connectivity
#[test]
#[ignore]
fn test_mdapi_client_connectivity() {
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

// =============================================================================
// Object Discovery Tests
// =============================================================================

/// Test listing objects from mdapi
#[test]
#[ignore]
fn test_mdapi_list_objects() {
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

/// Test listing buckets for an owner
#[test]
#[ignore]
fn test_mdapi_list_buckets() {
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

// =============================================================================
// Metadata Update Tests
// =============================================================================

/// Test updating a single object's metadata
#[test]
#[ignore]
fn test_mdapi_put_object() {
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

// =============================================================================
// Batch Processing Tests
// =============================================================================

/// Test batch update with chunking
#[test]
#[ignore]
fn test_mdapi_batch_update() {
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

// =============================================================================
// Error Handling Tests
// =============================================================================

/// Test behavior when mdapi endpoint is unreachable
#[test]
#[ignore]
fn test_mdapi_connection_failure() {
    // Use an invalid endpoint
    let result = manager::mdapi_client::create_client("invalid.endpoint:9999");

    // Client creation should succeed (lazy connection)
    assert!(result.is_ok());

    let client = result.unwrap();

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

    println!(
        "Got expected error: {:?}",
        list_result.err()
    );
}

/// Test behavior with invalid bucket ID
#[test]
#[ignore]
fn test_mdapi_invalid_bucket() {
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

// =============================================================================
// Hybrid Mode Tests (Moray + Mdapi)
// =============================================================================

/// Test configuration for hybrid mode
#[test]
#[ignore]
fn test_hybrid_mode_config() {
    let config = TestConfig::from_env();
    if !config.has_mdapi() || !config.has_moray() {
        eprintln!("Skipping: Both MDAPI and MORAY env vars required");
        return;
    }

    let mdapi_config = manager::config::MdapiConfig {
        enabled: true,
        endpoint: config.mdapi_endpoint.unwrap(),
        default_bucket_id: config.mdapi_bucket,
        connection_timeout_ms: 5000,
        single_bucket_mode: false,
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
// Performance Benchmarks
// =============================================================================

/// Benchmark object listing performance
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

/// Benchmark batch update performance
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
