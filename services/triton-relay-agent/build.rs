// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

fn main() {
    let _ = build_data::set_GIT_COMMIT_SHORT();
    let dirty = build_data::get_git_dirty().unwrap_or(false);
    let suffix = if dirty { "-dirty" } else { "" };
    println!("cargo:rustc-env=GIT_DIRTY_SUFFIX={suffix}");
    build_data::rerun_if_git_commit_or_branch_changed().ok();
    let _ = build_data::no_debug_rebuilds();
}
