-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, You can obtain one at https://mozilla.org/MPL/2.0/.
--
-- Copyright 2020 Joyent, Inc.
-- Copyright 2026 Edgecast Cloud LLC.

-- Schema for the rebalancer manager database
--
-- This schema stores job information and results for the rebalancer manager.
-- Jobs track evacuation operations and their progress.

-- Enable UUID extension if not already enabled
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

-- Jobs table: stores information about rebalancer jobs
CREATE TABLE IF NOT EXISTS jobs (
    -- Primary key: UUID for the job
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),

    -- Job type (currently only 'evacuate')
    action TEXT NOT NULL,

    -- Current state: init, setup, running, stopped, complete, failed
    state TEXT NOT NULL DEFAULT 'init',

    -- For evacuate jobs: the storage node being evacuated
    from_shark TEXT,

    -- For evacuate jobs: the datacenter of the source storage node
    from_shark_datacenter TEXT,

    -- Optional limit on number of objects to process
    max_objects INTEGER,

    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Job results table: stores status counts for each job
-- This tracks how many objects ended up in each status category
CREATE TABLE IF NOT EXISTS job_results (
    -- Foreign key to the job
    job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,

    -- Status category (e.g., 'complete', 'skipped:md5_mismatch', etc.)
    status TEXT NOT NULL,

    -- Count of objects in this status
    count BIGINT NOT NULL DEFAULT 0,

    -- Composite primary key
    PRIMARY KEY (job_id, status)
);

-- Index for efficient job queries
CREATE INDEX IF NOT EXISTS idx_jobs_state ON jobs(state);
CREATE INDEX IF NOT EXISTS idx_jobs_created_at ON jobs(created_at DESC);

-- Index for efficient result lookups
CREATE INDEX IF NOT EXISTS idx_job_results_job_id ON job_results(job_id);
