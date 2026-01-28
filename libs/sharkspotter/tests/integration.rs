/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 */

extern crate assert_cli;

#[cfg(test)]
mod cli {
    use assert_cli;

    // Test that invoking with no arguments produces a clap error
    // mentioning both required arguments.  Uses clap's
    // get_matches_from_safe directly so the binary doesn't need
    // to be built and the test is not version-sensitive.
    #[test]
    fn missing_all_args() {
        let app = sharkspotter::config::Config::get_app();
        let result = app.get_matches_from_safe(&["sharkspotter"]);
        assert!(result.is_err(), "Expected error when no args provided");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("--domain") || msg.contains("MORAY_DOMAIN"),
            "Error should mention --domain: {}",
            msg
        );
        assert!(
            msg.contains("--shark") || msg.contains("STORAGE_ID"),
            "Error should mention --shark: {}",
            msg
        );
    }

    #[test]
    fn missing_all_required_args() {
        const ERROR_STRING: &str =
            "error: The following required arguments were not provided:
    --domain <MORAY_DOMAIN>
    --shark <STORAGE_ID>";

        assert_cli::Assert::main_binary()
            .with_args(&["-m 1 -M 1 -c 1000"])
            .fails()
            .and()
            .stderr()
            .contains(ERROR_STRING)
            .unwrap();
    }

    #[test]
    fn invalid_arg() {
        const ERROR_STRING: &str = "error: Found argument '-z' which wasn't \
                                    expected, or isn't valid in this context";

        assert_cli::Assert::main_binary()
            .with_args(&["-z foo"])
            .fails()
            .and()
            .stderr()
            .contains(ERROR_STRING)
            .unwrap()
    }

    #[test]
    fn missing_shark() {
        const ERROR_STRING: &str =
            "error: The following required arguments were not provided:
    --shark <STORAGE_ID>";

        assert_cli::Assert::main_binary()
            .with_args(&["-d east.joyent.us -m 1 -M 1"])
            .fails()
            .and()
            .stderr()
            .contains(ERROR_STRING)
            .unwrap()
    }

    #[test]
    fn missing_domain() {
        const ERROR_STRING: &str =
            "error: The following required arguments were not provided:
    --domain <MORAY_DOMAIN>";

        assert_cli::Assert::main_binary()
            .with_args(&["-m 1 -M 1 -s 1.stor"])
            .fails()
            .and()
            .stderr()
            .contains(ERROR_STRING)
            .unwrap()
    }
}

mod direct_db {
    use sharkspotter::{
        config, object_id_from_manta_obj, run_multithreaded, util,
        SharkspotterMessage,
    };
    use slog::{warn, Level, Logger};
    use std::collections::HashSet;
    use std::thread;

    fn get_ids_from_direct_db(
        conf: config::Config,
        log: Logger,
    ) -> Result<HashSet<String>, std::io::Error> {
        let mut obj_ids = HashSet::new();
        let th_log = log.clone();
        let (obj_tx, obj_rx): (
            crossbeam_channel::Sender<SharkspotterMessage>,
            crossbeam_channel::Receiver<SharkspotterMessage>,
        ) = crossbeam_channel::bounded(5);

        let handle = thread::spawn(move || {
            // TODO: call run_multithreaded directly?
            run_multithreaded(&conf, th_log, obj_tx)
        });

        loop {
            match obj_rx.recv() {
                Ok(ssmsg) => {
                    let obj_id = object_id_from_manta_obj(&ssmsg.manta_value)
                        .expect("obj id");

                    // Assert no duplicates
                    assert!(obj_ids.insert(obj_id));
                }
                Err(e) => {
                    warn!(log, "Could not RX, TX channel dropped: {}", e);
                    break;
                }
            }
        }

        if let Err(e) = handle.join().expect("thread join") {
            return Err(e);
        }

        Ok(obj_ids)
    }

    #[test]
    #[ignore] // Requires Joyent Moray infrastructure (DNS resolution of moray zones)
    fn directdb_test() {
        // The log level has a significant impact on the runtime of this test.
        // If an error is encountered consider bumping this log level to
        // Trace and re-running.
        let conf = config::Config {
            direct_db: true,
            min_shard: 1,
            max_shard: 2,
            domain: "east.joyent.us".to_string(),
            sharks: vec!["1.stor.east.joyent.us".to_string()],
            log_level: Level::Info,
            ..Default::default()
        };
        let _guard = util::init_global_logger(Some(conf.log_level));
        let log = slog_scope::logger();

        let first_count = get_ids_from_direct_db(conf.clone(), log.clone())
            .expect("first count")
            .len();
        let second_count = get_ids_from_direct_db(conf.clone(), log.clone())
            .expect("second count")
            .len();

        assert_eq!(first_count, second_count);
    }

    #[test]
    // Test that the proper error is returned when we attempt to connect to a
    // non-existant rebalancer-postgres database.  Use a ridiculously high
    // shard number to ensure a connection failure.
    fn directdb_test_connect_fail() {
        let conf = config::Config {
            direct_db: true,
            min_shard: 999999,
            max_shard: 999999,
            domain: "east.joyent.us".to_string(),
            sharks: vec!["1.stor.east.joyent.us".to_string()],
            log_level: Level::Trace,
            ..Default::default()
        };
        let _guard = util::init_global_logger(Some(conf.log_level));
        let log = slog_scope::logger();

        assert!(get_ids_from_direct_db(conf.clone(), log.clone()).is_err());
    }
}
