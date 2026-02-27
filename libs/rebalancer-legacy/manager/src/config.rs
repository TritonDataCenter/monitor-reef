/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

extern crate clap;

use std::fs::File;
use std::io::BufReader;
use std::sync::{Arc, Barrier, Mutex};

use crossbeam_channel::TrySendError;
use serde::{de, de::Error as de_Error, Deserialize, Deserializer};
use signal_hook::{self, iterator::Signals};

use rebalancer::error::Error;
use rebalancer::util;
use slog::Level;
use std::thread;
use std::thread::JoinHandle;

static DEFAULT_CONFIG_PATH: &str = "/opt/smartdc/rebalancer/config.json";

// TODO: Determine max and min values for each (MANTA-5284)

// The maximum number of tasks we will send in a single assignment to the agent.
static DEFAULT_MAX_TASKS_PER_ASSIGNMENT: usize = 50;

// The maximum number of threads that will be used for metadata updates.
// Each thread has its own hash of moray clients.
static DEFAULT_MAX_METADATA_UPDATE_THREADS: usize = 10;

// The maximum number of sharks we will use as destinations for things like
// evacuate job.  This is the top 5 of an ordered list which could mean a
// different set of sharks each time we get a snapshot from the storinfo zone.
static DEFAULT_MAX_SHARKS: usize = 5;

// The number of elements the bounded metadata update queue will be set to.
// For evacuate jobs this represents the number of assignments that can be in
// the post processing state waiting for a metadata update thread to become
// available.
static DEFAULT_STATIC_QUEUE_DEPTH: usize = 10;

// The maximum amount of time in seconds that an assignment should remain in
// memory before it is posted to an agent.  This is not a hard and fast rule.
// This will only be checked synchronously every time we gather another set of
// destination sharks.
static DEFAULT_MAX_ASSIGNMENT_AGE: u64 = 600;

// The chunk size used when scanning the metadata tier or during a retry when
// reading from the local database.
static DEFAULT_METADATA_READ_CHUNK_SIZE: usize = 10000;

// Default maximum number of per-shard threads that we will use to scan the
// metadata tier.
static DEFAULT_MAX_METADATA_READ_THREADS: usize = 10;

// Default delay in milliseconds between retries when getting the shark list.
static DEFAULT_SHARK_LIST_RETRY_DELAY_MS: u64 = 500;

pub const MAX_TUNABLE_MD_UPDATE_THREADS: usize = 250;

#[derive(Deserialize, Default, Debug, Clone)]
pub struct Shard {
    pub host: String,
}

// Until we can determine a reasonable set of defaults and limits these
// tunables are intentionally not exposed in the documentation.
#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(default)]
pub struct ConfigOptions {
    pub max_tasks_per_assignment: usize,
    pub max_metadata_update_threads: usize,
    pub max_sharks: usize,
    pub use_static_md_update_threads: bool,
    pub static_queue_depth: usize,
    pub max_assignment_age: u64,
    pub use_batched_updates: bool,
    pub md_read_chunk_size: usize,
    pub max_md_read_threads: usize,
    pub shark_list_retry_delay_ms: u64,
}

impl Default for ConfigOptions {
    fn default() -> ConfigOptions {
        ConfigOptions {
            max_tasks_per_assignment: DEFAULT_MAX_TASKS_PER_ASSIGNMENT,
            max_metadata_update_threads: DEFAULT_MAX_METADATA_UPDATE_THREADS,
            max_sharks: DEFAULT_MAX_SHARKS,
            use_static_md_update_threads: false,
            static_queue_depth: DEFAULT_STATIC_QUEUE_DEPTH,
            max_assignment_age: DEFAULT_MAX_ASSIGNMENT_AGE,
            use_batched_updates: true,
            md_read_chunk_size: DEFAULT_METADATA_READ_CHUNK_SIZE,
            max_md_read_threads: DEFAULT_MAX_METADATA_READ_THREADS,
            shark_list_retry_delay_ms: DEFAULT_SHARK_LIST_RETRY_DELAY_MS,
        }
    }
}

/// A single mdapi shard endpoint, mirroring the existing `Shard` struct.
///
/// Populated from `BUCKETS_MORAY_SHARDS` SAPI metadata, which contains
/// entries like `{"host": "1.buckets-mdapi.coal.joyent.us"}`.
#[derive(Deserialize, Default, Debug, Clone)]
pub struct MdapiShard {
    pub host: String,
}

/// Configuration for manta-buckets-mdapi client integration
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct MdapiConfig {
    /// Mdapi shard endpoints. Each entry corresponds to one mdapi instance.
    /// When non-empty, mdapi is used for bucket object discovery and updates.
    pub shards: Vec<MdapiShard>,
    /// Connection timeout in milliseconds
    pub connection_timeout_ms: u64,
    /// Maximum number of objects to process in a single batch update.
    ///
    /// Large batches can overload the mdapi server and cause timeouts.
    /// If a batch exceeds this limit, it will be automatically chunked
    /// into smaller batches. Default: 100.
    pub max_batch_size: usize,
    /// Timeout in milliseconds for individual update operations.
    ///
    /// This timeout applies to each individual object update within a batch.
    /// If an update takes longer than this, it will be marked as failed.
    /// Default: 30000 (30 seconds).
    pub operation_timeout_ms: u64,
    /// Maximum number of retries for failed operations.
    ///
    /// When an operation fails, it will be retried up to this many times
    /// with exponential backoff between attempts. Set to 0 to disable retries.
    /// Default: 3.
    pub max_retries: u32,
    /// Initial backoff delay in milliseconds between retry attempts.
    ///
    /// The delay doubles after each retry (exponential backoff) up to
    /// max_backoff_ms. Default: 100ms.
    pub initial_backoff_ms: u64,
    /// Maximum backoff delay in milliseconds.
    ///
    /// The exponential backoff will not exceed this value.
    /// Default: 5000ms (5 seconds).
    pub max_backoff_ms: u64,
}

/// Default maximum batch size for mdapi updates
pub const DEFAULT_MDAPI_MAX_BATCH_SIZE: usize = 100;

/// Default operation timeout in milliseconds (30 seconds)
pub const DEFAULT_MDAPI_OPERATION_TIMEOUT_MS: u64 = 30000;

/// Default maximum number of retries
pub const DEFAULT_MDAPI_MAX_RETRIES: u32 = 3;

/// Default initial backoff delay in milliseconds
pub const DEFAULT_MDAPI_INITIAL_BACKOFF_MS: u64 = 100;

/// Default maximum backoff delay in milliseconds (5 seconds)
pub const DEFAULT_MDAPI_MAX_BACKOFF_MS: u64 = 5000;

impl Default for MdapiConfig {
    fn default() -> Self {
        MdapiConfig {
            shards: vec![],
            connection_timeout_ms: 5000,
            max_batch_size: DEFAULT_MDAPI_MAX_BATCH_SIZE,
            operation_timeout_ms: DEFAULT_MDAPI_OPERATION_TIMEOUT_MS,
            max_retries: DEFAULT_MDAPI_MAX_RETRIES,
            initial_backoff_ms: DEFAULT_MDAPI_INITIAL_BACKOFF_MS,
            max_backoff_ms: DEFAULT_MDAPI_MAX_BACKOFF_MS,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub domain_name: String,

    /// The `parse_config()` method sorts these shards by shard number.  If
    /// this struct is created ad-hoc without parsing the config file directly
    /// the `min_shard_num()` and `max_shard_num()` functions will not return
    /// accurate values, so this field must remain private.
    shards: Vec<Shard>,

    #[serde(default)]
    pub snaplink_cleanup_required: bool,

    #[serde(default)]
    pub options: ConfigOptions,

    #[serde(default)]
    pub mdapi: MdapiConfig,

    /// Use direct PostgreSQL access (rebalancer-postgres) for
    /// sharkspotter instead of moray RPC.  Requires provisioning
    /// rebalancer-postgres instances via pgclone.sh.
    #[serde(default)]
    pub direct_db: bool,

    #[serde(default = "Config::default_port")]
    pub listen_port: u16,

    #[serde(default = "Config::default_max_fill_percentage")]
    pub max_fill_percentage: u32,

    #[serde(
        deserialize_with = "log_level_deserialize",
        default = "Config::default_log_level"
    )]
    pub log_level: Level,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            domain_name: String::new(),
            shards: vec![],
            snaplink_cleanup_required: false,
            options: ConfigOptions::default(),
            mdapi: MdapiConfig::default(),
            direct_db: false,
            listen_port: 80,
            max_fill_percentage: 100,
            log_level: Level::Debug,
        }
    }
}

fn log_level_deserialize<'de, D>(deserializer: D) -> Result<Level, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?.to_lowercase();
    match s.as_str() {
        "critical" | "crit" => Ok(Level::Critical),
        "error" => Ok(Level::Error),
        "warning" | "warn" => Ok(Level::Warning),
        "info" => Ok(Level::Info),
        "debug" => Ok(Level::Debug),
        "trace" => Ok(Level::Trace),
        _ => Err(D::Error::invalid_value(
            de::Unexpected::Str(s.as_str()),
            &"slog Level string",
        )),
    }
}

impl Config {
    /// This method assumes the `shards: Vec<Shard>` is sorted.
    pub fn min_shard_num(&self) -> u32 {
        util::shard_host2num(self.shards.first().expect("first").host.as_str())
    }

    /// This method assumes the `shards: Vec<Shard>` is sorted.
    pub fn max_shard_num(&self) -> u32 {
        util::shard_host2num(self.shards.last().expect("last").host.as_str())
    }

    fn default_port() -> u16 {
        80
    }

    fn default_max_fill_percentage() -> u32 {
        100
    }

    fn default_log_level() -> Level {
        Level::Debug
    }

    pub fn parse_config(config_path: &Option<String>) -> Result<Config, Error> {
        let config_path = config_path
            .to_owned()
            .unwrap_or_else(|| DEFAULT_CONFIG_PATH.to_string());
        let file = File::open(config_path)?;
        let reader = BufReader::new(file);
        let mut config: Config = serde_json::from_reader(reader)?;

        if config.shards.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Configuration must contain at least one shard",
            )
            .into());
        }

        // Both min_shard_num() and max_shard_num() depend on this vector
        // being sorted.  Do not change or remove this line without making a
        // complementary change to those two functions.
        config
            .shards
            .sort_by_key(|s| util::shard_host2num(s.host.as_str()));

        Ok(config)
    }

    fn config_updater(
        config_update_rx: crossbeam_channel::Receiver<()>,
        update_config: Arc<Mutex<Config>>,
        config_file: Option<String>,
    ) -> JoinHandle<()> {
        // Capture the logger before spawning so the new thread can use it.
        // slog-scope uses thread-local storage, so spawned threads don't
        // inherit the logger automatically.
        let logger = slog_scope::logger();
        thread::Builder::new()
            .name(String::from("config updater"))
            .spawn(move || {
                slog_scope::scope(&logger, || loop {
                    match config_update_rx.recv() {
                        Ok(()) => {
                            let new_config =
                                match Config::parse_config(&config_file) {
                                    Ok(c) => c,
                                    Err(e) => {
                                        error!(
                                            "Error parsing config after signal \
                                             received. Not updating: {}",
                                            e
                                        );
                                        continue;
                                    }
                                };
                            let mut config_lock = update_config
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());

                            *config_lock = new_config;
                            debug!(
                                "Configuration has been updated: {:#?}",
                                *config_lock
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Channel has been disconnected, exiting \
                                 thread: {}",
                                e
                            );
                            return;
                        }
                    }
                })
            })
            .expect("Start config updater")
    }

    // Run a thread that listens for the SIGUSR1 signal which config-agent
    // should be sending us via SMF when the config file is updated.  When a
    // signal is trapped it simply sends an empty message to the updater thread
    // which handles updating the configuration state in memory.  We don't want
    // to block or take any locks here because the signal is asynchronous.
    fn config_update_signal_handler(
        config_update_tx: crossbeam_channel::Sender<()>,
        update_barrier: Arc<Barrier>,
    ) -> JoinHandle<()> {
        thread::Builder::new()
            .name(String::from("config update signal handler"))
            .spawn(move || {
                _config_update_signal_handler(config_update_tx, update_barrier)
            })
            .expect("Start Config Update Signal Handler")
    }

    // This thread spawns two other threads.  One of them handles the SIGUSR1
    // signal and in turn notifies the other that the config file needs to be
    // re-parsed.  This function returns a JoinHandle that will only join
    // after both of the other threads have completed.
    pub fn start_config_watcher(
        config: Arc<Mutex<Config>>,
        config_file: Option<String>,
    ) -> JoinHandle<()> {
        thread::Builder::new()
            .name("config watcher".to_string())
            .spawn(move || {
                let (update_tx, update_rx) = crossbeam_channel::bounded(1);
                let barrier = Arc::new(Barrier::new(2));
                let update_barrier = Arc::clone(&barrier);
                let sig_handler_handle = Config::config_update_signal_handler(
                    update_tx,
                    update_barrier,
                );
                barrier.wait();

                let update_config = Arc::clone(&config);
                let config_updater_handle = Config::config_updater(
                    update_rx,
                    update_config,
                    config_file,
                );

                config_updater_handle.join().expect("join config updater");
                sig_handler_handle.join().expect("join signal handler");
            })
            .expect("start config watcher")
    }
}

fn _config_update_signal_handler(
    config_update_tx: crossbeam_channel::Sender<()>,
    update_barrier: Arc<Barrier>,
) {
    let signals =
        Signals::new(&[signal_hook::SIGUSR1]).expect("register signals");

    update_barrier.wait();

    for signal in signals.forever() {
        trace!("Signal Received: {}", signal);
        match signal {
            signal_hook::SIGUSR1 => {
                // If there is already a message in the buffer
                // (i.e. TrySendError::Full), then the updater
                // thread will be doing an update anyway so no
                // sense in clogging things up further.
                match config_update_tx.try_send(()) {
                    Err(TrySendError::Disconnected(_)) => {
                        warn!("config_update listener is closed");
                        break;
                    }
                    Ok(()) | Err(TrySendError::Full(_)) => {
                        continue;
                    }
                }
            }
            _ => continue, // Only SIGUSR1 registered; defensive guard
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TestConfig;
    use lazy_static::lazy_static;
    use mustache::MapBuilder;
    use std::fs::File;
    use std::io::Read;

    lazy_static! {
        static ref INITIALIZED: Mutex<bool> = Mutex::new(false);
    }

    fn unit_test_init() {
        let mut init = INITIALIZED.lock().unwrap_or_else(|e| e.into_inner());
        if *init {
            return;
        }

        *init = true;

        thread::spawn(move || {
            let _guard = util::init_global_logger(None);
            loop {
                // Loop around ::park() in the event of spurious wake ups.
                std::thread::park();
            }
        });
    }

    #[test]
    fn min_max_shards() {
        unit_test_init();

        let vars = MapBuilder::new()
            .insert_str("DOMAIN_NAME", "fake.joyent.us")
            .insert_bool("SNAPLINK_CLEANUP_REQUIRED", true)
            .insert_vec("INDEX_MORAY_SHARDS", |builder| {
                builder.push_map(|bld| {
                    bld.insert_str("host", "3.fake.joyent.us")
                        .insert_bool("last", true)
                })
            })
            .build();
        let mut test_config = TestConfig::with_vars(&vars);

        assert_eq!(test_config.config.min_shard_num(), 3);
        assert_eq!(test_config.config.max_shard_num(), 3);

        let vars = MapBuilder::new()
            .insert_str("DOMAIN_NAME", "fake.joyent.us")
            .insert_bool("SNAPLINK_CLEANUP_REQUIRED", true)
            .insert_vec("INDEX_MORAY_SHARDS", |builder| {
                builder
                    .push_map(|bld| bld.insert_str("host", "99.fake.joyent.us"))
                    .push_map(|bld| {
                        bld.insert_str("host", "1000.fake.joyent.us")
                    })
                    .push_map(|bld| {
                        bld.insert_str("host", "200.fake.joyent.us")
                    })
                    .push_map(|bld| bld.insert_str("host", "2.fake.joyent.us"))
                    .push_map(|bld| {
                        bld.insert_str("host", "100.fake.joyent.us")
                            .insert_bool("last", true)
                    })
            })
            .build();

        test_config.update_with_vars(&vars);

        assert_eq!(test_config.config.min_shard_num(), 2);
        assert_eq!(test_config.config.max_shard_num(), 1000);
        // TestConfig automatically cleans up the temp file when dropped
    }

    #[test]
    fn config_basic_test() {
        unit_test_init();
        let test_config = TestConfig::new();

        // The template does not have a listen_port entry, so it should
        // default to 80.
        assert_eq!(test_config.config.listen_port, 80);

        File::open(&test_config.config_path)
            .and_then(|mut f| {
                let mut config_file = String::new();

                f.read_to_string(&mut config_file).expect("config file");

                assert!(config_file.contains("options"));
                assert!(config_file.contains("max_tasks_per_assignment"));
                assert!(config_file.contains("max_metadata_update_threads"));
                assert!(config_file.contains("max_sharks"));
                assert!(config_file.contains("use_static_md_update_threads"));
                assert!(config_file.contains("static_queue_depth"));
                assert!(config_file.contains("max_assignment_age"));

                Ok(())
            })
            .expect("config_basic_test");
        // TestConfig automatically cleans up the temp file when dropped
    }

    #[test]
    fn config_options_test() {
        unit_test_init();

        let file_contents = r#"{
                "options": {
                    "max_tasks_per_assignment": 1111,
                    "max_metadata_update_threads": 2222,
                    "max_sharks": 3333
                },
                "domain_name": "perf1.scloud.host",
                "shards": [
                    {
                        "host": "1.moray.perf1.scloud.host"
                    }
                ]
            }
        "#;

        let test_config = TestConfig::from_contents(file_contents.as_bytes());

        assert_eq!(test_config.config.options.max_tasks_per_assignment, 1111);
        assert_eq!(test_config.config.options.max_metadata_update_threads, 2222);
        assert_eq!(test_config.config.options.max_sharks, 3333);
        assert_eq!(test_config.config.options.use_static_md_update_threads, false);
        assert_eq!(
            test_config.config.options.static_queue_depth,
            DEFAULT_STATIC_QUEUE_DEPTH
        );
        assert_eq!(
            test_config.config.options.max_assignment_age,
            DEFAULT_MAX_ASSIGNMENT_AGE
        );
        // TestConfig automatically cleans up the temp file when dropped
    }

    #[test]
    fn missing_snaplink_cleanup_required() {
        unit_test_init();

        let vars = MapBuilder::new()
            .insert_str("DOMAIN_NAME", "fake.joyent.us")
            .insert_vec("INDEX_MORAY_SHARDS", |builder| {
                builder.push_map(|bld| {
                    bld.insert_str("host", "1.fake.joyent.us")
                        .insert_bool("last", true)
                })
            })
            .build();

        let test_config = TestConfig::with_vars(&vars);

        assert_eq!(test_config.config.snaplink_cleanup_required, false);
        // TestConfig automatically cleans up the temp file when dropped
    }

    // Verify that config_updater re-parses the config file and updates
    // the in-memory Config when notified via channel.  This exercises
    // the same code path as the production SIGUSR1 handler without
    // sending a process-wide signal.
    //
    // 1. Create a config (both file and in memory).
    // 2. Start config_updater directly with a test-controlled channel.
    // 3. Update the config file created in step 1.
    // 4. Send a notification on the channel (what the signal handler
    //    would do in production).
    // 5. Confirm that our in-memory config reflects the changes from
    //    step 3.
    #[test]
    fn channel_config_update() {
        unit_test_init();

        // Generate a config with snaplink_cleanup_required=true.
        let mut test_config = TestConfig::new();
        let config = Arc::new(Mutex::new(test_config.config.clone()));

        assert!(
            config
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snaplink_cleanup_required
        );

        // Create a channel to drive config_updater directly,
        // bypassing the SIGUSR1 signal handler.
        let (update_tx, update_rx) = crossbeam_channel::bounded(1);
        let updater_handle = Config::config_updater(
            update_rx,
            Arc::clone(&config),
            Some(test_config.path_string()),
        );

        // Change SNAPLINK_CLEANUP_REQUIRED to false in the file.
        let vars = MapBuilder::new()
            .insert_str("DOMAIN_NAME", "fake.joyent.us")
            .insert_bool("SNAPLINK_CLEANUP_REQUIRED", false)
            .insert_vec("INDEX_MORAY_SHARDS", |builder| {
                builder.push_map(|bld| {
                    bld.insert_str("host", "1.fake.joyent.us")
                        .insert_bool("last", true)
                })
            })
            .build();
        test_config.update_with_vars(&vars);

        // Notify config_updater that the file changed.
        update_tx.send(()).expect("send config update notification");

        // Give the updater thread time to re-parse and apply.
        thread::sleep(std::time::Duration::from_millis(500));

        // Assert that our in-memory config's snaplink_cleanup_required
        // field has changed to false.
        let check_config = config.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(check_config.snaplink_cleanup_required, false);

        // Drop the sender so the updater thread exits cleanly.
        drop(update_tx);
        drop(check_config);
        updater_handle.join().expect("join config updater");
        // TestConfig automatically cleans up the temp file when dropped
    }

    // =========================================================================
    // Tests for MdapiConfig defaults and deserialization
    // =========================================================================

    #[test]
    fn mdapi_config_defaults() {
        let config = MdapiConfig::default();
        assert!(config.shards.is_empty());
        assert_eq!(config.connection_timeout_ms, 5000);
        assert_eq!(config.max_batch_size, DEFAULT_MDAPI_MAX_BATCH_SIZE);
        assert_eq!(config.operation_timeout_ms, DEFAULT_MDAPI_OPERATION_TIMEOUT_MS);
        assert_eq!(config.max_retries, DEFAULT_MDAPI_MAX_RETRIES);
        assert_eq!(config.initial_backoff_ms, DEFAULT_MDAPI_INITIAL_BACKOFF_MS);
        assert_eq!(config.max_backoff_ms, DEFAULT_MDAPI_MAX_BACKOFF_MS);
    }

    #[test]
    fn mdapi_config_deserialization() {
        let json = r#"{
            "shards": [
                {"host": "1.buckets-mdapi.us-east.joyent.us"},
                {"host": "2.buckets-mdapi.us-east.joyent.us"}
            ],
            "connection_timeout_ms": 10000
        }"#;
        let config: MdapiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.shards.len(), 2);
        assert_eq!(
            config.shards[0].host,
            "1.buckets-mdapi.us-east.joyent.us"
        );
        assert_eq!(config.connection_timeout_ms, 10000);
    }

    #[test]
    fn mdapi_config_empty_json_uses_defaults() {
        let json = "{}";
        let config: MdapiConfig = serde_json::from_str(json).unwrap();
        assert!(config.shards.is_empty());
        assert_eq!(config.max_batch_size, DEFAULT_MDAPI_MAX_BATCH_SIZE);
        assert_eq!(config.operation_timeout_ms, DEFAULT_MDAPI_OPERATION_TIMEOUT_MS);
        assert_eq!(config.max_retries, DEFAULT_MDAPI_MAX_RETRIES);
        assert_eq!(config.initial_backoff_ms, DEFAULT_MDAPI_INITIAL_BACKOFF_MS);
        assert_eq!(config.max_backoff_ms, DEFAULT_MDAPI_MAX_BACKOFF_MS);
    }

    #[test]
    fn mdapi_config_batch_and_timeout_override() {
        let json = r#"{
            "shards": [{"host": "1.buckets-mdapi.host"}],
            "max_batch_size": 50,
            "operation_timeout_ms": 60000
        }"#;
        let config: MdapiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.max_batch_size, 50);
        assert_eq!(config.operation_timeout_ms, 60000);
    }

    #[test]
    fn mdapi_config_retry_override() {
        let json = r#"{
            "shards": [{"host": "1.buckets-mdapi.host"}],
            "max_retries": 5,
            "initial_backoff_ms": 200,
            "max_backoff_ms": 10000
        }"#;
        let config: MdapiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.max_retries, 5);
        assert_eq!(config.initial_backoff_ms, 200);
        assert_eq!(config.max_backoff_ms, 10000);
    }

    #[test]
    fn mdapi_config_disable_retries() {
        let json = r#"{
            "shards": [{"host": "1.buckets-mdapi.host"}],
            "max_retries": 0
        }"#;
        let config: MdapiConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.max_retries, 0);
    }

    // =========================================================================
    // Tests for ConfigOptions defaults
    // =========================================================================

    #[test]
    fn config_options_defaults() {
        let opts = ConfigOptions::default();
        assert_eq!(opts.max_tasks_per_assignment, DEFAULT_MAX_TASKS_PER_ASSIGNMENT);
        assert_eq!(opts.max_metadata_update_threads, DEFAULT_MAX_METADATA_UPDATE_THREADS);
        assert_eq!(opts.max_sharks, DEFAULT_MAX_SHARKS);
        assert_eq!(opts.use_static_md_update_threads, false);
        assert_eq!(opts.static_queue_depth, DEFAULT_STATIC_QUEUE_DEPTH);
        assert_eq!(opts.max_assignment_age, DEFAULT_MAX_ASSIGNMENT_AGE);
        assert_eq!(opts.use_batched_updates, true);
        assert_eq!(opts.md_read_chunk_size, DEFAULT_METADATA_READ_CHUNK_SIZE);
        assert_eq!(opts.max_md_read_threads, DEFAULT_MAX_METADATA_READ_THREADS);
    }

    #[test]
    fn config_options_partial_override() {
        let json = r#"{"max_sharks": 99, "max_assignment_age": 300}"#;
        let opts: ConfigOptions = serde_json::from_str(json).unwrap();
        assert_eq!(opts.max_sharks, 99);
        assert_eq!(opts.max_assignment_age, 300);
        // Non-overridden fields should keep defaults
        assert_eq!(opts.max_tasks_per_assignment, DEFAULT_MAX_TASKS_PER_ASSIGNMENT);
        assert_eq!(opts.use_batched_updates, true);
    }

    // =========================================================================
    // Tests for log_level_deserialize
    // =========================================================================

    #[test]
    fn log_level_deserialize_all_variants() {
        let cases = vec![
            (r#""critical""#, Level::Critical),
            (r#""crit""#, Level::Critical),
            (r#""error""#, Level::Error),
            (r#""warning""#, Level::Warning),
            (r#""warn""#, Level::Warning),
            (r#""info""#, Level::Info),
            (r#""debug""#, Level::Debug),
            (r#""trace""#, Level::Trace),
        ];

        for (input, expected) in cases {
            // Wrap in a struct since log_level_deserialize is a custom fn
            let json = format!(r#"{{"log_level": {}}}"#, input);
            #[derive(Deserialize)]
            struct TestLogLevel {
                #[serde(deserialize_with = "log_level_deserialize")]
                log_level: Level,
            }
            let parsed: TestLogLevel =
                serde_json::from_str(&json).unwrap();
            assert_eq!(parsed.log_level, expected, "input: {}", input);
        }
    }

    #[test]
    fn log_level_deserialize_case_insensitive() {
        let json = r#"{"log_level": "INFO"}"#;
        #[derive(Deserialize)]
        struct TestLogLevel {
            #[serde(deserialize_with = "log_level_deserialize")]
            log_level: Level,
        }
        let parsed: TestLogLevel = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.log_level, Level::Info);
    }

    #[test]
    fn log_level_deserialize_invalid_rejects() {
        let json = r#"{"log_level": "nonsense"}"#;
        #[derive(Deserialize)]
        struct TestLogLevel {
            #[serde(deserialize_with = "log_level_deserialize")]
            log_level: Level,
        }
        let result: Result<TestLogLevel, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // =========================================================================
    // Tests for Config defaults
    // =========================================================================

    #[test]
    fn config_defaults() {
        let config = Config::default();
        assert_eq!(config.domain_name, "");
        assert_eq!(config.listen_port, 80);
        assert_eq!(config.max_fill_percentage, 100);
        assert_eq!(config.log_level, Level::Debug);
        assert_eq!(config.snaplink_cleanup_required, false);
        assert!(config.mdapi.shards.is_empty());
    }

    #[test]
    fn config_full_json_deserialization() {
        unit_test_init();

        let json = r#"{
            "domain_name": "us-east.joyent.us",
            "shards": [
                {"host": "1.moray.us-east.joyent.us"},
                {"host": "2.moray.us-east.joyent.us"}
            ],
            "snaplink_cleanup_required": true,
            "listen_port": 8080,
            "max_fill_percentage": 90,
            "log_level": "info",
            "mdapi": {
                "shards": [
                    {"host": "1.buckets-mdapi.us-east.joyent.us"}
                ]
            },
            "options": {
                "max_sharks": 10,
                "max_tasks_per_assignment": 100
            }
        }"#;

        let test_config = TestConfig::from_contents(json.as_bytes());
        assert_eq!(test_config.config.domain_name, "us-east.joyent.us");
        assert_eq!(test_config.config.listen_port, 8080);
        assert_eq!(test_config.config.max_fill_percentage, 90);
        assert_eq!(test_config.config.log_level, Level::Info);
        assert_eq!(test_config.config.snaplink_cleanup_required, true);
        assert_eq!(test_config.config.mdapi.shards.len(), 1);
        assert_eq!(
            test_config.config.mdapi.shards[0].host,
            "1.buckets-mdapi.us-east.joyent.us"
        );
        assert_eq!(test_config.config.options.max_sharks, 10);
        assert_eq!(test_config.config.options.max_tasks_per_assignment, 100);
    }

    #[test]
    fn config_shard_sorting() {
        unit_test_init();

        let json = r#"{
            "domain_name": "test.domain",
            "shards": [
                {"host": "10.moray.test.domain"},
                {"host": "2.moray.test.domain"},
                {"host": "100.moray.test.domain"},
                {"host": "1.moray.test.domain"}
            ]
        }"#;

        let test_config = TestConfig::from_contents(json.as_bytes());
        // parse_config sorts shards by shard number
        assert_eq!(test_config.config.min_shard_num(), 1);
        assert_eq!(test_config.config.max_shard_num(), 100);
    }
}
