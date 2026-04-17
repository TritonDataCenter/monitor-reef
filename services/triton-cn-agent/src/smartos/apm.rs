// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! APM — Ain't a Package Manager.
//!
//! A minimal Node-package-ish installer used only for Triton SDC
//! agents. Each agent ships as a tarball containing a single top-level
//! directory with a `package.json`. APM's job is:
//!
//! * Extract that tarball to `/opt/smartdc/agents/lib/node_modules/<name>`.
//! * Symlink any `bin` entries from the manifest into
//!   `/opt/smartdc/agents/bin/`.
//! * Run `preinstall`/`postinstall` lifecycle scripts with the expected
//!   `npm_config_*` environment variables.
//! * Reverse all of the above on `uninstall`.
//!
//! This module ports the subset of the legacy `lib/apm.js` that
//! `agent_install` / `agents_uninstall` call. We don't implement
//! `list`, `update`, or the CLI wrapper since they aren't part of the
//! task surface.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const DEFAULT_PREFIX: &str = "/opt/smartdc/agents";
pub const DEFAULT_TMP: &str = "/var/tmp";

#[derive(Debug, Error)]
pub enum ApmError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("spawn {program} failed: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{program} exited with {status}: {stderr}")]
    NonZeroExit {
        program: String,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error("tarball {path} did not contain a single top-level directory")]
    MalformedTarball { path: PathBuf },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{script} lifecycle script failed with exit code {code}")]
    Lifecycle { script: &'static str, code: i32 },
}

/// Filesystem layout APM operates on. Injectable so tests can stand up
/// a sandbox tree under a tempdir.
#[derive(Debug, Clone)]
pub struct ApmPaths {
    pub prefix: PathBuf,
    pub modules_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub smf_dir: PathBuf,
    pub etc_dir: PathBuf,
    pub db_dir: PathBuf,
    pub tmp_dir: PathBuf,
}

impl ApmPaths {
    pub fn production() -> Self {
        let prefix = PathBuf::from(DEFAULT_PREFIX);
        Self::from_prefix(prefix)
    }

    pub fn from_prefix(prefix: PathBuf) -> Self {
        Self {
            modules_dir: prefix.join("lib/node_modules"),
            bin_dir: prefix.join("bin"),
            smf_dir: prefix.join("smf"),
            etc_dir: prefix.join("etc"),
            db_dir: prefix.join("db"),
            tmp_dir: PathBuf::from(DEFAULT_TMP),
            prefix,
        }
    }

    pub fn with_tmp_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.tmp_dir = dir.into();
        self
    }

    /// Full path of the installed package directory for a given name.
    pub fn package_path(&self, name: &str) -> PathBuf {
        self.modules_dir.join(name)
    }

    /// Instance-UUID file path for a given agent.
    pub fn instance_uuid_path(&self, name: &str) -> PathBuf {
        self.etc_dir.join(name)
    }
}

/// Subset of the fields APM reads from `package.json`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PackageJson {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Optional map of bin-name → relative path inside the package.
    #[serde(default)]
    pub bin: Option<HashMap<String, String>>,
    /// Optional lifecycle scripts. Only `preinstall`, `postinstall`,
    /// `preuninstall`, `postuninstall` are respected.
    #[serde(default)]
    pub scripts: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct Apm {
    pub paths: ApmPaths,
    tar_bin: PathBuf,
}

impl Apm {
    pub fn production() -> Self {
        Self::new(ApmPaths::production())
    }

    pub fn new(paths: ApmPaths) -> Self {
        Self {
            paths,
            tar_bin: PathBuf::from("/usr/bin/tar"),
        }
    }

    pub fn with_tar_bin(mut self, bin: impl Into<PathBuf>) -> Self {
        self.tar_bin = bin.into();
        self
    }

    /// Install a package from a tarball path. Replaces any existing
    /// install with the same name (runs uninstall first).
    pub async fn install_tarball(&self, tarball: &Path) -> Result<PackageJson, ApmError> {
        self.ensure_layout().await?;

        // Extract to a per-install tempdir so we can parse package.json
        // before committing to the move.
        let work_tmp = self
            .paths
            .tmp_dir
            .join(format!("apm-install-{}", random_tag()));
        tokio::fs::create_dir_all(&work_tmp)
            .await
            .map_err(|source| ApmError::Io {
                path: work_tmp.clone(),
                source,
            })?;
        let extract_root = work_tmp.join("package");
        tokio::fs::create_dir_all(&extract_root)
            .await
            .map_err(|source| ApmError::Io {
                path: extract_root.clone(),
                source,
            })?;

        // Always clean up the tmp dir, success or failure.
        let result = self.install_inner(tarball, &extract_root).await;

        if let Err(e) = tokio::fs::remove_dir_all(&work_tmp).await {
            tracing::warn!(
                path = %work_tmp.display(),
                error = %e,
                "failed to clean apm tempdir"
            );
        }

        result
    }

    async fn install_inner(
        &self,
        tarball: &Path,
        extract_root: &Path,
    ) -> Result<PackageJson, ApmError> {
        self.extract_tarball(tarball, extract_root).await?;

        // Find the single top-level directory in the extracted tree.
        let mut top_dirs: Vec<PathBuf> = Vec::new();
        let mut entries =
            tokio::fs::read_dir(extract_root)
                .await
                .map_err(|source| ApmError::Io {
                    path: extract_root.to_path_buf(),
                    source,
                })?;
        while let Some(entry) = entries.next_entry().await.map_err(|source| ApmError::Io {
            path: extract_root.to_path_buf(),
            source,
        })? {
            let ft = entry.file_type().await.map_err(|source| ApmError::Io {
                path: entry.path(),
                source,
            })?;
            if ft.is_dir() {
                top_dirs.push(entry.path());
            }
        }
        if top_dirs.len() != 1 {
            return Err(ApmError::MalformedTarball {
                path: tarball.to_path_buf(),
            });
        }
        let pkg_src = &top_dirs[0];

        let package_json = read_package_json(&pkg_src.join("package.json")).await?;

        // If the same package is already installed, uninstall first so
        // lifecycle scripts see a consistent starting state.
        if self.paths.package_path(&package_json.name).exists() {
            tracing::warn!(
                name = %package_json.name,
                "package already installed; uninstalling before continuing"
            );
            let _ = self.uninstall(&package_json.name).await;
        }

        self.run_lifecycle("preinstall", &package_json, pkg_src)
            .await?;

        let dest = self.paths.package_path(&package_json.name);
        move_dir(pkg_src, &dest).await?;

        self.install_bin_symlinks(&package_json).await?;
        self.run_lifecycle("postinstall", &package_json, &dest)
            .await?;

        Ok(package_json)
    }

    /// Uninstall a package by name. No-op if the package is not installed.
    pub async fn uninstall(&self, name: &str) -> Result<(), ApmError> {
        let pkg_path = self.paths.package_path(name);
        if !pkg_path.exists() {
            tracing::warn!(name, "package does not appear to be installed");
            // Still remove the instance-uuid file in case it's stale.
            let _ = tokio::fs::remove_file(self.paths.instance_uuid_path(name)).await;
            return Ok(());
        }

        let pkg_json = read_package_json(&pkg_path.join("package.json")).await.ok();

        if let Some(pj) = &pkg_json {
            self.run_lifecycle("preuninstall", pj, &pkg_path).await?;
            self.run_lifecycle("postuninstall", pj, &pkg_path).await?;

            if let Some(bins) = &pj.bin {
                for bin_name in bins.keys() {
                    let link = self.paths.bin_dir.join(bin_name);
                    match tokio::fs::remove_file(&link).await {
                        Ok(()) => {
                            tracing::info!(link = %link.display(), "removed bin link");
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            tracing::warn!(
                                link = %link.display(),
                                error = %e,
                                "failed to remove bin link"
                            );
                        }
                    }
                }
            }
        }

        // rm -rf the package directory.
        tokio::fs::remove_dir_all(&pkg_path)
            .await
            .map_err(|source| ApmError::Io {
                path: pkg_path.clone(),
                source,
            })?;

        // Best-effort cleanup of the instance-uuid file so
        // AgentsCollector stops reporting a stale uuid.
        let _ = tokio::fs::remove_file(self.paths.instance_uuid_path(name)).await;
        Ok(())
    }

    /// Ensure the directory layout exists. APM creates every directory
    /// lazily; failure here means the caller can't even stage temp
    /// files, so we bail.
    async fn ensure_layout(&self) -> Result<(), ApmError> {
        for dir in [
            &self.paths.modules_dir,
            &self.paths.bin_dir,
            &self.paths.smf_dir,
            &self.paths.etc_dir,
            &self.paths.db_dir,
            &self.paths.tmp_dir,
        ] {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|source| ApmError::Io {
                    path: dir.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    async fn extract_tarball(&self, tarball: &Path, dest: &Path) -> Result<(), ApmError> {
        let tarball_str = tarball.display().to_string();
        let dest_str = dest.display().to_string();
        let output = tokio::process::Command::new(&self.tar_bin)
            .args(["zxf", &tarball_str, "-C", &dest_str])
            .output()
            .await
            .map_err(|source| ApmError::Spawn {
                program: self.tar_bin.display().to_string(),
                source,
            })?;
        if !output.status.success() {
            return Err(ApmError::NonZeroExit {
                program: self.tar_bin.display().to_string(),
                status: output.status,
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(())
    }

    /// Install symlinks from `bin_dir/<binName>` → relative path
    /// pointing into the package's install location, then chmod the
    /// target 0755. Matches the legacy symlink strategy.
    async fn install_bin_symlinks(&self, pkg: &PackageJson) -> Result<(), ApmError> {
        let Some(bins) = &pkg.bin else {
            return Ok(());
        };
        for (name, rel_target) in bins {
            // Target path inside the package (relative to bin_dir, one
            // level up into modules/<name>/<rel>). Legacy constructs
            // ../lib/node_modules/<pkg>/<rel>.
            let target = PathBuf::from("..")
                .join(rel_path_from_prefix(&self.paths, &pkg.name))
                .join(rel_target.trim_start_matches("./"));
            let link = self.paths.bin_dir.join(name);
            // Chmod the actual file 0755 so the symlink is executable.
            let abs_target = self.paths.bin_dir.join(&target);
            if let Ok(md) = tokio::fs::metadata(&abs_target).await {
                let mut perms = md.permissions();
                perms.set_mode(0o755);
                if let Err(e) = tokio::fs::set_permissions(&abs_target, perms).await {
                    tracing::warn!(
                        path = %abs_target.display(),
                        error = %e,
                        "failed to chmod bin target"
                    );
                }
            }
            // Remove any stale link first so symlink() doesn't fail.
            let _ = tokio::fs::remove_file(&link).await;
            tokio::fs::symlink(&target, &link)
                .await
                .map_err(|source| ApmError::Io {
                    path: link.clone(),
                    source,
                })?;
            tracing::info!(link = %link.display(), target = %target.display(), "installed bin link");
        }
        Ok(())
    }

    async fn run_lifecycle(
        &self,
        which: &'static str,
        pkg: &PackageJson,
        cwd: &Path,
    ) -> Result<(), ApmError> {
        let Some(scripts) = &pkg.scripts else {
            return Ok(());
        };
        let Some(script) = scripts.get(which) else {
            return Ok(());
        };
        if script.is_empty() {
            return Ok(());
        }
        // Legacy resolves the script path relative to the package dir.
        let script_path = cwd.join(script);
        if !tokio::fs::try_exists(&script_path).await.unwrap_or(false) {
            tracing::warn!(
                script = %script_path.display(),
                "lifecycle script file missing; skipping"
            );
            return Ok(());
        }
        let mut env = std::env::vars().collect::<HashMap<_, _>>();
        env.insert(
            "npm_config_prefix".into(),
            self.paths.prefix.display().to_string(),
        );
        env.insert(
            "npm_config_smfdir".into(),
            self.paths.smf_dir.display().to_string(),
        );
        env.insert(
            "npm_config_etc".into(),
            self.paths.etc_dir.display().to_string(),
        );
        env.insert(
            "npm_config_dbdir".into(),
            self.paths.db_dir.display().to_string(),
        );
        env.insert("npm_package_name".into(), pkg.name.clone());
        if let Some(v) = &pkg.version {
            env.insert("npm_package_version".into(), v.clone());
        }
        let output = tokio::process::Command::new(&script_path)
            .current_dir(cwd)
            .envs(&env)
            .output()
            .await
            .map_err(|source| ApmError::Spawn {
                program: script_path.display().to_string(),
                source,
            })?;
        if !output.status.success() {
            return Err(ApmError::Lifecycle {
                script: which,
                code: output.status.code().unwrap_or(-1),
            });
        }
        Ok(())
    }
}

/// Relative path from `bin_dir` to `modules_dir/<pkg_name>`.
///
/// The legacy code builds `['..', modules, pkg.name, ...].join('/')`.
/// With paths.prefix=/opt/smartdc/agents, this becomes
/// `../lib/node_modules/<pkg>`. We construct it directly rather than
/// diff-pathing because the result must be stable regardless of the
/// absolute prefix the caller uses.
fn rel_path_from_prefix(paths: &ApmPaths, pkg_name: &str) -> PathBuf {
    // modules_dir relative to bin_dir. In production:
    //   bin_dir     = prefix/bin
    //   modules_dir = prefix/lib/node_modules
    //   relative    = ../lib/node_modules
    let bin_components: Vec<_> = paths.bin_dir.components().collect();
    let modules_components: Vec<_> = paths.modules_dir.components().collect();
    let common_len = bin_components
        .iter()
        .zip(&modules_components)
        .take_while(|(a, b)| a == b)
        .count();
    let mut rel = PathBuf::new();
    for _ in common_len..bin_components.len() {
        rel.push("..");
    }
    for comp in &modules_components[common_len..] {
        rel.push(comp.as_os_str());
    }
    rel.join(pkg_name)
}

async fn read_package_json(path: &Path) -> Result<PackageJson, ApmError> {
    let bytes = tokio::fs::read(path).await.map_err(|source| ApmError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| ApmError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Rename a directory across filesystems by trying `rename` first and
/// falling back to `cp -r` + `rm -rf`. Tempfile + install dir often
/// live on the same filesystem, so rename usually wins.
async fn move_dir(src: &Path, dst: &Path) -> Result<(), ApmError> {
    if let Err(rename_err) = tokio::fs::rename(src, dst).await {
        // Cross-device move: legacy uses `mv`, which falls back to copy
        // + delete. Shell out to /usr/bin/mv for the same behavior.
        let status = tokio::process::Command::new("/usr/bin/mv")
            .arg(src)
            .arg(dst)
            .status()
            .await
            .map_err(|source| ApmError::Spawn {
                program: "/usr/bin/mv".to_string(),
                source,
            })?;
        if !status.success() {
            return Err(ApmError::Io {
                path: src.to_path_buf(),
                source: std::io::Error::other(format!(
                    "rename failed ({rename_err}), mv fallback exited {status}"
                )),
            });
        }
    }
    Ok(())
}

fn random_tag() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:08x}", now ^ std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(dir: &Path) -> ApmPaths {
        ApmPaths::from_prefix(dir.join("smartdc-agents")).with_tmp_dir(dir.join("tmp"))
    }

    #[test]
    fn rel_path_for_standard_prefix() {
        let paths = ApmPaths::production();
        let rel = rel_path_from_prefix(&paths, "net-agent");
        assert_eq!(rel, PathBuf::from("../lib/node_modules/net-agent"));
    }

    #[tokio::test]
    async fn install_and_uninstall_round_trip() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let paths = paths_in(tmp.path());

        // Build a minimal tarball: single top-level "my-agent" dir
        // containing package.json.
        let staging = tmp.path().join("staging/my-agent");
        tokio::fs::create_dir_all(&staging).await.expect("mkdir");
        tokio::fs::write(
            staging.join("package.json"),
            br#"{"name":"my-agent","version":"0.1.0"}"#,
        )
        .await
        .expect("write pkg json");
        let tarball = tmp.path().join("my-agent.tgz");
        let status = tokio::process::Command::new("/usr/bin/tar")
            .args(["zcf", tarball.to_str().unwrap(), "-C"])
            .arg(tmp.path().join("staging"))
            .arg("my-agent")
            .status()
            .await
            .expect("tar");
        assert!(status.success());

        let apm = Apm::new(paths.clone());
        let pkg = apm.install_tarball(&tarball).await.expect("install");
        assert_eq!(pkg.name, "my-agent");
        assert!(paths.package_path("my-agent").join("package.json").exists());

        apm.uninstall("my-agent").await.expect("uninstall");
        assert!(!paths.package_path("my-agent").exists());
    }
}
