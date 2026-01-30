/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

use quickcheck::{Arbitrary, Gen};
use quickcheck_helpers::random::string as random_string;
use rebalancer::error::Error;
use reqwest::{self, Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::{thread, time};

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct StorageNode {
    #[serde(alias = "availableMB")]
    pub available_mb: u64,

    #[serde(alias = "percentUsed")]
    pub percent_used: u8,
    pub filesystem: String,
    pub datacenter: String,
    pub manta_storage_id: String,
    pub timestamp: u64, // TODO: can this be deserialized as a datetime type?
}

impl Arbitrary for StorageNode {
    fn arbitrary<G: Gen>(g: &mut G) -> StorageNode {
        let len: usize = (g.next_u32() % 20) as usize;
        StorageNode {
            available_mb: g.next_u64(),
            percent_used: (g.next_u32() % 100) as u8,
            filesystem: random_string(g, len),
            datacenter: random_string(g, len),
            manta_storage_id: format!(
                "{}.{}.{}",
                random_string(g, len),
                random_string(g, len),
                random_string(g, len),
            ),
            timestamp: g.next_u64(),
        }
    }
}

pub struct Storinfo {
    sharks: Arc<Mutex<Option<Vec<StorageNode>>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
    running: Arc<AtomicBool>,
    host: String,
}

///
/// The algorithms available for choosing sharks.
///
///  * Default:
///     Provide a list of storage nodes that have at least a <minimum
///     available capacity> and are not in a <blacklist of datacenters>
pub enum ChooseAlgorithm<'a> {
    Default(&'a DefaultChooseAlgorithm),
}

#[derive(Default)]
pub struct DefaultChooseAlgorithm {
    pub blacklist: Vec<String>,
    pub min_avail_mb: Option<u64>,
}

impl<'a> ChooseAlgorithm<'a> {
    fn choose(&self, sharks: &[StorageNode]) -> Vec<StorageNode> {
        match self {
            ChooseAlgorithm::Default(algo) => algo.method(sharks),
        }
    }
}

impl DefaultChooseAlgorithm {
    fn method(&self, sharks: &[StorageNode]) -> Vec<StorageNode> {
        let mut ret: Vec<StorageNode> = vec![];

        for s in sharks.iter() {
            // Always filter blacklisted datacenters
            if self.blacklist.contains(&s.datacenter) {
                continue;
            }

            // If min_avail_mb is specified, skip sharks with
            // insufficient available space
            if let Some(min_avail_mb) = self.min_avail_mb {
                if s.available_mb < min_avail_mb {
                    continue;
                }
            }

            ret.push(s.to_owned())
        }

        ret
    }
}

impl Storinfo {
    pub fn new(domain: &str) -> Result<Self, Error> {
        let storinfo_domain_name = format!("storinfo.{}", domain);
        Ok(Storinfo {
            running: Arc::new(AtomicBool::new(true)),
            handle: Mutex::new(None),
            sharks: Arc::new(Mutex::new(Some(vec![]))),
            host: storinfo_domain_name,
        })
    }

    /// Populate the storinfo's sharks field, and start the storinfo updater thread.
    pub fn start(&mut self) -> Result<(), Error> {
        let client = Client::new();
        let mut locked_sharks = self.sharks.lock().unwrap_or_else(|e| e.into_inner());
        // TODO: MANTA-4961, don't start job if picker cannot be reached.
        *locked_sharks = Some(fetch_sharks(&client, &self.host));

        let handle = Self::updater(
            self.host.clone(),
            Arc::clone(&self.sharks),
            Arc::clone(&self.running),
        );
        let mut locked_handle = self.handle.lock().unwrap_or_else(|e| e.into_inner());
        *locked_handle = Some(handle);
        Ok(())
    }

    pub fn fini(&self) {
        self.running.swap(false, Ordering::Release);

        if let Some(handle) = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
            handle.join().expect("failed to stop updater thread");
        } else {
            warn!("Updater thread not started");
        }
    }

    /// Get the the Vec<sharks> from the storinfo service.
    pub fn get_sharks(&self) -> Option<Vec<StorageNode>> {
        self.sharks.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn updater(
        host: String,
        sharks: Arc<Mutex<Option<Vec<StorageNode>>>>,
        running: Arc<AtomicBool>,
    ) -> JoinHandle<()> {
        let updater_sharks = Arc::clone(&sharks);
        let keep_running = Arc::clone(&running);

        thread::spawn(move || {
            let sleep_time = time::Duration::from_secs(10);
            let client = Client::new();
            while keep_running.load(Ordering::Acquire) {
                thread::sleep(sleep_time);

                let mut new_sharks = fetch_sharks(&client, &host);
                new_sharks.sort_by(|a, b| a.available_mb.cmp(&b.available_mb));

                let mut old_sharks = updater_sharks.lock().unwrap_or_else(|e| e.into_inner());
                *old_sharks = Some(new_sharks);
                info!("Sharks updated, sleeping for {:?}", sleep_time);
            }
        })
    }
}

// TODO: MANTA-4519
impl SharkSource for Storinfo {
    /// Choose the sharks based on the specified algorithm
    fn choose(&self, algo: &ChooseAlgorithm) -> Option<Vec<StorageNode>> {
        match self.get_sharks() {
            Some(s) => Some(algo.choose(&s)),
            None => None,
        }
    }
}

pub trait SharkSource: Sync + Send {
    fn choose(&self, algo: &ChooseAlgorithm) -> Option<Vec<StorageNode>>;
}

fn fetch_sharks(client: &Client, host: &str) -> Vec<StorageNode> {
    let mut new_sharks = vec![];
    let mut done = false;
    let mut after_id = String::new();
    let base_url = format!("http://{}/storagenodes", host);
    let limit = 100;

    while !done {
        let url = format!("{}?limit={}&after_id={}", base_url, limit, after_id);
        let mut response = match client.get(&url).send() {
            Ok(r) => r,
            Err(e) => {
                error!(
                    "Error requesting list of sharks from storinfo \
                     service: {}",
                    e
                );
                return vec![];
            }
        };

        trace!("Got picker response: {:#?}", response);

        // Storinfo, or our connection to it, is sick.  So instead of
        // breaking out of the loop and possibly returning partial results we
        // return an empty Vec.
        if response.status() != StatusCode::OK {
            error!("Could not contact storinfo service {:#?}", response);
            return vec![];
        }

        let result: Vec<StorageNode> = match response.json() {
            Ok(r) => r,
            Err(e) => {
                error!(
                    "Failed to parse storinfo response as \
                     Vec<StorageNode>: {}",
                    e
                );
                return vec![];
            }
        };

        // .last() returns None on an empty Vec, so we can just break out of the
        // loop if that is the case.
        match result.last() {
            Some(r) => after_id = r.manta_storage_id.clone(),
            None => break,
        }

        if result.len() < limit {
            done = true;
        }

        new_sharks.extend(result);
    }

    debug!("storinfo updated with {} new sharks", new_sharks.len());
    new_sharks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_shark(dc: &str, storage_id: &str, avail_mb: u64) -> StorageNode {
        StorageNode {
            available_mb: avail_mb,
            percent_used: 50,
            filesystem: "/manta".to_string(),
            datacenter: dc.to_string(),
            manta_storage_id: storage_id.to_string(),
            timestamp: 0,
        }
    }

    #[test]
    fn default_choose_no_filter() {
        let algo = DefaultChooseAlgorithm {
            blacklist: vec![],
            min_avail_mb: None,
        };
        let sharks = vec![
            make_shark("dc1", "1.stor", 1000),
            make_shark("dc2", "2.stor", 2000),
        ];
        let result = algo.method(&sharks);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn default_choose_blacklist_filters_dc() {
        let algo = DefaultChooseAlgorithm {
            blacklist: vec!["dc1".to_string()],
            min_avail_mb: Some(0),
        };
        let sharks = vec![
            make_shark("dc1", "1.stor", 1000),
            make_shark("dc2", "2.stor", 2000),
        ];
        let result = algo.method(&sharks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].datacenter, "dc2");
    }

    #[test]
    fn default_choose_min_avail_filters() {
        let algo = DefaultChooseAlgorithm {
            blacklist: vec![],
            min_avail_mb: Some(1500),
        };
        let sharks = vec![
            make_shark("dc1", "1.stor", 1000),
            make_shark("dc2", "2.stor", 2000),
            make_shark("dc3", "3.stor", 500),
        ];
        let result = algo.method(&sharks);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].manta_storage_id, "2.stor");
    }

    #[test]
    fn default_choose_combined_filter() {
        let algo = DefaultChooseAlgorithm {
            blacklist: vec!["dc2".to_string()],
            min_avail_mb: Some(500),
        };
        let sharks = vec![
            make_shark("dc1", "1.stor", 1000),
            make_shark("dc2", "2.stor", 2000),
            make_shark("dc3", "3.stor", 100),
        ];
        let result = algo.method(&sharks);
        // dc2 is blacklisted, dc3 has < 500MB
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].manta_storage_id, "1.stor");
    }

    #[test]
    fn default_choose_empty_input() {
        let algo = DefaultChooseAlgorithm {
            blacklist: vec![],
            min_avail_mb: Some(100),
        };
        let sharks: Vec<StorageNode> = vec![];
        let result = algo.method(&sharks);
        assert!(result.is_empty());
    }

    #[test]
    fn default_choose_all_blacklisted() {
        let algo = DefaultChooseAlgorithm {
            blacklist: vec!["dc1".to_string(), "dc2".to_string()],
            min_avail_mb: Some(0),
        };
        let sharks = vec![
            make_shark("dc1", "1.stor", 1000),
            make_shark("dc2", "2.stor", 2000),
        ];
        let result = algo.method(&sharks);
        assert!(result.is_empty());
    }

    #[test]
    fn default_choose_no_min_avail_still_filters_blacklist() {
        // When min_avail_mb is None, blacklisted sharks are still filtered
        let algo = DefaultChooseAlgorithm {
            blacklist: vec!["dc1".to_string()],
            min_avail_mb: None,
        };
        let sharks = vec![
            make_shark("dc1", "1.stor", 1000),
            make_shark("dc2", "2.stor", 2000),
        ];
        let result = algo.method(&sharks);
        // dc1 is blacklisted, only dc2 should pass
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].datacenter, "dc2");
    }

    #[test]
    fn storage_node_serialization_roundtrip() {
        let shark = make_shark("dc1", "1.stor.domain", 5000);
        let json = serde_json::to_string(&shark).unwrap();
        let deserialized: StorageNode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.datacenter, "dc1");
        assert_eq!(deserialized.manta_storage_id, "1.stor.domain");
        assert_eq!(deserialized.available_mb, 5000);
    }

    #[test]
    fn storage_node_deserialization_camel_case() {
        // Storinfo API returns camelCase field names
        let json = r#"{
            "availableMB": 3000,
            "percentUsed": 42,
            "filesystem": "/manta",
            "datacenter": "us-east",
            "manta_storage_id": "1.stor.east",
            "timestamp": 1234567890
        }"#;
        let shark: StorageNode = serde_json::from_str(json).unwrap();
        assert_eq!(shark.available_mb, 3000);
        assert_eq!(shark.percent_used, 42);
    }
}
