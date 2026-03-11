// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Job types
//!
//! VMAPI uses snake_case for all JSON field names.
//! Tests verify Job, TaskChainEntry, TaskResult, and JobExecution types.

mod common;

use uuid::Uuid;
use vmapi_api::types::{Job, JobExecution};

#[test]
fn test_job_provision_deserialize() {
    let job: Job = common::deserialize_fixture("job", "provision.json");

    assert_eq!(
        job.uuid,
        Uuid::parse_str("f6789012-6789-6789-6789-678901234567").unwrap()
    );
    assert_eq!(job.name.as_deref(), Some("provision"));
    assert_eq!(job.execution, Some(JobExecution::Succeeded));
    assert_eq!(
        job.vm_uuid,
        Some(Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap())
    );
}

#[test]
fn test_job_chain() {
    let job: Job = common::deserialize_fixture("job", "provision.json");

    let chain = job.chain.expect("chain should be present");
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].name, "cnapi.validate_vm");
    assert_eq!(chain[0].timeout, Some(30));
    assert!(chain[0].retry.is_none());
    assert_eq!(chain[1].name, "cnapi.provision_vm");
    assert_eq!(chain[1].timeout, Some(300));
    assert_eq!(chain[1].retry, Some(3));
}

#[test]
fn test_job_chain_results() {
    let job: Job = common::deserialize_fixture("job", "provision.json");

    let results = job.chain_results.expect("chain_results should be present");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].result.as_deref(), Some("OK"));
    assert_eq!(results[0].name.as_deref(), Some("cnapi.validate_vm"));
    assert!(results[0].started_at.is_some());
    assert!(results[0].finished_at.is_some());
    assert!(results[0].error.is_none());
}

#[test]
fn test_job_timing() {
    let job: Job = common::deserialize_fixture("job", "provision.json");

    assert!(job.created_at.is_some());
    assert_eq!(job.elapsed, Some(30.5));
    assert_eq!(job.timeout, Some(600));
    assert_eq!(job.num_tasks_done, Some(2));
}

#[test]
fn test_job_failed_deserialize() {
    let job: Job = common::deserialize_fixture("job", "failed.json");

    assert_eq!(job.execution, Some(JobExecution::Failed));

    let results = job.chain_results.expect("chain_results should be present");
    assert!(results[0].error.is_some(), "failed task should have error");
}

#[test]
fn test_job_onerror_chain() {
    let job: Job = common::deserialize_fixture("job", "failed.json");

    let onerror = job.onerror.expect("onerror should be present");
    assert_eq!(onerror.len(), 1);
    assert_eq!(onerror[0].name, "common.cleanup");

    let onerror_results = job
        .onerror_results
        .expect("onerror_results should be present");
    assert_eq!(onerror_results.len(), 1);
    assert_eq!(onerror_results[0].result.as_deref(), Some("OK"));
}

/// Test deserialization of all JobExecution enum variants.
#[test]
fn test_job_execution_variants() {
    let cases = [
        ("queued", JobExecution::Queued),
        ("running", JobExecution::Running),
        ("succeeded", JobExecution::Succeeded),
        ("failed", JobExecution::Failed),
        ("canceled", JobExecution::Canceled),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: JobExecution = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse job execution: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown execution states deserialize as Unknown.
#[test]
fn test_job_execution_unknown_variant() {
    let json = r#""retrying""#;
    let parsed: JobExecution = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, JobExecution::Unknown);
}

/// Test deserialization of a job list.
#[test]
fn test_job_list_deserialize() {
    let json = format!(
        "[{}, {}]",
        common::load_fixture("job", "provision.json"),
        common::load_fixture("job", "failed.json")
    );

    let jobs: Vec<Job> = serde_json::from_str(&json).expect("Failed to parse job list");
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].execution, Some(JobExecution::Succeeded));
    assert_eq!(jobs[1].execution, Some(JobExecution::Failed));
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_job_round_trip() {
    let original: Job = common::deserialize_fixture("job", "provision.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Job = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.uuid, deserialized.uuid);
    assert_eq!(original.name, deserialized.name);
    assert_eq!(original.execution, deserialized.execution);
    assert_eq!(original.vm_uuid, deserialized.vm_uuid);
    assert_eq!(original.elapsed, deserialized.elapsed);
}
