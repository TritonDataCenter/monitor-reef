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

    /// Bucket ID for MDAPI (v2) objects. None for v1 objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_id: Option<String>,

    /// MD5 hex digest of object name for MDAPI (v2) objects.
    /// None for v1 objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_name_hash: Option<String>,
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
            bucket_id: None,
            object_name_hash: None,
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

    // The assignment was stuck in a non-complete state on the agent for
    // longer than 2 * max_assignment_age.
    AgentAssignmentTimeout,

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

/// Compute MD5 hex digest of an object name.
///
/// Matches the Node.js algorithm:
///   crypto.createHash('md5').update(name).digest('hex')
pub fn object_name_md5_hex(name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.input(name.as_bytes());
    format!("{:x}", hasher.result())
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

    #[test]
    fn object_name_md5_hex_known_vector() {
        // echo -n "hello" | md5
        // 5d41402abc4b2a76b9719d911017c592
        assert_eq!(
            object_name_md5_hex("hello"),
            "5d41402abc4b2a76b9719d911017c592"
        );
    }

    #[test]
    fn object_name_md5_hex_empty_string() {
        // echo -n "" | md5
        // d41d8cd98f00b204e9800998ecf8427e
        assert_eq!(
            object_name_md5_hex(""),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
    }

    #[test]
    fn object_name_md5_hex_object_path() {
        // Typical Manta object name
        let hash = object_name_md5_hex(
            "/user/stor/myobject.txt",
        );
        assert_eq!(hash.len(), 32);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn task_serde_roundtrip_v2_fields() {
        let task = Task {
            object_id: "obj-123".to_string(),
            owner: "owner-456".to_string(),
            md5sum: "abc".to_string(),
            source: MantaObjectShark {
                datacenter: "dc1".to_string(),
                manta_storage_id: "1.stor".to_string(),
            },
            status: TaskStatus::Pending,
            bucket_id: Some("bucket-789".to_string()),
            object_name_hash: Some("aabbccdd".to_string()),
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("bucket_id"));
        assert!(json.contains("object_name_hash"));
        let round: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(
            round.bucket_id.as_deref(),
            Some("bucket-789"),
        );
        assert_eq!(
            round.object_name_hash.as_deref(),
            Some("aabbccdd"),
        );
    }

    #[test]
    fn task_serde_backward_compat_v1() {
        // Old v1 JSON without bucket_id or object_name_hash
        let json = r#"{
            "object_id": "obj-1",
            "owner": "owner-1",
            "md5sum": "abc",
            "source": {
                "datacenter": "dc1",
                "manta_storage_id": "1.stor"
            },
            "status": "Pending"
        }"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.bucket_id.is_none());
        assert!(task.object_name_hash.is_none());
    }

    #[test]
    fn task_serde_v1_skips_none_fields() {
        let task = Task {
            object_id: "obj-1".to_string(),
            owner: "owner-1".to_string(),
            md5sum: "abc".to_string(),
            source: MantaObjectShark {
                datacenter: "dc1".to_string(),
                manta_storage_id: "1.stor".to_string(),
            },
            status: TaskStatus::Pending,
            bucket_id: None,
            object_name_hash: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("bucket_id"),
            "v1 task should not serialize bucket_id"
        );
        assert!(
            !json.contains("object_name_hash"),
            "v1 task should not serialize object_name_hash"
        );
    }
}
