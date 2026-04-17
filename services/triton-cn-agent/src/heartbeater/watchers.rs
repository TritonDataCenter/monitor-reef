// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2026 Edgecast Cloud LLC.

//! Change-of-state watchers for the heartbeater.
//!
//! The legacy `StatusReporter` keeps a "dirty" flag. On every tick of
//! `status_check_interval` (500ms) it checks the flag; if set, a new
//! status sample is queued. Three signals mark the flag dirty:
//!
//! * `/usr/vm/sbin/zoneevent -i cn-agent` — streams zone state changes.
//! * `fs.watch('/etc/zones')` — covers zone configuration edits (XML
//!   rewrites, new/deleted zones).
//! * Any change to `/tmp/.sysinfo.json` — used by a separate notifier
//!   that triggers sysinfo re-registration.
//!
//! We reproduce these with tokio-native helpers: [`ZoneeventWatcher`]
//! spawns the CLI and streams lines, [`ZoneConfigWatcher`] uses the
//! `notify` crate (which picks FEN on illumos, fsevent on macOS, inotify
//! on Linux — so tests work everywhere), and [`SysinfoFileWatcher`] does
//! the same for a single file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};
use tokio::sync::Notify;

/// Default path to the zoneevent CLI.
pub const DEFAULT_ZONEEVENT_BIN: &str = "/usr/vm/sbin/zoneevent";

/// Default path to the zones config directory.
pub const DEFAULT_ZONES_DIR: &str = "/etc/zones";

/// Default path the SmartOS platform writes sysinfo to on boot.
pub const DEFAULT_SYSINFO_PATH: &str = "/tmp/.sysinfo.json";

/// How long to wait before restarting zoneevent if it exits. Matches the
/// legacy `ZONEEVENT_RESTART_INTERVAL`.
pub const ZONEEVENT_RESTART_INTERVAL: Duration = Duration::from_secs(30);

/// Shared "status is dirty" flag + a notifier the heartbeater awaits on.
///
/// Writers call [`mark`] on any interesting event; the heartbeater's
/// status-check arm consumes the flag via [`take`] before sampling.
#[derive(Debug, Clone, Default)]
pub struct DirtyFlag {
    flag: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl DirtyFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the flag and wake any waiter. Idempotent within a tick.
    pub fn mark(&self) {
        self.flag.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Atomically read-and-clear.
    pub fn take(&self) -> bool {
        self.flag.swap(false, Ordering::AcqRel)
    }

    /// Wait for the next `mark()` call (may return even if already dirty,
    /// so the caller should `take()` to actually drain).
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

/// Zoneevent watcher: spawns the CLI, marks dirty on any stdout line,
/// restarts on exit. Holds its own task handle so callers can stop it.
#[derive(Debug)]
pub struct ZoneeventWatcher {
    handle: tokio::task::JoinHandle<()>,
    cancel: Arc<Notify>,
}

impl ZoneeventWatcher {
    /// Start watching with the default binary path.
    pub fn spawn(dirty: DirtyFlag) -> Self {
        Self::spawn_with_bin(dirty, PathBuf::from(DEFAULT_ZONEEVENT_BIN))
    }

    /// Start watching with a specific binary (for tests).
    pub fn spawn_with_bin(dirty: DirtyFlag, bin: PathBuf) -> Self {
        let cancel = Arc::new(Notify::new());
        let cancel_token = cancel.clone();
        let handle = tokio::spawn(async move {
            run_zoneevent_loop(dirty, bin, cancel_token).await;
        });
        Self { handle, cancel }
    }

    /// Stop the watcher task and wait for it to exit.
    pub async fn stop(self) {
        self.cancel.notify_one();
        let _ = self.handle.await;
    }
}

async fn run_zoneevent_loop(dirty: DirtyFlag, bin: PathBuf, cancel: Arc<Notify>) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    loop {
        let mut child = match Command::new(&bin)
            .args(["-i", "cn-agent"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(bin = %bin.display(), error = %e, "failed to spawn zoneevent; retrying");
                if wait_or_cancel(ZONEEVENT_RESTART_INTERVAL, &cancel).await {
                    return;
                }
                continue;
            }
        };

        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => {
                tracing::warn!("zoneevent child has no stdout; giving up on this iteration");
                let _ = child.kill().await;
                if wait_or_cancel(ZONEEVENT_RESTART_INTERVAL, &cancel).await {
                    return;
                }
                continue;
            }
        };
        let mut lines = BufReader::new(stdout).lines();

        loop {
            tokio::select! {
                _ = cancel.notified() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return;
                }
                line = lines.next_line() => {
                    match line {
                        Ok(Some(_line)) => {
                            // The legacy agent also ignored the line
                            // contents — any activity is enough to mark
                            // the sample dirty.
                            dirty.mark();
                        }
                        Ok(None) => {
                            // EOF — zoneevent exited. Reap the child and
                            // restart below.
                            let _ = child.wait().await;
                            break;
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "zoneevent read error; restarting");
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            break;
                        }
                    }
                }
            }
        }

        if wait_or_cancel(ZONEEVENT_RESTART_INTERVAL, &cancel).await {
            return;
        }
    }
}

/// Sleep until either `dur` elapses (returns false) or cancellation is
/// signaled (returns true).
async fn wait_or_cancel(dur: Duration, cancel: &Notify) -> bool {
    tokio::select! {
        _ = cancel.notified() => true,
        _ = tokio::time::sleep(dur) => false,
    }
}

/// Watch a directory for any filesystem change and mark dirty. Wraps the
/// `notify` crate so callers don't have to deal with its low-level
/// channels.
pub struct ZoneConfigWatcher {
    // `notify`'s watcher owns a background thread; we keep it alive here.
    // The trait object itself doesn't implement Debug, which is why we
    // hand-roll one below.
    _inner: Box<dyn Watcher + Send + Sync>,
    path: PathBuf,
}

impl std::fmt::Debug for ZoneConfigWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZoneConfigWatcher")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ZoneConfigWatcher {
    pub fn spawn(dirty: DirtyFlag) -> notify::Result<Self> {
        Self::spawn_watching(dirty, PathBuf::from(DEFAULT_ZONES_DIR))
    }

    pub fn spawn_watching(dirty: DirtyFlag, path: PathBuf) -> notify::Result<Self> {
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(_ev) => dirty.mark(),
                Err(e) => {
                    tracing::warn!(error = %e, "notify watcher error");
                }
            })?;
        watcher.watch(&path, RecursiveMode::NonRecursive)?;
        Ok(Self {
            _inner: Box::new(watcher),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Watch a single file (e.g., `/tmp/.sysinfo.json`) and fire a callback
/// whenever it changes.
///
/// Unlike [`ZoneConfigWatcher`], sysinfo changes need richer handling:
/// the main.rs loop re-reads sysinfo and re-registers with CNAPI. So
/// this watcher calls a custom callback instead of marking the status
/// flag.
pub struct SysinfoFileWatcher {
    _inner: Box<dyn Watcher + Send + Sync>,
    path: PathBuf,
}

impl SysinfoFileWatcher {
    pub fn spawn<F>(on_change: F) -> notify::Result<Self>
    where
        F: Fn() + Send + Sync + 'static,
    {
        Self::spawn_watching(PathBuf::from(DEFAULT_SYSINFO_PATH), on_change)
    }

    pub fn spawn_watching<F>(path: PathBuf, on_change: F) -> notify::Result<Self>
    where
        F: Fn() + Send + Sync + 'static,
    {
        // We watch the parent directory rather than the file itself so
        // tools that replace /tmp/.sysinfo.json atomically (rename-over)
        // don't lose our subscription.
        let watched_dir = path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let target = path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| path.clone());

        let target_clone = target.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(ev) => {
                    if ev
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(PathBuf::from).as_ref() == Some(&target_clone))
                    {
                        on_change();
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "sysinfo watcher error");
                }
            })?;
        watcher.watch(&watched_dir, RecursiveMode::NonRecursive)?;
        Ok(Self {
            _inner: Box::new(watcher),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for SysinfoFileWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SysinfoFileWatcher")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_flag_take_is_clearing() {
        let f = DirtyFlag::new();
        assert!(!f.take());
        f.mark();
        assert!(f.take());
        assert!(!f.take());
    }

    #[tokio::test]
    async fn dirty_flag_notifies_waiters() {
        let f = DirtyFlag::new();
        let fc = f.clone();
        let h = tokio::spawn(async move {
            fc.notified().await;
            fc.take()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        f.mark();
        let taken = h.await.expect("join");
        assert!(taken);
    }

    #[tokio::test]
    async fn zone_config_watcher_fires_on_file_create() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let dirty = DirtyFlag::new();
        let watcher = ZoneConfigWatcher::spawn_watching(dirty.clone(), tmp.path().to_path_buf())
            .expect("watcher");

        // Create a file and wait briefly for the notify backend to fire.
        std::fs::write(tmp.path().join("new.xml"), b"<xml/>").expect("write");
        let fired = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if dirty.take() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            fired.is_ok(),
            "zone config watcher should fire within 3s of a file creation"
        );

        drop(watcher);
    }
}
