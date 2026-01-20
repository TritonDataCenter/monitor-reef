// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
//
// Copyright 2020 Joyent, Inc.
// Copyright 2026 Edgecast Cloud LLC.

//! Job execution for the rebalancer manager
//!
//! This module contains the job execution logic for running rebalancer jobs.
//! Currently supports evacuate jobs which move objects from a storage node
//! being decommissioned to other available storage nodes.

pub mod evacuate;

use thiserror::Error;

/// Job execution errors
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum JobError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Job cancelled")]
    Cancelled,

    #[error("Storage node not found: {0}")]
    StorageNodeNotFound(String),

    #[error("Agent unavailable: {0}")]
    AgentUnavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl From<tokio_postgres::Error> for JobError {
    fn from(e: tokio_postgres::Error) -> Self {
        JobError::Database(e.to_string())
    }
}

impl From<deadpool_postgres::PoolError> for JobError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        JobError::Database(e.to_string())
    }
}
