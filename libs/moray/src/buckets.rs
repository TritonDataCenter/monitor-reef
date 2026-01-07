/*
 * Copyright 2019 Joyent, Inc.
 */

use fast_rpc::{client as fast_client, protocol::FastMessageId};
use serde::{Deserialize, Serialize};
use serde_json::{self, Value, json};
use std::io::Error;
use std::net::TcpStream;
use uuid::Uuid;

/*
 * === Buckets ===
 */

// Options that are properties of the bucket itself.  Not the rpc method options for bucket
// manipulation.
#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
pub struct BucketOptions {
    #[serde(default)]
    version: u32,

    #[serde(alias = "guaranteeOrder", default)]
    guarantee_order: bool,

    #[serde(alias = "syncUpdates", default)]
    sync_updates: bool,
}

// TODO: We should be able to skip this step with per field deserializers
#[derive(Deserialize, Serialize, Debug, Clone)]
struct BucketIntermediate {
    index: String,
    mtime: String,
    name: String,
    options: String,
    post: String,
    pre: String,
}

#[derive(Deserialize, Serialize, PartialEq, Debug, Clone)]
pub struct Bucket {
    index: Value,
    mtime: String,
    name: String,
    options: BucketOptions,
    post: Vec<String>,
    pre: Vec<String>,
}

pub enum Methods {
    List,
    Get,
    Create,
}

impl Methods {
    fn method(&self) -> String {
        match *self {
            Methods::List => String::from("listBuckets"),
            Methods::Get => String::from("getBucket"),
            Methods::Create => String::from("createBucket"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MethodOptions {
    pub req_id: String, // UUID as string,
}

impl Default for MethodOptions {
    fn default() -> Self {
        Self {
            req_id: Uuid::new_v4().to_string(),
        }
    }
}

fn decode_bucket<F>(fm_data: &Value, mut cb: F) -> Result<(), Error>
where
    F: FnMut(Bucket) -> Result<(), Error>,
{
    let resp_data: Vec<Value> =
        serde_json::from_value(fm_data.clone()).unwrap();

    let result = Ok(());

    resp_data.iter().fold(result, |_r, bucket_data| {
        serde_json::from_value::<BucketIntermediate>(bucket_data.clone())
            .map_err(Error::other)
            .and_then(|bi| {
                cb(Bucket {
                    name: bi.name,
                    index: serde_json::from_str(bi.index.as_str()).unwrap(),
                    mtime: bi.mtime,
                    options: serde_json::from_str(bi.options.as_str()).unwrap(),
                    post: serde_json::from_str(bi.post.as_str()).unwrap(),
                    pre: serde_json::from_str(bi.pre.as_str()).unwrap(),
                })
            })
    })
}

pub fn create_bucket(
    stream: &mut TcpStream,
    name: &str,
    config: Value,
    opts: MethodOptions,
) -> Result<(), Error> {
    let arg = json!([name, config, opts]);
    let mut msg_id = FastMessageId::new();

    // TODO: ideally we'd try to get the bucket first, and if that fails then
    // create it.
    fast_client::send(Methods::Create.method(), arg, &mut msg_id, stream)
        .and_then(|_| {
            fast_client::receive(stream, |resp| {
                dbg!(resp); // createBucket returns empty response
                Ok(())
            })
        })?;

    Ok(())
}

pub fn get_list_buckets<F>(
    stream: &mut TcpStream,
    name: &str,
    opts: MethodOptions,
    method: Methods,
    mut bucket_handler: F,
) -> Result<(), Error>
where
    F: FnMut(&Bucket) -> Result<(), Error>, //FnOnce?
{
    let mut arg = json!([opts]);
    let mut msg_id = FastMessageId::new();

    match method {
        Methods::Get => {
            arg = json!([opts, name]);
        }
        Methods::List => {
            // Use default
        }
        _ => return Err(Error::other("Unsupported Method")),
    }

    fast_client::send(method.method(), arg, &mut msg_id, stream).and_then(
        |_| {
            fast_client::receive(stream, |resp| {
                decode_bucket(&resp.data.d, |b| bucket_handler(&b))
            })
        },
    )?;

    Ok(())
}

/*
 * ======== Tests
 */
#[cfg(test)]
mod tests {
    use super::*;
    use quickcheck::{Arbitrary, Gen, quickcheck};
    use rand::Rng;
    use rand::distr::Alphanumeric;
    use serde_json::Map;

    pub fn random_string(_g: &mut Gen, len: usize) -> String {
        rand::rng()
            .sample_iter(Alphanumeric)
            .take(len)
            .map(char::from)
            .collect()
    }

    impl Arbitrary for BucketOptions {
        fn arbitrary(_g: &mut Gen) -> BucketOptions {
            let mut rng = rand::rng();
            let version = rng.random::<u32>();
            let guarantee_order = rng.random::<bool>();
            let sync_updates = rng.random::<bool>();

            BucketOptions {
                version,
                guarantee_order,
                sync_updates,
            }
        }
    }

    impl Arbitrary for Bucket {
        fn arbitrary(g: &mut Gen) -> Bucket {
            let mut rng = rand::rng();
            let index_len = rng.random::<u8>() as usize;
            let mtime_len = rng.random::<u8>() as usize;
            let name_len = rng.random::<u8>() as usize;
            let post_len = rng.random::<u8>() as usize;
            let pre_len = rng.random::<u8>() as usize;

            // TODO: further randomize index
            let index = json!({
                random_string(g, index_len): random_string(g, index_len),
                random_string(g, index_len): random_string(g, index_len),
                random_string(g, index_len): random_string(g, index_len),
            });

            let mtime = random_string(g, mtime_len);
            let name = random_string(g, name_len);
            let options = BucketOptions::arbitrary(g);
            let post = vec![random_string(g, post_len)];
            let pre = vec![random_string(g, pre_len)];

            Bucket {
                index,
                mtime,
                name,
                options,
                post,
                pre,
            }
        }
    }

    fn create_intermediate_bucket(bucket: Bucket) -> BucketIntermediate {
        BucketIntermediate {
            index: serde_json::to_string(&bucket.index).unwrap(),
            mtime: bucket.mtime,
            name: bucket.name,
            options: serde_json::to_string(&bucket.options).unwrap(),
            post: serde_json::to_string(&bucket.post).unwrap(),
            pre: serde_json::to_string(&bucket.pre).unwrap(),
        }
    }

    // TODO: Create array of multiple buckets
    quickcheck! {
        fn decode_bucket_test(bucket: Bucket) -> bool {
            let mut pass = false;
            let bucket_clone = bucket.clone();
            let bi = create_intermediate_bucket(bucket);
            let mut map = Map::new();

            dbg!(&bi);
            map.insert(String::from("index"), Value::String(bi.index));
            map.insert(String::from("mtime"), Value::String(bi.mtime));
            map.insert(String::from("name"), Value::String(bi.name));
            map.insert(String::from("options"), Value::String(bi.options));
            map.insert(String::from("post"), Value::String(bi.post));
            map.insert(String::from("pre"), Value::String(bi.pre));

            let obj = Value::Object(map);
            let input = Value::Array(vec![obj]);
            dbg!(&input);
            match decode_bucket(&input, |b| {
                dbg!(&b);
                pass = b == bucket_clone;
                Ok(())
            }) {
                Ok(()) => pass,
                Err(_e) => false
            }
        }
    }
}
