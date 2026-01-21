# Rebalancer Manager Implementation Plan

## Overview

Connect the rebalancer manager API to actually execute EvacuateJob as background tasks with proper state management.

## Completed

### Phase 1: Wire Up Job Lifecycle - EvacuateJob Execution
- [x] Import EvacuateJob and EvacuateConfig in context.rs
- [x] After DB creation in `create_evacuate_job()`, spawn `EvacuateJob::run()` in background
- [x] Pass `Arc<Database>` to spawned task for state updates on failure
- [x] Handle errors by updating job state to "failed" in database

**Commit:** b9a3762 - Wire up EvacuateJob execution as background task

### Phase 2: Wire Up State Transitions

Connect the EvacuateJob to update job state in the manager database as it progresses through its lifecycle.

#### State Transitions
1. `init` - Job created in DB (done)
2. `setup` - EvacuateJob initializing (refresh storinfo, setup workers)
3. `running` - Processing objects (assignment manager active)
4. `complete` - Job finished successfully
5. `failed` - Job encountered unrecoverable error

#### Completed
- [x] Pass manager database reference and job UUID to EvacuateJob
- [x] Update state to "setup" at start of EvacuateJob::run()
- [x] Update state to "running" after workers are started
- [x] Update state to "complete" at end of successful run
- [x] State already updates to "failed" on error (from Phase 1)

**Commit:** c8e8974 - Wire up job state transitions in EvacuateJob

### Phase 3: Integrate Object Discovery

Connect object discovery to feed objects into the assignment manager.

#### Implemented
- [x] Add `ObjectSource` enum to configure object discovery source
- [x] Add `object_source` field to `EvacuateConfig`
- [x] Add `get_retryable_objects()` method to EvacuateDb for retry jobs
- [x] Add `spawn_object_discovery()` method to EvacuateJob
- [x] Wire object discovery into `run()` replacing the placeholder drop

#### Object Sources
1. `ObjectSource::None` - No objects (default, for testing scaffolding)
2. `ObjectSource::LocalDb` - Read from local evacuate database (for retry jobs)
3. Future: Sharkspotter integration for live object discovery

**Commit:** 1002973 - Integrate object discovery for evacuate jobs

### Phase 4: Connect Destination Shark Selection

Implement `select_destination()` to use storinfo data for choosing optimal destination sharks.

#### Implemented
- [x] Add `get_all_nodes_with_info()` to StorinfoClient for capacity data
- [x] Add `get_sharks()` and `get_content_length()` helper methods to EvacuateObject
- [x] Implement `select_destination()` with full filtering logic:
  - Filter out the source shark
  - Filter out sharks the object is already on
  - Filter out sharks in datacenters where object already exists (fault domain)
  - Filter out sharks without enough available capacity
  - Select shark with most available space
- [x] Add `init_dest_sharks()` to initialize destination cache from storinfo
- [x] Add `update_assigned_capacity()` to track capacity as objects are assigned
- [x] Wire up destination cache initialization in `run()` method

**Commit:** da91438 - Implement destination shark selection for evacuate jobs

## In Progress

## Future Work

### Phase 5: Update Result Counts
- Call `db.increment_result_count()` as objects complete
- Track: Total, Complete, Failed, Skipped counts
- Update counts in real-time during job execution
