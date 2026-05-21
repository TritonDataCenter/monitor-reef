// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Round-trip and schema-version tests for the channel manifest.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use triton_channel::{CURRENT_SCHEMA, ParseError, parse_channel};

const EXAMPLE: &[u8] = include_bytes!("fixtures/example_channel.json");

#[test]
fn example_parses_with_all_expected_entries() {
    let channel = parse_channel(EXAMPLE).expect("example channel manifest should parse");

    assert_eq!(channel.channel, "edge");
    assert_eq!(channel.schema, CURRENT_SCHEMA);
    assert_eq!(channel.publisher, "nick.wilkens@mnxsolutions.com");

    // Image entries: triton-tritond and triton-fdb in the fixture.
    let tritond = channel
        .images
        .get("triton-tritond")
        .expect("triton-tritond image present");
    assert_eq!(tritond.stamp, "20260521T140000Z");
    assert_eq!(tritond.data_format_version, 1);
    assert_eq!(tritond.data_format_min_read, 1);
    assert_eq!(tritond.pi_min.as_deref(), Some("20260518T184011Z"));

    let fdb = channel
        .images
        .get("triton-fdb")
        .expect("triton-fdb image present");
    // The fdb fixture intentionally omits pi_min to exercise the
    // serde default.
    assert!(fdb.pi_min.is_none());
    assert_eq!(fdb.data_format_version, 730);

    // Agent entries.
    assert!(channel.agents.contains_key("tritonagent"));
    assert!(channel.agents.contains_key("proteusadm"));

    // tcadm entries are keyed by target triple. Old clients tolerate
    // unknown triples by simply finding no entry for their own.
    assert!(channel.tcadm.contains_key("x86_64-unknown-illumos"));
    assert!(channel.tcadm.contains_key("aarch64-apple-darwin"));
}

#[test]
fn parsed_manifest_round_trips_through_serde() {
    let channel = parse_channel(EXAMPLE).expect("parse");
    let re_serialized = serde_json::to_vec(&channel).expect("serialize");
    let re_parsed = parse_channel(&re_serialized).expect("re-parse");
    assert_eq!(channel, re_parsed);
}

#[test]
fn future_schema_is_rejected() {
    // Take the canonical example, bump the schema field, and confirm
    // parse_channel refuses rather than silently ignoring the
    // upgrade.
    let mut value: serde_json::Value =
        serde_json::from_slice(EXAMPLE).expect("fixture is valid json");
    value["schema"] = serde_json::Value::from(CURRENT_SCHEMA + 1);
    let bytes = serde_json::to_vec(&value).expect("re-serialize");

    match parse_channel(&bytes) {
        Err(ParseError::UnsupportedSchema { found, supported }) => {
            assert_eq!(found, CURRENT_SCHEMA + 1);
            assert_eq!(supported, CURRENT_SCHEMA);
        }
        other => panic!("expected UnsupportedSchema, got {other:?}"),
    }
}

#[test]
fn malformed_json_produces_json_error() {
    let bytes = b"this is not json";
    match parse_channel(bytes) {
        Err(ParseError::Json(_)) => {}
        other => panic!("expected ParseError::Json, got {other:?}"),
    }
}

#[test]
fn empty_image_and_agent_maps_default_to_empty() {
    let minimal = br#"{
        "channel": "stable",
        "schema": 1,
        "updated_at": "2026-05-21T14:00:00Z",
        "publisher": "tester"
    }"#;
    let channel = parse_channel(minimal).expect("minimal manifest parses");
    assert!(channel.images.is_empty());
    assert!(channel.agents.is_empty());
    assert!(channel.tcadm.is_empty());
}
