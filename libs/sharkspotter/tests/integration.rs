/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 * Copyright 2026 Edgecast Cloud LLC.
 */

// Integration tests for direct database access.
// These tests require access to real Manta infrastructure and are ignored by default.

mod direct_db {
    use sharkspotter::{
        SharkspotterMessage, config, object_id_from_manta_obj,
        run_multithreaded, util,
    };
    use slog::{Level, Logger, warn};
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

        let handle =
            thread::spawn(move || run_multithreaded(&conf, th_log, obj_tx));

        loop {
            match obj_rx.recv() {
                Ok(ssmsg) => {
                    let obj_id = object_id_from_manta_obj(&ssmsg.manta_value)
                        .map_err(std::io::Error::other)?;

                    // Assert no duplicates
                    assert!(obj_ids.insert(obj_id));
                }
                Err(e) => {
                    warn!(log, "Could not RX, TX channel dropped: {}", e);
                    break;
                }
            }
        }

        handle
            .join()
            .map_err(|_| std::io::Error::other("thread panicked"))??;

        Ok(obj_ids)
    }

    #[test]
    #[ignore] // Requires real Manta infrastructure
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
    #[ignore] // Requires DNS resolution of east.joyent.us
    // Test that the proper error is returned when we attempt to connect to a
    // non-existent rebalancer-postgres database. Use a ridiculously high
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
