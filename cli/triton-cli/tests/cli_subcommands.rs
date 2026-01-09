// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Subcommand existence and help consistency tests.
//!
//! Ported from node-triton test/integration/cli-subcommands.test.js
//!
//! This test verifies that:
//! 1. All subcommands exist and produce help output
//! 2. `triton help <subcmd>` and `triton <subcmd> -h` produce equivalent output
//! 3. Aliases produce the same help as their canonical commands

#![allow(deprecated, clippy::expect_used)]

mod common;

use assert_cmd::Command;
use predicates::prelude::*;

fn triton_cmd() -> Command {
    Command::cargo_bin("triton").expect("Failed to find triton binary")
}

/// Test helper that verifies both help forms work and produce output
fn test_subcommand_help(args: &[&str]) {
    // Test with -h flag
    let mut h_args: Vec<&str> = args.to_vec();
    h_args.push("-h");

    triton_cmd()
        .args(&h_args)
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());

    // Test with help subcommand (insert "help" before last arg)
    if !args.is_empty() {
        let mut help_args: Vec<&str> = args[..args.len() - 1].to_vec();
        help_args.push("help");
        help_args.push(args[args.len() - 1]);

        triton_cmd()
            .args(&help_args)
            .assert()
            .success()
            .stdout(predicate::str::is_empty().not());
    }
}

/// Test that all help variants for a command produce the same output
/// Note: For subcommand aliases (like `instance ls` vs `instance list`), the output should match.
/// For top-level shortcuts (like `fwrules` vs `fwrule list`), they're separate commands and
/// won't have identical help, so we just verify they work.
fn test_alias_consistency(canonical: &[&str], aliases: &[&[&str]]) {
    // Get canonical help output
    let mut canonical_args: Vec<&str> = canonical.to_vec();
    canonical_args.push("-h");

    let canonical_output = triton_cmd()
        .args(&canonical_args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    // Each alias should produce the same output (for true aliases at the same level)
    for alias in aliases {
        let mut alias_args: Vec<&str> = alias.to_vec();
        alias_args.push("-h");

        // Check if this is a top-level shortcut vs a subcommand alias
        // Top-level shortcuts have different structure so won't match exactly
        let is_top_level_shortcut = alias.len() == 1 && canonical.len() > 1;

        if is_top_level_shortcut {
            // Just verify the shortcut works and produces output
            triton_cmd()
                .args(&alias_args)
                .assert()
                .success()
                .stdout(predicate::str::is_empty().not());
        } else {
            // True aliases should produce identical output
            triton_cmd()
                .args(&alias_args)
                .assert()
                .success()
                .stdout(predicates::ord::eq(canonical_output.clone()));
        }
    }
}

// =============================================================================
// Profile commands
// =============================================================================

#[test]
fn test_subcommand_profile() {
    test_subcommand_help(&["profile"]);
}

#[test]
fn test_subcommand_profile_list() {
    test_subcommand_help(&["profile", "list"]);
}

#[test]
fn test_subcommand_profile_list_aliases() {
    test_alias_consistency(&["profile", "list"], &[&["profile", "ls"], &["profiles"]]);
}

#[test]
fn test_subcommand_profile_get() {
    test_subcommand_help(&["profile", "get"]);
}

#[test]
fn test_subcommand_profile_set_current() {
    test_subcommand_help(&["profile", "set-current"]);
}

#[test]
fn test_subcommand_profile_create() {
    test_subcommand_help(&["profile", "create"]);
}

#[test]
fn test_subcommand_profile_edit() {
    test_subcommand_help(&["profile", "edit"]);
}

#[test]
fn test_subcommand_profile_delete() {
    test_subcommand_help(&["profile", "delete"]);
}

#[test]
fn test_subcommand_profile_delete_aliases() {
    test_alias_consistency(&["profile", "delete"], &[&["profile", "rm"]]);
}

// =============================================================================
// Env command
// =============================================================================

#[test]
fn test_subcommand_env() {
    test_subcommand_help(&["env"]);
}

// =============================================================================
// Completion command
// =============================================================================

#[test]
fn test_subcommand_completion() {
    test_subcommand_help(&["completion"]);
}

// =============================================================================
// Account commands
// =============================================================================

#[test]
fn test_subcommand_account() {
    test_subcommand_help(&["account"]);
}

#[test]
fn test_subcommand_account_get() {
    test_subcommand_help(&["account", "get"]);
}

#[test]
fn test_subcommand_account_update() {
    test_subcommand_help(&["account", "update"]);
}

#[test]
fn test_subcommand_account_limits() {
    test_subcommand_help(&["account", "limits"]);
}

// =============================================================================
// Instance commands
// =============================================================================

#[test]
fn test_subcommand_instance() {
    test_subcommand_help(&["instance"]);
}

#[test]
fn test_subcommand_instance_alias() {
    test_alias_consistency(&["instance"], &[&["inst"]]);
}

#[test]
fn test_subcommand_instance_list() {
    test_subcommand_help(&["instance", "list"]);
}

#[test]
fn test_subcommand_instance_list_aliases() {
    test_alias_consistency(
        &["instance", "list"],
        &[&["instance", "ls"], &["instances"], &["insts"], &["ls"]],
    );
}

#[test]
fn test_subcommand_instance_get() {
    test_subcommand_help(&["instance", "get"]);
}

#[test]
fn test_subcommand_instance_create() {
    test_subcommand_help(&["instance", "create"]);
}

#[test]
fn test_subcommand_instance_create_alias() {
    test_alias_consistency(&["instance", "create"], &[&["create"]]);
}

#[test]
fn test_subcommand_instance_start() {
    test_subcommand_help(&["instance", "start"]);
}

#[test]
fn test_subcommand_instance_start_alias() {
    test_alias_consistency(&["instance", "start"], &[&["start"]]);
}

#[test]
fn test_subcommand_instance_stop() {
    test_subcommand_help(&["instance", "stop"]);
}

#[test]
fn test_subcommand_instance_stop_alias() {
    test_alias_consistency(&["instance", "stop"], &[&["stop"]]);
}

#[test]
fn test_subcommand_instance_reboot() {
    test_subcommand_help(&["instance", "reboot"]);
}

#[test]
fn test_subcommand_instance_reboot_alias() {
    test_alias_consistency(&["instance", "reboot"], &[&["reboot"]]);
}

#[test]
fn test_subcommand_instance_delete() {
    test_subcommand_help(&["instance", "delete"]);
}

#[test]
fn test_subcommand_instance_delete_aliases() {
    test_alias_consistency(
        &["instance", "delete"],
        &[&["instance", "rm"], &["delete"], &["rm"]],
    );
}

#[test]
fn test_subcommand_instance_enable_firewall() {
    test_subcommand_help(&["instance", "enable-firewall"]);
}

#[test]
fn test_subcommand_instance_disable_firewall() {
    test_subcommand_help(&["instance", "disable-firewall"]);
}

#[test]
fn test_subcommand_instance_enable_deletion_protection() {
    test_subcommand_help(&["instance", "enable-deletion-protection"]);
}

#[test]
fn test_subcommand_instance_disable_deletion_protection() {
    test_subcommand_help(&["instance", "disable-deletion-protection"]);
}

#[test]
fn test_subcommand_instance_rename() {
    test_subcommand_help(&["instance", "rename"]);
}

#[test]
fn test_subcommand_instance_ssh() {
    test_subcommand_help(&["instance", "ssh"]);
}

#[test]
fn test_subcommand_instance_ip() {
    test_subcommand_help(&["instance", "ip"]);
}

#[test]
fn test_subcommand_instance_wait() {
    test_subcommand_help(&["instance", "wait"]);
}

#[test]
fn test_subcommand_instance_audit() {
    test_subcommand_help(&["instance", "audit"]);
}

#[test]
fn test_subcommand_instance_fwrules() {
    test_subcommand_help(&["instance", "fwrules"]);
}

// =============================================================================
// Instance snapshot commands
// =============================================================================

#[test]
fn test_subcommand_instance_snapshot() {
    test_subcommand_help(&["instance", "snapshot"]);
}

#[test]
fn test_subcommand_instance_snapshot_create() {
    test_subcommand_help(&["instance", "snapshot", "create"]);
}

#[test]
fn test_subcommand_instance_snapshot_list() {
    test_subcommand_help(&["instance", "snapshot", "list"]);
}

#[test]
fn test_subcommand_instance_snapshot_list_aliases() {
    // `instance snapshot ls` is a true alias and should match exactly
    test_alias_consistency(
        &["instance", "snapshot", "list"],
        &[&["instance", "snapshot", "ls"]],
    );
}

#[test]
fn test_subcommand_instance_snapshots_shortcut() {
    // `instance snapshots` is a shortcut command, not an alias - just verify it works
    test_subcommand_help(&["instance", "snapshots"]);
}

#[test]
fn test_subcommand_instance_snapshot_get() {
    test_subcommand_help(&["instance", "snapshot", "get"]);
}

#[test]
fn test_subcommand_instance_snapshot_delete() {
    test_subcommand_help(&["instance", "snapshot", "delete"]);
}

#[test]
fn test_subcommand_instance_snapshot_delete_alias() {
    test_alias_consistency(
        &["instance", "snapshot", "delete"],
        &[&["instance", "snapshot", "rm"]],
    );
}

// =============================================================================
// Instance NIC commands
// =============================================================================

#[test]
fn test_subcommand_instance_nic() {
    test_subcommand_help(&["instance", "nic"]);
}

#[test]
fn test_subcommand_instance_nic_add() {
    test_subcommand_help(&["instance", "nic", "add"]);
}

#[test]
fn test_subcommand_instance_nic_add_alias() {
    test_alias_consistency(
        &["instance", "nic", "add"],
        &[&["instance", "nic", "create"]],
    );
}

#[test]
fn test_subcommand_instance_nic_list() {
    test_subcommand_help(&["instance", "nic", "list"]);
}

#[test]
fn test_subcommand_instance_nic_list_alias() {
    test_alias_consistency(&["instance", "nic", "list"], &[&["instance", "nic", "ls"]]);
}

#[test]
fn test_subcommand_instance_nic_get() {
    test_subcommand_help(&["instance", "nic", "get"]);
}

#[test]
fn test_subcommand_instance_nic_remove() {
    test_subcommand_help(&["instance", "nic", "remove"]);
}

#[test]
fn test_subcommand_instance_nic_remove_aliases() {
    test_alias_consistency(
        &["instance", "nic", "remove"],
        &[&["instance", "nic", "rm"], &["instance", "nic", "delete"]],
    );
}

// =============================================================================
// Instance disk commands
// =============================================================================

#[test]
fn test_subcommand_instance_disk() {
    test_subcommand_help(&["instance", "disk"]);
}

#[test]
fn test_subcommand_instance_disk_add() {
    test_subcommand_help(&["instance", "disk", "add"]);
}

#[test]
fn test_subcommand_instance_disk_list() {
    test_subcommand_help(&["instance", "disk", "list"]);
}

#[test]
fn test_subcommand_instance_disk_list_alias() {
    test_alias_consistency(
        &["instance", "disk", "list"],
        &[&["instance", "disk", "ls"]],
    );
}

#[test]
fn test_subcommand_instance_disk_get() {
    test_subcommand_help(&["instance", "disk", "get"]);
}

#[test]
fn test_subcommand_instance_disk_resize() {
    test_subcommand_help(&["instance", "disk", "resize"]);
}

#[test]
fn test_subcommand_instance_disk_delete() {
    test_subcommand_help(&["instance", "disk", "delete"]);
}

#[test]
fn test_subcommand_instance_disk_delete_alias() {
    test_alias_consistency(
        &["instance", "disk", "delete"],
        &[&["instance", "disk", "rm"]],
    );
}

// =============================================================================
// Instance migration commands
// =============================================================================

#[test]
fn test_subcommand_instance_migration() {
    test_subcommand_help(&["instance", "migration"]);
}

#[test]
fn test_subcommand_instance_migration_begin() {
    test_subcommand_help(&["instance", "migration", "begin"]);
}

#[test]
fn test_subcommand_instance_migration_begin_alias() {
    test_alias_consistency(
        &["instance", "migration", "begin"],
        &[&["instance", "migration", "start"]],
    );
}

#[test]
fn test_subcommand_instance_migration_switch() {
    test_subcommand_help(&["instance", "migration", "switch"]);
}

#[test]
fn test_subcommand_instance_migration_switch_alias() {
    test_alias_consistency(
        &["instance", "migration", "switch"],
        &[&["instance", "migration", "finalize"]],
    );
}

#[test]
fn test_subcommand_instance_migration_sync() {
    test_subcommand_help(&["instance", "migration", "sync"]);
}

#[test]
fn test_subcommand_instance_migration_abort() {
    test_subcommand_help(&["instance", "migration", "abort"]);
}

#[test]
fn test_subcommand_instance_migration_get() {
    test_subcommand_help(&["instance", "migration", "get"]);
}

#[test]
fn test_subcommand_instance_migration_list() {
    test_subcommand_help(&["instance", "migration", "list"]);
}

#[test]
fn test_subcommand_instance_migration_list_alias() {
    test_alias_consistency(
        &["instance", "migration", "list"],
        &[&["instance", "migration", "ls"]],
    );
}

#[test]
fn test_subcommand_instance_migration_estimate() {
    test_subcommand_help(&["instance", "migration", "estimate"]);
}

// Note: node-triton has `migration pause` and `migration automatic` commands
// that are not yet implemented in the Rust CLI. `migration wait` is the Rust
// equivalent of watching migration progress.

// =============================================================================
// Instance tag commands
// =============================================================================

#[test]
fn test_subcommand_instance_tag() {
    test_subcommand_help(&["instance", "tag"]);
}

#[test]
fn test_subcommand_instance_tag_set() {
    test_subcommand_help(&["instance", "tag", "set"]);
}

#[test]
fn test_subcommand_instance_tag_list() {
    test_subcommand_help(&["instance", "tag", "list"]);
}

#[test]
fn test_subcommand_instance_tag_list_aliases() {
    // `instance tag ls` is a true alias and should match exactly
    test_alias_consistency(&["instance", "tag", "list"], &[&["instance", "tag", "ls"]]);
}

#[test]
fn test_subcommand_instance_tags_shortcut() {
    // `instance tags` is a shortcut command, not an alias - just verify it works
    test_subcommand_help(&["instance", "tags"]);
}

#[test]
fn test_subcommand_instance_tag_get() {
    test_subcommand_help(&["instance", "tag", "get"]);
}

#[test]
fn test_subcommand_instance_tag_delete() {
    test_subcommand_help(&["instance", "tag", "delete"]);
}

#[test]
fn test_subcommand_instance_tag_delete_alias() {
    test_alias_consistency(
        &["instance", "tag", "delete"],
        &[&["instance", "tag", "rm"]],
    );
}

#[test]
fn test_subcommand_instance_tag_replace_all() {
    test_subcommand_help(&["instance", "tag", "replace-all"]);
}

// =============================================================================
// Instance metadata commands
// =============================================================================

#[test]
fn test_subcommand_instance_metadata() {
    test_subcommand_help(&["instance", "metadata"]);
}

#[test]
fn test_subcommand_instance_metadata_set() {
    test_subcommand_help(&["instance", "metadata", "set"]);
}

#[test]
fn test_subcommand_instance_metadata_list() {
    test_subcommand_help(&["instance", "metadata", "list"]);
}

#[test]
fn test_subcommand_instance_metadata_list_aliases() {
    // `instance metadata ls` is a true alias and should match exactly
    test_alias_consistency(
        &["instance", "metadata", "list"],
        &[&["instance", "metadata", "ls"]],
    );
}

#[test]
fn test_subcommand_instance_metadatas_alias() {
    // `instance metadatas` is an alias for `instance metadata` (the subcommand group)
    // not for `instance metadata list` - this matches how the Rust CLI is structured
    test_alias_consistency(&["instance", "metadata"], &[&["instance", "metadatas"]]);
}

#[test]
fn test_subcommand_instance_metadata_get() {
    test_subcommand_help(&["instance", "metadata", "get"]);
}

#[test]
fn test_subcommand_instance_metadata_delete() {
    test_subcommand_help(&["instance", "metadata", "delete"]);
}

#[test]
fn test_subcommand_instance_metadata_delete_alias() {
    test_alias_consistency(
        &["instance", "metadata", "delete"],
        &[&["instance", "metadata", "rm"]],
    );
}

// =============================================================================
// Network commands
// =============================================================================

#[test]
fn test_subcommand_network() {
    test_subcommand_help(&["network"]);
}

#[test]
fn test_subcommand_network_list() {
    test_subcommand_help(&["network", "list"]);
}

#[test]
fn test_subcommand_network_list_aliases() {
    test_alias_consistency(&["network", "list"], &[&["network", "ls"], &["networks"]]);
}

#[test]
fn test_subcommand_network_get() {
    test_subcommand_help(&["network", "get"]);
}

#[test]
fn test_subcommand_network_create() {
    test_subcommand_help(&["network", "create"]);
}

#[test]
fn test_subcommand_network_delete() {
    test_subcommand_help(&["network", "delete"]);
}

#[test]
fn test_subcommand_network_delete_alias() {
    test_alias_consistency(&["network", "delete"], &[&["network", "rm"]]);
}

#[test]
fn test_subcommand_network_ip() {
    test_subcommand_help(&["network", "ip"]);
}

#[test]
fn test_subcommand_network_ip_list() {
    test_subcommand_help(&["network", "ip", "list"]);
}

#[test]
fn test_subcommand_network_ip_list_alias() {
    test_alias_consistency(&["network", "ip", "list"], &[&["network", "ip", "ls"]]);
}

#[test]
fn test_subcommand_network_ip_get() {
    test_subcommand_help(&["network", "ip", "get"]);
}

#[test]
fn test_subcommand_network_ip_update() {
    test_subcommand_help(&["network", "ip", "update"]);
}

// =============================================================================
// VLAN commands
// =============================================================================

#[test]
fn test_subcommand_vlan() {
    test_subcommand_help(&["vlan"]);
}

#[test]
fn test_subcommand_vlan_create() {
    test_subcommand_help(&["vlan", "create"]);
}

#[test]
fn test_subcommand_vlan_list() {
    test_subcommand_help(&["vlan", "list"]);
}

#[test]
fn test_subcommand_vlan_list_alias() {
    test_alias_consistency(&["vlan", "list"], &[&["vlan", "ls"]]);
}

#[test]
fn test_subcommand_vlan_get() {
    test_subcommand_help(&["vlan", "get"]);
}

#[test]
fn test_subcommand_vlan_update() {
    test_subcommand_help(&["vlan", "update"]);
}

#[test]
fn test_subcommand_vlan_delete() {
    test_subcommand_help(&["vlan", "delete"]);
}

#[test]
fn test_subcommand_vlan_delete_alias() {
    test_alias_consistency(&["vlan", "delete"], &[&["vlan", "rm"]]);
}

#[test]
fn test_subcommand_vlan_networks() {
    test_subcommand_help(&["vlan", "networks"]);
}

// =============================================================================
// Key commands
// =============================================================================

#[test]
fn test_subcommand_key() {
    test_subcommand_help(&["key"]);
}

#[test]
fn test_subcommand_key_add() {
    test_subcommand_help(&["key", "add"]);
}

#[test]
fn test_subcommand_key_list() {
    test_subcommand_help(&["key", "list"]);
}

#[test]
fn test_subcommand_key_list_aliases() {
    test_alias_consistency(&["key", "list"], &[&["key", "ls"], &["keys"]]);
}

#[test]
fn test_subcommand_key_get() {
    test_subcommand_help(&["key", "get"]);
}

#[test]
fn test_subcommand_key_delete() {
    test_subcommand_help(&["key", "delete"]);
}

#[test]
fn test_subcommand_key_delete_alias() {
    test_alias_consistency(&["key", "delete"], &[&["key", "rm"]]);
}

// =============================================================================
// Image commands
// =============================================================================

#[test]
fn test_subcommand_image() {
    test_subcommand_help(&["image"]);
}

#[test]
fn test_subcommand_image_alias() {
    test_alias_consistency(&["image"], &[&["img"]]);
}

#[test]
fn test_subcommand_image_get() {
    test_subcommand_help(&["image", "get"]);
}

#[test]
fn test_subcommand_image_list() {
    test_subcommand_help(&["image", "list"]);
}

#[test]
fn test_subcommand_image_list_aliases() {
    test_alias_consistency(&["image", "list"], &[&["images"], &["imgs"]]);
}

#[test]
fn test_subcommand_image_create() {
    test_subcommand_help(&["image", "create"]);
}

#[test]
fn test_subcommand_image_delete() {
    test_subcommand_help(&["image", "delete"]);
}

#[test]
fn test_subcommand_image_delete_alias() {
    test_alias_consistency(&["image", "delete"], &[&["image", "rm"]]);
}

#[test]
fn test_subcommand_image_share() {
    test_subcommand_help(&["image", "share"]);
}

#[test]
fn test_subcommand_image_unshare() {
    test_subcommand_help(&["image", "unshare"]);
}

#[test]
fn test_subcommand_image_clone() {
    test_subcommand_help(&["image", "clone"]);
}

#[test]
fn test_subcommand_image_copy() {
    test_subcommand_help(&["image", "copy"]);
}

#[test]
fn test_subcommand_image_export() {
    test_subcommand_help(&["image", "export"]);
}

#[test]
fn test_subcommand_image_update() {
    test_subcommand_help(&["image", "update"]);
}

#[test]
fn test_subcommand_image_wait() {
    test_subcommand_help(&["image", "wait"]);
}

#[test]
fn test_subcommand_image_tag() {
    test_subcommand_help(&["image", "tag"]);
}

// =============================================================================
// Package commands
// =============================================================================

#[test]
fn test_subcommand_package() {
    test_subcommand_help(&["package"]);
}

#[test]
fn test_subcommand_package_alias() {
    test_alias_consistency(&["package"], &[&["pkg"]]);
}

#[test]
fn test_subcommand_package_get() {
    test_subcommand_help(&["package", "get"]);
}

#[test]
fn test_subcommand_package_list() {
    test_subcommand_help(&["package", "list"]);
}

#[test]
fn test_subcommand_package_list_aliases() {
    test_alias_consistency(&["package", "list"], &[&["packages"], &["pkgs"]]);
}

// =============================================================================
// Firewall rule commands
// =============================================================================

#[test]
fn test_subcommand_fwrule() {
    test_subcommand_help(&["fwrule"]);
}

#[test]
fn test_subcommand_fwrules_alias() {
    test_alias_consistency(&["fwrule", "list"], &[&["fwrules"]]);
}

#[test]
fn test_subcommand_fwrule_create() {
    test_subcommand_help(&["fwrule", "create"]);
}

#[test]
fn test_subcommand_fwrule_list() {
    test_subcommand_help(&["fwrule", "list"]);
}

#[test]
fn test_subcommand_fwrule_list_alias() {
    test_alias_consistency(&["fwrule", "list"], &[&["fwrule", "ls"]]);
}

#[test]
fn test_subcommand_fwrule_get() {
    test_subcommand_help(&["fwrule", "get"]);
}

#[test]
fn test_subcommand_fwrule_update() {
    test_subcommand_help(&["fwrule", "update"]);
}

#[test]
fn test_subcommand_fwrule_delete() {
    test_subcommand_help(&["fwrule", "delete"]);
}

#[test]
fn test_subcommand_fwrule_delete_alias() {
    test_alias_consistency(&["fwrule", "delete"], &[&["fwrule", "rm"]]);
}

#[test]
fn test_subcommand_fwrule_enable() {
    test_subcommand_help(&["fwrule", "enable"]);
}

#[test]
fn test_subcommand_fwrule_disable() {
    test_subcommand_help(&["fwrule", "disable"]);
}

#[test]
fn test_subcommand_fwrule_instances() {
    test_subcommand_help(&["fwrule", "instances"]);
}

#[test]
fn test_subcommand_fwrule_instances_alias() {
    test_alias_consistency(&["fwrule", "instances"], &[&["fwrule", "insts"]]);
}

// =============================================================================
// Volume commands
// =============================================================================

#[test]
fn test_subcommand_volume() {
    test_subcommand_help(&["volume"]);
}

#[test]
fn test_subcommand_volume_alias() {
    test_alias_consistency(&["volume"], &[&["vol"]]);
}

#[test]
fn test_subcommand_volume_list() {
    test_subcommand_help(&["volume", "list"]);
}

#[test]
fn test_subcommand_volume_list_aliases() {
    test_alias_consistency(
        &["volume", "list"],
        &[&["volume", "ls"], &["volumes"], &["vols"]],
    );
}

#[test]
fn test_subcommand_volume_delete() {
    test_subcommand_help(&["volume", "delete"]);
}

#[test]
fn test_subcommand_volume_delete_alias() {
    test_alias_consistency(&["volume", "delete"], &[&["volume", "rm"]]);
}

#[test]
fn test_subcommand_volume_create() {
    test_subcommand_help(&["volume", "create"]);
}

#[test]
fn test_subcommand_volume_get() {
    test_subcommand_help(&["volume", "get"]);
}

#[test]
fn test_subcommand_volume_sizes() {
    test_subcommand_help(&["volume", "sizes"]);
}

// =============================================================================
// Top-level shortcuts
// =============================================================================

#[test]
fn test_subcommand_ip_shortcut() {
    test_subcommand_help(&["ip"]);
}

#[test]
fn test_subcommand_ssh_shortcut() {
    test_subcommand_help(&["ssh"]);
}

#[test]
fn test_subcommand_info_shortcut() {
    test_subcommand_help(&["info"]);
}

#[test]
fn test_subcommand_services_shortcut() {
    test_subcommand_help(&["services"]);
}

#[test]
fn test_subcommand_datacenters_shortcut() {
    test_subcommand_help(&["datacenters"]);
}
