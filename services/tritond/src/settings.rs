// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Resolving the effective [`Settings`] the daemon runs with.
//!
//! `tritond` reads cluster-wide [`Settings`] from FoundationDB at
//! startup, then layers any legacy `TRITOND_*` environment variables
//! on top — precedence is **env > FDB > built-in default** — so an
//! operator keeps a boot-time escape hatch even when FDB holds a bad
//! value. Most settings are applied once at startup and need a
//! restart; the exceptions are re-read live per placement pick (see
//! [`ConfigKey::restart_required`]).

use tritond_store::{ConfigKey, MetricsBackend, Settings};

/// Layer legacy env-var overrides on top of the FDB-stored [`Settings`].
pub fn resolve_settings(stored: Settings) -> Settings {
    apply_env_overrides(stored, |name| std::env::var(name).ok())
}

/// The legacy env var currently overriding `key` at boot — set in the
/// process environment and non-empty — if any. Drives the
/// `env_override` field of a `ConfigEntry`.
pub fn env_override_for(key: ConfigKey) -> Option<&'static str> {
    key.env_var()
        .filter(|name| std::env::var(name).is_ok_and(|v| !v.trim().is_empty()))
}

/// Every [`ConfigKey`] whose legacy env var is currently shadowing its
/// stored value at boot. Used by `tcadm config list` / the admin
/// console to flag overridden keys.
pub fn active_env_overrides() -> Vec<ConfigKey> {
    ConfigKey::ALL
        .into_iter()
        .filter(|k| env_override_for(*k).is_some())
        .collect()
}

fn apply_env_overrides(mut s: Settings, env: impl Fn(&str) -> Option<String>) -> Settings {
    let flag = |name: &str| -> Option<bool> {
        match env(name).as_deref().map(str::trim) {
            Some("1" | "true" | "True" | "TRUE") => Some(true),
            Some("0" | "false" | "False" | "FALSE") => Some(false),
            _ => None,
        }
    };
    let u64v = |name: &str| -> Option<u64> { env(name).and_then(|v| v.trim().parse().ok()) };
    let strv = |name: &str| -> Option<String> {
        env(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    if let Some(v) = flag(env_str_key(ConfigKey::ProvisionerInprocessDisabled)) {
        s.provisioner_inprocess_disabled = v;
    }
    if let Some(v) = u64v(env_str_key(ConfigKey::SweeperIntervalSecs)) {
        s.sweeper_interval_secs = v;
    }
    if let Some(v) = u64v(env_str_key(ConfigKey::StaleClaimThresholdSecs)) {
        s.stale_claim_threshold_secs = v;
    }
    if let Some(v) = u64v(env_str_key(ConfigKey::DhcpReconcileIntervalSecs)) {
        s.dhcp_reconcile_interval_secs = v;
    }
    if let Some(v) = u64v(env_str_key(ConfigKey::DhcpLeaseGcThresholdSecs)) {
        s.dhcp_lease_gc_threshold_secs = v;
    }
    if let Some(raw) = strv(env_str_key(ConfigKey::MetricsBackend)) {
        // Mirrors the pre-config behaviour: only "clickhouse" was ever
        // recognised; anything else means the in-memory ring buffer.
        s.metrics_backend = if raw.eq_ignore_ascii_case("clickhouse") {
            MetricsBackend::Clickhouse
        } else {
            MetricsBackend::Memory
        };
    }
    if let Some(url) = strv(env_str_key(ConfigKey::MetricsClickhouseUrl)) {
        s.metrics_clickhouse_url = Some(url);
    }
    if let Some(v) = flag(env_str_key(ConfigKey::ImdsEnabledDefault)) {
        s.imds_enabled_default = v;
    }
    if let Some(v) = u64v(env_str_key(ConfigKey::ImdsHopLimitDefault)) {
        s.imds_hop_limit_default = v;
    }
    if let Some(v) = u64v(env_str_key(ConfigKey::SagaRetentionSecs)) {
        s.saga_retention_secs = v;
    }
    if let Some(v) = u64v(env_str_key(
        ConfigKey::PlacementLoadMaterializerIntervalSecs,
    )) {
        s.placement_load_materializer_interval_secs = v;
    }
    if let Some(v) = u64v(env_str_key(
        ConfigKey::PlacementLoadMaterializerStalenessTicks,
    )) {
        s.placement_load_materializer_staleness_ticks = v;
    }
    if let Some(url) = strv(env_str_key(
        ConfigKey::PlacementLoadMaterializerClickhouseUrl,
    )) {
        s.placement_load_materializer_clickhouse_url = Some(url);
    }
    s
}

fn env_str_key(key: ConfigKey) -> &'static str {
    key.env_var()
        .expect("every current ConfigKey has a legacy env override")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |name| map.get(name).map(|v| v.to_string())
    }

    #[test]
    fn no_env_keeps_stored() {
        let mut stored = Settings::default();
        stored.sweeper_interval_secs = 123;
        stored.metrics_backend = MetricsBackend::Clickhouse;
        let got = apply_env_overrides(stored.clone(), env_from(&[]));
        assert_eq!(got, stored);
    }

    #[test]
    fn env_overrides_each_field() {
        let stored = Settings::default();
        let got = apply_env_overrides(
            stored,
            env_from(&[
                ("TRITOND_DISABLE_INPROCESS_PROVISIONER", "1"),
                ("TRITOND_SWEEPER_INTERVAL_SECS", "11"),
                ("TRITOND_STALE_CLAIM_THRESHOLD_SECS", "22"),
                ("TRITOND_DHCP_RECONCILE_INTERVAL_SECS", "33"),
                ("TRITOND_DHCP_LEASE_GC_THRESHOLD_SECS", "44"),
                ("TRITOND_METRICS_STORE", "clickhouse"),
                ("TRITOND_METRICS_CLICKHOUSE_URL", "http://ch:8123"),
            ]),
        );
        assert!(got.provisioner_inprocess_disabled);
        assert_eq!(got.sweeper_interval_secs, 11);
        assert_eq!(got.stale_claim_threshold_secs, 22);
        assert_eq!(got.dhcp_reconcile_interval_secs, 33);
        assert_eq!(got.dhcp_lease_gc_threshold_secs, 44);
        assert_eq!(got.metrics_backend, MetricsBackend::Clickhouse);
        assert_eq!(
            got.metrics_clickhouse_url.as_deref(),
            Some("http://ch:8123")
        );
    }

    #[test]
    fn garbage_numeric_env_does_not_override() {
        let mut stored = Settings::default();
        stored.sweeper_interval_secs = 999;
        let got = apply_env_overrides(
            stored,
            env_from(&[("TRITOND_SWEEPER_INTERVAL_SECS", "not-a-number")]),
        );
        assert_eq!(got.sweeper_interval_secs, 999);
    }

    #[test]
    fn metrics_store_non_clickhouse_means_memory() {
        let mut stored = Settings::default();
        stored.metrics_backend = MetricsBackend::Clickhouse;
        let got = apply_env_overrides(stored, env_from(&[("TRITOND_METRICS_STORE", "memory")]));
        assert_eq!(got.metrics_backend, MetricsBackend::Memory);
    }

    #[test]
    fn flag_false_disables_override() {
        let mut stored = Settings::default();
        stored.provisioner_inprocess_disabled = true;
        let got = apply_env_overrides(
            stored,
            env_from(&[("TRITOND_DISABLE_INPROCESS_PROVISIONER", "false")]),
        );
        assert!(!got.provisioner_inprocess_disabled);
    }
}
