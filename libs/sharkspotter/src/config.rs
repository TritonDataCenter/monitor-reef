/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 * Copyright 2026 Edgecast Cloud LLC.
 */

use clap::Parser;
use slog::Level;
use std::io::Error;
use std::str::FromStr;

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
        }
    }
}

/// A tool for finding all of the Manta objects that reside on a given set of
/// sharks (storage zones).
#[derive(Parser, Debug)]
#[command(name = "sharkspotter", version)]
struct Args {
    /// Beginning shard number
    #[arg(short = 'm', long, default_value_t = 1)]
    min_shard: u32,

    /// Ending shard number
    #[arg(short = 'M', long, default_value_t = 1)]
    max_shard: u32,

    /// Domain that the moray zones are in
    #[arg(short = 'd', long)]
    domain: String,

    /// Find objects that belong to this shark (can be specified multiple times)
    #[arg(short = 's', long = "shark", required = true)]
    sharks: Vec<String>,

    /// Number of records to scan per call to moray
    #[arg(short = 'c', long = "chunk-size", default_value_t = 1000)]
    chunk_size: u64,

    /// Index to begin scanning at
    #[arg(short = 'b', long = "begin", default_value_t = 0)]
    begin: u64,

    /// Index to stop scanning at
    #[arg(short = 'e', long = "end", default_value_t = 0)]
    end: u64,

    /// Output filename (default <shark>/shard_<shard_num>.objs)
    #[arg(short = 'f', long = "file")]
    output_file: Option<String>,

    /// Run with multiple threads, one per shard
    #[arg(short = 'T', long)]
    multithreaded: bool,

    /// Maximum number of threads to run with
    #[arg(short = 't', long, requires = "multithreaded")]
    max_threads: Option<usize>,

    /// Skip shark validation. Useful if shark is in readonly mode.
    #[arg(short = 'x')]
    skip_validate_sharks: bool,

    /// Output only the object ID
    #[arg(short = 'O', long = "object_id_only")]
    obj_id_only: bool,

    /// Use direct DB access instead of moray
    #[arg(short = 'D', long)]
    direct_db: bool,

    /// Set log level (trace, debug, info, warning, error, critical)
    #[arg(short = 'l', long)]
    log_level: Option<String>,
}

impl Config {
    pub fn from_args() -> Result<Config, Error> {
        let args = Args::parse();

        let log_level = if let Some(level_str) = &args.log_level {
            Level::from_str(level_str).map_err(|_| {
                let msg =
                    format!("Could not parse '{}' as a log_level", level_str);
                eprintln!("{}", msg);
                Error::other(msg)
            })?
        } else {
            Level::Debug
        };

        let mut config = Config {
            min_shard: args.min_shard,
            max_shard: args.max_shard,
            domain: args.domain,
            sharks: args.sharks,
            chunk_size: args.chunk_size,
            begin: args.begin,
            end: args.end,
            skip_validate_sharks: args.skip_validate_sharks,
            output_file: args.output_file,
            obj_id_only: args.obj_id_only,
            multithreaded: args.multithreaded,
            max_threads: args.max_threads.unwrap_or(50),
            direct_db: args.direct_db,
            log_level,
        };

        normalize_config(&mut config);
        Ok(config)
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
        let args = Args::parse_from([
            "sharkspotter",
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
        ]);

        assert!(args.skip_validate_sharks);
        assert_eq!(args.max_shard, 2);
        assert_eq!(args.min_shard, 1);
        assert_eq!(args.begin, 3);
        assert_eq!(args.end, 10);
        assert_eq!(args.chunk_size, 20);
        assert_eq!(args.output_file, Some(String::from("foo.txt")));
        assert_eq!(args.domain, String::from("east.joyent.us"));
        assert_eq!(
            args.sharks,
            vec![String::from("1.stor"), String::from("2.stor")]
        );
    }
}
