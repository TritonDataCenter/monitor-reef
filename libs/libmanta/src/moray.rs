// Copyright 2019 Joyent, Inc.

use crate::util;
use base64;
use diesel::sql_types;
use md5;
use quickcheck::{Arbitrary, Gen};
use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
use std::io::Write;

#[cfg(any(feature = "sqlite", feature = "postgres"))]
use diesel::{
    deserialize::{self, FromSql},
    serialize::{self, IsNull, Output, ToSql},
};

#[cfg(feature = "sqlite")]
use diesel::sqlite::Sqlite;

#[cfg(feature = "sqlite")]
use diesel::backend;

#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
#[serde(tag = "type")]
pub enum ObjectType {
    #[serde(alias = "object")]
    Object(MantaObject),

    #[serde(alias = "directory")]
    Directory(MantaDirectory),
}

#[derive(
    Deserialize,
    Serialize,
    Default,
    PartialEq,
    Debug,
    Clone,
    FromSqlRow,
    AsExpression,
)]
#[serde(rename_all = "camelCase")]
#[sql_type = "sql_types::Text"]
pub struct MantaObject {
    pub headers: Value,
    pub key: String,
    pub mtime: i64,
    pub name: String,
    pub creator: String,
    pub dirname: String,
    pub owner: String,
    pub roles: Vec<String>, // TODO: double check this is a String
    pub vnode: i64,

    #[serde(alias = "contentLength", default)]
    pub content_length: u64,

    #[serde(alias = "contentMD5", default)]
    pub content_md5: String,

    #[serde(alias = "contentType", default)]
    pub content_type: String,

    #[serde(alias = "objectId", default)]
    pub object_id: String,

    #[serde(default)]
    pub etag: String,

    #[serde(default)]
    pub sharks: Vec<MantaObjectShark>,

    #[serde(alias = "type", rename(serialize = "type"), default)]
    pub obj_type: String,
}

#[cfg(feature = "sqlite")]
impl ToSql<sql_types::Text, Sqlite> for MantaObject {
    fn to_sql<W: Write>(
        &self,
        out: &mut Output<W, Sqlite>,
    ) -> serialize::Result {
        let manta_str = serde_json::to_string(&self).unwrap();
        out.write_all(manta_str.as_bytes())?;

        Ok(IsNull::No)
    }
}

#[cfg(feature = "sqlite")]
impl FromSql<sql_types::Text, Sqlite> for MantaObject {
    fn from_sql(
        bytes: Option<backend::RawValue<Sqlite>>,
    ) -> deserialize::Result<Self> {
        let manta_obj: MantaObject =
            serde_json::from_str(not_none!(bytes).read_text())?;
        Ok(manta_obj)
    }
}

#[cfg(feature = "postgres")]
use diesel::pg::{Pg, PgValue};

#[cfg(feature = "postgres")]
impl ToSql<sql_types::Text, Pg> for MantaObject {
    fn to_sql<W: Write>(&self, out: &mut Output<W, Pg>) -> serialize::Result {
        let manta_str = serde_json::to_string(&self).unwrap();
        out.write_all(manta_str.as_bytes())?;

        Ok(IsNull::No)
    }
}

#[cfg(feature = "postgres")]
impl FromSql<sql_types::Text, Pg> for MantaObject {
    fn from_sql(bytes: Option<PgValue<'_>>) -> deserialize::Result<Self> {
        let t: PgValue = not_none!(bytes);
        let t_str = String::from_utf8_lossy(t.as_bytes());
        let manta_obj: MantaObject = serde_json::from_str(&t_str)?;
        Ok(manta_obj)
    }
}

#[derive(Deserialize, Serialize, Default, PartialEq, Debug, Clone)]
pub struct MantaObjectShark {
    pub datacenter: String,
    pub manta_storage_id: String,
}

#[derive(Deserialize, Default, Serialize, PartialEq, Debug, Clone)]
pub struct MantaDirectory {
    pub creator: String,
    pub dirname: String,
    pub headers: Value,
    pub key: String,
    pub mtime: i64,
    pub name: String,
    pub owner: String,
    pub roles: Vec<String>, // TODO: double check this is a String
    #[serde(alias = "type", rename(serialize = "type"), default)]
    pub dir_type: String,
    pub vnode: i64,
}

// Implement Arbitrary traits for testing
impl Arbitrary for MantaObjectShark {
    fn arbitrary<G: Gen>(g: &mut G) -> MantaObjectShark {
        let len = g.gen_range(1, 100) as usize;
        let msid = format!(
            "{}.{}.{}",
            len,
            util::random_string(g, len),
            util::random_string(g, len)
        );
        MantaObjectShark {
            datacenter: util::random_string(g, len),
            manta_storage_id: msid,
        }
    }
}

impl Arbitrary for MantaObject {
    fn arbitrary<G: Gen>(g: &mut G) -> MantaObject {
        let len = g.gen::<u8>() as usize;

        let mut headers_map = Map::new();
        headers_map.insert(
            util::random_string(g, len),
            Value::String(util::random_string(g, len)),
        );

        headers_map.insert(
            util::random_string(g, len),
            Value::String(util::random_string(g, len)),
        );

        headers_map.insert(
            util::random_string(g, len),
            Value::String(util::random_string(g, len)),
        );

        // We don't want negative numbers here, but these fields are
        // indexes and postgres bigint's the max of which is i64::MAX.
        let mtime: i64 = g.gen_range(0, std::i64::MAX);
        let vnode: i64 = g.gen_range(0, std::i64::MAX);

        let content_length: u64 = g.gen();
        let headers = Value::Object(headers_map);
        let key = util::random_string(g, len);
        let creator = util::random_string(g, len);
        let dirname = util::random_string(g, len);
        let name = util::random_string(g, len);
        let owner = Uuid::new_v4().to_string();
        let roles: Vec<String> = vec![util::random_string(g, len)];

        let md5_sum = md5::compute(util::random_string(g, len));
        let content_md5: String = base64::encode(&*md5_sum);

        let etag: String = Uuid::new_v4().to_string();
        let content_type: String = util::random_string(g, len);
        let object_id: String = Uuid::new_v4().to_string();
        let sharks: Vec<MantaObjectShark> = vec![
            MantaObjectShark::arbitrary(g),
            MantaObjectShark::arbitrary(g),
        ];
        let obj_type = String::from("object");

        MantaObject {
            headers,
            key,
            mtime,
            name,
            dirname,
            creator,
            owner,
            roles,
            vnode,
            content_length,
            content_md5,
            content_type,
            object_id,
            etag,
            sharks,
            obj_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::quickcheck;
    use regex::Regex;
    use serde_json;
    use std::str::FromStr;

    quickcheck!(
        fn create_manta_object(mobj: MantaObject) -> bool {
            dbg!(&mobj);

            let str_etag = Uuid::from_str(mobj.etag.as_str());
            let str_owner = Uuid::from_str(mobj.owner.as_str());
            let str_object_id = Uuid::from_str(mobj.object_id.as_str());
            assert!(str_etag.is_ok());
            assert!(str_owner.is_ok());
            assert!(str_object_id.is_ok());

            assert_eq!(str_etag.unwrap().to_string(), mobj.etag);
            assert_eq!(str_owner.unwrap().to_string(), mobj.owner);
            assert_eq!(str_object_id.unwrap().to_string(), mobj.object_id);

            let re = Regex::new(r"(?i)\d+.[a-z0-9-]+.[a-z0-9-]+").unwrap();

            for shark in mobj.sharks.iter() {
                dbg!(&shark.manta_storage_id);
                assert!(re.is_match(&shark.manta_storage_id));
            }

            let to_value = serde_json::to_value(mobj.clone()).unwrap();

            assert!(to_value.get("type").is_some());
            dbg!(&to_value);

            let from_value: MantaObject =
                serde_json::from_value(to_value).unwrap();

            assert_eq!(from_value, mobj);

            true
        }
    );
}
