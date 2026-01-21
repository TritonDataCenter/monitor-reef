<!--
This Source Code Form is subject to the terms of the Mozilla Public
License, v. 2.0. If a copy of the MPL was not distributed with this
file, You can obtain one at https://mozilla.org/MPL/2.0/.

Copyright 2025 Edgecast Cloud LLC.
-->

# Manta-Rebalancer Migration Review Findings

**Review Date:** 2025-01-21
**Reviewed By:** Claude Code (pr-review-toolkit agents)
**Branch:** modernization-skill
**Status:** Review Complete - Action Items Pending

This document captures findings from comparing the new Dropshot-based manta-rebalancer implementation against the legacy Gotham-based code in `libs/rebalancer-legacy/`.

## Executive Summary

| Component | Migration Status | Production Ready |
|-----------|------------------|------------------|
| Rebalancer Agent | ~90% Complete | Testing/Staging |
| Shared Types/API | 100% Complete | Yes |
| Storinfo Client | ~85% Complete | Testing/Staging |
| Manager Database | ~95% Complete | Testing/Staging |
| Evacuate Job | ~70% Complete | **No** - Missing critical integrations |

---

## Critical Issues (Must Fix)

### CRIT-1: Missing Sharkspotter Integration

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs`
**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs:790-812`

**Description:** The `ObjectSource::Sharkspotter` variant exists but has no implementation. Without this, initial evacuate jobs cannot discover objects on storage nodes.

**Impact:** Evacuate jobs cannot be started - no way to discover objects to evacuate.

**Action Required:**
- [ ] Implement sharkspotter client integration
- [ ] Wire up object discovery channel
- [ ] Add tests for sharkspotter error handling

---

### CRIT-2: Missing Moray Metadata Updates

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs:842-851`
**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs` (metadata update threads)

**Description:** The `update_object_metadata()` function is a stub. Objects will be copied to new locations but the Manta metadata (sharks array) won't be updated.

**Impact:** Data integrity issue - objects appear evacuated but metadata still points to old location.

**Action Required:**
- [ ] Implement Moray client (see CRIT-3)
- [ ] Implement metadata update logic
- [ ] Add tests for metadata update failures

---

### CRIT-3: No Moray Client

**Location:** N/A - Does not exist
**Legacy Reference:** `libs/rebalancer-legacy/manager/src/moray_client.rs`

**Description:** No Moray client exists in the new codebase. Required for updating object metadata after successful copy.

**Action Required:**
- [ ] Create `libs/moray-client/` or add to existing `libs/moray/`
- [ ] Implement async Moray client using the existing `libs/moray/` as reference
- [ ] Add bucket update operations needed by evacuate job

---

### CRIT-4: HTTP Client Fallback Silently Discards Timeout

**Location:** `services/rebalancer-agent/src/processor.rs:38-41`

```rust
let client = Client::builder()
    .timeout(Duration::from_secs(config.download_timeout_secs))
    .build()
    .unwrap_or_else(|_| Client::new());  // <-- Silent fallback
```

**Impact:** If client creation fails, downloads proceed without timeout protection, potentially hanging indefinitely.

**Action Required:**
- [ ] Replace with proper error propagation
- [ ] Log error details before failing
- [ ] Add test for client creation failure

---

### CRIT-5: Corrupted File Removal Ignored

**Location:** `services/rebalancer-agent/src/processor.rs:269`

```rust
let _ = fs::remove_file(&dest_path).await;  // <-- Ignored
```

**Legacy Reference:** `libs/rebalancer-legacy/rebalancer/src/libagent.rs:817-824` (explicit error handling)

**Impact:** Corrupted files may persist on storage nodes after MD5 mismatch detection.

**Action Required:**
- [ ] Log error if removal fails
- [ ] Consider retry logic
- [ ] Add test for removal failure scenario

---

### CRIT-6: Skipped Reason Parse Uses Silent Default

**Location:** `services/rebalancer-agent/src/storage.rs:189-195`

```rust
let reason: ObjectSkippedReason = serde_json::from_str(&reason)
    .unwrap_or(ObjectSkippedReason::NetworkError);  // <-- Silent default
```

**Impact:** Masks actual failure reasons, corrupts debugging data and statistics.

**Action Required:**
- [ ] Add logging when parse fails
- [ ] Consider adding `ObjectSkippedReason::Unknown(String)` variant
- [ ] Add test for malformed reason handling

---

### CRIT-7: Object Discovery Errors Discarded

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs:266-272`

```rust
let _ = object_discovery
    .await
    .inspect_err(|e| error!("Object discovery task panicked: {}", e))
    .ok()  // <-- Error discarded
```

**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs:867-873` (captures error in job result)

**Impact:** Jobs may appear successful when object discovery actually failed.

**Action Required:**
- [ ] Track discovery result
- [ ] Reflect discovery failures in job completion status
- [ ] Add test for discovery failure propagation

---

### CRIT-8: No Manager HTTP API Tests

**Legacy Tests Missing:**
- `basic` - GET /jobs, GET /jobs/{id}
- `post_test` - POST /jobs (job creation)
- `job_dynamic_update` - PUT /jobs/{id} (runtime updates)

**Location:** `libs/rebalancer-legacy/manager/src/main.rs` (tests module)

**Impact:** HTTP endpoint regressions could ship undetected.

**Action Required:**
- [ ] Port `basic` test to new codebase
- [ ] Port `post_test` test
- [ ] Port `job_dynamic_update` test
- [ ] Add tests to `services/rebalancer-manager/tests/`

---

## Important Issues (Should Fix)

### IMP-1: Max Fill Percentage Not Implemented

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs` (destination selection)
**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs:1059-1063`

**Description:** Legacy respects `config.max_fill_percentage` when calculating available space on destination sharks. New code doesn't account for this.

**Action Required:**
- [ ] Add max_fill_percentage to EvacuateConfig
- [ ] Factor into available space calculation
- [ ] Add test

---

### IMP-2: Duplicate Object Tracking Not Populated

**Location:** `services/rebalancer-manager/src/jobs/evacuate/db.rs`
**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs:1194-1196`

**Description:** The `duplicates` table is created but never populated. Legacy tracks duplicate object IDs across shards.

**Action Required:**
- [ ] Implement duplicate detection in object processing
- [ ] Populate duplicates table
- [ ] Add test for duplicate handling

---

### IMP-3: Agent Metrics Collection Missing

**Location:** `services/rebalancer-agent/src/`
**Legacy Reference:** `libs/rebalancer-legacy/rebalancer/src/metrics.rs`

**Description:** Legacy tracks BYTES_COUNT, OBJECT_COUNT, ERROR_COUNT, ASSIGNMENT_TIME. New agent has no metrics.

**Action Required:**
- [ ] Add metrics collection (consider Prometheus integration)
- [ ] Track bytes transferred, objects processed, errors
- [ ] Expose metrics endpoint

---

### IMP-4: Resume Interrupted Assignments on Startup

**Location:** `services/rebalancer-agent/src/`
**Legacy Reference:** `libs/rebalancer-legacy/rebalancer/src/libagent.rs:276-287`

**Description:** Legacy re-discovers and processes incomplete assignments after restart. New agent does not resume interrupted assignments.

**Action Required:**
- [ ] On startup, scan for incomplete assignments in SQLite
- [ ] Resume processing for any found
- [ ] Add test for crash recovery

---

### IMP-5: Assignment State Update Best-Effort

**Location:** `services/rebalancer-agent/src/processor.rs:115-126`

**Description:** Assignment state update to "complete" logs error but continues. May cause duplicate processing on restart.

**Action Required:**
- [ ] Consider retry logic
- [ ] Track state update failures for monitoring
- [ ] Document expected behavior on failure

---

### IMP-6: Task Completion Recording Best-Effort

**Location:** `services/rebalancer-agent/src/processor.rs:172-206`

**Description:** Task completion/failure recording errors are logged but discarded. Statistics will be inaccurate.

**Action Required:**
- [ ] Consider batch updates with retry
- [ ] Track recording failures for monitoring

---

### IMP-7: Result Count Increment Failures

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs:333-339` (and multiple other locations)

**Description:** Counter increment failures are logged as warnings but job continues. Statistics become inaccurate over time.

**Action Required:**
- [ ] Consider batching counter updates
- [ ] Implement retry mechanism
- [ ] Track discrepancy for monitoring

---

### IMP-8: Worker Task Results Discarded

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs:292-294`

```rust
let _ = assignment_poster.await;
let _ = assignment_checker.await;
let _ = metadata_updater.await;
```

**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs:875-897` (explicit error handling)

**Description:** Worker completion results are discarded. If any worker fails, it's not reflected in job status.

**Action Required:**
- [ ] Capture worker results
- [ ] Propagate errors to job status
- [ ] Add test for worker failure handling

---

### IMP-9: Unknown Job States Default to Init

**Location:** `services/rebalancer-manager/src/db.rs:85-95`

**Description:** Unknown job states silently default to `Init`. Could mask database corruption or schema issues.

**Action Required:**
- [ ] Add logging when unknown state encountered
- [ ] Consider `Unknown(String)` variant

---

### IMP-10: No Configuration Parsing Tests

**Legacy Tests Missing (5 tests):**
- `min_max_shards`
- `config_basic_test`
- `config_options_test`
- `missing_snaplink_cleanup_required`
- `signal_handler_config_update`

**Location:** `libs/rebalancer-legacy/manager/src/config.rs`

**Action Required:**
- [ ] Port configuration tests to new codebase
- [ ] Add to `services/rebalancer-manager/src/config.rs`

---

### IMP-11: No CLI/Admin Tests

**Legacy Tests Missing (5 tests):**
- `no_params`
- `job_list_extra_params`
- `job_get_no_params`
- `job_create_no_params`
- `job_evacuate_no_params`

**Location:** `libs/rebalancer-legacy/manager/src/rebalancer-adm.rs`

**Action Required:**
- [ ] Port CLI tests to `cli/rebalancer-adm/`
- [ ] Add argument validation tests

---

### IMP-12: Jobs Module Basic Test Missing

**Legacy Test:** `basic` in `libs/rebalancer-legacy/manager/src/jobs/mod.rs`

**Description:** Tests JobBuilder creation.

**Action Required:**
- [ ] Port test to new jobs module

---

## Minor Issues (Nice to Have)

### MIN-1: Semaphore Acquisition Unchecked

**Location:** `services/rebalancer-agent/src/processor.rs:154`

```rust
let _permit = self.semaphore.acquire().await;
```

Should use `.expect()` or handle `AcquireError`.

---

### MIN-2: Assignment Update Doesn't Verify Row Affected

**Location:** `services/rebalancer-agent/src/storage.rs:231-238`

UPDATE statement doesn't verify any rows were affected.

---

### MIN-3: Shutdown Signal Uses .ok()

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs:279, 283, 288, 941`

Pattern makes it hard to distinguish intentional discards from accidental ones.

---

### MIN-4: Destination Selection Error Skips Without DB Update

**Location:** `services/rebalancer-manager/src/jobs/evacuate/mod.rs:342-346`

Object is skipped but not marked as skipped in database.

---

### MIN-5: Temporary File Cleanup on Agent Startup

**Legacy Reference:** `libs/rebalancer-legacy/rebalancer/src/libagent.rs:1159-1166`

Legacy removes partial downloads from temp dir at startup.

---

### MIN-6: Blacklist Support in Storinfo

**Legacy Reference:** `libs/rebalancer-legacy/manager/src/storinfo.rs` (ChooseAlgorithm trait)

New storinfo lacks blacklist feature for excluding problematic sharks.

---

### MIN-7: Dynamic Thread Tuning

**Legacy Reference:** `libs/rebalancer-legacy/manager/src/jobs/evacuate.rs:545-577`

Legacy allows runtime adjustment of metadata update threads via `EvacuateJobUpdateMessage::SetMetadataThreads`.

---

## Test Coverage Summary

| Category | Legacy | New | Coverage | Priority |
|----------|--------|-----|----------|----------|
| Agent Integration | 6 | 6 | 100% | - |
| Agent Storage | 0 | 4 | NEW | - |
| Manager Status | 4 | 4 | 100% | - |
| Evacuate Job Logic | 12 | 29 | 100%+ | - |
| Manager HTTP API | 3 | 0 | **0%** | Critical |
| Configuration | 5 | 0 | **0%** | Important |
| CLI/Admin | 5 | 0 | **0%** | Important |
| Type Serialization | 0 | 4 | NEW | - |

---

## Improvements in New Implementation

The new implementation includes several improvements over legacy:

1. **Modern async architecture** - Tokio async/await replaces sync threads
2. **Better test coverage for evacuate logic** - 29 tests vs 12 legacy
3. **Type-safe API definitions** - Dropshot traits provide compile-time contracts
4. **Graceful shutdown** - Coordinated worker termination via watch channels
5. **Async database access** - Non-blocking DB operations with deadpool
6. **New storage layer tests** - 4 tests for SQLite storage that didn't exist
7. **Type serialization tests** - 4 tests for API type serialization

---

## Recommended Priority Order

### Phase 1: Critical (Before any testing)
1. CRIT-3: Moray Client
2. CRIT-1: Sharkspotter Integration
3. CRIT-2: Metadata Updates
4. CRIT-8: HTTP API Tests

### Phase 2: Critical Error Handling (Before staging)
5. CRIT-4: HTTP Client Fallback
6. CRIT-5: Corrupted File Removal
7. CRIT-6: Skipped Reason Parse
8. CRIT-7: Discovery Error Propagation

### Phase 3: Important (Before production)
9. IMP-1: Max Fill Percentage
10. IMP-10: Configuration Tests
11. IMP-8: Worker Task Results
12. IMP-2: Duplicate Object Tracking

### Phase 4: Polish (Post-production)
13. Remaining important issues
14. Minor issues
15. Additional metrics and monitoring

---

## References

- Legacy code: `libs/rebalancer-legacy/`
- New agent: `services/rebalancer-agent/`
- New manager: `services/rebalancer-manager/`
- API types: `apis/rebalancer-types/`
- Conversion plan: `conversion-plans/manta-rebalancer/plan.md`
