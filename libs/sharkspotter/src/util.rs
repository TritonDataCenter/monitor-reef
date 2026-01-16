/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/.
 */

/*
 * Copyright 2020 Joyent, Inc.
 * Copyright 2026 Edgecast Cloud LLC.
 */

use slog::{Drain, Level, LevelFilter, Logger, o};
use std::io;
use std::sync::Mutex;

fn create_bunyan_logger<W>(io: W, level: Level) -> Logger
where
    W: io::Write + std::marker::Send + 'static,
{
    Logger::root(
        Mutex::new(LevelFilter::new(
            slog_bunyan::with_name(env!("CARGO_PKG_NAME"), io).build(),
            level,
        ))
        .fuse(),
        o!("build-id" => env!("CARGO_PKG_VERSION")),
    )
}

pub fn init_global_logger(
    log_level: Option<Level>,
) -> slog_scope::GlobalLoggerGuard {
    let mut level = Level::Trace;

    if let Some(l) = log_level {
        level = l;
    }

    let log = create_bunyan_logger(std::io::stdout(), level);
    slog_scope::set_global_logger(log)
}
