// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let repo_root = manifest_dir.join("../..");
    let talos_api_dir = repo_root.join("talos/api");
    let talos_vendor_dir = talos_api_dir.join("vendor");
    let machine_proto = talos_api_dir.join("machine/machine.proto");
    let cluster_proto = talos_api_dir.join("cluster/cluster.proto");

    // Build metadata
    build_data::set_GIT_COMMIT_SHORT();

    // Check git dirty status directly and emit a human-friendly suffix
    let dirty = build_data::get_git_dirty().unwrap_or(false);
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=GIT_DIRTY_SUFFIX={suffix}");

    // Re-run build.rs when git state changes (commits, branch switches)
    // so the version string stays accurate.
    build_data::rerun_if_git_commit_or_branch_changed().ok();

    build_data::no_debug_rebuilds();

    // Compile Talos protocol buffers
    println!("cargo:rerun-if-changed={}", machine_proto.display());
    println!("cargo:rerun-if-changed={}", cluster_proto.display());
    println!("cargo:rerun-if-changed={}", talos_api_dir.display());
    println!("cargo:rerun-if-changed={}", talos_vendor_dir.display());

    tonic_build::configure()
        .build_server(false)
        .compile_protos(
            &[machine_proto, cluster_proto],
            &[talos_api_dir, talos_vendor_dir],
        )?;

    Ok(())
}
