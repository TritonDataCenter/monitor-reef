// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Deserialization tests for Migration types
//!
//! VMAPI uses snake_case for all JSON field names.
//! Tests verify Migration, MigrationProgress, MigrationState, and
//! MigrationPhase types.

mod common;

use uuid::Uuid;
use vmapi_api::types::{Migration, MigrationPhase, MigrationState};

#[test]
fn test_migration_in_progress_deserialize() {
    let migration: Migration = common::deserialize_fixture("migration", "in_progress.json");

    assert_eq!(
        migration.vm_uuid,
        Uuid::parse_str("a1234567-1234-1234-1234-123456789012").unwrap()
    );
    assert_eq!(migration.state, MigrationState::Sync);
    assert_eq!(migration.phase, Some(MigrationPhase::Sync));
    assert_eq!(
        migration.source_server_uuid,
        Some(Uuid::parse_str("44454c4c-5300-1057-8050-b7c04f533532").unwrap())
    );
    assert_eq!(
        migration.target_server_uuid,
        Some(Uuid::parse_str("55565c5c-6400-2068-9060-c8d15f644643").unwrap())
    );
    assert_eq!(migration.automatic, Some(false));
}

#[test]
fn test_migration_timestamps() {
    let migration: Migration = common::deserialize_fixture("migration", "in_progress.json");

    assert!(migration.created_timestamp.is_some());
    assert!(migration.started_timestamp.is_some());
    assert!(migration.finished_timestamp.is_none());
    assert!(migration.duration_ms.is_none());
}

#[test]
fn test_migration_progress_history() {
    let migration: Migration = common::deserialize_fixture("migration", "in_progress.json");

    let history = migration
        .progress_history
        .expect("progress_history should be present");
    assert_eq!(history.len(), 2);

    // First entry: begin phase completed
    assert_eq!(history[0].progress_type, "progress");
    assert_eq!(history[0].phase, Some(MigrationPhase::Begin));
    assert_eq!(history[0].state, Some(MigrationState::Running));
    assert_eq!(history[0].percentage, Some(100.0));

    // Second entry: sync in progress
    assert_eq!(history[1].phase, Some(MigrationPhase::Sync));
    assert_eq!(history[1].percentage, Some(45.5));
    assert_eq!(history[1].transferred_bytes, Some(536870912));
    assert_eq!(history[1].total_bytes, Some(1073741824));
    assert_eq!(history[1].eta_ms, Some(30000));
}

#[test]
fn test_migration_completed_deserialize() {
    let migration: Migration = common::deserialize_fixture("migration", "completed.json");

    assert_eq!(migration.state, MigrationState::Successful);
    assert_eq!(migration.phase, Some(MigrationPhase::Switch));
    assert!(migration.finished_timestamp.is_some());
    assert_eq!(migration.duration_ms, Some(299999));
    assert_eq!(migration.automatic, Some(true));
    assert!(migration.error.is_none());
    assert!(migration.progress_history.is_none());
}

/// Test deserialization of all MigrationState enum variants.
#[test]
fn test_migration_state_variants() {
    let cases = [
        ("begin", MigrationState::Begin),
        ("estimate", MigrationState::Estimate),
        ("sync", MigrationState::Sync),
        ("paused", MigrationState::Paused),
        ("switch", MigrationState::Switch),
        ("aborted", MigrationState::Aborted),
        ("rollback", MigrationState::RolledBack),
        ("successful", MigrationState::Successful),
        ("failed", MigrationState::Failed),
        ("running", MigrationState::Running),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: MigrationState = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse migration state: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown migration states deserialize as Unknown.
#[test]
fn test_migration_state_unknown_variant() {
    let json = r#""preparing""#;
    let parsed: MigrationState = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, MigrationState::Unknown);
}

/// Test deserialization of all MigrationPhase enum variants.
#[test]
fn test_migration_phase_variants() {
    let cases = [
        ("begin", MigrationPhase::Begin),
        ("sync", MigrationPhase::Sync),
        ("switch", MigrationPhase::Switch),
    ];

    for (json_value, expected) in cases {
        let json = format!(r#""{}""#, json_value);
        let parsed: MigrationPhase = serde_json::from_str(&json)
            .unwrap_or_else(|_| panic!("Failed to parse migration phase: {}", json_value));
        assert_eq!(parsed, expected);
    }
}

/// Test forward compatibility: unknown migration phases deserialize as Unknown.
#[test]
fn test_migration_phase_unknown_variant() {
    let json = r#""cleanup""#;
    let parsed: MigrationPhase = serde_json::from_str(json).unwrap();
    assert_eq!(parsed, MigrationPhase::Unknown);
}

/// Test deserialization of a migration list.
#[test]
fn test_migration_list_deserialize() {
    let json = format!(
        "[{}, {}]",
        common::load_fixture("migration", "in_progress.json"),
        common::load_fixture("migration", "completed.json")
    );

    let migrations: Vec<Migration> =
        serde_json::from_str(&json).expect("Failed to parse migration list");
    assert_eq!(migrations.len(), 2);
    assert_eq!(migrations[0].state, MigrationState::Sync);
    assert_eq!(migrations[1].state, MigrationState::Successful);
}

/// Test round-trip serialization/deserialization preserves data.
#[test]
fn test_migration_round_trip() {
    let original: Migration = common::deserialize_fixture("migration", "in_progress.json");
    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: Migration = serde_json::from_str(&serialized).unwrap();

    assert_eq!(original.vm_uuid, deserialized.vm_uuid);
    assert_eq!(original.state, deserialized.state);
    assert_eq!(original.phase, deserialized.phase);
    assert_eq!(original.source_server_uuid, deserialized.source_server_uuid);
    assert_eq!(original.automatic, deserialized.automatic);
}
