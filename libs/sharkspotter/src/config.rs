/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

use clap::{value_t, App, AppSettings, Arg, ArgMatches};
use slog::Level;
use std::io::{Error, ErrorKind};
use std::str::FromStr;
use uuid::Uuid;

const MAX_THREADS: usize = 100;

#[derive(Clone, Debug)]
pub struct Config {
    pub min_shard: u32,
    pub max_shard: u32,
    pub domain: String,
    pub sharks: Vec<String>,
    pub chunk_size: u64,
    pub begin: u64,
    pub end: u64,
    pub skip_validate_sharks: bool,
    pub output_file: Option<String>,
    pub obj_id_only: bool,
    pub multithreaded: bool,
    pub max_threads: usize,
    pub direct_db: bool,
    pub log_level: Level,
    /// Mdapi endpoint for bucket object discovery (e.g., "mdapi.domain.com:2030")
    pub mdapi_endpoint: Option<String>,
    /// Owners to query for bucket objects (required for mdapi discovery)
    pub owners: Option<Vec<Uuid>>,
    /// Mdapi vnodes to query for bucket discovery. These are the virtual node
    /// numbers from the buckets-mdplacement ring, NOT moray shard numbers.
    /// If not specified, defaults to using min_shard..=max_shard (legacy behavior).
    pub mdapi_vnodes: Option<Vec<u32>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_shard: 1,
            max_shard: 1,
            domain: String::from(""),
            sharks: vec![String::from("")],
            begin: 0,
            end: 0,
            chunk_size: 1000,
            skip_validate_sharks: false,
            output_file: None,
            obj_id_only: false,
            multithreaded: false,
            max_threads: 50,
            direct_db: false,
            log_level: Level::Debug,
            mdapi_endpoint: None,
            owners: None,
            mdapi_vnodes: None,
        }
    }
}

fn parse_log_level(matches: &ArgMatches) -> Result<Level, Error> {
    let level = match value_t!(matches, "log_level", String) {
        Ok(l) => l,
        Err(e) => {
            let msg = format!("Could not parse 'log_level': {}", e);
            eprintln!("{}", msg);
            return Err(Error::new(ErrorKind::Other, msg));
        }
    };

    Level::from_str(&level).map_err(|_| {
        let msg = format!("Could not parse '{}' as a log_level", level);
        eprintln!("{}", msg);
        Error::new(ErrorKind::Other, msg)
    })
}

impl<'a, 'b> Config {
    pub fn get_app() -> App<'a, 'b> {
        let version = env!("CARGO_PKG_VERSION");
        App::new("sharkspotter")
            .version(version)
            .about("A tool for finding all of the Manta objects that reside \
            on a given set of sharks (storage zones).")
            .setting(AppSettings::ArgRequiredElseHelp)
            .arg(Arg::with_name("min_shard")
                .short("m")
                .long("min_shard")
                .value_name("MIN_SHARD")
                .help("Beginning shard number (default: 1)")
                .takes_value(true))
            .arg(Arg::with_name("max_shard")
                .short("M")
                .long("max_shard")
                .value_name("MAX_SHARD")
                .help("Ending shard number (default: 1)")
                .takes_value(true))
            .arg(Arg::with_name("domain")
                .short("d")
                .long("domain")
                .value_name("MORAY_DOMAIN")
                .help("Domain that the moray zones are in")
                .required(true)
                .takes_value(true))
            .arg(Arg::with_name("shark")
                .short("s")
                .long("shark")
                .value_name("STORAGE_ID")
                .help("Find objects that belong to this shark")
                .required(true)
                .number_of_values(1) // only 1 value per occurrence
                .multiple(true) // allow multiple occurrences
                .takes_value(true))
            .arg(Arg::with_name("chunk-size")
                .short("c")
                .long("chunk-size")
                .value_name("NUM_RECORDS")
                .help("number of records to scan per call to moray (default: \
                100)")
                .takes_value(true))
            .arg(Arg::with_name("begin-index")
                .short("b")
                .long("begin")
                .value_name("INDEX")
                .help("index to being scanning at (default: 0)")
                .takes_value(true))
            .arg(Arg::with_name("end-index")
                .short("e")
                .long("end")
                .value_name("INDEX")
                .help("index to stop scanning at (default: 0)")
                .takes_value(true))
            .arg(Arg::with_name("output_file")
                .short("f")
                .long("file")
                .value_name("FILE_NAME")
                .help("output filename (default <shark>/shard_<shard_num>.objs")
                .takes_value(true))
            .arg(Arg::with_name("multithreaded")
                .short("T")
                .help("Run with multiple threads, one per shard")
                .long("multithreaded")
                .takes_value(false))
            .arg(Arg::with_name("max_threads")
                .short("t")
                .help("maximum number of threads to run with")
                .long("max_threads")
                .requires("multithreaded")
                .takes_value(true))
            .arg(Arg::with_name("skip_validate_sharks")
                .short("x")
                .help("Skip shark validation. Useful if shark is in readonly \
                mode.")
                .takes_value(false))
            .arg(Arg::with_name("obj_id_only")
                .short("O")
                .long("object_id_only")
                .help("Output only the object ID")
                .takes_value(false))
            .arg(Arg::with_name("direct_db")
                .short("-D")
                .long("direct_db")
                .help("use direct DB access instead of moray")
                .takes_value(false))
            .arg(Arg::with_name("log_level")
                .short("l")
                .long("log_level")
                .help("Set log level")
                .takes_value(true))
            .arg(Arg::with_name("mdapi_endpoint")
                .long("mdapi-endpoint")
                .value_name("ENDPOINT")
                .help("Mdapi endpoint for bucket object discovery (e.g., mdapi.domain:2030)")
                .takes_value(true))
            .arg(Arg::with_name("owners")
                .long("owners")
                .value_name("UUID")
                .help("Owner UUIDs to query for bucket objects (comma-separated or multiple flags)")
                .number_of_values(1)
                .multiple(true)
                .takes_value(true))
            .arg(Arg::with_name("mdapi_vnodes")
                .long("mdapi-vnodes")
                .value_name("VNODE")
                .help("Mdapi vnodes to query for bucket discovery (from buckets-mdplacement ring)")
                .number_of_values(1)
                .multiple(true)
                .takes_value(true))
    }

    // TODO: This has grown over time and is now causing a clippy warning.
    // We should consider using a yaml file to parse the matches.
    #[allow(clippy::cognitive_complexity)]
    fn config_from_matches(matches: ArgMatches) -> Result<Config, Error> {
        let mut config = Config::default();

        if let Ok(max_shard) = value_t!(matches, "max_shard", u32) {
            config.max_shard = max_shard;
        }

        if let Ok(min_shard) = value_t!(matches, "min_shard", u32) {
            config.min_shard = min_shard;
        }

        if let Ok(begin) = value_t!(matches, "begin-index", u64) {
            config.begin = begin;
        }

        if let Ok(end) = value_t!(matches, "end-index", u64) {
            config.end = end;
        }

        if let Ok(chunk_size) = value_t!(matches, "chunk-size", u64) {
            config.chunk_size = chunk_size;
        }

        if let Ok(output_file) = value_t!(matches, "output_file", String) {
            config.output_file = Some(output_file);
        }

        if matches.is_present("skip_validate_sharks") {
            config.skip_validate_sharks = true;
        }

        if matches.is_present("obj_id_only") {
            config.obj_id_only = true;
        }

        if matches.is_present("multithreaded") {
            config.multithreaded = true;
        }

        if matches.is_present("direct_db") {
            config.direct_db = true;
        }

        if let Ok(max_threads) = value_t!(matches, "max_threads", usize) {
            config.max_threads = max_threads;
        }

        if matches.is_present("log_level") {
            config.log_level = parse_log_level(&matches)?;
        }

        config.domain = matches.value_of("domain").unwrap().to_string();
        config.sharks = matches
            .values_of("shark")
            .unwrap()
            .map(String::from)
            .collect();

        // Parse mdapi endpoint
        if let Some(endpoint) = matches.value_of("mdapi_endpoint") {
            config.mdapi_endpoint = Some(endpoint.to_string());
        }

        // Parse owner UUIDs
        if let Some(owners) = matches.values_of("owners") {
            let mut parsed_owners = Vec::new();
            for owner_str in owners {
                match Uuid::parse_str(owner_str) {
                    Ok(uuid) => parsed_owners.push(uuid),
                    Err(e) => {
                        let msg = format!(
                            "Invalid owner UUID '{}': {}",
                            owner_str, e
                        );
                        eprintln!("{}", msg);
                        return Err(Error::new(ErrorKind::Other, msg));
                    }
                }
            }
            if !parsed_owners.is_empty() {
                config.owners = Some(parsed_owners);
            }
        }

        // Parse mdapi vnodes
        if let Some(vnodes) = matches.values_of("mdapi_vnodes") {
            let mut parsed_vnodes = Vec::new();
            for vnode_str in vnodes {
                match vnode_str.parse::<u32>() {
                    Ok(vnode) => parsed_vnodes.push(vnode),
                    Err(e) => {
                        let msg = format!(
                            "Invalid mdapi vnode '{}': {}",
                            vnode_str, e
                        );
                        eprintln!("{}", msg);
                        return Err(Error::new(ErrorKind::Other, msg));
                    }
                }
            }
            if !parsed_vnodes.is_empty() {
                config.mdapi_vnodes = Some(parsed_vnodes);
            }
        }

        normalize_config(&mut config);

        Ok(config)
    }

    pub fn from_args() -> Result<Config, Error> {
        let matches = Self::get_app().get_matches();
        Self::config_from_matches(matches)
    }
}

pub fn normalize_config(conf: &mut Config) {
    if conf.max_threads > MAX_THREADS {
        eprintln!(
            "Max threads of {} exceeds max.  Setting to {}.",
            conf.max_threads, MAX_THREADS
        );
        conf.max_threads = MAX_THREADS;
    }

    if conf.begin > 0 && conf.end > 0 && conf.end < conf.begin {
        eprintln!("'end' is smaller than 'begin', discard 'end' value given");
        conf.end = 0;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_args() {
        let args = vec![
            "target/debug/sharkspotter",
            "-x",
            "--domain",
            "east.joyent.us",
            "--shark",
            "1.stor",
            "--shark",
            "2.stor",
            "-m",
            "1",
            "-M",
            "2",
            "-e",
            "10",
            "-b",
            "3",
            "-c",
            "20",
            "-f",
            "foo.txt",
        ];

        let matches = Config::get_app().get_matches_from(args);
        let config = Config::config_from_matches(matches).expect("config");

        assert!(config.skip_validate_sharks);

        assert_eq!(config.max_shard, 2);
        assert_eq!(config.min_shard, 1);
        assert_eq!(config.begin, 3);
        assert_eq!(config.end, 10);
        assert_eq!(config.chunk_size, 20);

        assert_eq!(config.output_file, Some(String::from("foo.txt")));
        assert_eq!(config.domain, String::from("east.joyent.us"));

        assert_eq!(
            config.sharks,
            vec![String::from("1.stor"), String::from("2.stor")]
        );
    }

    #[test]
    fn parse_mdapi_args() {
        let args = vec![
            "target/debug/sharkspotter",
            "--domain",
            "east.joyent.us",
            "--shark",
            "1.stor",
            "--mdapi-endpoint",
            "mdapi.east.joyent.us:2030",
            "--owners",
            "550e8400-e29b-41d4-a716-446655440000",
            "--owners",
            "660e8400-e29b-41d4-a716-446655440001",
        ];

        let matches = Config::get_app().get_matches_from(args);
        let config = Config::config_from_matches(matches).expect("config");

        assert_eq!(
            config.mdapi_endpoint,
            Some(String::from("mdapi.east.joyent.us:2030"))
        );

        let owners = config.owners.expect("owners should be set");
        assert_eq!(owners.len(), 2);
        assert_eq!(
            owners[0],
            Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
        );
        assert_eq!(
            owners[1],
            Uuid::parse_str("660e8400-e29b-41d4-a716-446655440001").unwrap()
        );
    }

    #[test]
    fn parse_invalid_owner_uuid() {
        let args = vec![
            "target/debug/sharkspotter",
            "--domain",
            "east.joyent.us",
            "--shark",
            "1.stor",
            "--owners",
            "not-a-valid-uuid",
        ];

        let matches = Config::get_app().get_matches_from(args);
        let result = Config::config_from_matches(matches);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_config_clamps_max_threads() {
        let mut config = Config::default();
        config.max_threads = 200;
        normalize_config(&mut config);
        assert_eq!(config.max_threads, MAX_THREADS);
    }

    #[test]
    fn normalize_config_keeps_valid_threads() {
        let mut config = Config::default();
        config.max_threads = 50;
        normalize_config(&mut config);
        assert_eq!(config.max_threads, 50);
    }

    #[test]
    fn normalize_config_resets_end_less_than_begin() {
        let mut config = Config::default();
        config.begin = 100;
        config.end = 50;
        normalize_config(&mut config);
        assert_eq!(config.end, 0);
    }

    #[test]
    fn normalize_config_preserves_valid_end() {
        let mut config = Config::default();
        config.begin = 50;
        config.end = 100;
        normalize_config(&mut config);
        assert_eq!(config.end, 100);
    }

    #[test]
    fn normalize_config_zero_begin_end_unchanged() {
        let mut config = Config::default();
        config.begin = 0;
        config.end = 0;
        normalize_config(&mut config);
        assert_eq!(config.begin, 0);
        assert_eq!(config.end, 0);
    }

    #[test]
    fn config_default_values() {
        let config = Config::default();
        assert_eq!(config.min_shard, 1);
        assert_eq!(config.max_shard, 1);
        assert_eq!(config.chunk_size, 1000);
        assert_eq!(config.begin, 0);
        assert_eq!(config.end, 0);
        assert!(!config.skip_validate_sharks);
        assert!(!config.multithreaded);
        assert_eq!(config.max_threads, 50);
        assert!(!config.direct_db);
        assert!(config.mdapi_endpoint.is_none());
        assert!(config.owners.is_none());
        assert!(config.mdapi_vnodes.is_none());
    }

    #[test]
    fn parse_mdapi_vnodes() {
        let args = vec![
            "target/debug/sharkspotter",
            "--domain",
            "east.joyent.us",
            "--shark",
            "1.stor",
            "--mdapi-endpoint",
            "mdapi.east.joyent.us:2030",
            "--owners",
            "550e8400-e29b-41d4-a716-446655440000",
            "--mdapi-vnodes",
            "0",
            "--mdapi-vnodes",
            "100",
            "--mdapi-vnodes",
            "200",
        ];

        let matches = Config::get_app().get_matches_from(args);
        let config = Config::config_from_matches(matches).expect("config");

        let vnodes = config.mdapi_vnodes.expect("vnodes should be set");
        assert_eq!(vnodes.len(), 3);
        assert_eq!(vnodes[0], 0);
        assert_eq!(vnodes[1], 100);
        assert_eq!(vnodes[2], 200);
    }

    #[test]
    fn parse_invalid_mdapi_vnode() {
        let args = vec![
            "target/debug/sharkspotter",
            "--domain",
            "east.joyent.us",
            "--shark",
            "1.stor",
            "--mdapi-vnodes",
            "not-a-number",
        ];

        let matches = Config::get_app().get_matches_from(args);
        let result = Config::config_from_matches(matches);
        assert!(result.is_err());
    }
}
