// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2025 Edgecast Cloud LLC.

//! Logging utilities for K8s commands

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::state::clusters_base_dir;

/// Log entry with timestamp and message
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp: DateTime<Utc>,
    pub level: LogLevel,
    pub message: String,
}

/// Log level for categorizing entries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
    Cmd,
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Cmd => write!(f, "CMD"),
        }
    }
}

impl LogEntry {
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now(),
            level,
            message: message.into(),
        }
    }

    pub fn format(&self) -> String {
        format!(
            "[{}] [{}] {}",
            self.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            self.level,
            self.message
        )
    }
}

/// A writer that captures log output to both stderr and a file
pub struct LogWriter {
    cluster_uuid: Uuid,
    operation: String,
    entries: Arc<Mutex<Vec<LogEntry>>>,
    log_file: Option<PathBuf>,
}

impl LogWriter {
    /// Create a new log writer for a cluster operation
    pub async fn new(cluster_uuid: Uuid, operation: &str) -> Result<Self> {
        let log_dir = clusters_base_dir()?
            .join(cluster_uuid.to_string())
            .join("logs");
        tokio::fs::create_dir_all(&log_dir).await?;

        let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
        let log_file = log_dir.join(format!("{}-{}.log", operation, timestamp));

        Ok(Self {
            cluster_uuid,
            operation: operation.to_string(),
            entries: Arc::new(Mutex::new(Vec::new())),
            log_file: Some(log_file),
        })
    }

    /// Get the path to the log file
    pub fn log_file_path(&self) -> Option<&PathBuf> {
        self.log_file.as_ref()
    }

    /// Log a message at the specified level
    pub fn log(&self, level: LogLevel, message: impl Into<String>) {
        let entry = LogEntry::new(level, message);
        if let Ok(mut entries) = self.entries.lock() {
            entries.push(entry);
        }
    }

    /// Log an info message
    pub fn info(&self, message: impl Into<String>) {
        self.log(LogLevel::Info, message);
    }

    /// Log a warning message
    #[allow(dead_code)]
    pub fn warn(&self, message: impl Into<String>) {
        self.log(LogLevel::Warn, message);
    }

    /// Log an error message
    pub fn error(&self, message: impl Into<String>) {
        self.log(LogLevel::Error, message);
    }

    /// Log a debug message
    #[allow(dead_code)]
    pub fn debug(&self, message: impl Into<String>) {
        self.log(LogLevel::Debug, message);
    }

    /// Log a command execution
    #[allow(dead_code)]
    pub fn cmd(&self, command: impl Into<String>) {
        self.log(LogLevel::Cmd, command);
    }

    /// Log command output (stdout/stderr)
    #[allow(dead_code)]
    pub fn cmd_output(&self, stdout: &[u8], stderr: &[u8]) {
        if !stdout.is_empty() {
            let stdout_str = String::from_utf8_lossy(stdout);
            for line in stdout_str.lines() {
                self.log(LogLevel::Debug, format!("  stdout: {}", line));
            }
        }
        if !stderr.is_empty() {
            let stderr_str = String::from_utf8_lossy(stderr);
            for line in stderr_str.lines() {
                self.log(LogLevel::Debug, format!("  stderr: {}", line));
            }
        }
    }

    /// Write all accumulated logs to the log file
    pub async fn flush(&self) -> Result<()> {
        let Some(log_file) = &self.log_file else {
            return Ok(());
        };

        let entries = {
            let guard = self
                .entries
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire lock on log entries: {}", e))?;
            guard.clone()
        };

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)?;

        // Write header if file is new/empty
        let metadata = file.metadata()?;
        if metadata.len() == 0 {
            writeln!(file, "# Triton K8s {} Log", self.operation)?;
            writeln!(file, "# Cluster: {}", self.cluster_uuid)?;
            writeln!(
                file,
                "# Started: {}",
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            )?;
            writeln!(file, "#")?;
        }

        for entry in &entries {
            writeln!(file, "{}", entry.format())?;
        }

        // Clear the in-memory entries after flushing
        if let Ok(mut guard) = self.entries.lock() {
            guard.clear();
        }

        Ok(())
    }

    /// Get cluster UUID
    #[allow(dead_code)]
    pub fn cluster_uuid(&self) -> Uuid {
        self.cluster_uuid
    }

    /// Create a "latest" symlink pointing to this log file
    pub async fn create_latest_symlink(&self) -> Result<()> {
        let Some(log_file) = &self.log_file else {
            return Ok(());
        };

        let log_dir = log_file
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Log file has no parent directory"))?;

        let latest_link = log_dir.join(format!("{}-latest.log", self.operation));

        // Remove existing symlink if present
        let _ = tokio::fs::remove_file(&latest_link).await;

        // Create new symlink
        #[cfg(unix)]
        {
            let filename = log_file
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("Log file has no filename"))?;
            tokio::fs::symlink(filename, &latest_link).await?;
        }

        Ok(())
    }
}

impl Drop for LogWriter {
    fn drop(&mut self) {
        // Best-effort flush on drop
        let Ok(entries) = self.entries.lock() else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        let Some(log_file) = &self.log_file else {
            return;
        };
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
        else {
            return;
        };
        for entry in entries.iter() {
            let _ = writeln!(file, "{}", entry.format());
        }
    }
}

/// Helper to format command arguments for logging
#[allow(dead_code)]
pub fn format_command(cmd: &str, args: &[&str]) -> String {
    let mut parts = vec![cmd.to_string()];
    parts.extend(args.iter().map(|s| s.to_string()));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_entry_format() {
        let entry = LogEntry {
            timestamp: DateTime::parse_from_rfc3339("2025-03-31T12:00:00.123Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
            level: LogLevel::Info,
            message: "Test message".to_string(),
        };

        let formatted = entry.format();
        assert!(formatted.contains("[INFO]"));
        assert!(formatted.contains("Test message"));
        assert!(formatted.contains("2025-03-31T12:00:00.123Z"));
    }

    #[test]
    fn test_format_command() {
        let cmd = format_command(
            "talosctl",
            &["apply-config", "--insecure", "-n", "10.0.0.5"],
        );
        assert_eq!(cmd, "talosctl apply-config --insecure -n 10.0.0.5");
    }
}
