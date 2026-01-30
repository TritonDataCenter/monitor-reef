/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

#[cfg(feature = "postgres")]
use std::io::Write;

#[cfg(feature = "postgres")]
use std::str::FromStr;

use crate::error::{Error, InternalError, InternalErrorCode};
use libmanta::moray::MantaObjectShark;
use md5::{Digest, Md5};
use quickcheck::{Arbitrary, Gen};
use quickcheck_helpers::random::string as random_string;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[cfg(feature = "postgres")]
use diesel::deserialize::{self, FromSql};

#[cfg(feature = "postgres")]
use diesel::pg::Pg;

#[cfg(feature = "postgres")]
use diesel::serialize::{self, IsNull, Output, ToSql};

use diesel::sql_types;
use strum::{EnumCount, IntoEnumIterator};

pub type HttpStatusCode = u16;
pub type ObjectId = String; // UUID

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentPayload {
    pub id: String,
    pub tasks: Vec<Task>,
}

impl From<AssignmentPayload> for (String, Vec<Task>) {
    fn from(p: AssignmentPayload) -> (String, Vec<Task>) {
        let AssignmentPayload { id, tasks } = p;
        (id, tasks)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Task {
    pub object_id: String, // or Uuid
    pub owner: String,     // or Uuid
    pub md5sum: String,
    pub source: MantaObjectShark,

    #[serde(default = "TaskStatus::default")]
    pub status: TaskStatus,
}

impl Task {
    pub fn set_status(&mut self, status: TaskStatus) {
        self.status = status;
    }
}

impl Arbitrary for Task {
    fn arbitrary<G: Gen>(g: &mut G) -> Task {
        let len: usize = (g.next_u32() % 20) as usize;
        let mut hasher = Md5::new();
        hasher.input(random_string(g, len).as_bytes());
        let md5checksum = hasher.result();
        let md5sum = base64::encode(&md5checksum);

        Task {
            object_id: Uuid::new_v4().to_string(),
            owner: Uuid::new_v4().to_string(),
            md5sum,
            source: MantaObjectShark::arbitrary(g),
            status: TaskStatus::arbitrary(g),
        }
    }
}

// Note: if you change or add any of the fields here be sure to update the
// Arbitrary implementation.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, EnumCount)]
pub enum TaskStatus {
    Pending,
    Complete,
    Failed(ObjectSkippedReason),
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::Pending
    }
}

impl Arbitrary for TaskStatus {
    fn arbitrary<G: Gen>(g: &mut G) -> TaskStatus {
        let i = g.next_u32() % (TaskStatus::count() as u32);
        match i {
            0 => TaskStatus::Pending,
            1 => TaskStatus::Complete,
            2 => TaskStatus::Failed(Arbitrary::arbitrary(g)),
            _ => panic!(),
        }
    }
}

#[derive(
    AsExpression,
    Clone,
    Copy,
    Debug,
    Deserialize,
    Display,
    EnumString,
    EnumVariantNames,
    EnumIter,
    Eq,
    FromSqlRow,
    Hash,
    PartialEq,
    Serialize,
)]
#[strum(serialize_all = "snake_case")]
#[sql_type = "sql_types::Text"]
pub enum ObjectSkippedReason {
    // Agent encountered a local filesystem error
    AgentFSError,

    // The specified agent does not have that assignment
    AgentAssignmentNoEnt,

    // The agent is busy and cant accept assignments at this time.
    AgentBusy,

    // Internal Assignment Error
    AssignmentError,

    // A mismatch of assignment data between the agent and the zone
    AssignmentMismatch,

    // The assignment was rejected by the agent.
    AssignmentRejected,

    // Not enough space on destination SN
    DestinationInsufficientSpace,

    // Destination agent was not reachable
    DestinationUnreachable,

    // MD5 Mismatch between the file on disk and the metadata.
    MD5Mismatch,

    // Catchall for unspecified network errors.
    NetworkError,

    // The object is already on the proposed destination shark, using it as a
    // destination for rebalance would reduce the durability by 1.
    ObjectAlreadyOnDestShark,

    // The object is already in the proposed destination datacenter, using it
    // as a destination for rebalance would reduce the failure domain.
    ObjectAlreadyInDatacenter,

    // Encountered some other http error (not 400 or 500) while attempting to
    // contact the source of the object.
    SourceOtherError,

    // The only source available is the shark that is being evacuated.
    SourceIsEvacShark,

    HTTPStatusCode(HttpStatusCode),
}

#[cfg(feature = "postgres")]
fn _osr_from_sql(ts: String) -> deserialize::Result<ObjectSkippedReason> {
    if ts.starts_with('{') && ts.ends_with('}') {
        // Start with:
        //      "{skipped_reason:status_code}"
        let matches: &[_] = &['{', '}'];

        // trim_matches:
        //      "skipped_reason:status_code"
        //
        // split().collect():
        //      ["skipped_reason", "status_code"]
        let sr_sc: Vec<&str> = ts.trim_matches(matches).split(':').collect();
        assert_eq!(sr_sc.len(), 2);

        // ["skipped_reason", "status_code"]
        let reason = ObjectSkippedReason::from_str(&sr_sc[0])?;
        match reason {
            ObjectSkippedReason::HTTPStatusCode(_) => {
                Ok(ObjectSkippedReason::HTTPStatusCode(sr_sc[1].parse()?))
            }
            _ => {
                Err("variant with value not found".into())
            }
        }
    } else {
        ObjectSkippedReason::from_str(&ts).map_err(std::convert::Into::into)
    }
}

impl ObjectSkippedReason {
    // The "Strum" crate already provides a "to_string()" method which we
    // want to use here.  This is for handling the special case of variants
    // with values/fields.
    pub fn into_string(self) -> String {
        match self {
            ObjectSkippedReason::HTTPStatusCode(sc) => {
                format!("{{{}:{}}}", self, sc)
            }
            _ => self.to_string(),
        }
    }
}

impl Arbitrary for ObjectSkippedReason {
    fn arbitrary<G: Gen>(g: &mut G) -> ObjectSkippedReason {
        let i: usize = g.next_u32() as usize % Self::iter().count();
        let status_code: u16 = (g.next_u32() as u16 % 500) + 100;
        let reason = Self::iter().nth(i).unwrap();
        match reason {
            ObjectSkippedReason::HTTPStatusCode(_) => {
                ObjectSkippedReason::HTTPStatusCode(status_code)
            }
            _ => reason,
        }
    }
}

#[cfg(feature = "postgres")]
impl ToSql<sql_types::Text, Pg> for ObjectSkippedReason {
    fn to_sql<W: Write>(&self, out: &mut Output<W, Pg>) -> serialize::Result {
        let sr = self.into_string();
        out.write_all(sr.as_bytes())?;

        Ok(IsNull::No)
    }
}

#[cfg(feature = "postgres")]
impl FromSql<sql_types::Text, Pg> for ObjectSkippedReason {
    fn from_sql(bytes: Option<&[u8]>) -> deserialize::Result<Self> {
        let t = not_none!(bytes);
        let t_str = String::from_utf8_lossy(t);
        let ts: String = t_str.to_string();
        _osr_from_sql(ts)
    }
}

pub fn get_sharks_from_value(
    manta_object: &Value,
) -> Result<Vec<MantaObjectShark>, Error> {
    let sharks_array = match manta_object.get("sharks") {
        Some(sa) => sa,
        None => {
            return Err(InternalError::new(
                Some(InternalErrorCode::BadMantaObject),
                "Missing sharks array",
            )
            .into());
        }
    };
    serde_json::from_value(sharks_array.to_owned()).map_err(Error::from)
}

#[allow(non_snake_case)]
pub fn get_objectId_from_value(
    manta_object: &Value,
) -> Result<ObjectId, Error> {
    let id = match manta_object.get("objectId") {
        Some(i) => match serde_json::to_string(i) {
            Ok(id_str) => id_str.replace("\"", ""),
            Err(e) => {
                let msg = format!(
                    "Could not parse objectId from {:#?}\n({})",
                    manta_object, e
                );
                error!("{}", msg);
                return Err(InternalError::new(
                    Some(InternalErrorCode::BadMantaObject),
                    msg,
                )
                .into());
            }
        },
        None => {
            let msg = format!("Missing objectId from {:#?}", manta_object);
            error!("{}", msg);
            return Err(InternalError::new(
                Some(InternalErrorCode::BadMantaObject),
                msg,
            )
            .into());
        }
    };

    Ok(id)
}

pub fn get_key_from_object_value(object: &Value) -> Result<String, Error> {
    let key = match object.get("key") {
        Some(k) => match serde_json::to_string(k) {
            Ok(ky) => ky.replace("\"", ""),
            Err(e) => {
                error!(
                    "Could not parse key field in object {:#?} ({})",
                    object, e
                );
                return Err(InternalError::new(
                    Some(InternalErrorCode::BadMantaObject),
                    "Could not parse Manta Object Key",
                )
                .into());
            }
        },
        None => {
            error!("Missing key field in object {:#?}", object);
            return Err(InternalError::new(
                Some(InternalErrorCode::BadMantaObject),
                "Missing Manta Object Key",
            )
            .into());
        }
    };

    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{InternalError, InternalErrorCode};
    use serde_json::json;

    #[test]
    fn get_sharks_from_value_valid() {
        let obj = json!({
            "sharks": [
                {"datacenter": "dc1", "manta_storage_id": "1.stor.domain"},
                {"datacenter": "dc2", "manta_storage_id": "2.stor.domain"}
            ]
        });
        let sharks = get_sharks_from_value(&obj).unwrap();
        assert_eq!(sharks.len(), 2);
        assert_eq!(sharks[0].manta_storage_id, "1.stor.domain");
        assert_eq!(sharks[1].datacenter, "dc2");
    }

    #[test]
    fn get_sharks_from_value_missing() {
        let obj = json!({"objectId": "abc"});
        assert!(get_sharks_from_value(&obj).is_err());
    }

    #[test]
    fn get_sharks_from_value_empty() {
        let obj = json!({"sharks": []});
        let sharks = get_sharks_from_value(&obj).unwrap();
        assert!(sharks.is_empty());
    }

    #[test]
    fn get_object_id_from_value_valid() {
        let obj = json!({"objectId": "test-uuid-123"});
        let id = get_objectId_from_value(&obj).unwrap();
        assert_eq!(id, "test-uuid-123");
    }

    #[test]
    fn get_object_id_from_value_missing() {
        let obj = json!({"key": "/test/file"});
        assert!(get_objectId_from_value(&obj).is_err());
    }

    #[test]
    fn get_key_from_object_value_valid() {
        let obj = json!({"key": "/user/stor/file.txt"});
        let key = get_key_from_object_value(&obj).unwrap();
        assert_eq!(key, "/user/stor/file.txt");
    }

    #[test]
    fn get_key_from_object_value_missing() {
        let obj = json!({"objectId": "abc"});
        assert!(get_key_from_object_value(&obj).is_err());
    }

    #[test]
    fn object_skipped_reason_into_string_simple() {
        let reason = ObjectSkippedReason::AgentFSError;
        assert_eq!(reason.into_string(), "agent_fs_error");
    }

    #[test]
    fn object_skipped_reason_into_string_http_status() {
        let reason = ObjectSkippedReason::HTTPStatusCode(404);
        let s = reason.into_string();
        assert!(s.contains("404"));
        assert!(s.starts_with('{'));
        assert!(s.ends_with('}'));
    }

    #[test]
    fn object_skipped_reason_into_string_variants() {
        assert_eq!(
            ObjectSkippedReason::NetworkError.into_string(),
            "network_error"
        );
        assert_eq!(
            ObjectSkippedReason::MD5Mismatch.into_string(),
            "md5_mismatch"
        );
        assert_eq!(
            ObjectSkippedReason::SourceIsEvacShark.into_string(),
            "source_is_evac_shark"
        );
    }

    #[test]
    fn assignment_payload_from_trait() {
        let payload = AssignmentPayload {
            id: "test-id".to_string(),
            tasks: vec![Task::default()],
        };
        let (id, tasks): (String, Vec<Task>) = payload.into();
        assert_eq!(id, "test-id");
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn task_status_default_is_pending() {
        let status = TaskStatus::default();
        assert_eq!(status, TaskStatus::Pending);
    }

    #[test]
    fn task_set_status() {
        let mut task = Task::default();
        assert_eq!(task.status, TaskStatus::Pending);
        task.set_status(TaskStatus::Complete);
        assert_eq!(task.status, TaskStatus::Complete);
    }

    #[test]
    fn internal_error_new_with_code() {
        let err = InternalError::new(
            Some(InternalErrorCode::BadMantaObject),
            "test error",
        );
        assert_eq!(err.code, InternalErrorCode::BadMantaObject);
    }

    #[test]
    fn internal_error_new_without_code() {
        let err = InternalError::new(None, "test error");
        assert_eq!(err.code, InternalErrorCode::Other);
    }

    #[test]
    fn internal_error_display() {
        let err = InternalError::new(
            Some(InternalErrorCode::SharkNotFound),
            "shark missing",
        );
        let display = format!("{}", err);
        assert!(display.contains("shark missing"));
    }
}
