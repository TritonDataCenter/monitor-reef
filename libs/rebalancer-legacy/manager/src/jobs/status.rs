/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

use super::evacuate::EvacuateObjectStatus;

use crate::jobs::evacuate::EvacuateJobDbConfig;
use crate::jobs::{JobActionDbEntry, JobDbEntry, JobState, REBALANCER_DB};
use crate::pg_db;
use rebalancer::error::Error;

use std::collections::HashMap;
use std::string::ToString;

use diesel::prelude::*;
use diesel::result::ConnectionError;
use diesel::sql_query;
use diesel::sql_types::{BigInt, Text};
use inflector::cases::titlecase::to_title_case;
use libmanta::moray::MantaObjectShark;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use uuid::Uuid;

static STATUS_COUNT_QUERY: &str = "SELECT status, count(status) \
                                   FROM  evacuateobjects  GROUP BY status";

#[derive(Debug, EnumString)]
pub enum StatusError {
    DBExists,
    LookupError,
    Unknown,
}

#[derive(QueryableByName, Debug)]
struct StatusCount {
    #[sql_type = "Text"]
    status: String,
    #[sql_type = "BigInt"]
    count: i64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action")]
pub enum JobStatusConfig {
    Evacuate(JobConfigEvacuate),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum JobStatusResults {
    Evacuate(JobStatusResultsEvacuate),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JobStatus {
    pub config: JobStatusConfig,
    pub results: JobStatusResults,
    pub state: JobState,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JobConfigEvacuate {
    pub from_shark: MantaObjectShark,
}

type JobStatusResultsEvacuate = HashMap<String, i64>;

fn get_rebalancer_db_conn() -> Result<PgConnection, StatusError> {
    pg_db::connect_or_create_db(REBALANCER_DB).map_err(|e| {
        error!("Error connecting to rebalancer DB: {}", e);
        StatusError::Unknown
    })
}

fn get_job_db_entry(uuid: &Uuid) -> Result<JobDbEntry, StatusError> {
    use crate::jobs::jobs::dsl::{id as job_id, jobs as jobs_db};

    let job_uuid = uuid.to_string();
    let conn = get_rebalancer_db_conn()?;

    jobs_db
        .filter(job_id.eq(&job_uuid))
        .first(&conn)
        .map_err(|e| {
            error!("Could not find job ({}): {}", job_uuid, e);
            StatusError::LookupError
        })
}

fn get_job_db_conn_common(uuid: &Uuid) -> Result<PgConnection, StatusError> {
    let db_name = uuid.to_string();
    pg_db::connect_db(&db_name).map_err(|e| {
        if let Error::DieselConnection(conn_err) = &e {
            if let ConnectionError::BadConnection(err) = conn_err {
                error!("Status DB connection: {}", err);
                return StatusError::DBExists;
            }
        }
        error!("Unknown status DB connection error: {}", e);
        StatusError::Unknown
    })
}

/// Build the status results HashMap from raw status counts and a
/// duplicate count.  This is the pure aggregation logic extracted
/// from `get_evacaute_job_status` so it can be tested without a
/// database connection.
fn aggregate_status_counts(
    status_counts: &[StatusCount],
    duplicate_count: i64,
) -> JobStatusResultsEvacuate {
    let mut ret = HashMap::new();
    let mut total_count: i64 = 0;

    for status_count in status_counts.iter() {
        total_count += status_count.count;
        ret.insert(to_title_case(&status_count.status), status_count.count);
    }

    // Statuses with 0 records won't appear in the query results,
    // so fill them in here.
    for status_value in EvacuateObjectStatus::iter() {
        ret.entry(to_title_case(&status_value.to_string()))
            .or_insert(0);
    }

    total_count += duplicate_count;
    ret.insert("Duplicates".into(), duplicate_count);
    ret.insert("Total".into(), total_count);

    ret
}

fn get_evacaute_job_status(
    uuid: &Uuid,
) -> Result<JobStatusResultsEvacuate, StatusError> {
    use crate::jobs::evacuate::duplicates::dsl::duplicates;
    use diesel::dsl::count_star;

    let conn = get_job_db_conn_common(&uuid)?;

    // Unfortunately diesel doesn't have GROUP BY support yet, so we do a raw
    // query here.
    // See https://github.com/diesel-rs/diesel/issues/210
    let status_counts: Vec<StatusCount> =
        match sql_query(STATUS_COUNT_QUERY).load::<StatusCount>(&conn) {
            Ok(res) => res,
            Err(e) => {
                error!("Status DB query: {}", e);
                return Err(StatusError::LookupError);
            }
        };

    let duplicate_count =
        duplicates.select(count_star()).first(&conn).unwrap_or(0);

    Ok(aggregate_status_counts(&status_counts, duplicate_count))
}

fn get_evacuate_job_config(
    uuid: &Uuid,
) -> Result<JobConfigEvacuate, StatusError> {
    use crate::jobs::evacuate::config::dsl::config as config_table;
    let conn = get_job_db_conn_common(&uuid)?;

    let config: EvacuateJobDbConfig =
        config_table.first(&conn).map_err(|e| {
            error!("Could not find job config ({}): {}", uuid.to_string(), e);
            StatusError::LookupError
        })?;

    let from_shark: MantaObjectShark =
        serde_json::from_value(config.from_shark).map_err(|e| {
            error!(
                "Could not deserialize job config ({}): {}",
                uuid.to_string(),
                e
            );
            StatusError::Unknown
        })?;

    Ok(JobConfigEvacuate { from_shark })
}

pub fn get_job_status(
    uuid: &Uuid,
    action: &JobActionDbEntry,
) -> Result<JobStatusResults, StatusError> {
    match action {
        JobActionDbEntry::Evacuate => {
            Ok(JobStatusResults::Evacuate(get_evacaute_job_status(uuid)?))
        }
        _ => unreachable!(),
    }
}

fn get_job_config(
    uuid: &Uuid,
    action: &JobActionDbEntry,
) -> Result<JobStatusConfig, StatusError> {
    match action {
        JobActionDbEntry::Evacuate => {
            Ok(JobStatusConfig::Evacuate(get_evacuate_job_config(&uuid)?))
        }
        _ => unreachable!(),
    }
}

pub fn get_job(uuid: Uuid) -> Result<JobStatus, StatusError> {
    let job_entry = get_job_db_entry(&uuid)?;
    let results = get_job_status(&uuid, &job_entry.action)?;
    let config = get_job_config(&uuid, &job_entry.action)?;

    // get job config
    Ok(JobStatus {
        results,
        config,
        state: job_entry.state,
    })
}

pub fn list_jobs() -> Result<Vec<JobDbEntry>, StatusError> {
    use crate::jobs::jobs::dsl::jobs as jobs_db;

    let conn = get_rebalancer_db_conn()?;
    let job_list = match jobs_db.load::<JobDbEntry>(&conn) {
        Ok(list) => list,
        Err(e) => {
            error!("Error listing jobs: {}", e);
            return Err(StatusError::Unknown);
        }
    };

    Ok(job_list)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rebalancer::util;

    #[test]
    fn bad_job_id() {
        let _guard = util::init_global_logger(None);
        let uuid = Uuid::new_v4();
        assert!(get_job(uuid).is_err());
    }

    // Mock tests for status aggregation logic (no PostgreSQL needed).

    #[test]
    fn aggregate_status_total() {
        // Simulate query results: 50 unprocessed, 30 assigned,
        // 20 complete, 100 error.
        let status_counts = vec![
            StatusCount {
                status: "unprocessed".into(),
                count: 50,
            },
            StatusCount {
                status: "assigned".into(),
                count: 30,
            },
            StatusCount {
                status: "complete".into(),
                count: 20,
            },
            StatusCount {
                status: "error".into(),
                count: 100,
            },
        ];
        let duplicate_count = 0;

        let results = aggregate_status_counts(&status_counts, duplicate_count);
        let total = *results.get("Total").expect("Total count");

        assert_eq!(total, 200);
        assert_eq!(*results.get("Unprocessed").unwrap(), 50);
        assert_eq!(*results.get("Assigned").unwrap(), 30);
        assert_eq!(*results.get("Complete").unwrap(), 20);
        assert_eq!(*results.get("Error").unwrap(), 100);
        assert_eq!(*results.get("Duplicates").unwrap(), 0);
    }

    #[test]
    fn aggregate_status_with_duplicates() {
        let status_counts = vec![
            StatusCount {
                status: "complete".into(),
                count: 80,
            },
            StatusCount {
                status: "error".into(),
                count: 10,
            },
        ];
        let duplicate_count = 5;

        let results = aggregate_status_counts(&status_counts, duplicate_count);
        let total = *results.get("Total").expect("Total count");

        // Total = 80 + 10 + 5 (duplicates)
        assert_eq!(total, 95);
        assert_eq!(*results.get("Duplicates").unwrap(), 5);
    }

    #[test]
    fn aggregate_status_zero_values() {
        // Simulate results where PostProcessing never appears in
        // the query (0 records with that status).
        let status_counts = vec![
            StatusCount {
                status: "unprocessed".into(),
                count: 100,
            },
            StatusCount {
                status: "assigned".into(),
                count: 50,
            },
            StatusCount {
                status: "complete".into(),
                count: 30,
            },
            StatusCount {
                status: "error".into(),
                count: 20,
            },
        ];
        let duplicate_count = 0;

        let results = aggregate_status_counts(&status_counts, duplicate_count);
        let total = *results.get("Total").expect("Total count");

        assert_eq!(total, 200);
        // PostProcessing and Skipped were not in query results,
        // so they must be filled in with 0.
        assert_eq!(*results.get("Post Processing").unwrap(), 0);
        assert_eq!(*results.get("Skipped").unwrap(), 0);
    }

    #[test]
    fn aggregate_status_empty_input() {
        // No records at all — every status should be 0.
        let results = aggregate_status_counts(&[], 0);
        let total = *results.get("Total").expect("Total count");

        assert_eq!(total, 0);
        assert_eq!(*results.get("Duplicates").unwrap(), 0);

        // All EvacuateObjectStatus variants must be present with 0.
        for status_value in EvacuateObjectStatus::iter() {
            let key = to_title_case(&status_value.to_string());
            assert_eq!(
                *results.get(&key).unwrap_or(&-1),
                0,
                "Expected 0 for status '{}'",
                key
            );
        }
    }
}
